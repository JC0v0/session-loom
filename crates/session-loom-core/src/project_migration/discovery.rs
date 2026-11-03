use super::{
    adapters,
    model::{
        content_hash, AssetKind, AssetScope, Diagnostic, EnvironmentAsset, EnvironmentRoots,
        ProjectIdentity, SelectedSession, SourceDocument, SourceDocumentKind, SourceFile,
        SourceProbe, UsageEvidence,
    },
};
use crate::canonical::SourceTool;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryOptions {
    pub dsh_profile: Option<String>,
    #[serde(default)]
    pub extra_scripts: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryReport {
    pub project: ProjectIdentity,
    pub session_ids: Vec<String>,
    pub assets: Vec<EnvironmentAsset>,
    pub diagnostics: Vec<Diagnostic>,
    pub source_probes: Vec<SourceProbe>,
}

pub fn discover_environment(
    project: &ProjectIdentity,
    sessions: &[SelectedSession],
    roots: &EnvironmentRoots,
    options: &DiscoveryOptions,
) -> Result<DiscoveryReport, String> {
    if !project.accessible || sessions.is_empty() {
        return Err(
            "environment discovery needs an accessible project and selected sessions".to_string(),
        );
    }
    for session in sessions {
        if super::projects::resolve_project(&session.metadata.cwd).id != project.id {
            return Err("selected session belongs to another project".to_string());
        }
    }
    let mut scanner = Scanner {
        project,
        sessions,
        roots,
        options,
        assets: BTreeMap::new(),
        diagnostics: vec![],
        probes: BTreeMap::new(),
        import_stack: HashSet::new(),
    };
    for tool in [
        SourceTool::Codex,
        SourceTool::Claude,
        SourceTool::OpenCode,
        SourceTool::Dsh,
        SourceTool::Pi,
    ] {
        if sessions
            .iter()
            .any(|session| session.metadata.source_tool == tool)
        {
            match tool {
                SourceTool::Codex => adapters::codex::discover(&mut scanner),
                SourceTool::Claude => adapters::claude::discover(&mut scanner),
                SourceTool::OpenCode => adapters::opencode::discover(&mut scanner),
                SourceTool::Dsh => adapters::dsh::discover(&mut scanner),
                SourceTool::Pi => adapters::pi::discover(&mut scanner),
            }
        }
    }
    let tool = sessions[0].metadata.source_tool;
    scanner.resolve_shadowing();
    scanner.project_scripts(tool);
    scanner.add_usage_evidence();
    Ok(DiscoveryReport {
        project: project.clone(),
        session_ids: sessions
            .iter()
            .map(|session| session.metadata.session_id.clone())
            .collect(),
        assets: scanner.assets.into_values().collect(),
        diagnostics: scanner.diagnostics,
        source_probes: scanner.probes.into_values().collect(),
    })
}

pub(crate) struct Scanner<'a> {
    pub project: &'a ProjectIdentity,
    pub sessions: &'a [SelectedSession],
    pub roots: &'a EnvironmentRoots,
    pub options: &'a DiscoveryOptions,
    assets: BTreeMap<String, EnvironmentAsset>,
    pub diagnostics: Vec<Diagnostic>,
    probes: BTreeMap<(PathBuf, bool), SourceProbe>,
    import_stack: HashSet<PathBuf>,
}

