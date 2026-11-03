use super::{
    adapters,
    config_edit::{jsonc_set, toml_set, upsert_managed_block, upsert_yaml_patch},
    model::{
        content_hash, AssetKind, AssetScope, AssetSnapshot, ConfigEditFormat, Diagnostic,
        EnvironmentAsset, MigrationRun, OpenCodeConfigVersion, Readiness, RenderContribution,
        RenderPlan, RenderedFile, RenderedFileBody, RenderedItem, SnapshotFile, SourceConflict,
        TargetEnvironment, TargetScope, VersionSupport,
    },
    readiness::{
        configuration_requirements, program_available, script_requirements, CapabilityReport,
    },
    run_store::RunStore,
};
use crate::canonical::SourceTool;
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

const CODEX_DEFAULT_PROJECT_DOC_MAX_BYTES: u64 = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DocumentFormat {
    Text,
    Jsonc,
    Toml,
    Yaml,
}

#[derive(Debug, Clone)]
struct PendingDocument {
    target_scope: TargetScope,
    scope_root: PathBuf,
    text: String,
    owners: Vec<String>,
    keys: BTreeMap<String, String>,
    contributions: Vec<(String, RenderContribution)>,
}

#[derive(Debug, Clone, Default)]
struct TargetFileOwner {
    item_id: String,
}

/// Build an in-memory target plan. This function does not write target files,
/// start scripts, connect to services, install dependencies or evaluate
/// configuration expressions.
pub fn render_environment(
    runs: &RunStore,
    run: &MigrationRun,
    environment: &TargetEnvironment,
) -> Result<RenderPlan, String> {
    run.validate()?;
    if runs.load(&run.id)? != *run {
        return Err("migration changed; reload before rendering its environment".to_string());
    }
    let target = run
        .target
        .as_ref()
        .ok_or_else(|| "choose a migration target before rendering".to_string())?;
    if target != &environment.target {
        return Err("target environment does not match the migration target".to_string());
    }
    if !(target.project_root.is_absolute()
        && environment.roots.user_home.is_absolute()
        && environment.roots.tool_home(target.tool).is_absolute())
    {
        return Err("target roots must be explicit absolute paths".to_string());
    }
    let selected: BTreeSet<_> = run
        .assets
        .iter()
        .map(|asset| asset.asset.id.clone())
        .collect();
    for asset in &run.assets {
        for file in &asset.files {
            if !selected.contains(&asset.asset.id) {
                return Err("rendering contains an implicit environment item".to_string());
            }
            super::model::validate_resource_path(&file.snapshot_path)?;
        }
    }
    let run_directory = runs.run_directory(&run.id)?;
    let assets = run
        .assets
        .iter()
        .map(|asset| (asset.asset.id.clone(), asset.clone()))
        .collect();
    let source_paths = run
        .assets
        .iter()
        .flat_map(|asset| &asset.files)
        .filter_map(|file| file.source.canonicalize().ok())
        .chain(
            run.sessions
                .iter()
                .filter_map(|session| session.source_document.as_ref())
                .map(|document| document.resolved_path.clone()),
        )
        .collect();
    let mut renderer = Renderer::new(
        environment.clone(),
        run_directory,
        selected,
        assets,
        source_paths,
    );
    for snapshot in &run.assets {
        renderer.render(snapshot);
    }
    Ok(renderer.finish())
}

struct Renderer {
    environment: TargetEnvironment,
    run_root: PathBuf,
    selected: BTreeSet<String>,
    assets: BTreeMap<String, AssetSnapshot>,
    source_paths: BTreeSet<PathBuf>,
    files: BTreeMap<PathBuf, RenderedFile>,
    file_owners: BTreeMap<PathBuf, TargetFileOwner>,
    documents: BTreeMap<PathBuf, PendingDocument>,
    items: Vec<RenderedItem>,
    conflicts: Vec<SourceConflict>,
    diagnostics: Vec<Diagnostic>,
}

impl Renderer {
    fn new(
        environment: TargetEnvironment,
        run_root: PathBuf,
        selected: BTreeSet<String>,
        assets: BTreeMap<String, AssetSnapshot>,
        source_paths: BTreeSet<PathBuf>,
    ) -> Self {
        Self {
            environment,
            run_root,
            selected,
            assets,
            source_paths,
            files: BTreeMap::new(),
            file_owners: BTreeMap::new(),
            documents: BTreeMap::new(),
            items: vec![],
            conflicts: vec![],
            diagnostics: vec![],
        }
    }

    fn render(&mut self, snapshot: &AssetSnapshot) {
        let asset = &snapshot.asset;
        let (target_scope, scope_root) = self.target_scope(asset);
        let mut item = RenderedItem {
            item_id: asset.id.clone(),
            kind: asset.kind,
            target_scope,
            format: target_format(&self.environment, asset),
            readiness: Readiness::Available,
            action: "plan-write".to_string(),
            message: "内容可以写入目标的原生位置".to_string(),
            next_steps: vec![],
            evidence: vec![],
            diagnostics: asset.diagnostics.clone(),
            target_paths: vec![],
        };
        if !asset.source_active {
            item.readiness = Readiness::NeedsConfiguration;
            item.message = "来源定义被更高优先级内容遮蔽，需选择生效版本".to_string();
            item.next_steps
                .push("重新扫描或在清单中选择另一个生效定义".to_string());
        }
        if asset
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "credentialRequired")
        {
            item.readiness = Readiness::NeedsConfiguration;
            item.message = "来源中的敏感值未迁移，目标仍需补充凭据".to_string();
            item.next_steps
                .push("在目标工具中重新配置被移除的凭据字段".to_string());
        }
        if scope_root.is_err() {
            item.readiness = Readiness::Failed;
            item.message = scope_root
                .as_ref()
                .err()
                .cloned()
                .unwrap_or_else(|| "target scope is unavailable".to_string());
            self.items.push(item);
            return;
        }
        let scope_root = scope_root.expect("checked above");
        match asset.kind {
            AssetKind::Instruction | AssetKind::Memory => {
                self.render_instruction(snapshot, scope_root, &mut item);
            }
            AssetKind::Skill => self.render_skill(snapshot, scope_root, &mut item),
            AssetKind::Mcp => self.render_mcp(snapshot, scope_root, &mut item),
            AssetKind::Script => self.render_script(snapshot, scope_root, &mut item),
            AssetKind::Command => self.render_command(snapshot, scope_root, &mut item),
        }
        self.items.push(item);
    }

    fn finish(mut self) -> RenderPlan {
        self.apply_instruction_limits();
        for (path, document) in self.documents {
            if path
                .canonicalize()
                .is_ok_and(|resolved| self.source_paths.contains(&resolved))
            {
                self.diagnostics.push(Diagnostic::new(
                    "sourceTargetOverlap",
                    "generated target document is also a source file",
                    Some(path.clone()),
                ));
                for owner in &document.owners {
                    if let Some(item) = self.items.iter_mut().find(|item| &item.item_id == owner) {
                        item.readiness = Readiness::NeedsConfiguration;
                        item.message = "来源与目标是同一物理文件，不能直接覆盖来源内容".to_string();
                        item.next_steps
                            .push("选择另一个目标项目或手工确认同一文件的最终内容".to_string());
                        item.diagnostics.push(Diagnostic::new(
                            "sourceTargetOverlap",
                            item.message.clone(),
                            Some(path.clone()),
                        ));
                    }
                }
            }
            for owner in &document.owners {
                if let Some(item) = self.items.iter_mut().find(|item| &item.item_id == owner) {
                    if !item.target_paths.contains(&path) {
                        item.target_paths.push(path.clone());
                    }
                }
            }
            self.files.insert(
                path.clone(),
                RenderedFile {
                    target_scope: document.target_scope,
                    scope_root: document.scope_root,
                    target_path: path,
                    source_item_ids: document.owners,
                    content: RenderedFileBody::Bytes(document.text.into_bytes()),
                    contributions: document.contributions,
                    executable: false,
                },
            );
        }
        RenderPlan {
            target: self.environment.target,
            files: self.files.into_values().collect(),
            items: self.items,
            conflicts: self.conflicts,
            diagnostics: self.diagnostics,
        }
    }

    fn apply_instruction_limits(&mut self) {
        let Some(codex_project_doc_limit) = self.codex_project_doc_limit() else {
            return;
        };
        let project_root = self.environment.target.project_root.clone();
        let instruction_path = self.instruction_path(TargetScope::Project);
        let Ok(instruction_path) = instruction_path else {
            return;
        };
        if !instruction_path.starts_with(&project_root) {
            return;
        }
        let Some(document) = self.documents.get(&instruction_path) else {
            return;
        };
        let size = document.text.len() as u64;
        if size <= codex_project_doc_limit {
            return;
        }
        let reason = format!(
            "合并后的项目指令为 {size} 字节，超过目标 Codex 的 {codex_project_doc_limit} 字节上限"
        );
        for owner in &document.owners {
            if let Some(item) = self.items.iter_mut().find(|item| &item.item_id == owner) {
                if matches!(item.kind, AssetKind::Instruction | AssetKind::Memory) {
                    item.readiness = Readiness::NeedsConfiguration;
                    item.message = reason.clone();
                    item.next_steps
                        .push("减少迁入指令或提高目标 Codex 的项目文档字节上限".to_string());
                    item.diagnostics.push(Diagnostic::new(
                        "targetInstructionLimit",
                        reason.clone(),
                        Some(instruction_path.clone()),
                    ));
                }
            }
        }
    }

    fn codex_project_doc_limit(&self) -> Option<u64> {
        if self.environment.target.tool != SourceTool::Codex {
            return None;
        }
        for path in [
            self.environment
                .target
                .project_root
                .join(".codex/config.toml"),
            self.environment.roots.codex.join("config.toml"),
        ] {
            let Some(text) = adapters::read_text(&path).ok().flatten() else {
                continue;
            };
            if let Ok(value) = adapters::parse_toml(&text) {
                if let Some(limit) = value.get("project_doc_max_bytes").and_then(Value::as_u64) {
                    return Some(limit);
                }
            }
        }
        Some(CODEX_DEFAULT_PROJECT_DOC_MAX_BYTES)
    }
}

