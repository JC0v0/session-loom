use super::{
    model::{
        content_hash, ApplyReport, ItemResult, MigrationRun, PreviewFile, PreviewRevision,
        Readiness, RunState, SessionApplyResult, TargetSessionRoots,
    },
    preview::{load_preview, read_preview_file, read_target, verify_run_snapshots},
    run_store::{private_directory, private_file, RunStore},
};
use crate::{
    adapters::{claude, codex, dsh, opencode, pi},
    canonical::{CanonicalSession, SourceTool},
    restore::RestoreRoots,
    store::Store,
};
use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

const APPLY_SCHEMA_VERSION: u32 = 1;

impl From<&RestoreRoots> for TargetSessionRoots {
    fn from(roots: &RestoreRoots) -> Self {
        Self {
            codex: roots.codex.clone(),
            codex_home: roots.codex_home.clone(),
            claude: roots.claude.clone(),
            opencode: roots.opencode.clone(),
            dsh: roots.dsh.clone(),
            pi: roots.pi.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ApplyFileStep {
    item_ids: Vec<String>,
    target_path: PathBuf,
    scope_root: PathBuf,
    private_path: PathBuf,
    output_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_hash: Option<String>,
    executable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ApplySessionStep {
    source_session_id: String,
    snapshot_path: PathBuf,
    content_hash: String,
    conversation_id: String,
    target_session_id: String,
    target_cwd: PathBuf,
    timestamp: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    codex_target_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct ApplyPlan {
    schema_version: u32,
    run_id: String,
    preview_id: String,
    run_revision: u64,
    created_at: String,
    session_roots: TargetSessionRoots,
    files: Vec<ApplyFileStep>,
    sessions: Vec<ApplySessionStep>,
}

/// Apply the confirmed preview. Every target session id and every media
/// dependent path is pinned in a private plan before the first write, so a
/// retry after a partial failure or a process crash reuses them instead of
/// creating another native copy.
pub fn apply_preview(
    store: &Store,
    runs: &RunStore,
    run_id: &str,
    preview_id: &str,
    roots: &RestoreRoots,
) -> Result<ApplyReport, String> {
    let run = runs.load(run_id)?;
    let preview = load_preview(runs, run_id, preview_id)?;
    if run.preview.as_ref().map(|preview| preview.id.as_str()) != Some(preview_id) {
        return Err("preview is no longer the current migration preview".to_string());
    }
    if preview.run_id != run.id {
        return Err("preview does not belong to this migration".to_string());
    }
    if run.target.as_ref() != Some(&preview.target) {
        return Err("preview target no longer matches the migration".to_string());
    }
    if run.state == RunState::Previewed && run.revision != preview.run_revision {
        return Err("preview revision is stale; regenerate the preview".to_string());
    }
    if !preview.ready_to_apply || preview.conflicts.iter().any(|conflict| !conflict.resolved) {
        return Err(
            "preview still has unresolved conflicts or blockers; resolve them before applying"
                .to_string(),
        );
    }
    match run.state {
        RunState::Completed => {
            return run
                .apply_report
                .ok_or_else(|| "completed migration has no stored result".to_string())
        }
        RunState::Previewed | RunState::Applying | RunState::Partial | RunState::Interrupted => {}
        RunState::Draft => return Err("create a preview before applying the migration".to_string()),
    }
    verify_preview_for_apply(runs, &run, &preview)?;

    let run_directory = runs.run_directory(run_id)?;
    let apply_directory = run_directory.join("apply");
    private_directory(&apply_directory)?;
    private_directory(&apply_directory.join("backups"))?;
    let plan = load_or_create_plan(&apply_directory, store, &run, &preview, roots)?;

    if run.state == RunState::Previewed {
        let mut applying = run.clone();
        applying.revision = run
            .revision
            .checked_add(1)
            .ok_or_else(|| "migration revision exhausted".to_string())?;
        applying.updated_at = Utc::now().to_rfc3339();
        applying.state = RunState::Applying;
        runs.update(&applying, run.revision)?;
    }

    let done = read_journal(&apply_directory)?;
    let session_results = apply_sessions(store, runs, &run, &preview, &plan, &apply_directory);
    let file_failures = apply_files(runs, &preview, &plan, &apply_directory, &done);

    let mut items = vec![];
    for item in &preview.items {
        let mut result = item.result();
        if let Some(errors) = file_failures.get(&item.item_id) {
            result.readiness = Readiness::Failed;
            result.message = format!("目标文件写入失败: {}", errors.join("；"));
            result
                .next_steps
                .push("修复目标路径或权限后重试".to_string());
        } else if result.readiness == Readiness::Available {
            result.evidence.push("已按预览写入目标原生位置".to_string());
        }
        items.push(result);
    }

    let all_ready = items
        .iter()
        .all(|item| item.readiness == Readiness::Available)
        && session_results
            .iter()
            .all(|session| session.readiness == Readiness::Available);
    let state = if all_ready {
        RunState::Completed
    } else {
        RunState::Partial
    };
    let mut session_items = session_results.iter().map(session_item_result).collect();
    items.append(&mut session_items);
    let report = ApplyReport {
        run_id: run.id.clone(),
        preview_id: preview.id.clone(),
        state,
        items: items.clone(),
        sessions: session_results,
        diagnostics: vec![],
    };

    let mut updated = runs.load(run_id)?;
    updated.revision = updated
        .revision
        .checked_add(1)
        .ok_or_else(|| "migration revision exhausted".to_string())?;
    updated.updated_at = Utc::now().to_rfc3339();
    updated.state = state;
    updated.results = items;
    updated.apply_report = Some(report.clone());
    runs.update(&updated, updated.revision - 1)?;
    Ok(report)
}

fn session_item_result(session: &SessionApplyResult) -> ItemResult {
    ItemResult {
        item_id: session.source_session_id.clone(),
        action: session.action.clone(),
        readiness: session.readiness,
        message: session.message.clone(),
        next_steps: session.next_steps.clone(),
        evidence: session
            .target_path
            .as_ref()
            .map(|path| vec![format!("目标会话: {}", path.display())])
            .unwrap_or_default(),
    }
}

fn verify_preview_for_apply(
    runs: &RunStore,
    run: &MigrationRun,
    preview: &PreviewRevision,
) -> Result<(), String> {
    verify_run_snapshots(runs, run)?;
    for file in &preview.files {
        let output = read_preview_file(runs, preview, file)?;
        if content_hash(&output) != file.output_hash {
            return Err(format!(
                "preview output changed: {}",
                file.target_path.display()
            ));
        }
        let current = read_target(&file.target_path)?;
        let current_hash = current.as_deref().map(content_hash);
        let acceptable = current_hash == file.target_hash
            || current_hash.as_deref() == Some(file.output_hash.as_str());
        if !acceptable {
            return Err(format!(
                "target changed after preview: {}",
                file.target_path.display()
            ));
        }
    }
    Ok(())
}

fn load_or_create_plan(
    apply_directory: &Path,
    store: &Store,
    run: &MigrationRun,
    preview: &PreviewRevision,
    roots: &RestoreRoots,
) -> Result<ApplyPlan, String> {
    let path = apply_directory.join("plan.json");
    if path.exists() {
        let plan = read_plan(&path)?;
        if plan.schema_version != APPLY_SCHEMA_VERSION
            || plan.run_id != run.id
            || plan.preview_id != preview.id
        {
            return Err("stored apply plan belongs to another preview".to_string());
        }
        let current = preview
            .environment
            .session_roots
            .clone()
            .unwrap_or_else(|| TargetSessionRoots::from(roots));
        if plan.session_roots != current {
            return Err("target session roots changed since the plan was created".to_string());
        }
        return Ok(plan);
    }
    let timestamp_ms = DateTime::parse_from_rfc3339(&preview.created_at)
        .map(|value| value.timestamp_millis())
        .unwrap_or_else(|_| Utc::now().timestamp_millis());
    let timestamp = DateTime::parse_from_rfc3339(&preview.created_at)
        .map(|value| value.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now())
        .to_rfc3339_opts(SecondsFormat::Millis, true);
    let session_roots = preview
        .environment
        .session_roots
        .clone()
        .unwrap_or_else(|| TargetSessionRoots::from(roots));
    let mut sessions = vec![];
    for session in &run.sessions {
        let conversation_id = store
            .conversation_id(&session.metadata.session_id)
            .unwrap_or_else(|_| session.metadata.session_id.clone());
        let target_session_id = pinned_session_id(preview.target.tool, timestamp_ms);
        let target_cwd = map_target_cwd(run, &session.metadata.cwd, &preview.target.project_root)?;
        let codex_target_path = if preview.target.tool == SourceTool::Codex {
            Some(codex_rollout_path(
                &session_roots.codex,
                &timestamp,
                &target_session_id,
            )?)
        } else {
            None
        };
        sessions.push(ApplySessionStep {
            source_session_id: session.metadata.session_id.clone(),
            snapshot_path: session.snapshot_path.clone(),
            content_hash: session.content_hash.clone(),
            conversation_id,
            target_session_id,
            target_cwd,
            timestamp: timestamp.clone(),
            codex_target_path,
        });
    }
    let files = preview
        .files
        .iter()
        .map(|file| ApplyFileStep {
            item_ids: file.item_ids.clone(),
            target_path: file.target_path.clone(),
            scope_root: file.scope_root.clone(),
            private_path: file.private_path.clone(),
            output_hash: file.output_hash.clone(),
            target_hash: file.target_hash.clone(),
            executable: file.executable,
        })
        .collect();
    let plan = ApplyPlan {
        schema_version: APPLY_SCHEMA_VERSION,
        run_id: run.id.clone(),
        preview_id: preview.id.clone(),
        run_revision: preview.run_revision,
        created_at: Utc::now().to_rfc3339(),
        session_roots,
        files,
        sessions,
    };
    write_plan(&path, &plan)?;
    Ok(plan)
}

fn pinned_session_id(tool: SourceTool, timestamp_ms: i64) -> String {
    match tool {
        SourceTool::Codex | SourceTool::Claude | SourceTool::Pi => Uuid::new_v4().to_string(),
        SourceTool::OpenCode => opencode::generate_id("ses", timestamp_ms),
        SourceTool::Dsh => format!("session-{}", Uuid::new_v4()),
    }
}

fn map_target_cwd(
    run: &MigrationRun,
    source_cwd: &str,
    target_project: &Path,
) -> Result<PathBuf, String> {
    let source = Path::new(source_cwd);
    let source_root = &run.project.root;
    let canonical_cwd = source.canonicalize().ok();
    let canonical_root = source_root.canonicalize().ok();
    let relative = canonical_cwd
        .as_deref()
        .zip(canonical_root.as_deref())
        .and_then(|(cwd, root)| cwd.strip_prefix(root).ok())
        .or_else(|| source.strip_prefix(source_root).ok())
        .ok_or_else(|| format!("session cwd is outside the source project: {source_cwd}"))?;
    // A clone in the same physical project keeps the spelling of the native
    // source cwd, matching the ordinary session restore path and card grouping.
    if canonical_cwd.is_some()
        && canonical_root.is_some()
        && target_project.canonicalize().ok() == canonical_root
    {
        return Ok(source.to_path_buf());
    }
    if relative.as_os_str().is_empty() {
        Ok(target_project.to_path_buf())
    } else {
        Ok(target_project.join(relative))
    }
}

fn codex_rollout_path(
    sessions_root: &Path,
    timestamp: &str,
    session_id: &str,
) -> Result<PathBuf, String> {
    let parsed = DateTime::parse_from_rfc3339(timestamp)
        .map_err(|error| format!("invalid apply timestamp: {error}"))?
        .with_timezone(&Local);
    use chrono::{Datelike, Timelike};
    let (year, month, day) = (parsed.year(), parsed.month(), parsed.day());
    Ok(sessions_root
        .join(format!("{year:04}"))
        .join(format!("{month:02}"))
        .join(format!("{day:02}"))
        .join(format!(
            "rollout-{year:04}-{month:02}-{day:02}T{:02}-{:02}-{:02}-{session_id}.jsonl",
            parsed.hour(),
            parsed.minute(),
            parsed.second()
        )))
}

fn read_plan(path: &Path) -> Result<ApplyPlan, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("apply plan is not a regular file".to_string());
    }
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let plan: ApplyPlan = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if plan.schema_version != APPLY_SCHEMA_VERSION {
        return Err("unsupported apply plan version".to_string());
    }
    Ok(plan)
}

fn write_plan(path: &Path, plan: &ApplyPlan) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(plan).map_err(|error| error.to_string())?;
    let mut file = private_file(path, true)?;
    file.write_all(&bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

fn read_journal(directory: &Path) -> Result<BTreeMap<String, String>, String> {
    let path = directory.join("journal.jsonl");
    let mut done = BTreeMap::new();
    let Ok(text) = fs::read_to_string(&path) else {
        return Ok(done);
    };
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let Some(id) = value.get("id").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(status) = value.get("status").and_then(serde_json::Value::as_str) else {
            continue;
        };
        done.insert(id.to_string(), status.to_string());
    }
    Ok(done)
}

fn append_journal(directory: &Path, id: &str, status: &str, message: &str) -> Result<(), String> {
    let path = directory.join("journal.jsonl");
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| error.to_string())?;
    let line = json!({
        "id": id,
        "status": status,
        "at": Utc::now().to_rfc3339(),
        "message": message
    });
    writeln!(file, "{}", line).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

fn apply_files(
    runs: &RunStore,
    preview: &PreviewRevision,
    plan: &ApplyPlan,
    apply_directory: &Path,
    done: &BTreeMap<String, String>,
) -> BTreeMap<String, Vec<String>> {
    let mut failures: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for step in &plan.files {
        let id = format!("file:{}", step.target_path.display());
        let preview_file = preview
            .files
            .iter()
            .find(|file| file.target_path == step.target_path);
        let Some(preview_file) = preview_file else {
            for item in &step.item_ids {
                failures
                    .entry(item.clone())
                    .or_default()
                    .push("preview no longer contains this target file".to_string());
            }
            continue;
        };
        if done.get(&id).map(String::as_str) == Some("done")
            && current_matches(&step.target_path, &step.output_hash)
        {
            continue;
        }
        match apply_file(runs, preview, preview_file, step, apply_directory) {
            Ok(()) => {
                let _ = append_journal(apply_directory, &id, "done", "");
            }
            Err(error) => {
                let _ = append_journal(apply_directory, &id, "failed", &error);
                for item in &step.item_ids {
                    failures
                        .entry(item.clone())
                        .or_default()
                        .push(error.clone());
                }
            }
        }
    }
    failures
}

fn current_matches(path: &Path, hash: &str) -> bool {
    read_target(path)
        .ok()
        .flatten()
        .map(|bytes| content_hash(&bytes) == hash)
        .unwrap_or(false)
}

fn apply_file(
    runs: &RunStore,
    preview: &PreviewRevision,
    preview_file: &PreviewFile,
    step: &ApplyFileStep,
    apply_directory: &Path,
) -> Result<(), String> {
    let current = read_target(&step.target_path)?;
    let current_hash = current.as_deref().map(content_hash);
    if current_hash.as_deref() == Some(step.output_hash.as_str()) {
        return Ok(());
    }
    if current_hash != step.target_hash {
        return Err("target changed after preview; refresh the preview".to_string());
    }
    ensure_target_path_allowed(step)?;
    let bytes = read_preview_file(runs, preview, preview_file)?;
    if let Some(original) = current.as_deref() {
        backup_target(apply_directory, original)?;
    }
    if let Some(parent) = step.target_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    atomic_write(&step.target_path, &bytes, step.executable)?;
    let written = fs::read(&step.target_path).map_err(|error| error.to_string())?;
    if content_hash(&written) != step.output_hash {
        return Err("written target does not match the preview".to_string());
    }
    Ok(())
}

fn ensure_target_path_allowed(step: &ApplyFileStep) -> Result<(), String> {
    if !step.target_path.is_absolute() || !step.target_path.starts_with(&step.scope_root) {
        return Err("target path leaves its selected scope".to_string());
    }
    if step
        .target_path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("target path contains a parent traversal".to_string());
    }
    if fs::symlink_metadata(&step.target_path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err("target path is a symbolic link".to_string());
    }
    let scope_root = if step.scope_root.exists() {
        step.scope_root
            .canonicalize()
            .map_err(|error| error.to_string())?
    } else {
        step.scope_root.clone()
    };
    let mut ancestor = step.target_path.parent();
    while let Some(path) = ancestor {
        if !path.starts_with(&step.scope_root) {
            break;
        }
        if path.exists() {
            let resolved = path.canonicalize().map_err(|error| error.to_string())?;
            if !resolved.starts_with(&scope_root) {
                return Err("target parent resolves outside its selected scope".to_string());
            }
            break;
        }
        ancestor = path.parent();
    }
    Ok(())
}

fn backup_target(apply_directory: &Path, bytes: &[u8]) -> Result<(), String> {
    let hash = content_hash(bytes);
    let path = apply_directory.join("backups").join(format!("{hash}.bin"));
    if path.exists() {
        return Ok(());
    }
    let mut file = private_file(&path, true)?;
    file.write_all(bytes).map_err(|error| error.to_string())?;
    file.sync_all().map_err(|error| error.to_string())
}

fn atomic_write(path: &Path, bytes: &[u8], executable: bool) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "target path has no parent".to_string())?;
    let temporary = parent.join(format!(".session-loom-{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .map_err(|error| error.to_string())?;
    let result = (|| {
        file.write_all(bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())
    })();
    drop(file);
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o777)
            .unwrap_or(0o644)
            | if executable { 0o111 } else { 0 };
        fs::set_permissions(&temporary, fs::Permissions::from_mode(mode))
            .map_err(|error| error.to_string())?;
    }
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error.to_string());
    }
    if let Ok(directory) = fs::File::open(parent) {
        let _ = directory.sync_all();
    }
    Ok(())
}