impl Scanner<'_> {
    fn observe(&mut self, path: &Path, is_pattern: bool) {
        let key = (path.to_path_buf(), is_pattern);
        if self.probes.contains_key(&key) {
            return;
        }
        match observe_source(path, is_pattern) {
            Ok(probe) => {
                self.probes.insert(key, probe);
            }
            Err(error) => self.problem("sourceObservationFailed", error, path),
        }
    }
    pub fn problem(&mut self, code: &str, message: impl Into<String>, path: &Path) {
        self.diagnostics
            .push(Diagnostic::new(code, message, Some(path.to_path_buf())));
    }

    pub fn document(&mut self, path: &Path, format: &str) -> Option<(Value, SourceDocument)> {
        self.observe(path, false);
        let text = match adapters::read_text(path) {
            Ok(Some(text)) => text,
            Ok(None) => return None,
            Err(error) => {
                self.problem("configurationReadFailed", error, path);
                return None;
            }
        };
        let value = match format {
            "toml" => adapters::parse_toml(&text),
            "yaml" => adapters::parse_yaml(&text),
            _ => adapters::parse_json(&text),
        };
        match value {
            Ok(value)
                if (format == "yaml" && !value.is_array())
                    || (format != "yaml" && !value.is_object()) =>
            {
                self.problem(
                    "configurationInvalid",
                    "configuration has an invalid root structure",
                    path,
                );
                None
            }
            Ok(value) => match path.canonicalize() {
                Ok(resolved_path) => Some((
                    value,
                    SourceDocument {
                        path: path.to_path_buf(),
                        resolved_path,
                        kind: SourceDocumentKind::File,
                        content_hash: content_hash(text.as_bytes()),
                    },
                )),
                Err(error) => {
                    self.problem("configurationChanged", error.to_string(), path);
                    None
                }
            },
            Err(error) => {
                self.problem("configurationInvalid", error, path);
                None
            }
        }
    }

    pub fn insert(&mut self, asset: EnvironmentAsset) {
        match self.assets.get_mut(&asset.id) {
            Some(existing) => {
                if asset.scope == AssetScope::Project && existing.scope != AssetScope::Project {
                    existing.scope = AssetScope::Project;
                    existing.scope_root = asset.scope_root.clone();
                    existing.source_tool = asset.source_tool;
                }
                for document in &asset.source_documents {
                    if !existing.source_documents.contains(document) {
                        existing.source_documents.push(document.clone());
                    }
                }
                for evidence in asset.evidence {
                    if !existing.evidence.contains(&evidence) {
                        existing.evidence.push(evidence);
                    }
                }
                existing.selected_by_default |= asset.selected_by_default;
            }
            None => {
                self.assets.insert(asset.id.clone(), asset);
            }
        }
    }

    pub fn file(
        &mut self,
        tool: SourceTool,
        kind: AssetKind,
        scope: AssetScope,
        path: &Path,
        format: &str,
    ) -> Option<String> {
        if credential_file(path) {
            self.problem(
                "credentialResourceExcluded",
                "credential files are not portable context resources",
                path,
            );
            return None;
        }
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.observe(path, false);
                return None;
            }
            Err(error) => {
                self.problem("resourceReadFailed", error.to_string(), path);
                return None;
            }
            Ok(_) => {}
        }
        let resolved = match path.canonicalize() {
            Ok(path) if path.is_file() => path,
            Ok(_) => {
                self.problem("resourceNotFile", "expected a file", path);
                return None;
            }
            Err(error) => {
                self.problem("resourceUnavailable", error.to_string(), path);
                return None;
            }
        };
        if credential_file(&resolved) {
            self.problem(
                "credentialResourceExcluded",
                "resource resolves to a credential file",
                path,
            );
            return None;
        }
        self.observe(path, false);
        let document = match fingerprint(path) {
            Ok(document) => document,
            Err(error) => {
                self.problem("resourceReadFailed", error, path);
                return None;
            }
        };
        if document.resolved_path != resolved {
            self.problem(
                "resourceChanged",
                "resource link changed during discovery",
                path,
            );
            return None;
        }
        if kind == AssetKind::Instruction
            && tool == SourceTool::Codex
            && adapters::read_text(&resolved)
                .ok()
                .flatten()
                .is_some_and(|text| text.trim().is_empty())
        {
            return None;
        }
        let name = path.file_name()?.to_string_lossy().to_string();
        let id = asset_id(kind, &resolved, &name);
        let mut definition = json!({ "fileName": name });
        let mut diagnostics = vec![];
        if matches!(kind, AssetKind::Skill | AssetKind::Command) {
            match adapters::read_text(&resolved)
                .and_then(|text| adapters::frontmatter(&text.unwrap_or_default()))
            {
                Ok(metadata) => definition["metadata"] = metadata,
                Err(error) => diagnostics.push(Diagnostic::new(
                    "frontmatterInvalid",
                    error,
                    Some(path.to_path_buf()),
                )),
            }
        }
        let root = resolved.parent()?.to_path_buf();
        self.insert(EnvironmentAsset {
            id: id.clone(),
            kind,
            name: if kind == AssetKind::Command {
                path.file_stem()?.to_string_lossy().to_string()
            } else {
                name.clone()
            },
            source_tool: tool,
            scope,
            scope_root: if scope == AssetScope::Project {
                self.project.root.clone()
            } else {
                self.roots.tool_home(tool).to_path_buf()
            },
            resource_root: Some(root),
            format: format.to_string(),
            files: vec![SourceFile {
                path: resolved.clone(),
                relative_path: PathBuf::from(name),
            }],
            source_documents: vec![document],
            definition: Some(definition),
            evidence: vec![],
            selected_by_default: scope == AssetScope::Project,
            source_priority: self.source_priority(tool, kind, scope, format, path),
            source_active: true,
            diagnostics,
        });
        if tool == SourceTool::Claude && kind == AssetKind::Instruction {
            self.instruction_imports(tool, scope, &resolved, &id);
        }
        Some(id)
    }

    fn instruction_imports(&mut self, tool: SourceTool, scope: AssetScope, path: &Path, id: &str) {
        if self.import_stack.len() >= 16 || !self.import_stack.insert(path.to_path_buf()) {
            if let Some(asset) = self.assets.get_mut(id) {
                asset.diagnostics.push(Diagnostic::new(
                    "instructionImportCycle",
                    "instruction imports are cyclic or exceed the depth limit",
                    Some(path.to_path_buf()),
                ));
            }
            return;
        }
        let text = match adapters::read_text(path) {
            Ok(Some(text)) => text,
            Ok(None) => {
                self.import_stack.remove(path);
                return;
            }
            Err(error) => {
                self.problem("instructionImportReadFailed", error, path);
                self.import_stack.remove(path);
                return;
            }
        };
        let mut imports = vec![];
        for literal in super::markdown::import_paths(&text) {
            let Some(target) =
                self.expand_path(&literal, path.parent().unwrap_or(&self.project.root))
            else {
                if let Some(asset) = self.assets.get_mut(id) {
                    asset.diagnostics.push(Diagnostic::new(
                        "instructionImportUnresolved",
                        "import requires unsupported path expansion",
                        Some(path.to_path_buf()),
                    ));
                }
                continue;
            };
            let within_project = target
                .canonicalize()
                .is_ok_and(|target| target.starts_with(&self.project.root));
            let import_scope = if scope == AssetScope::Project && within_project {
                AssetScope::Project
            } else {
                AssetScope::User
            };
            let imported = self.file(
                tool,
                AssetKind::Instruction,
                import_scope,
                &target,
                "claude-import",
            );
            if let Some(imported) = imported {
                if let Some(asset) = self.assets.get_mut(&imported) {
                    asset.evidence.push(UsageEvidence {
                        kind: "instructionImport".into(),
                        detail: format!("referenced by {}", path.display()),
                        session_id: None,
                    });
                }
                imports.push(json!({"literal":literal,"assetId":imported}));
            } else if literal.contains(['/', '\\', '.']) {
                if let Some(asset) = self.assets.get_mut(id) {
                    asset.diagnostics.push(Diagnostic::new(
                        "instructionImportUnavailable",
                        "import is missing, excluded, or unreadable",
                        Some(target),
                    ));
                }
            }
        }
        if let Some(asset) = self.assets.get_mut(id) {
            asset.definition.get_or_insert_with(|| json!({}))["imports"] = Value::Array(imports);
        }
        self.import_stack.remove(path);
    }

    pub fn instruction_chain(
        &mut self,
        tool: SourceTool,
        candidates: &[String],
        additive: &[&str],
    ) {
        for directory in self.project_directories(tool) {
            for name in candidates {
                let path = directory.join(name);
                if self
                    .file(
                        tool,
                        AssetKind::Instruction,
                        AssetScope::Project,
                        &path,
                        "instructions",
                    )
                    .is_some()
                {
                    break;
                }
            }
            for name in additive {
                self.file(
                    tool,
                    AssetKind::Instruction,
                    AssetScope::Project,
                    &directory.join(name),
                    "instructions",
                );
            }
        }
    }

    pub fn project_directories(&self, tool: SourceTool) -> Vec<PathBuf> {
        let mut directories = vec![self.project.root.clone()];
        for session in self
            .sessions
            .iter()
            .filter(|session| session.metadata.source_tool == tool)
        {
            let cwd = match Path::new(&session.metadata.cwd).canonicalize() {
                Ok(path) => path,
                Err(_) => continue,
            };
            if let Ok(relative) = cwd.strip_prefix(&self.project.root) {
                let mut path = self.project.root.clone();
                for part in relative.components() {
                    path.push(part);
                    if !directories.contains(&path) {
                        directories.push(path.clone());
                    }
                }
            }
        }
        directories
    }

    pub fn markdown_directory(
        &mut self,
        tool: SourceTool,
        kind: AssetKind,
        scope: AssetScope,
        root: &Path,
    ) {
        self.observe(root, false);
        let mut files = vec![];
        let mut diagnostics = vec![];
        collect_resources(
            root,
            root,
            &mut HashSet::new(),
            &mut files,
            &mut diagnostics,
            0,
        );
        self.diagnostics.extend(diagnostics);
        for file in files
            .into_iter()
            .filter(|file| file.path.extension().and_then(|value| value.to_str()) == Some("md"))
        {
            self.file(
                tool,
                kind,
                scope,
                &file.path,
                if kind == AssetKind::Memory {
                    "markdown-memory"
                } else {
                    "markdown-command"
                },
            );
        }
    }

    pub fn skills(&mut self, tool: SourceTool, scope: AssetScope, root: &Path, recursive: bool) {
        self.observe(root, false);
        let mut mains = vec![];
        let mut diagnostics = vec![];
        find_skills(
            root,
            recursive,
            matches!(tool, SourceTool::Dsh | SourceTool::Pi),
            &mut HashSet::new(),
            &mut mains,
            &mut diagnostics,
            0,
        );
        self.diagnostics.extend(diagnostics);
        for main in mains {
            let Some(id) = self.file(tool, AssetKind::Skill, scope, &main, "agent-skill") else {
                continue;
            };
            if main.file_name().and_then(|name| name.to_str()) != Some("SKILL.md") {
                if let Some(asset) = self.assets.get_mut(&id) {
                    asset.name = asset
                        .definition
                        .as_ref()
                        .and_then(|value| value.pointer("/metadata/name"))
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            main.file_stem()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string()
                        });
                    asset.format = "flat-skill".to_string();
                }
                continue;
            }
            let package = match main.parent().and_then(|path| path.canonicalize().ok()) {
                Some(path) => path,
                None => continue,
            };
            self.bundle(&id, &package);
            if let Some(asset) = self.assets.get_mut(&id) {
                asset.name = asset
                    .definition
                    .as_ref()
                    .and_then(|value| value.pointer("/metadata/name"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        package
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string()
                    });
            }
        }
    }

    fn bundle(&mut self, id: &str, root: &Path) {
        self.observe(root, false);
        let mut files = vec![];
        let mut diagnostics = vec![];
        collect_resources(
            root,
            root,
            &mut HashSet::new(),
            &mut files,
            &mut diagnostics,
            0,
        );
        let mut documents = self
            .assets
            .get(id)
            .map(|asset| asset.source_documents.clone())
            .unwrap_or_default();
        for file in &files {
            match fingerprint(&file.path) {
                Ok(document) => {
                    if !documents.contains(&document) {
                        documents.push(document);
                    }
                }
                Err(error) => diagnostics.push(Diagnostic::new(
                    "resourceReadFailed",
                    error,
                    Some(file.path.clone()),
                )),
            }
        }
        if let Some(asset) = self.assets.get_mut(id) {
            asset.resource_root = Some(root.to_path_buf());
            asset.files = files;
            asset.source_documents = documents;
            asset.definition.get_or_insert_with(|| json!({}))["resourceBundle"] = Value::Bool(true);
            asset.diagnostics.extend(diagnostics);
        }
    }

    fn script(&mut self, tool: SourceTool, path: &Path) {
        let Some(id) = self.file(
            tool,
            AssetKind::Script,
            AssetScope::Project,
            path,
            "project-script",
        ) else {
            return;
        };
        let root = path.parent().and_then(|parent| {
            parent.ancestors().find(|parent| {
                *parent != self.project.root
                    && parent.starts_with(&self.project.root)
                    && parent
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| matches!(name, "scripts" | "tools" | "bin" | "tasks"))
            })
        });
        if let Some(root) = root {
            self.bundle(&id, root);
        } else if let Some(asset) = self.assets.get_mut(&id) {
            asset.diagnostics.push(Diagnostic::new(
                "scriptDependenciesUnverified",
                "review companion files of this standalone script",
                Some(path.to_path_buf()),
            ));
        }
    }

    pub fn mcp(
        &mut self,
        tool: SourceTool,
        scope: AssetScope,
        document: &SourceDocument,
        name: &str,
        value: &Value,
        format: &str,
    ) {
        let mut server = value.clone();
        let removed = adapters::strip_secrets(&mut server);
        let diagnostics = removed
            .into_iter()
            .map(|field| {
                Diagnostic::new(
                    "credentialRequired",
                    format!("configure the target credential field: {field}"),
                    Some(document.path.clone()),
                )
            })
            .collect();
        self.insert(EnvironmentAsset {
            id: asset_id(AssetKind::Mcp, &document.path, name),
            kind: AssetKind::Mcp,
            name: name.to_string(),
            source_tool: tool,
            scope,
            scope_root: if scope == AssetScope::Project {
                self.project.root.clone()
            } else {
                document
                    .path
                    .parent()
                    .unwrap_or(Path::new(""))
                    .to_path_buf()
            },
            resource_root: None,
            format: format.to_string(),
            files: vec![],
            source_documents: vec![document.clone()],
            definition: Some(server),
            evidence: vec![],
            selected_by_default: scope == AssetScope::Project,
            source_priority: self.source_priority(
                tool,
                AssetKind::Mcp,
                scope,
                format,
                &document.path,
            ),
            source_active: true,
            diagnostics,
        });
    }

    pub fn mcp_map(
        &mut self,
        tool: SourceTool,
        scope: AssetScope,
        document: &SourceDocument,
        map: Option<&Value>,
        format: &str,
    ) {
        if let Some(servers) = map.and_then(Value::as_object) {
            for (name, value) in servers {
                self.mcp(tool, scope, document, name, value, format);
            }
        }
    }

    pub fn command_map(
        &mut self,
        tool: SourceTool,
        scope: AssetScope,
        document: &SourceDocument,
        value: Option<&Value>,
        format: &str,
    ) {
        if let Some(commands) = value.and_then(Value::as_object) {
            for (name, definition) in commands {
                self.insert(EnvironmentAsset {
                    id: asset_id(AssetKind::Command, &document.path, name),
                    kind: AssetKind::Command,
                    name: name.clone(),
                    source_tool: tool,
                    scope,
                    scope_root: if scope == AssetScope::Project {
                        self.project.root.clone()
                    } else {
                        self.roots.tool_home(tool).to_path_buf()
                    },
                    resource_root: None,
                    format: format.to_string(),
                    files: vec![],
                    source_documents: vec![document.clone()],
                    definition: Some(definition.clone()),
                    evidence: vec![],
                    selected_by_default: scope == AssetScope::Project,
                    source_priority: self.source_priority(
                        tool,
                        AssetKind::Command,
                        scope,
                        format,
                        &document.path,
                    ),
                    source_active: true,
                    diagnostics: vec![],
                });
            }
        }
    }

    pub fn expand_path(&self, value: &str, base: &Path) -> Option<PathBuf> {
        if value.contains("${")
            || value.contains("{env:")
            || value.contains("://")
            || value.starts_with('!')
        {
            return None;
        }
        Some(if let Some(value) = value.strip_prefix("~/") {
            self.roots.user_home.join(value)
        } else if Path::new(value).is_absolute() {
            PathBuf::from(value)
        } else {
            base.join(value)
        })
    }

    pub fn referenced_files(
        &mut self,
        tool: SourceTool,
        scope: AssetScope,
        value: Option<&Value>,
        base: &Path,
    ) {
        let Some(values) = value.and_then(Value::as_array) else {
            return;
        };
        for pattern in values.iter().filter_map(Value::as_str) {
            if pattern.contains("://")
                || pattern.starts_with('~')
                || pattern.contains("${")
                || pattern.contains("{env:")
            {
                self.problem(
                    "externalReference",
                    "reference needs explicit source resolution",
                    &base.join(pattern),
                );
                continue;
            }
            let pattern = base.join(pattern).to_string_lossy().to_string();
            self.observe(Path::new(&pattern), true);
            match glob::glob(&pattern) {
                Ok(paths) => {
                    let mut found = false;
                    for path in paths {
                        match path {
                            Ok(path) => {
                                found = true;
                                self.file(
                                    tool,
                                    AssetKind::Instruction,
                                    scope,
                                    &path,
                                    "instructions",
                                );
                            }
                            Err(error) => {
                                self.problem("referenceReadFailed", error.to_string(), error.path())
                            }
                        }
                    }
                    if !found {
                        self.problem(
                            "referenceMissing",
                            "configured instruction reference did not match a file",
                            Path::new(&pattern),
                        );
                    }
                }
                Err(error) => self.problem(
                    "referencePatternInvalid",
                    error.to_string(),
                    Path::new(&pattern),
                ),
            }
        }
    }

    fn project_scripts(&mut self, tool: SourceTool) {
        let package = self.project.root.join("package.json");
        if let Some((value, document)) = self.document(&package, "json") {
            if let Some(scripts) = value.get("scripts").and_then(Value::as_object) {
                for (name, command) in scripts {
                    if let Some(command) = command.as_str() {
                        self.insert(EnvironmentAsset {
                            id: asset_id(AssetKind::Command, &package, name),
                            kind: AssetKind::Command,
                            name: name.clone(),
                            source_tool: tool,
                            scope: AssetScope::Project,
                            scope_root: self.project.root.clone(),
                            resource_root: Some(self.project.root.clone()),
                            format: "package-script".to_string(),
                            files: vec![],
                            source_documents: vec![document.clone()],
                            definition: Some(
                                json!({"command": command, "runner": "npm", "name": name}),
                            ),
                            evidence: vec![UsageEvidence {
                                kind: "projectConfiguration".into(),
                                detail: "package.json scripts".into(),
                                session_id: None,
                            }],
                            selected_by_default: true,
                            source_priority: 0,
                            source_active: true,
                            diagnostics: vec![],
                        });
                        for token in command.split_whitespace() {
                            let token = token.trim_matches(['\'', '"']);
                            let path = self.project.root.join(token);
                            let explicit_path = token.contains(['/', '\\']);
                            let script_extension = path
                                .extension()
                                .and_then(|value| value.to_str())
                                .is_some_and(|value| {
                                    matches!(
                                        value,
                                        "sh" | "bash"
                                            | "zsh"
                                            | "py"
                                            | "js"
                                            | "cjs"
                                            | "mjs"
                                            | "ts"
                                            | "ps1"
                                            | "rb"
                                    )
                                });
                            if (explicit_path || script_extension)
                                && path.is_file()
                                && path
                                    .canonicalize()
                                    .is_ok_and(|path| path.starts_with(&self.project.root))
                            {
                                self.script(tool, &path);
                            }
                        }
                    }
                }
            }
        }
        for path in &self.options.extra_scripts {
            let path = if path.is_absolute() {
                path.clone()
            } else {
                self.project.root.join(path)
            };
            match path.canonicalize() {
                Ok(resolved) if resolved.starts_with(&self.project.root) => {
                    self.script(tool, &resolved);
                }
                _ => self.problem(
                    "scriptOutsideProject",
                    "explicit script must be a file in the selected project",
                    &path,
                ),
            }
        }
    }

    fn source_priority(
        &self,
        tool: SourceTool,
        kind: AssetKind,
        scope: AssetScope,
        format: &str,
        path: &Path,
    ) -> u32 {
        if kind == AssetKind::Mcp {
            return match format {
                "claude-local-mcp" => 0,
                "claude-project-mcp" => 10,
                "claude-user-mcp" => 20,
                _ if scope == AssetScope::Profile => 0,
                _ if scope == AssetScope::Project => 10,
                _ => 20,
            };
        }
        if kind == AssetKind::Skill {
            match tool {
                SourceTool::Claude => return if scope == AssetScope::User { 0 } else { 10 },
                SourceTool::Pi => {
                    if path.starts_with(self.roots.pi.join("skills")) {
                        return 0;
                    }
                    if path.starts_with(self.project.root.join(".pi/skills")) {
                        return 10;
                    }
                    return if scope == AssetScope::User { 20 } else { 30 };
                }
                SourceTool::Dsh => {
                    if path.starts_with(self.project.root.join(".dsh/skills")) {
                        return 100;
                    }
                    if path.starts_with(self.project.root.join(".agents/skills")) {
                        return 200;
                    }
                    return if path.starts_with(self.roots.dsh.join("skills")) {
                        400
                    } else {
                        500
                    };
                }
                SourceTool::OpenCode => {
                    let base = if scope == AssetScope::Project { 0 } else { 100 };
                    let native = if scope == AssetScope::Project {
                        self.project_directories(tool)
                            .iter()
                            .any(|root| path.starts_with(root.join(".opencode/skills")))
                    } else {
                        path.starts_with(self.roots.opencode.join("skills"))
                    };
                    let claude = if scope == AssetScope::Project {
                        self.project_directories(tool)
                            .iter()
                            .any(|root| path.starts_with(root.join(".claude/skills")))
                    } else {
                        path.starts_with(self.roots.claude.join("skills"))
                    };
                    return base
                        + if native {
                            0
                        } else if claude {
                            10
                        } else {
                            20
                        };
                }
                SourceTool::Codex => return 0,
            }
        }
        if scope == AssetScope::Project {
            0
        } else {
            100
        }
    }

    fn resolve_shadowing(&mut self) {
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for asset in self.assets.values() {
            if matches!(asset.kind, AssetKind::Skill | AssetKind::Mcp)
                && !(asset.kind == AssetKind::Skill && asset.source_tool == SourceTool::Codex)
            {
                groups
                    .entry(format!(
                        "{:?}/{:?}/{}",
                        asset.source_tool, asset.kind, asset.name
                    ))
                    .or_default()
                    .push(asset.id.clone());
            }
        }
        for ids in groups.values().filter(|ids| ids.len() > 1) {
            let minimum = ids
                .iter()
                .filter_map(|id| self.assets.get(id))
                .map(|asset| asset.source_priority)
                .min()
                .unwrap_or(0);
            let winners: Vec<_> = ids
                .iter()
                .filter(|id| self.assets[*id].source_priority == minimum)
                .cloned()
                .collect();
            for id in ids {
                let active = winners.len() == 1 && winners[0] == *id;
                if let Some(asset) = self.assets.get_mut(id) {
                    asset.source_active = active;
                    asset.selected_by_default &= active;
                    if !active {
                        asset.diagnostics.push(Diagnostic::new(
                            if winners.len() == 1 {
                                "sourceShadowed"
                            } else {
                                "sourceAmbiguous"
                            },
                            if winners.len() == 1 {
                                "another native definition has higher precedence"
                            } else {
                                "multiple definitions require an explicit source choice"
                            },
                            asset
                                .source_documents
                                .first()
                                .map(|document| document.path.clone()),
                        ));
                    }
                }
            }
        }
    }

    fn add_usage_evidence(&mut self) {
        for asset in self.assets.values_mut() {
            for session in self.sessions {
                for message in &session.session.messages {
                    for call in &message.tool_calls {
                        let normalized = asset.name.replace('-', "_");
                        let used = asset.source_active
                            && ((asset.kind == AssetKind::Mcp
                                && session.metadata.source_tool == asset.source_tool
                                && call.name.starts_with(&format!("mcp__{normalized}__")))
                                || (asset.kind == AssetKind::Skill
                                    && call.name == "skill"
                                    && ["name", "skill"].iter().any(|key| {
                                        call.input.get(key).and_then(Value::as_str)
                                            == Some(&asset.name)
                                    })));
                        if used {
                            let evidence = UsageEvidence {
                                kind: "selectedSessionToolCall".into(),
                                detail: call.name.clone(),
                                session_id: Some(session.metadata.session_id.clone()),
                            };
                            if !asset.evidence.contains(&evidence) {
                                asset.evidence.push(evidence);
                            }
                        }
                    }
                }
            }
            if asset.scope != AssetScope::Project && asset.evidence.is_empty() {
                asset.evidence.push(UsageEvidence {
                    kind: "candidateOnly".into(),
                    detail: "available in user configuration; project use is not established"
                        .into(),
                    session_id: None,
                });
            }
        }
    }
}

