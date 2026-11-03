use super::{
    adapters,
    apply::apply_preview,
    discovery::{discover_environment, DiscoveryOptions, DiscoveryReport},
    model::{
        EnvironmentRoots, MigrationRun, MigrationTarget, OpenCodeConfigVersion, PreviewDecision,
        PreviewRevision, ProjectIdentity, SelectedSession, TargetEnvironment, TargetSessionRoots,
        VersionSupport,
    },
    preview::create_preview,
    projects::{list_projects, resolve_project, select_project_sessions},
    run_store::RunStore,
};
use crate::{restore::RestoreRoots, store::Store};
use std::{collections::BTreeSet, path::Path};

#[derive(Debug, Clone, Default)]
pub struct PrepareRequest {
    /// A project id from the project list, or an absolute project path.
    pub project: String,
    pub session_ids: Vec<String>,
    pub dsh_profile: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedMigration {
    pub run: MigrationRun,
    pub sessions: Vec<SelectedSession>,
    pub report: DiscoveryReport,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct TargetChoices {
    /// `None` auto-detects from the target project or user OpenCode config.
    pub opencode_version: Option<OpenCodeConfigVersion>,
    /// `None` means the target Pi version is unknown and MCP must not be
    /// reported as supported.
    pub pi_mcp: Option<VersionSupport>,
    pub dsh_profile: Option<String>,
    pub project_trusted: bool,
    /// Names only; values are never read or copied.
    pub environment_variables: Vec<String>,
}

pub fn list_migration_projects(store: &Store) -> Result<Vec<super::model::ProjectEntry>, String> {
    list_projects(store)
}

/// Discover the project environment and create a draft run. Nothing is
/// captured yet: the caller reviews the inventory, then captures the project
/// defaults plus explicitly chosen global items.
pub fn prepare_migration(
    store: &Store,
    runs: &RunStore,
    roots: &EnvironmentRoots,
    request: &PrepareRequest,
) -> Result<PreparedMigration, String> {
    if request.session_ids.is_empty() {
        return Err("select at least one native session".to_string());
    }
    let project = resolve_requested_project(store, &request.project)?;
    if !project.accessible {
        return Err("selected project directory is not accessible".to_string());
    }
    let sessions = select_project_sessions(store, &project.id, &request.session_ids)?;
    let options = DiscoveryOptions {
        dsh_profile: request.dsh_profile.clone(),
        extra_scripts: vec![],
    };
    let report = discover_environment(&project, &sessions, roots, &options)?;
    let mut run = MigrationRun::new(project, request.session_ids.clone())?;
    run.discovered_assets = report.assets.clone();
    run.discovery_probes = report.source_probes.clone();
    run.diagnostics = report.diagnostics.clone();
    runs.create(&run)?;
    Ok(PreparedMigration {
        run,
        sessions,
        report,
    })
}

/// Capture only the default-selected project items plus explicitly chosen
/// global items. The run's discovery inventory must still match the source.
pub fn capture_migration(
    store: &Store,
    runs: &RunStore,
    run_id: &str,
    asset_ids: &[String],
) -> Result<MigrationRun, String> {
    let run = runs.load(run_id)?;
    let report = DiscoveryReport {
        project: run.project.clone(),
        session_ids: run.session_ids.clone(),
        assets: run.discovered_assets.clone(),
        diagnostics: run.diagnostics.clone(),
        source_probes: run.discovery_probes.clone(),
    };
    let sessions = select_project_sessions(store, &run.project.id, &run.session_ids)?;
    let discovered: BTreeSet<_> = report
        .assets
        .iter()
        .map(|asset| asset.id.as_str())
        .collect();
    let mut selected = report
        .assets
        .iter()
        .filter(|asset| asset.selected_by_default)
        .map(|asset| asset.id.clone())
        .collect::<Vec<_>>();
    for id in asset_ids {
        if !discovered.contains(id.as_str()) {
            return Err(format!(
                "selected environment item was not discovered: {id}"
            ));
        }
        if !selected.contains(id) {
            selected.push(id.clone());
        }
    }
    super::snapshot::capture_selection(store, runs, &run, &report, &sessions, &selected)
}

/// Prepare the optional context for one existing native clone action. The
/// source instance and its project are resolved by the core, and the selected
/// destination retains that project rather than asking for another project.
pub fn prepare_session_clone(
    store: &Store,
    runs: &RunStore,
    roots: &EnvironmentRoots,
    session_id: &str,
    target_tool: crate::canonical::SourceTool,
) -> Result<PreparedMigration, String> {
    let source = store.read_session(session_id)?;
    let mut prepared = prepare_migration(
        store,
        runs,
        roots,
        &PrepareRequest {
            project: source.cwd,
            session_ids: vec![session_id.to_string()],
            dsh_profile: None,
        },
    )?;
    let target = MigrationTarget {
        tool: target_tool,
        project_root: prepared.run.project.root.clone(),
    };
    prepared.run = set_migration_target(runs, &prepared.run, &target)?;
    Ok(prepared)
}

/// Unlike the project CLI defaults, the clone dialog sends the complete
/// selection. An empty selection contains only the chosen native session.
pub fn capture_clone_selection(
    store: &Store,
    runs: &RunStore,
    run_id: &str,
    asset_ids: &[String],
) -> Result<MigrationRun, String> {
    let run = runs.load(run_id)?;
    if run.session_ids.len() != 1 {
        return Err("a session clone must select exactly one native instance".to_string());
    }
    let report = DiscoveryReport {
        project: run.project.clone(),
        session_ids: run.session_ids.clone(),
        assets: run.discovered_assets.clone(),
        diagnostics: run.diagnostics.clone(),
        source_probes: run.discovery_probes.clone(),
    };
    let sessions = select_project_sessions(store, &run.project.id, &run.session_ids)?;
    super::snapshot::capture_selection(store, runs, &run, &report, &sessions, asset_ids)
}

fn resolve_requested_project(store: &Store, value: &str) -> Result<ProjectIdentity, String> {
    if value.trim().is_empty() {
        return Err("choose a project by id or path".to_string());
    }
    if let Some(project) = list_projects(store)?
        .into_iter()
        .find(|entry| entry.project.id == value)
    {
        return Ok(project.project);
    }
    Ok(resolve_project(value))
}

pub fn target_environment(target: MigrationTarget, choices: &TargetChoices) -> TargetEnvironment {
    let roots = EnvironmentRoots::from_environment();
    let opencode_version = choices
        .opencode_version
        .unwrap_or_else(|| detect_opencode_version(&target.project_root, &roots.opencode));
    let search_path = std::env::var_os("PATH")
        .map(|value| std::env::split_paths(&value).collect())
        .unwrap_or_default();
    TargetEnvironment {
        target,
        session_roots: Some(TargetSessionRoots::from(&RestoreRoots::from_environment())),
        roots,
        opencode_version,
        pi_mcp: choices.pi_mcp.unwrap_or(VersionSupport::Unknown),
        dsh_profile: choices.dsh_profile.clone(),
        project_trusted: choices.project_trusted,
        executable_search_path: search_path,
        environment_variables: choices.environment_variables.clone(),
    }
}

pub fn detect_opencode_version(
    project_root: &Path,
    user_opencode_dir: &Path,
) -> OpenCodeConfigVersion {
    for base in [project_root, user_opencode_dir] {
        for name in ["opencode.jsonc", "opencode.json"] {
            let Ok(Some(text)) = adapters::read_text(&base.join(name)) else {
                continue;
            };
            let Ok(value) = adapters::parse_json(&text) else {
                continue;
            };
            if value.pointer("/mcp/servers").is_some() || value.get("commands").is_some() {
                return OpenCodeConfigVersion::V2;
            }
            if value.get("mcp").is_some() || value.get("command").is_some() {
                return OpenCodeConfigVersion::V1;
            }
        }
    }
    OpenCodeConfigVersion::Unknown
}

/// Set or confirm the migration target. Selections can only change before a
/// preview; partial runs must start a new migration instead of silently
/// reusing an applied target.
pub fn set_migration_target(
    runs: &RunStore,
    run: &MigrationRun,
    target: &MigrationTarget,
) -> Result<MigrationRun, String> {
    if run.target.as_ref() == Some(target) {
        return Ok(run.clone());
    }
    if !matches!(
        run.state,
        super::model::RunState::Draft | super::model::RunState::Previewed
    ) {
        return Err("target cannot change after the migration has started applying".to_string());
    }
    let mut updated = run.clone();
    updated.revision = run
        .revision
        .checked_add(1)
        .ok_or_else(|| "migration revision exhausted".to_string())?;
    updated.updated_at = chrono::Utc::now().to_rfc3339();
    updated.state = super::model::RunState::Draft;
    updated.preview = None;
    updated.target = Some(target.clone());
    runs.update(&updated, run.revision)?;
    Ok(updated)
}

pub fn preview_migration(
    runs: &RunStore,
    run: &MigrationRun,
    target: &MigrationTarget,
    choices: &TargetChoices,
    decisions: &[PreviewDecision],
) -> Result<(MigrationRun, PreviewRevision), String> {
    if !target.project_root.is_absolute() {
        return Err("target project root must be an absolute path".to_string());
    }
    let environment = target_environment(target.clone(), choices);
    let run = set_migration_target(runs, run, target)?;
    create_preview(runs, &run, &environment, decisions)
}

pub fn apply_migration(
    store: &Store,
    runs: &RunStore,
    run_id: &str,
    preview_id: Option<&str>,
) -> Result<super::model::ApplyReport, String> {
    let run = runs.load(run_id)?;
    let preview_id = preview_id
        .map(str::to_string)
        .or_else(|| run.preview.as_ref().map(|preview| preview.id.clone()))
        .ok_or_else(|| "migration has no preview to apply".to_string())?;
    apply_preview(
        store,
        runs,
        run_id,
        &preview_id,
        &RestoreRoots::from_environment(),
    )
}