fn apply_sessions(
    store: &Store,
    runs: &RunStore,
    run: &MigrationRun,
    preview: &PreviewRevision,
    plan: &ApplyPlan,
    apply_directory: &Path,
) -> Vec<SessionApplyResult> {
    let run_directory = match runs.run_directory(&run.id) {
        Ok(directory) => directory,
        Err(error) => {
            return plan
                .sessions
                .iter()
                .map(|step| SessionApplyResult {
                    source_session_id: step.source_session_id.clone(),
                    target_session_id: step.target_session_id.clone(),
                    target_path: None,
                    action: "write-session".to_string(),
                    readiness: Readiness::Failed,
                    message: error.clone(),
                    next_steps: vec!["修复迁移记录目录后重试".to_string()],
                })
                .collect()
        }
    };
    let mut results = vec![];
    for step in &plan.sessions {
        let native_id = format!("session-native:{}", step.source_session_id);
        let link_id = format!("session-link:{}", step.source_session_id);
        match apply_session(store, preview, plan, step, &run_directory) {
            Ok((result, native_written, linked)) => {
                if native_written {
                    let _ = append_journal(apply_directory, &native_id, "done", "");
                }
                if linked {
                    let _ = append_journal(apply_directory, &link_id, "done", "");
                }
                results.push(result);
            }
            Err(error) => results.push(SessionApplyResult {
                source_session_id: step.source_session_id.clone(),
                target_session_id: step.target_session_id.clone(),
                target_path: step.codex_target_path.clone(),
                action: "write-session".to_string(),
                readiness: Readiness::Failed,
                message: error,
                next_steps: vec!["修复错误后重试；已固定的目标会话不会重复创建".to_string()],
            }),
        }
    }
    results
}