pub(crate) fn asset_id(kind: AssetKind, path: &Path, name: &str) -> String {
    content_hash(format!("{kind:?}\0{}\0{name}", path.display()).as_bytes())
}

fn credential_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "auth.json"
            | "credentials.json"
            | ".credentials.json"
            | "mcp-auth.json"
            | ".claude.json"
            | "id_rsa"
            | "id_ed25519"
            | "id_ecdsa"
            | ".env"
            | ".envrc"
    ) || (name.starts_with(".env.")
        && !matches!(
            name.as_str(),
            ".env.example" | ".env.sample" | ".env.template"
        ))
}

pub(crate) fn fingerprint(path: &Path) -> Result<SourceDocument, String> {
    let mut file = fs::File::open(path).map_err(|error| error.to_string())?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("resource is not a regular file".to_string());
    }
    let mut hasher = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(SourceDocument {
        path: path.to_path_buf(),
        resolved_path: path.canonicalize().map_err(|error| error.to_string())?,
        kind: SourceDocumentKind::File,
        content_hash: format!("{:x}", hasher.finalize()),
    })
}

pub(crate) fn observe_source(path: &Path, is_pattern: bool) -> Result<SourceProbe, String> {
    let document = if is_pattern {
        let mut matches = glob::glob(&path.to_string_lossy())
            .map_err(|error| error.to_string())?
            .map(|entry| {
                entry
                    .map(|path| path.to_string_lossy().to_string())
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        matches.sort();
        Some(SourceDocument {
            path: path.to_path_buf(),
            resolved_path: path.to_path_buf(),
            kind: SourceDocumentKind::Pattern,
            content_hash: content_hash(
                &serde_json::to_vec(&matches).map_err(|error| error.to_string())?,
            ),
        })
    } else {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
            Ok(_) => {
                let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
                if metadata.is_dir() {
                    let entries = directory_entries(path)?
                        .into_iter()
                        .map(|entry| {
                            let kind = entry.file_type().map_err(|error| error.to_string())?;
                            let link = if kind.is_symlink() {
                                Some(
                                    fs::read_link(entry.path())
                                        .map_err(|error| error.to_string())?
                                        .to_string_lossy()
                                        .to_string(),
                                )
                            } else {
                                None
                            };
                            Ok((
                                entry.file_name().to_string_lossy().to_string(),
                                kind.is_dir(),
                                kind.is_file(),
                                link,
                            ))
                        })
                        .collect::<Result<Vec<_>, String>>()?;
                    Some(SourceDocument {
                        path: path.to_path_buf(),
                        resolved_path: path.canonicalize().map_err(|error| error.to_string())?,
                        kind: SourceDocumentKind::Directory,
                        content_hash: content_hash(
                            &serde_json::to_vec(&entries).map_err(|error| error.to_string())?,
                        ),
                    })
                } else {
                    Some(fingerprint(path)?)
                }
            }
        }
    };
    Ok(SourceProbe {
        path: path.to_path_buf(),
        is_pattern,
        document,
    })
}

fn directory_entries(root: &Path) -> Result<Vec<fs::DirEntry>, String> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                && fs::symlink_metadata(root).is_err() =>
        {
            return Ok(vec![])
        }
        Err(error) => return Err(error.to_string()),
    };
    let mut entries = entries
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