impl Renderer {
    fn target_scope(&self, asset: &EnvironmentAsset) -> (TargetScope, Result<PathBuf, String>) {
        if self.environment.target.tool == SourceTool::Codex && asset.kind == AssetKind::Mcp {
            return (TargetScope::User, Ok(self.environment.roots.codex.clone()));
        }
        if self.environment.target.tool == SourceTool::Dsh && asset.kind == AssetKind::Mcp {
            return (TargetScope::Profile, self.dsh_profile_root());
        }
        match asset.scope {
            AssetScope::Project => (
                TargetScope::Project,
                Ok(self.environment.target.project_root.clone()),
            ),
            AssetScope::User => (
                TargetScope::User,
                Ok(self
                    .environment
                    .roots
                    .tool_home(self.environment.target.tool)
                    .to_path_buf()),
            ),
            AssetScope::Profile => {
                if self.environment.target.tool == SourceTool::Dsh {
                    (TargetScope::Profile, self.dsh_profile_root())
                } else {
                    (
                        TargetScope::User,
                        Ok(self
                            .environment
                            .roots
                            .tool_home(self.environment.target.tool)
                            .to_path_buf()),
                    )
                }
            }
        }
    }

    fn dsh_profile_root(&self) -> Result<PathBuf, String> {
        let profile = self
            .environment
            .dsh_profile
            .as_deref()
            .filter(|value| {
                !value.is_empty()
                    && *value != "."
                    && *value != ".."
                    && value.chars().all(|value| {
                        value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.')
                    })
            })
            .ok_or_else(|| {
                "choose one valid DSH profile before rendering profile-scoped content".to_string()
            })?;
        Ok(self.environment.roots.dsh.join("profiles").join(profile))
    }

    fn add_copy(
        &mut self,
        item: &mut RenderedItem,
        file: &SnapshotFile,
        target_path: PathBuf,
        scope_root: &Path,
        target_scope: TargetScope,
    ) {
        super::model::validate_resource_path(&file.snapshot_path)
            .expect("snapshot paths are validated before rendering");
        let snapshot_path = self.run_root.join(&file.snapshot_path);
        if !snapshot_path.is_file() {
            self.fail_item(item, "snapshotMissing", "迁移快照文件不存在");
            return;
        }
        item.target_paths.push(target_path.clone());
        if let Ok(target_resolved) = target_path.canonicalize() {
            if file
                .source
                .canonicalize()
                .is_ok_and(|source| source == target_resolved)
            {
                let current = fs::read(&target_path).unwrap_or_default();
                if content_hash(&current) == file.content_hash {
                    item.action = "keep-existing".to_string();
                    item.evidence
                        .push("来源与目标是同一物理文件，内容相同".to_string());
                } else {
                    item.readiness = Readiness::NeedsConfiguration;
                    item.message = "来源与目标是同一物理文件，但快照后内容已经变化".to_string();
                    item.next_steps
                        .push("选择另一个目标项目或手工确认同一文件的最终内容".to_string());
                    self.diagnostics.push(Diagnostic::new(
                        "sourceTargetOverlap",
                        item.message.clone(),
                        Some(target_path.clone()),
                    ));
                }
                return;
            }
        }
        let owner = TargetFileOwner {
            item_id: item.item_id.clone(),
        };
        let rendered_body = RenderedFileBody::Copy {
            snapshot_path: snapshot_path.clone(),
            content_hash: file.content_hash.clone(),
        };
        let contribution = RenderContribution::WholeFile {
            body: rendered_body.clone(),
            executable: file.executable,
        };
        if let Some(existing) = self.file_owners.get(&target_path) {
            if existing.item_id != owner.item_id {
                let other_id = existing.item_id.clone();
                let message = format!("多个来源项映射到同一目标文件: {}", target_path.display());
                self.diagnostics.push(Diagnostic::new(
                    "sourceTargetConflict",
                    message.clone(),
                    Some(target_path.clone()),
                ));
                self.note_source_conflict(
                    &target_path,
                    "targetFile",
                    vec![other_id.clone(), owner.item_id.clone()],
                );
                if let Some(other) = self
                    .items
                    .iter_mut()
                    .find(|candidate| candidate.item_id == other_id)
                {
                    other.readiness = Readiness::NeedsConfiguration;
                    other.message = message.clone();
                    other
                        .next_steps
                        .push("在执行前选择保留、合并或替换的目标内容".to_string());
                }
                item.readiness = Readiness::NeedsConfiguration;
                item.message = message;
                item.next_steps
                    .push("在执行前选择保留、合并或替换的目标内容".to_string());
                if let Some(rendered) = self.files.get_mut(&target_path) {
                    rendered
                        .contributions
                        .push((owner.item_id.clone(), contribution));
                }
            }
            return;
        }
        self.file_owners.insert(target_path.clone(), owner);
        self.add_file(RenderedFile {
            target_scope,
            scope_root: scope_root.to_path_buf(),
            target_path,
            source_item_ids: vec![item.item_id.clone()],
            content: rendered_body,
            contributions: vec![(item.item_id.clone(), contribution)],
            executable: file.executable,
        });
    }

    fn add_file(&mut self, file: RenderedFile) {
        self.files.insert(file.target_path.clone(), file);
    }

    fn add_document_contribution(
        &mut self,
        path: &Path,
        item_id: &str,
        contribution: RenderContribution,
    ) {
        if let Some(document) = self.documents.get_mut(path) {
            document
                .contributions
                .push((item_id.to_string(), contribution));
        }
    }

    fn note_source_conflict(&mut self, target_path: &Path, kind: &str, mut item_ids: Vec<String>) {
        item_ids.sort();
        item_ids.dedup();
        if item_ids.len() < 2 {
            return;
        }
        let id = content_hash(
            format!(
                "{}\0{}\0{}",
                target_path.display(),
                kind,
                item_ids.join("\0")
            )
            .as_bytes(),
        );
        if self.conflicts.iter().all(|conflict| conflict.id != id) {
            self.conflicts.push(SourceConflict {
                id,
                target_path: target_path.to_path_buf(),
                kind: kind.to_string(),
                item_ids,
            });
        }
    }

    fn mark_existing_item_conflict(&mut self, item_id: &str, target_path: &Path, message: &str) {
        if let Some(item) = self.items.iter_mut().find(|item| item.item_id == item_id) {
            item.readiness = Readiness::NeedsConfiguration;
            item.message = message.to_string();
            item.next_steps
                .push("在执行前选择保留、合并或替换的目标内容".to_string());
            item.diagnostics.push(Diagnostic::new(
                "sourceTargetConflict",
                message,
                Some(target_path.to_path_buf()),
            ));
        }
    }

    fn fail_item(&mut self, item: &mut RenderedItem, code: &str, message: impl Into<String>) {
        let message = message.into();
        item.readiness = Readiness::Failed;
        item.message = message.clone();
        item.next_steps
            .push("修复该错误后重新扫描和预览".to_string());
        item.diagnostics.push(Diagnostic::new(code, message, None));
    }

    fn unsupported(&mut self, item: &mut RenderedItem, message: impl Into<String>) {
        let message = message.into();
        item.readiness = Readiness::Unsupported;
        item.message = message.clone();
        item.diagnostics
            .push(Diagnostic::new("targetUnsupported", message, None));
    }