fn apply_session(
    store: &Store,
    preview: &PreviewRevision,
    plan: &ApplyPlan,
    step: &ApplySessionStep,
    run_directory: &Path,
) -> Result<(SessionApplyResult, bool, bool), String> {
    let snapshot_path = run_directory.join(&step.snapshot_path);
    let bytes = fs::read(&snapshot_path).map_err(|error| error.to_string())?;
    if content_hash(&bytes) != step.content_hash {
        return Err("session snapshot changed; refresh the preview".to_string());
    }
    let source = CanonicalSession::from_json(
        std::str::from_utf8(&bytes).map_err(|_| "session snapshot is not UTF-8".to_string())?,
    )?;
    let mut linked = source.clone();
    linked.session_id = step.target_session_id.clone();
    linked.source_tool = preview.target.tool;
    linked.cwd = step.target_cwd.to_string_lossy().to_string();

    let artifact = Some(match preview.target.tool {
        SourceTool::Codex => {
            let path = step
                .codex_target_path
                .clone()
                .ok_or_else(|| "Codex apply plan has no rollout path".to_string())?;
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            let payload = codex::write_session(&linked)?;
            atomic_write(&path, payload.as_bytes(), false)?;
            codex::register_session_index(
                &linked,
                &step.target_session_id,
                &plan.session_roots.codex_home,
            )?;
            codex::register_thread(
                &linked,
                &step.target_session_id,
                &path,
                &plan.session_roots.codex_home,
            )?;
            path
        }
        SourceTool::Claude => {
            claude::write_session_to_root_with_id(
                &linked,
                &plan.session_roots.claude,
                &step.target_session_id,
            )?
            .session_file
        }
        SourceTool::OpenCode => {
            opencode::write_session_to_database_with_id(
                &linked,
                &plan.session_roots.opencode,
                &step.target_session_id,
            )?
            .database
        }
        SourceTool::Dsh => {
            dsh::write_session_to_root_with_id(
                &linked,
                &plan.session_roots.dsh,
                &step.target_session_id,
            )?
            .log_file
        }
        SourceTool::Pi => {
            pi::write_session_to_root_with_id(
                &linked,
                &plan.session_roots.pi,
                &step.target_session_id,
                &step.timestamp,
            )?
            .session_file
        }
    });
    if let Some(path) = &artifact {
        if !path.exists() {
            return Err("native session writer reported success without an artifact".to_string());
        }
    }
    if store.read_session(&step.target_session_id).is_err() {
        store.write_session_with_conversation(
            &linked,
            artifact.as_deref(),
            &step.conversation_id,
        )?;
    }
    Ok((
        SessionApplyResult {
            source_session_id: step.source_session_id.clone(),
            target_session_id: step.target_session_id.clone(),
            target_path: artifact,
            action: "write-session".to_string(),
            readiness: Readiness::Available,
            message: format!("已写入 {} 并登记目标会话", preview.target.tool.as_str()),
            next_steps: vec![],
        },
        true,
        true,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_plan_is_private_and_rejects_unknown_versions() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("plan.json");
        let plan = ApplyPlan {
            schema_version: APPLY_SCHEMA_VERSION + 1,
            run_id: "run".into(),
            preview_id: "preview".into(),
            run_revision: 1,
            created_at: "2026-11-02T00:00:00Z".into(),
            session_roots: TargetSessionRoots {
                codex: PathBuf::from("/codex"),
                codex_home: PathBuf::from("/codex-home"),
                claude: PathBuf::from("/claude"),
                opencode: PathBuf::from("/opencode"),
                dsh: PathBuf::from("/dsh"),
                pi: PathBuf::from("/pi"),
            },
            files: vec![],
            sessions: vec![],
        };
        fs::write(&path, serde_json::to_vec(&plan).unwrap()).unwrap();
        assert!(read_plan(&path)
            .unwrap_err()
            .contains("unsupported apply plan"));
    }
}