fn find_skills(
    root: &Path,
    recursive: bool,
    allow_flat: bool,
    seen: &mut HashSet<PathBuf>,
    mains: &mut Vec<PathBuf>,
    diagnostics: &mut Vec<Diagnostic>,
    depth: usize,
) {
    if depth > 32 {
        diagnostics.push(Diagnostic::new(
            "resourceDepthExceeded",
            "skill search is too deep",
            Some(root.to_path_buf()),
        ));
        return;
    }
    if let Ok(resolved) = root.canonicalize() {
        if !seen.insert(resolved) {
            diagnostics.push(Diagnostic::new(
                "resourceCycle",
                "skill directory is repeated or cyclic",
                Some(root.to_path_buf()),
            ));
            return;
        }
    }
    let entries = match directory_entries(root) {
        Ok(entries) => entries,
        Err(error) => {
            diagnostics.push(Diagnostic::new(
                "resourceReadFailed",
                error,
                Some(root.to_path_buf()),
            ));
            return;
        }
    };
    for entry in entries {
        let path = entry.path();
        if path
            .file_name()
            .is_some_and(|name| name == ".system" || name == ".git")
        {
            continue;
        }
        if path.is_dir() {
            if path.join("SKILL.md").is_file() {
                mains.push(path.join("SKILL.md"));
            } else if recursive {
                find_skills(&path, true, false, seen, mains, diagnostics, depth + 1);
            }
        } else if allow_flat
            && path.extension().and_then(|extension| extension.to_str()) == Some("md")
        {
            mains.push(path);
        }
    }
}