    fn needs_configuration(
        &mut self,
        item: &mut RenderedItem,
        message: impl Into<String>,
        steps: Vec<String>,
    ) {
        item.readiness = Readiness::NeedsConfiguration;
        item.message = message.into();
        for step in steps {
            if !item.next_steps.contains(&step) {
                item.next_steps.push(step);
            }
        }
    }

    fn read_primary(
        &mut self,
        item: &mut RenderedItem,
        asset: &AssetSnapshot,
    ) -> Option<(String, SnapshotFile)> {
        let file = asset
            .files
            .iter()
            .find(|file| {
                file.relative_path
                    .extension()
                    .and_then(|value| value.to_str())
                    == Some("md")
            })
            .or_else(|| asset.files.first())
            .cloned();
        let Some(file) = file else {
            self.fail_item(item, "snapshotContentMissing", "环境项没有可读取的内容");
            return None;
        };
        let path = self.run_root.join(&file.snapshot_path);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.fail_item(
                    item,
                    "snapshotReadFailed",
                    format!("无法读取迁移快照: {error}"),
                );
                return None;
            }
        };
        if content_hash(&bytes) != file.content_hash {
            self.fail_item(
                item,
                "snapshotHashMismatch",
                "迁移快照内容与发现时记录的摘要不一致",
            );
            return None;
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                self.fail_item(
                    item,
                    "snapshotNotText",
                    "该环境项需要文本内容，但快照不是 UTF-8 文本",
                );
                return None;
            }
        };
        Some((text, file))
    }

    fn instruction_path(&self, scope: TargetScope) -> Result<PathBuf, String> {
        let name = if self.environment.target.tool == SourceTool::Claude {
            "CLAUDE.md"
        } else {
            "AGENTS.md"
        };
        match scope {
            TargetScope::Project => Ok(self.environment.target.project_root.join(name)),
            TargetScope::User => Ok(self
                .environment
                .roots
                .tool_home(self.environment.target.tool)
                .join(name)),
            TargetScope::Profile => Ok(self.dsh_profile_root()?.join(name)),
        }
    }

    fn instruction_managed_path(
        &self,
        asset: &AssetSnapshot,
        scope_root: &Path,
        file: &SnapshotFile,
    ) -> PathBuf {
        scope_root
            .join(".session-loom/instructions")
            .join(&asset.asset.id)
            .join(&file.relative_path)
    }

    fn relative_managed_ref(scope_root: &Path, target: &Path) -> Result<String, String> {
        target
            .strip_prefix(scope_root)
            .map(|value| value.to_string_lossy().replace('\\', "/"))
            .map_err(|_| "managed instruction path leaves its target scope".to_string())
    }

    fn instruction_body(
        &mut self,
        item: &mut RenderedItem,
        asset: &AssetSnapshot,
        source: &str,
        source_file: &SnapshotFile,
        scope_root: &Path,
    ) -> Option<String> {
        let heading = match asset.asset.kind {
            AssetKind::Memory => "迁移的记忆",
            AssetKind::Instruction => "迁移的指令",
            _ => "迁移内容",
        };
        let mut body = format!(
            "## {heading}: {}\n\n来源: {} / {}",
            asset.asset.name,
            asset.asset.source_tool.as_str(),
            asset.asset.id
        );
        let mut primary = source.to_string();
        if self.environment.target.tool == SourceTool::Claude
            && asset.asset.kind == AssetKind::Instruction
        {
            let imports = asset
                .asset
                .definition
                .as_ref()
                .and_then(|value| value.get("imports"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for import in imports {
                let Some(literal) = import.get("literal").and_then(Value::as_str) else {
                    continue;
                };
                let Some(imported_id) = import.get("assetId").and_then(Value::as_str) else {
                    continue;
                };
                if !self.selected.contains(imported_id) {
                    self.needs_configuration(
                        item,
                        "指令引用的资源未在本次迁移中勾选",
                        vec![format!("勾选关联资源 {imported_id} 后重新预览")],
                    );
                    continue;
                }
                let Some(imported) = self.assets.get(imported_id) else {
                    continue;
                };
                let Some(imported_file) = imported.files.first() else {
                    continue;
                };
                let target = self.instruction_managed_path(imported, scope_root, imported_file);
                let Ok(relative) = Self::relative_managed_ref(scope_root, &target) else {
                    continue;
                };
                primary = primary.replace(&format!("@{literal}"), &format!("@{relative}"));
            }
        }
        body.push_str("\n\n");
        body.push_str(primary.trim_end());
        if source_file
            .relative_path
            .parent()
            .is_some_and(|parent| !parent.as_os_str().is_empty())
        {
            body.push_str("\n\n原始相对路径: ");
            body.push_str(&source_file.relative_path.to_string_lossy());
        }
        Some(body)
    }

    fn render_instruction(
        &mut self,
        asset: &AssetSnapshot,
        scope_root: PathBuf,
        item: &mut RenderedItem,
    ) {
        let Some((source, source_file)) = self.read_primary(item, asset) else {
            return;
        };
        // Tools that share a native entry (for example AGENTS.md) already
        // read this file in the same project. Cloning the conversation must
        // not rewrite it merely to add a managed wrapper around itself.
        if let Ok(path) = self.instruction_path(item.target_scope) {
            if path.canonicalize().ok().as_ref() == Some(&source_file.source) {
                item.action = "keep-existing".to_string();
                item.message = "目标工具已可从当前项目读取这份内容，保留现有文件".to_string();
                item.target_paths.push(path);
                item.evidence
                    .push("来源与目标使用同一原生指令入口".to_string());
                return;
            }
        }
        let target_copy = self.instruction_managed_path(asset, &scope_root, &source_file);
        self.add_copy(
            item,
            &source_file,
            target_copy,
            &scope_root,
            item.target_scope,
        );
        let Some(body) = self.instruction_body(item, asset, &source, &source_file, &scope_root)
        else {
            return;
        };
        let path = match self.instruction_path(item.target_scope) {
            Ok(path) => path,
            Err(error) => {
                self.fail_item(item, "targetInstructionPath", error);
                return;
            }
        };
        let document = match self.ensure_document(
            &path,
            item.target_scope,
            &scope_root,
            DocumentFormat::Text,
            "",
        ) {
            Ok(document) => document,
            Err(error) => {
                self.fail_item(
                    item,
                    "targetInstructionRead",
                    format!("无法读取目标指令: {error}"),
                );
                return;
            }
        };
        let candidate = match upsert_managed_block(&document.text, &item.item_id, &body) {
            Ok(candidate) => candidate,
            Err(error) => {
                self.fail_item(item, "profile", error);
                return;
            }
        };
        let Some(document) = self.documents.get_mut(&path) else {
            return;
        };
        document.text = candidate;
        if !document.owners.contains(&item.item_id) {
            document.owners.push(item.item_id.clone());
        }
        document.contributions.push((
            item.item_id.clone(),
            RenderContribution::ManagedBlock {
                marker: item.item_id.clone(),
                body,
            },
        ));
        if !item.target_paths.contains(&path) {
            item.target_paths.push(path);
        }
        item.evidence
            .push("写入目标工具实际加载的 Markdown 指令入口".to_string());
        if asset.asset.scope == AssetScope::Project
            && matches!(
                self.environment.target.tool,
                SourceTool::Claude | SourceTool::Pi | SourceTool::Codex
            )
            && !self.environment.project_trusted
        {
            self.needs_configuration(
                item,
                "项目指令已生成，但目标项目尚未标记为受信任",
                vec!["在目标工具中确认项目 trust 后再验证加载".to_string()],
            );
        }
    }

    fn skill_root(&self, scope_root: &Path, scope: TargetScope) -> PathBuf {
        if scope == TargetScope::Project {
            return scope_root.join(match self.environment.target.tool {
                SourceTool::Codex => ".agents/skills",
                SourceTool::Claude => ".claude/skills",
                SourceTool::OpenCode => ".opencode/skills",
                SourceTool::Dsh => ".dsh/skills",
                SourceTool::Pi => ".pi/skills",
            });
        }
        match self.environment.target.tool {
            SourceTool::Codex => self.environment.roots.agents.join("skills"),
            SourceTool::Claude => self.environment.roots.claude.join("skills"),
            SourceTool::OpenCode => self.environment.roots.opencode.join("skills"),
            SourceTool::Dsh => self.environment.roots.dsh.join("skills"),
            SourceTool::Pi => self.environment.roots.pi.join("skills"),
        }
    }

    fn global_skill_path(&self, name: &str) -> Option<PathBuf> {
        let roots = match self.environment.target.tool {
            SourceTool::Codex => vec![
                self.environment.roots.agents.join("skills"),
                self.environment.roots.codex.join("skills"),
            ],
            SourceTool::Claude => vec![self.environment.roots.claude.join("skills")],
            SourceTool::OpenCode => vec![self.environment.roots.opencode.join("skills")],
            SourceTool::Dsh => vec![
                self.environment.roots.dsh.join("skills"),
                self.environment.roots.dsh_agents.join("skills"),
            ],
            SourceTool::Pi => vec![
                self.environment.roots.pi.join("skills"),
                self.environment.roots.agents.join("skills"),
            ],
        };
        roots.into_iter().find_map(|root| {
            let directory = root.join(name);
            if directory.join("SKILL.md").is_file() {
                Some(directory)
            } else {
                let flat = root.join(format!("{name}.md"));
                flat.is_file().then_some(flat)
            }
        })
    }

    fn render_skill(
        &mut self,
        asset: &AssetSnapshot,
        scope_root: PathBuf,
        item: &mut RenderedItem,
    ) {
        let root = self.skill_root(&scope_root, item.target_scope);
        let name = slug(&asset.asset.name);
        let flat = asset.asset.format == "flat-skill";
        let flat_target_supported = self.environment.target.tool == SourceTool::Dsh;
        if item.target_scope == TargetScope::Project {
            if let Some(existing) = self.global_skill_path(&name) {
                let message = format!(
                    "目标已有同名全局技能，可能遮蔽项目技能: {}",
                    existing.display()
                );
                item.diagnostics.push(Diagnostic::new(
                    "targetGlobalSkillShadowing",
                    message.clone(),
                    Some(existing),
                ));
                self.needs_configuration(
                    item,
                    message,
                    vec!["确认目标优先级，或重命名其中一个技能后重新预览".to_string()],
                );
            }
        }
        if asset.asset.files.is_empty() {
            self.fail_item(item, "skillResourcesMissing", "技能没有可迁移的资源文件");
            return;
        }
        for file in &asset.files {
            let target_path = if flat && !flat_target_supported {
                if file
                    .relative_path
                    .extension()
                    .and_then(|value| value.to_str())
                    != Some("md")
                {
                    continue;
                }
                root.join(&name).join("SKILL.md")
            } else if flat {
                root.join(format!("{name}.md"))
            } else {
                root.join(&name).join(&file.relative_path)
            };
            self.add_copy(item, file, target_path, &scope_root, item.target_scope);
        }
        let metadata = asset
            .asset
            .definition
            .as_ref()
            .and_then(|value| value.get("metadata"))
            .and_then(Value::as_object);
        let name_valid = metadata
            .and_then(|value| value.get("name"))
            .and_then(Value::as_str)
            .is_some_and(valid_skill_name);
        let description_present = metadata
            .and_then(|value| value.get("description"))
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty());
        if !name_valid || !description_present {
            self.needs_configuration(
                item,
                "技能已写入原生目录，但 frontmatter 缺少目标可识别的名称或描述",
                vec!["补齐 SKILL.md 的 name 和 description 字段".to_string()],
            );
        }
        let controls = metadata_control_fields(&asset.asset);
        if !controls.is_empty() && asset.asset.source_tool != self.environment.target.tool {
            self.needs_configuration(
                item,
                "技能的调用控制字段已原样保留，但目标可能忽略部分字段",
                vec![format!(
                    "检查目标技能的调用控制字段: {}",
                    controls.join(", ")
                )],
            );
        }
        if item.target_scope == TargetScope::Project
            && matches!(
                self.environment.target.tool,
                SourceTool::Claude | SourceTool::Pi | SourceTool::Codex
            )
            && !self.environment.project_trusted
        {
            self.needs_configuration(
                item,
                "项目技能位于目标原生目录，但项目尚未受信任",
                vec!["在目标工具中确认项目 trust 后再验证技能发现".to_string()],
            );
        }
        item.evidence
            .push("技能主体与资源已映射到目标工具的原生发现目录".to_string());
    }

    fn render_script(
        &mut self,
        asset: &AssetSnapshot,
        scope_root: PathBuf,
        item: &mut RenderedItem,
    ) {
        if asset.files.is_empty() {
            self.fail_item(item, "scriptFilesMissing", "脚本项没有可迁移的文件");
            return;
        }
        let resource_prefix = asset
            .asset
            .resource_root
            .as_ref()
            .and_then(|root| root.strip_prefix(&asset.asset.scope_root).ok())
            .filter(|value| !value.as_os_str().is_empty())
            .map(Path::to_path_buf);
        for file in &asset.files {
            let relative = match &resource_prefix {
                Some(prefix) => prefix.join(&file.relative_path),
                None => file.relative_path.clone(),
            };
            let target = scope_root.join(relative);
            self.add_copy(item, file, target, &scope_root, item.target_scope);
        }
        let report = script_requirements(&asset.files, &self.run_root, &self.environment);
        self.apply_capabilities(item, report);
        item.evidence
            .push("脚本文件和配套资源已复制到项目路径，但迁移过程不会启动脚本".to_string());
    }

    fn apply_capabilities(&mut self, item: &mut RenderedItem, report: CapabilityReport) {
        let evidence = report.evidence();
        if !evidence.is_empty() {
            item.evidence.extend(evidence);
        }
        if report.is_blocked() || !report.unverified_dependencies.is_empty() {
            self.needs_configuration(
                item,
                "内容已生成，但在目标环境中仍缺少运行条件",
                report.next_steps(),
            );
        }
    }

    fn ensure_document<'b>(
        &'b mut self,
        path: &Path,
        target_scope: TargetScope,
        scope_root: &Path,
        format: DocumentFormat,
        default: &str,
    ) -> Result<&'b mut PendingDocument, String> {
        if !self.documents.contains_key(path) {
            let text = adapters::read_text(path)?.unwrap_or_else(|| default.to_string());
            match format {
                DocumentFormat::Text => {}
                DocumentFormat::Jsonc => {
                    adapters::parse_json(&text)?;
                }
                DocumentFormat::Toml => {
                    adapters::parse_toml(&text)?;
                }
                DocumentFormat::Yaml => {
                    adapters::parse_yaml(&text)?;
                }
            }
            self.documents.insert(
                path.to_path_buf(),
                PendingDocument {
                    target_scope,
                    scope_root: scope_root.to_path_buf(),
                    text,
                    owners: vec![],
                    keys: BTreeMap::new(),
                    contributions: vec![],
                },
            );
        }
        self.documents
            .get_mut(path)
            .ok_or_else(|| "target document was not initialized".to_string())
    }

    fn command_root(&self, scope_root: &Path, scope: TargetScope) -> Result<PathBuf, String> {
        Ok(match self.environment.target.tool {
            SourceTool::Claude => {
                if scope == TargetScope::Project {
                    scope_root.join(".claude/commands")
                } else {
                    self.environment.roots.claude.join("commands")
                }
            }
            SourceTool::Pi => {
                if scope == TargetScope::Project {
                    scope_root.join(".pi/prompts")
                } else {
                    self.environment.roots.pi.join("prompts")
                }
            }
            SourceTool::Codex => self.environment.roots.codex.join("prompts"),
            SourceTool::OpenCode => {
                return Err("OpenCode commands are configured in opencode.jsonc".to_string())
            }
            SourceTool::Dsh => {
                return Err("the target DSH profile has no portable command format".to_string())
            }
        })
    }

    fn command_config_path(&self, scope_root: &Path, scope: TargetScope) -> PathBuf {
        if scope == TargetScope::Project {
            scope_root.join("opencode.jsonc")
        } else {
            self.environment.roots.opencode.join("opencode.jsonc")
        }
    }

    fn render_command(
        &mut self,
        asset: &AssetSnapshot,
        scope_root: PathBuf,
        item: &mut RenderedItem,
    ) {
        if asset.asset.format == "package-script" {
            self.render_package_script(asset, &scope_root, item);
            return;
        }
        match self.environment.target.tool {
            SourceTool::OpenCode => self.render_opencode_command(asset, &scope_root, item),
            SourceTool::Claude | SourceTool::Pi => {
                self.render_markdown_command(asset, &scope_root, item)
            }
            SourceTool::Codex => {
                if item.target_scope == TargetScope::User {
                    self.render_markdown_command(asset, &scope_root, item)
                } else {
                    self.render_inactive_command_copy(
                        asset,
                        &scope_root,
                        item,
                        "Codex 的自定义 prompt 只能从用户级 prompts 目录加载",
                    );
                }
            }
            SourceTool::Dsh => self.render_inactive_command_copy(
                asset,
                &scope_root,
                item,
                "目标 DSH profile 未提供与该命令等价的稳定原生格式",
            ),
        }
    }

    fn render_markdown_command(
        &mut self,
        asset: &AssetSnapshot,
        scope_root: &Path,
        item: &mut RenderedItem,
    ) {
        let Some((source, source_file)) = self.read_primary(item, asset) else {
            return;
        };
        let root = match self.command_root(scope_root, item.target_scope) {
            Ok(root) => root,
            Err(error) => {
                self.unsupported(item, error);
                return;
            }
        };
        let target = root.join(format!("{}.md", slug(&asset.asset.name)));
        self.add_copy(item, &source_file, target, scope_root, item.target_scope);
        let frontmatter = adapters::frontmatter(&source).unwrap_or_default();
        let controls = frontmatter
            .as_object()
            .into_iter()
            .flat_map(|value| value.keys())
            .filter(|key| !matches!(key.as_str(), "description" | "argument-hint"))
            .cloned()
            .collect::<Vec<_>>();
        if !controls.is_empty() && asset.asset.source_tool != self.environment.target.tool {
            self.needs_configuration(
                item,
                "命令 frontmatter 已原样保留，但目标可能忽略部分调用控制",
                vec![format!(
                    "检查目标命令的调用控制字段: {}",
                    controls.join(", ")
                )],
            );
        }
        item.evidence
            .push("命令已映射到目标工具的 Markdown 命令目录".to_string());
    }

    fn render_inactive_command_copy(
        &mut self,
        asset: &AssetSnapshot,
        scope_root: &Path,
        item: &mut RenderedItem,
        reason: &str,
    ) {
        let Some((_, source_file)) = self.read_primary(item, asset) else {
            return;
        };
        let target = scope_root
            .join(".session-loom/commands")
            .join(format!("{}.md", slug(&asset.asset.name)));
        self.add_copy(item, &source_file, target, scope_root, item.target_scope);
        item.action = "preserve-only".to_string();
        if self.environment.target.tool == SourceTool::Codex {
            self.needs_configuration(
                item,
                format!("{reason}；内容已保留在项目中"),
                vec!["将命令放入 Codex 用户 prompts 目录并重新预览".to_string()],
            );
        } else {
            self.unsupported(
                item,
                format!("{reason}；只保留了可审读副本，目标不会把它识别为命令"),
            );
        }
    }

    fn render_opencode_command(
        &mut self,
        asset: &AssetSnapshot,
        scope_root: &Path,
        item: &mut RenderedItem,
    ) {
        let version = self.environment.opencode_version;
        if version == OpenCodeConfigVersion::Unknown {
            self.unsupported(item, "无法确认目标 OpenCode 的命令配置版本");
            return;
        }
        let path = self.command_config_path(scope_root, item.target_scope);
        let key_path = match version {
            OpenCodeConfigVersion::V1 => "command",
            OpenCodeConfigVersion::V2 => "commands",
            OpenCodeConfigVersion::Unknown => unreachable!(),
        };
        let conflict_key = format!("{key_path}.{}", asset.asset.name);
        let current_text = match self.ensure_document(
            &path,
            item.target_scope,
            scope_root,
            DocumentFormat::Jsonc,
            "{\n}\n",
        ) {
            Ok(document) => document.text.clone(),
            Err(error) => {
                self.fail_item(
                    item,
                    "targetConfigurationRead",
                    format!("无法读取目标 OpenCode 配置: {error}"),
                );
                return;
            }
        };
        let markdown_source = if asset
            .asset
            .definition
            .as_ref()
            .and_then(|value| value.get("template"))
            .is_some()
        {
            None
        } else {
            self.read_primary(item, asset).map(|(source, _)| source)
        };
        let (command, compatibility_note) =
            match normalize_command(asset, markdown_source.as_deref()) {
                Ok(command) => command,
                Err(error) => {
                    self.unsupported(item, error);
                    return;
                }
            };
        let object_path = if version == OpenCodeConfigVersion::V2 {
            vec!["commands".to_string()]
        } else {
            vec!["command".to_string()]
        };
        self.add_document_contribution(
            &path,
            &item.item_id,
            RenderContribution::ConfigValue {
                format: ConfigEditFormat::Jsonc,
                object_path: object_path.clone(),
                key: asset.asset.name.clone(),
                value: command.clone(),
            },
        );
        let existing = self
            .documents
            .get(&path)
            .and_then(|document| document.keys.get(&conflict_key))
            .cloned();
        if let Some(existing) = existing {
            if existing != item.item_id {
                let message = format!("多个来源命令映射到同一 OpenCode 节点: {}", asset.asset.name);
                self.mark_existing_item_conflict(&existing, &path, &message);
                self.note_source_conflict(
                    &path,
                    "configValue",
                    vec![existing, item.item_id.clone()],
                );
                self.diagnostics.push(Diagnostic::new(
                    "sourceTargetConflict",
                    message.clone(),
                    Some(path.clone()),
                ));
                self.needs_configuration(
                    item,
                    message,
                    vec!["在执行前选择保留、合并或替换的目标内容".to_string()],
                );
                return;
            }
        }
        let object_paths: Vec<&str> = object_path.iter().map(String::as_str).collect();
        let candidate = match jsonc_set(&current_text, &object_paths, &asset.asset.name, &command) {
            Ok(candidate) => candidate,
            Err(error) => {
                self.fail_item(
                    item,
                    "targetConfigurationEdit",
                    format!("无法生成目标 OpenCode 命令: {error}"),
                );
                return;
            }
        };
        let Some(document) = self.documents.get_mut(&path) else {
            return;
        };
        let owner = item.item_id.clone();
        document.text = candidate;
        document.keys.insert(conflict_key, owner.clone());
        if !document.owners.contains(&owner) {
            document.owners.push(owner);
        }
        if !item.target_paths.contains(&path) {
            item.target_paths.push(path);
        }
        if let Some(note) = compatibility_note {
            self.needs_configuration(
                item,
                note,
                vec!["检查目标命令的模板参数和调用控制".to_string()],
            );
        }
        item.evidence
            .push(format!("命令已写入 OpenCode {key_path} 配置命名空间"));
    }

    fn render_package_script(
        &mut self,
        asset: &AssetSnapshot,
        scope_root: &Path,
        item: &mut RenderedItem,
    ) {
        if item.target_scope != TargetScope::Project {
            self.unsupported(item, "项目 package script 不能隐式扩大为用户级命令");
            return;
        }
        let command = asset
            .asset
            .definition
            .as_ref()
            .and_then(|value| value.get("command"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let Some(command) = command else {
            self.fail_item(item, "packageScriptMissing", "package script 没有命令内容");
            return;
        };
        let path = scope_root.join("package.json");
        let conflict_key = format!("scripts.{}", asset.asset.name);
        let current_text = match self.ensure_document(
            &path,
            item.target_scope,
            scope_root,
            DocumentFormat::Jsonc,
            "{\n}\n",
        ) {
            Ok(document) => document.text.clone(),
            Err(error) => {
                self.fail_item(
                    item,
                    "targetConfigurationRead",
                    format!("无法读取目标 package.json: {error}"),
                );
                return;
            }
        };
        self.add_document_contribution(
            &path,
            &item.item_id,
            RenderContribution::ConfigValue {
                format: ConfigEditFormat::Jsonc,
                object_path: vec!["scripts".to_string()],
                key: asset.asset.name.clone(),
                value: Value::String(command.clone()),
            },
        );
        let existing = self
            .documents
            .get(&path)
            .and_then(|document| document.keys.get(&conflict_key))
            .cloned();
        if let Some(existing) = existing {
            if existing != item.item_id {
                let message = "多个 package script 使用同一名称".to_string();
                self.mark_existing_item_conflict(&existing, &path, &message);
                self.note_source_conflict(
                    &path,
                    "configValue",
                    vec![existing, item.item_id.clone()],
                );
                self.diagnostics.push(Diagnostic::new(
                    "sourceTargetConflict",
                    message.clone(),
                    Some(path.clone()),
                ));
                self.needs_configuration(
                    item,
                    message,
                    vec!["在执行前选择保留、合并或替换的目标内容".to_string()],
                );
                return;
            }
        }
        let candidate = match jsonc_set(
            &current_text,
            &["scripts"],
            &asset.asset.name,
            &Value::String(command.clone()),
        ) {
            Ok(candidate) => candidate,
            Err(error) => {
                self.fail_item(
                    item,
                    "targetConfigurationEdit",
                    format!("无法生成 package script: {error}"),
                );
                return;
            }
        };
        let Some(document) = self.documents.get_mut(&path) else {
            return;
        };
        let owner = item.item_id.clone();
        document.text = candidate;
        document.keys.insert(conflict_key, owner.clone());
        if !document.owners.contains(&owner) {
            document.owners.push(owner);
        }
        if !item.target_paths.contains(&path) {
            item.target_paths.push(path);
        }
        self.needs_configuration(
            item,
            "package script 已写入项目配置，但依赖尚未安装或验证",
            vec![
                "在目标项目安装依赖并手工运行该命令".to_string(),
                format!("确认命令内容: {command}"),
            ],
        );
    }

    fn mcp_path(
        &self,
        asset: &EnvironmentAsset,
        scope_root: &Path,
        item: &RenderedItem,
    ) -> Result<(PathBuf, DocumentFormat), String> {
        Ok(match self.environment.target.tool {
            SourceTool::Codex => (
                self.environment.roots.codex.join("config.toml"),
                DocumentFormat::Toml,
            ),
            SourceTool::Claude => (
                if item.target_scope == TargetScope::Project {
                    scope_root.join(".mcp.json")
                } else {
                    self.environment.roots.claude_user_config.clone()
                },
                DocumentFormat::Jsonc,
            ),
            SourceTool::OpenCode => (
                if item.target_scope == TargetScope::Project {
                    scope_root.join("opencode.jsonc")
                } else {
                    self.environment.roots.opencode.join("opencode.jsonc")
                },
                DocumentFormat::Jsonc,
            ),
            SourceTool::Dsh => (scope_root.join("cordis.patch.yml"), DocumentFormat::Yaml),
            SourceTool::Pi => (
                if item.target_scope == TargetScope::Project {
                    scope_root.join(".pi/mcp.json")
                } else {
                    self.environment.roots.pi.join("mcp.json")
                },
                DocumentFormat::Jsonc,
            ),
        })
        .map(|(path, format)| {
            let _ = asset;
            (path, format)
        })
    }

    fn render_mcp(&mut self, asset: &AssetSnapshot, scope_root: PathBuf, item: &mut RenderedItem) {
        let target_tool = self.environment.target.tool;
        let opencode_version = self.environment.opencode_version;
        if target_tool == SourceTool::OpenCode && opencode_version == OpenCodeConfigVersion::Unknown
        {
            self.unsupported(item, "无法确认目标 OpenCode 的 MCP 配置版本");
            return;
        }
        if target_tool == SourceTool::Pi && self.environment.pi_mcp != VersionSupport::Supported {
            self.unsupported(item, "目标 Pi 版本没有已确认的原生 MCP 格式");
            return;
        }
        let normalized = match normalize_mcp(&asset.asset) {
            Ok(normalized) => normalized,
            Err(error) => {
                self.unsupported(item, error);
                return;
            }
        };
        let (path, format) = match self.mcp_path(&asset.asset, &scope_root, item) {
            Ok(path) => path,
            Err(error) => {
                self.fail_item(item, "targetConfigurationPath", error);
                return;
            }
        };
        if target_tool == SourceTool::Dsh && normalized.transport != McpTransport::Stdio {
            self.unsupported(item, "目标 DSH profile 只确认了 stdio MCP 插件格式");
            return;
        }
        let target_value = match target_mcp_value(target_tool, opencode_version, &normalized) {
            Ok(target_value) => target_value,
            Err(error) => {
                self.unsupported(item, error);
                return;
            }
        };
        let conflict_key = format!("mcp.{}", asset.asset.name);
        let current_text = match self.ensure_document(
            &path,
            item.target_scope,
            &scope_root,
            format,
            match format {
                DocumentFormat::Toml | DocumentFormat::Text => "",
                DocumentFormat::Yaml => "[]\n",
                DocumentFormat::Jsonc => "{\n}\n",
            },
        ) {
            Ok(document) => document.text.clone(),
            Err(error) => {
                self.fail_item(
                    item,
                    "targetConfigurationRead",
                    format!("无法读取目标 MCP 配置: {error}"),
                );
                return;
            }
        };
        let jsonc_path = match target_tool {
            SourceTool::Claude | SourceTool::Pi => vec!["mcpServers".to_string()],
            SourceTool::OpenCode if opencode_version == OpenCodeConfigVersion::V2 => {
                vec!["mcp".to_string(), "servers".to_string()]
            }
            SourceTool::OpenCode => vec!["mcp".to_string()],
            _ => vec![],
        };
        let yaml_patch = (format == DocumentFormat::Yaml).then(|| {
            let row = json!({
                "id": format!("session-loom-{}", &asset.asset.id[..16.min(asset.asset.id.len())]),
                "name": "@deepseek-ai/dsh-mcp-client",
                "config": dsh_mcp_config(&asset.asset.name, &normalized)
            });
            format!(
                "- insert:\n    - {}",
                serde_json::to_string(&row).expect("JSON object serialization cannot fail")
            )
        });
        let contribution = match format {
            DocumentFormat::Toml => RenderContribution::ConfigValue {
                format: ConfigEditFormat::Toml,
                object_path: vec!["mcp_servers".to_string()],
                key: asset.asset.name.clone(),
                value: target_value.clone(),
            },
            DocumentFormat::Jsonc => RenderContribution::ConfigValue {
                format: ConfigEditFormat::Jsonc,
                object_path: jsonc_path.clone(),
                key: asset.asset.name.clone(),
                value: target_value.clone(),
            },
            DocumentFormat::Yaml => RenderContribution::YamlPatch {
                marker: asset.asset.id.clone(),
                patch: yaml_patch
                    .clone()
                    .expect("YAML documents have a generated patch"),
            },
            DocumentFormat::Text => unreachable!("MCP is never stored in plain text"),
        };
        self.add_document_contribution(&path, &item.item_id, contribution);
        let existing = self
            .documents
            .get(&path)
            .and_then(|document| document.keys.get(&conflict_key))
            .cloned();
        if let Some(existing) = existing {
            if existing != item.item_id {
                let message = "多个来源连接使用同一名称".to_string();
                self.mark_existing_item_conflict(&existing, &path, &message);
                self.note_source_conflict(
                    &path,
                    "configValue",
                    vec![existing, item.item_id.clone()],
                );
                self.diagnostics.push(Diagnostic::new(
                    "sourceTargetConflict",
                    message.clone(),
                    Some(path.clone()),
                ));
                self.needs_configuration(
                    item,
                    message,
                    vec!["在执行前选择保留、合并或替换的目标内容".to_string()],
                );
                return;
            }
        }
        let candidate = match format {
            DocumentFormat::Toml => toml_set(
                &current_text,
                &["mcp_servers"],
                &asset.asset.name,
                &target_value,
            ),
            DocumentFormat::Jsonc => {
                let path_segments: Vec<&str> = jsonc_path.iter().map(String::as_str).collect();
                jsonc_set(
                    &current_text,
                    &path_segments,
                    &asset.asset.name,
                    &target_value,
                )
            }
            DocumentFormat::Yaml => {
                let patch = yaml_patch
                    .as_deref()
                    .expect("YAML documents have a generated patch");
                upsert_yaml_patch(&current_text, &asset.asset.id, patch)
            }
            DocumentFormat::Text => unreachable!("MCP is never stored in plain text"),
        };
        let candidate = match candidate {
            Ok(candidate) => candidate,
            Err(error) => {
                self.fail_item(
                    item,
                    "targetConfigurationEdit",
                    format!("无法生成目标 MCP 配置: {error}"),
                );
                return;
            }
        };
        let Some(document) = self.documents.get_mut(&path) else {
            return;
        };
        let owner = item.item_id.clone();
        document.text = candidate;
        document.keys.insert(conflict_key, owner.clone());
        if !document.owners.contains(&owner) {
            document.owners.push(owner);
        }
        if !item.target_paths.contains(&path) {
            item.target_paths.push(path);
        }
        let mut report = configuration_requirements(
            asset.asset.definition.as_ref().unwrap_or(&Value::Null),
            &self.environment,
        );
        if let Some(command) = &normalized.command {
            if !program_available(command, &self.environment.executable_search_path) {
                report.missing_commands.push(command.clone());
                report.missing_commands.sort();
                report.missing_commands.dedup();
            }
        }
        self.needs_configuration(
            item,
            "MCP 配置已生成，但连接、授权和工具发现尚未验证",
            vec!["启动目标工具并验证该连接后，结果才能标记为可用".to_string()],
        );
        self.apply_capabilities(item, report);
        if !normalized.unsupported_fields.is_empty() {
            self.needs_configuration(
                item,
                "部分来源字段没有对应的目标配置表达",
                vec![format!(
                    "检查未映射的连接字段: {}",
                    normalized.unsupported_fields.join(", ")
                )],
            );
        }
        if item.target_scope == TargetScope::Project && !self.environment.project_trusted {
            self.needs_configuration(
                item,
                "项目级 MCP 配置需要在目标项目中确认信任",
                vec!["确认目标项目 trust 并批准项目连接".to_string()],
            );
        }
        if target_tool == SourceTool::Dsh {
            self.needs_configuration(
                item,
                "DSH MCP 配置已加入 profile，仍需安装或启用客户端插件",
                vec!["在目标 DSH profile 启用 @deepseek-ai/dsh-mcp-client".to_string()],
            );
        }
        if asset.asset.scope == AssetScope::Project && item.target_scope != TargetScope::Project {
            self.needs_configuration(
                item,
                format!(
                    "目标工具没有可用的项目级 MCP 位置，配置实际会作用于 {:?} 范围",
                    item.target_scope
                ),
                vec!["确认该连接成为目标用户或 profile 级配置".to_string()],
            );
        }
        item.evidence.push(format!("目标格式: {}", item.format));
    }
}

fn normalize_command(
    asset: &AssetSnapshot,
    markdown: Option<&str>,
) -> Result<(Value, Option<String>), String> {
    if let Some(definition) = asset
        .asset
        .definition
        .as_ref()
        .and_then(Value::as_object)
        .filter(|definition| definition.get("template").and_then(Value::as_str).is_some())
    {
        let mut command = definition.clone();
        command
            .entry("template")
            .or_insert(Value::String(String::new()));
        let note = (asset.asset.source_tool != SourceTool::OpenCode)
            .then(|| "来源命令模板的参数语法需要在目标中重新核对".to_string());
        return Ok((Value::Object(command), note));
    }
    let source = markdown.ok_or_else(|| "命令没有可读取的模板正文".to_string())?;
    let metadata = adapters::frontmatter(source).unwrap_or_default();
    let template = strip_markdown_frontmatter(source)?;
    let mut command = Map::new();
    command.insert("template".to_string(), Value::String(template));
    if let Some(description) = metadata.get("description").and_then(Value::as_str) {
        if !description.trim().is_empty() {
            command.insert(
                "description".to_string(),
                Value::String(description.to_string()),
            );
        }
    }
    let mut note = (asset.asset.source_tool != SourceTool::OpenCode)
        .then(|| "来源命令已转为 OpenCode template，参数语法需要核对".to_string());
    let controls = metadata
        .as_object()
        .into_iter()
        .flat_map(|metadata| metadata.keys())
        .filter(|key| !matches!(key.as_str(), "description" | "argument-hint"))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if !controls.is_empty() {
        note = Some(format!(
            "OpenCode 未转换这些命令调用控制字段: {}",
            controls.join(", ")
        ));
    }
    Ok((Value::Object(command), note))
}

fn strip_markdown_frontmatter(source: &str) -> Result<String, String> {
    let normalized = source.replace("\r\n", "\n");
    let Some(rest) = normalized.strip_prefix("---\n") else {
        return Ok(normalized);
    };
    let end = rest
        .find("\n---")
        .ok_or_else(|| "命令 frontmatter 没有结束分隔符".to_string())?;
    let after = rest[end + 4..]
        .strip_prefix('\n')
        .unwrap_or(&rest[end + 4..]);
    Ok(after.to_string())
}

fn valid_skill_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpTransport {
    Stdio,
    Http,
    Sse,
    WebSocket,
}

#[derive(Debug, Clone)]
struct NormalizedMcp {
    transport: McpTransport,
    command: Option<String>,
    args: Vec<String>,
    env: BTreeMap<String, String>,
    cwd: Option<String>,
    url: Option<String>,
    headers: BTreeMap<String, String>,
    enabled: bool,
    unsupported_fields: Vec<String>,
}

fn normalize_mcp(asset: &EnvironmentAsset) -> Result<NormalizedMcp, String> {
    let definition = asset
        .definition
        .as_ref()
        .ok_or_else(|| "MCP item has no definition".to_string())?;
    let server = if asset.source_tool == SourceTool::Dsh {
        definition.get("config").unwrap_or(definition)
    } else {
        definition
    };
    let object = server
        .as_object()
        .ok_or_else(|| "MCP definition must be an object".to_string())?;
    let explicit_type = object
        .get("type")
        .or_else(|| object.get("transport"))
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    let command_value = object.get("command");
    let (command, args) = match command_value {
        Some(Value::Array(values)) => {
            let mut values = values.iter();
            let command = values
                .next()
                .and_then(Value::as_str)
                .ok_or_else(|| "MCP command array needs an executable".to_string())?
                .to_string();
            let mut args = vec![];
            for value in values {
                args.push(
                    value
                        .as_str()
                        .ok_or_else(|| "MCP command arguments must be strings".to_string())?
                        .to_string(),
                );
            }
            (Some(command), args)
        }
        Some(Value::String(command)) => {
            if command.trim().is_empty() || command.chars().any(|value| value.is_whitespace()) {
                return Err(
                    "MCP command must name one executable; shell command strings require manual review"
                        .to_string(),
                );
            }
            let args = object
                .get("args")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|value| {
                    value
                        .as_str()
                        .map(str::to_string)
                        .ok_or_else(|| "MCP arguments must be strings".to_string())
                })
                .collect::<Result<Vec<_>, _>>()?;
            (Some(command.clone()), args)
        }
        Some(_) => return Err("MCP command has an unsupported type".to_string()),
        None => (None, vec![]),
    };
    let url = object
        .get("url")
        .and_then(Value::as_str)
        .map(str::to_string);
    let transport = match explicit_type.as_deref() {
        Some("local" | "stdio") => McpTransport::Stdio,
        Some("remote" | "http" | "streamable-http" | "streamable_http") => McpTransport::Http,
        Some("sse") => McpTransport::Sse,
        Some("ws" | "websocket") => McpTransport::WebSocket,
        Some(value) => {
            return Err(format!("MCP transport has no portable mapping: {value}"));
        }
        None if url.is_some() => McpTransport::Http,
        None if command.is_some() => McpTransport::Stdio,
        None => return Err("MCP definition has neither command nor URL".to_string()),
    };
    match transport {
        McpTransport::Stdio if command.is_none() => {
            return Err("stdio MCP definition has no executable".to_string())
        }
        McpTransport::Http | McpTransport::Sse | McpTransport::WebSocket if url.is_none() => {
            return Err("remote MCP definition has no URL".to_string())
        }
        _ => {}
    }
    let env = string_map(object.get("env").or_else(|| object.get("environment")))?;
    let mut headers = string_map(object.get("headers").or_else(|| object.get("http_headers")))?;
    for (header, variable) in string_map(object.get("env_http_headers"))? {
        headers.insert(header, format!("${{{variable}}}"));
    }
    if let Some(variable) = object.get("bearer_token_env_var").and_then(Value::as_str) {
        headers
            .entry("Authorization".to_string())
            .or_insert_with(|| format!("Bearer ${{{variable}}}"));
    }
    let cwd = object
        .get("cwd")
        .and_then(Value::as_str)
        .map(str::to_string);
    let enabled = object
        .get("disabled")
        .and_then(Value::as_bool)
        .map(|disabled| !disabled)
        .or_else(|| object.get("enabled").and_then(Value::as_bool))
        .unwrap_or(true);
    let unsupported_fields = object
        .keys()
        .filter(|key| {
            !matches!(
                key.as_str(),
                "type"
                    | "transport"
                    | "command"
                    | "args"
                    | "env"
                    | "environment"
                    | "cwd"
                    | "url"
                    | "headers"
                    | "http_headers"
                    | "env_http_headers"
                    | "bearer_token_env_var"
                    | "enabled"
                    | "disabled"
            )
        })
        .cloned()
        .collect();
    Ok(NormalizedMcp {
        transport,
        command,
        args,
        env,
        cwd,
        url,
        headers,
        enabled,
        unsupported_fields,
    })
}

fn string_map(value: Option<&Value>) -> Result<BTreeMap<String, String>, String> {
    let mut result = BTreeMap::new();
    let Some(value) = value else {
        return Ok(result);
    };
    let object = value
        .as_object()
        .ok_or_else(|| "MCP environment or header map must be an object".to_string())?;
    for (key, value) in object {
        result.insert(
            key.clone(),
            value
                .as_str()
                .ok_or_else(|| "MCP environment and header values must be strings".to_string())?
                .to_string(),
        );
    }
    Ok(result)
}

fn map_value(values: &BTreeMap<String, String>) -> Value {
    Value::Object(
        values
            .iter()
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect(),
    )
}

fn insert_if_present(object: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        object.insert(key.to_string(), Value::String(value.to_string()));
    }
}

fn target_mcp_value(
    target: SourceTool,
    opencode_version: OpenCodeConfigVersion,
    mcp: &NormalizedMcp,
) -> Result<Value, String> {
    let mut object = Map::new();
    match target {
        SourceTool::Codex => match mcp.transport {
            McpTransport::Stdio => {
                object.insert(
                    "command".to_string(),
                    Value::String(mcp.command.clone().expect("validated stdio command")),
                );
                if !mcp.args.is_empty() {
                    object.insert(
                        "args".to_string(),
                        Value::Array(mcp.args.iter().cloned().map(Value::String).collect()),
                    );
                }
                if !mcp.env.is_empty() {
                    object.insert("env".to_string(), map_value(&mcp.env));
                }
                insert_if_present(&mut object, "cwd", mcp.cwd.as_deref());
            }
            McpTransport::Http => {
                insert_if_present(&mut object, "url", mcp.url.as_deref());
                if !mcp.headers.is_empty() {
                    object.insert("http_headers".to_string(), map_value(&mcp.headers));
                }
            }
            McpTransport::Sse | McpTransport::WebSocket => {
                return Err("目标 Codex 只生成已确认的 stdio 或 HTTP MCP 格式".to_string())
            }
        },
        SourceTool::Claude => {
            let transport = match mcp.transport {
                McpTransport::Stdio => "stdio",
                McpTransport::Http => "http",
                McpTransport::Sse => "sse",
                McpTransport::WebSocket => "ws",
            };
            object.insert("type".to_string(), Value::String(transport.to_string()));
            if mcp.transport == McpTransport::Stdio {
                object.insert(
                    "command".to_string(),
                    Value::String(mcp.command.clone().expect("validated stdio command")),
                );
                if !mcp.args.is_empty() {
                    object.insert(
                        "args".to_string(),
                        Value::Array(mcp.args.iter().cloned().map(Value::String).collect()),
                    );
                }
                if !mcp.env.is_empty() {
                    object.insert("env".to_string(), map_value(&mcp.env));
                }
                insert_if_present(&mut object, "cwd", mcp.cwd.as_deref());
            } else {
                insert_if_present(&mut object, "url", mcp.url.as_deref());
                if !mcp.headers.is_empty() {
                    object.insert("headers".to_string(), map_value(&mcp.headers));
                }
            }
        }
        SourceTool::OpenCode => {
            if opencode_version == OpenCodeConfigVersion::Unknown {
                return Err("unknown OpenCode configuration version".to_string());
            }
            match mcp.transport {
                McpTransport::Stdio => {
                    object.insert("type".to_string(), Value::String("local".to_string()));
                    let mut command = vec![Value::String(
                        mcp.command.clone().expect("validated stdio command"),
                    )];
                    command.extend(mcp.args.iter().cloned().map(Value::String));
                    object.insert("command".to_string(), Value::Array(command));
                    if !mcp.env.is_empty() {
                        object.insert("environment".to_string(), map_value(&mcp.env));
                    }
                    insert_if_present(&mut object, "cwd", mcp.cwd.as_deref());
                }
                McpTransport::Http => {
                    object.insert("type".to_string(), Value::String("remote".to_string()));
                    insert_if_present(&mut object, "url", mcp.url.as_deref());
                    if !mcp.headers.is_empty() {
                        object.insert("headers".to_string(), map_value(&mcp.headers));
                    }
                }
                McpTransport::Sse | McpTransport::WebSocket => {
                    return Err(
                        "目标 OpenCode 版本没有已确认的 SSE 或 WebSocket MCP 映射".to_string()
                    )
                }
            }
            if opencode_version == OpenCodeConfigVersion::V1 {
                object.insert("enabled".to_string(), Value::Bool(mcp.enabled));
            } else if !mcp.enabled {
                object.insert("disabled".to_string(), Value::Bool(true));
            }
        }
        SourceTool::Pi => match mcp.transport {
            McpTransport::Stdio => {
                object.insert(
                    "command".to_string(),
                    Value::String(mcp.command.clone().expect("validated stdio command")),
                );
                if !mcp.args.is_empty() {
                    object.insert(
                        "args".to_string(),
                        Value::Array(mcp.args.iter().cloned().map(Value::String).collect()),
                    );
                }
                if !mcp.env.is_empty() {
                    object.insert("env".to_string(), map_value(&mcp.env));
                }
                insert_if_present(&mut object, "cwd", mcp.cwd.as_deref());
            }
            McpTransport::Http => {
                insert_if_present(&mut object, "url", mcp.url.as_deref());
                if !mcp.headers.is_empty() {
                    object.insert("headers".to_string(), map_value(&mcp.headers));
                }
            }
            McpTransport::Sse | McpTransport::WebSocket => {
                return Err("目标 Pi 不支持 SSE 或 WebSocket MCP 格式".to_string())
            }
        },
        SourceTool::Dsh => {
            return Ok(dsh_mcp_config("", mcp));
        }
    }
    Ok(Value::Object(object))
}