pub(crate) fn collect_resources(
    root: &Path,
    directory: &Path,
    seen: &mut HashSet<PathBuf>,
    files: &mut Vec<SourceFile>,
    diagnostics: &mut Vec<Diagnostic>,
    depth: usize,
) {
    if depth > 64 {
        diagnostics.push(Diagnostic::new(
            "resourceDepthExceeded",
            "resource tree is too deep",
            Some(directory.to_path_buf()),
        ));
        return;
    }
    let resolved = match directory.canonicalize() {
        Ok(path) => path,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound
                || fs::symlink_metadata(directory).is_ok()
            {
                diagnostics.push(Diagnostic::new(
                    "resourceUnavailable",
                    error.to_string(),
                    Some(directory.to_path_buf()),
                ));
            }
            return;
        }
    };
    let allowed = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if !resolved.starts_with(&allowed) {
        diagnostics.push(Diagnostic::new(
            "externalResource",
            "resource link leaves its package",
            Some(directory.to_path_buf()),
        ));
        return;
    }
    if !seen.insert(resolved) {
        diagnostics.push(Diagnostic::new(
            "resourceCycle",
            "resource directory is repeated or cyclic",
            Some(directory.to_path_buf()),
        ));
        return;
    }
    let entries = match directory_entries(directory) {
        Ok(entries) => entries,
        Err(error) => {
            diagnostics.push(Diagnostic::new(
                "resourceReadFailed",
                error,
                Some(directory.to_path_buf()),
            ));
            return;
        }
    };
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        if credential_file(&path) {
            diagnostics.push(Diagnostic::new(
                "credentialResourceExcluded",
                "supply credential resources separately in the target",
                Some(path),
            ));
            continue;
        }
        if name == ".git" || name == "node_modules" || name == "__pycache__" {
            if name != ".git" {
                diagnostics.push(Diagnostic::new(
                    "dependencyDirectory",
                    "generated dependency directory must be installed in the target",
                    Some(path),
                ));
            }
            continue;
        }
        match path.canonicalize() {
            Ok(actual) if !actual.starts_with(&allowed) => diagnostics.push(Diagnostic::new(
                "externalResource",
                "resource link leaves its package",
                Some(path),
            )),
            Ok(actual) if actual.is_dir() => {
                collect_resources(root, &path, seen, files, diagnostics, depth + 1)
            }
            Ok(actual) if credential_file(&actual) => diagnostics.push(Diagnostic::new(
                "credentialResourceExcluded",
                "resource link resolves to a credential file",
                Some(path),
            )),
            Ok(actual) if actual.is_file() => files.push(SourceFile {
                path: actual,
                relative_path: path.strip_prefix(root).unwrap_or(&path).to_path_buf(),
            }),
            Ok(_) => diagnostics.push(Diagnostic::new(
                "resourceNotFile",
                "resource is not a regular file",
                Some(path),
            )),
            Err(error) => diagnostics.push(Diagnostic::new(
                "resourceUnavailable",
                error.to_string(),
                Some(path),
            )),
        }
    }
}