fn dsh_mcp_config(name: &str, mcp: &NormalizedMcp) -> Value {
    let mut config = Map::new();
    if !name.is_empty() {
        config.insert("serverName".to_string(), Value::String(name.to_string()));
    }
    config.insert("transport".to_string(), Value::String("stdio".to_string()));
    if let Some(command) = &mcp.command {
        config.insert("command".to_string(), Value::String(command.clone()));
    }
    config.insert(
        "args".to_string(),
        Value::Array(mcp.args.iter().cloned().map(Value::String).collect()),
    );
    if !mcp.env.is_empty() {
        config.insert("env".to_string(), map_value(&mcp.env));
    }
    insert_if_present(&mut config, "cwd", mcp.cwd.as_deref());
    Value::Object(config)
}

fn target_format(environment: &TargetEnvironment, asset: &EnvironmentAsset) -> String {
    match (environment.target.tool, asset.kind) {
        (SourceTool::Codex, AssetKind::Instruction) => "agents-instructions".into(),
        (SourceTool::Claude, AssetKind::Instruction) => "claude-instructions".into(),
        (SourceTool::OpenCode | SourceTool::Dsh | SourceTool::Pi, AssetKind::Instruction) => {
            "native-instructions".into()
        }
        (_, AssetKind::Memory) => "native-instructions".into(),
        (SourceTool::Codex, AssetKind::Skill) => "agents-skill".into(),
        (SourceTool::Claude, AssetKind::Skill) => "claude-skill".into(),
        (SourceTool::OpenCode, AssetKind::Skill) => "opencode-skill".into(),
        (SourceTool::Dsh, AssetKind::Skill) => "dsh-skill".into(),
        (SourceTool::Pi, AssetKind::Skill) => "pi-skill".into(),
        (SourceTool::Codex, AssetKind::Mcp) => "codex-mcp".into(),
        (SourceTool::Claude, AssetKind::Mcp) => "claude-mcp".into(),
        (SourceTool::OpenCode, AssetKind::Mcp) => match environment.opencode_version {
            OpenCodeConfigVersion::V1 => "opencode-v1-mcp".into(),
            OpenCodeConfigVersion::V2 => "opencode-v2-mcp".into(),
            OpenCodeConfigVersion::Unknown => "opencode-unknown-mcp".into(),
        },
        (SourceTool::Dsh, AssetKind::Mcp) => "dsh-cordis-mcp".into(),
        (SourceTool::Pi, AssetKind::Mcp) => "pi-mcp".into(),
        (_, AssetKind::Script) => "project-script".into(),
        (SourceTool::Claude, AssetKind::Command) => "claude-command".into(),
        (SourceTool::OpenCode, AssetKind::Command) => match environment.opencode_version {
            OpenCodeConfigVersion::V1 => "opencode-v1-command".into(),
            OpenCodeConfigVersion::V2 => "opencode-v2-command".into(),
            OpenCodeConfigVersion::Unknown => "opencode-unknown-command".into(),
        },
        (SourceTool::Pi, AssetKind::Command) => "pi-prompt".into(),
        (SourceTool::Codex, AssetKind::Command) => "codex-prompt".into(),
        (SourceTool::Dsh, AssetKind::Command) => "unsupported-command".into(),
    }
}

fn slug(value: &str) -> String {
    let mut output = String::new();
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            output.push(character.to_ascii_lowercase());
        } else if matches!(character, '-' | '_' | '.') {
            output.push(character);
        } else {
            output.push('-');
        }
    }
    let output = output.trim_matches(['-', '.']).to_string();
    if output.is_empty() {
        "migrated-item".to_string()
    } else {
        output
    }
}

fn metadata_control_fields(asset: &EnvironmentAsset) -> Vec<String> {
    let metadata = asset
        .definition
        .as_ref()
        .and_then(|value| value.get("metadata"))
        .and_then(Value::as_object);
    metadata
        .into_iter()
        .flat_map(|metadata| metadata.keys())
        .filter(|key| !matches!(key.as_str(), "name" | "description"))
        .cloned()
        .collect()
}
