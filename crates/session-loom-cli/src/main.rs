use clap::{Parser, Subcommand};
use session_loom_core::{
    canonical::SourceTool,
    daemon, delete, paths,
    project_migration::{
        apply_migration, list_migration_projects, prepare_migration, preview_migration,
        EnvironmentRoots, MigrationTarget, OpenCodeConfigVersion, PrepareRequest, PreviewDecision,
        RunStore, TargetChoices, VersionSupport,
    },
    restore::{restore_session, RestoreRoots},
    store::{ListFilter, Store},
    trash::{restore_from_trash, Trash},
    watcher::{SessionWatcher, WatchTarget},
};
use std::{io::Read, process::ExitCode, str::FromStr, time::Duration};

#[derive(Parser)]
#[command(
    name = "ssl",
    version,
    about = "Mirror Claude Code, Codex, OpenCode, DeepSeek Harness, and Pi sessions into a durable canonical format"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Daemon {
        #[command(subcommand)]
        action: Option<DaemonAction>,
    },
    Restore {
        #[arg(long, value_parser = ["codex", "claude", "opencode", "dsh", "pi"])]
        to: String,
        session_id: Option<String>,
    },
    List {
        #[arg(long, value_parser = ["codex", "claude", "opencode", "dsh", "pi"])]
        tool: Option<String>,
    },
    Search {
        #[arg(required = true)]
        query: Vec<String>,
    },
    Export {
        session_id: String,
    },
    Delete {
        session_id: String,
    },
    Trash {
        #[command(subcommand)]
        action: TrashAction,
    },
    Migrate {
        #[command(subcommand)]
        action: MigrateAction,
    },
}

#[derive(Subcommand)]
enum MigrateAction {
    /// List projects that have mirrored native sessions.
    Projects {
        #[arg(long)]
        json: bool,
    },
    /// Discover a project environment, capture the chosen items, and create a draft run.
    Prepare {
        /// Project id from `migrate projects`, or an absolute project path.
        #[arg(long)]
        project: String,
        #[arg(long = "session", required = true)]
        sessions: Vec<String>,
        #[arg(long)]
        dsh_profile: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Capture the default project items plus explicitly chosen global items.
    Capture {
        #[arg(long)]
        run: String,
        /// A discovered global or profile item to include.
        #[arg(long = "asset")]
        assets: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Render a target preview; pass decisions from a previous preview to resolve conflicts.
    Preview {
        #[arg(long)]
        run: String,
        #[arg(long, value_parser = ["codex", "claude", "opencode", "dsh", "pi"])]
        to: String,
        #[arg(long)]
        project_root: String,
        /// JSON file containing an array of preview decisions, or `-` for stdin.
        #[arg(long)]
        decisions: Option<String>,
        #[arg(long, value_parser = ["auto", "v1", "v2", "unknown"])]
        opencode_version: Option<String>,
        #[arg(long, value_parser = ["supported", "unsupported", "unknown"])]
        pi_mcp: Option<String>,
        #[arg(long)]
        dsh_profile: Option<String>,
        #[arg(long)]
        project_trusted: bool,
        /// Name of an environment variable the target will provide. Values are never read.
        #[arg(long = "env")]
        environment_variables: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Apply the current preview; repeated calls resume the same fixed session ids.
    Apply {
        #[arg(long)]
        run: String,
        #[arg(long)]
        preview: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Show the persisted migration state, preview summary, and item results.
    Status {
        #[arg(long)]
        run: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum TrashAction {
    List,
    Restore { session_id: String },
    Delete { session_id: String },
}

#[derive(Subcommand)]
enum DaemonAction {
    Run,
    Start,
    Stop,
    Status,
}

#[derive(Debug)]
struct CommandOutput {
    text: String,
    success: bool,
}

impl CommandOutput {
    fn ok(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            success: true,
        }
    }

    fn partial(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            success: false,
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(output) => {
            if !output.text.is_empty() {
                println!("{}", output.text);
            }
            if output.success {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<CommandOutput, String> {
    match cli.command {
        Command::Daemon { action } => run_daemon(action.unwrap_or(DaemonAction::Start)),
        Command::Restore { to, session_id } => {
            let store = Store::from_environment()?;
            let target = SourceTool::from_str(&to)?;
            let result = restore_session(
                &store,
                target,
                session_id.as_deref(),
                &RestoreRoots::from_environment(),
            );
            if result.ok {
                Ok(CommandOutput::ok(result.message))
            } else {
                Err(result.message)
            }
        }
        Command::List { tool } => {
            let store = Store::from_environment()?;
            let tool = tool.as_deref().map(SourceTool::from_str).transpose()?;
            let output = store
                .list_cards(ListFilter { tool, query: None })?
                .into_iter()
                .map(|card| {
                    format!(
                        "{}\t{}\t{}\t{}\t{}",
                        card.session_id,
                        card.source_tool,
                        card.cwd,
                        card.updated_at,
                        card.tools.join(",")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(CommandOutput::ok(output))
        }
        Command::Search { query } => {
            let store = Store::from_environment()?;
            let output = store
                .search_hits(&query.join(" "))?
                .into_iter()
                .map(|session| {
                    format!(
                        "{}\t{}\t{}\t{}",
                        session.session_id,
                        session.source_tool.as_str(),
                        session.cwd,
                        session.updated_at
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(CommandOutput::ok(output))
        }
        Command::Export { session_id } => {
            let store = Store::from_environment()?;
            Ok(CommandOutput::ok(store.export_session(&session_id)?))
        }
        Command::Delete { session_id } => {
            let store = Store::from_environment()?;
            let result =
                delete::delete_session(&store, &session_id, &RestoreRoots::from_environment());
            if result.ok {
                Ok(CommandOutput::ok(result.message))
            } else {
                Err(result.message)
            }
        }
        Command::Migrate { action } => run_migrate(action),
        Command::Trash { action } => {
            let store = Store::from_environment()?;
            let trash = Trash::new(store.root());
            match action {
                TrashAction::List => {
                    let output = trash
                        .list()?
                        .into_iter()
                        .map(|entry| {
                            format!(
                                "{}\t{}\t{}\t{}",
                                entry.session.session_id,
                                entry.session.source_tool.as_str(),
                                entry.session.cwd,
                                entry.deleted_at
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    Ok(CommandOutput::ok(output))
                }
                TrashAction::Restore { session_id } => {
                    let session = restore_from_trash(&store, &trash, &session_id)?;
                    Ok(CommandOutput::ok(format!(
                        "restored {} back to the session list",
                        session.session_id
                    )))
                }
                TrashAction::Delete { session_id } => {
                    trash.remove(&session_id)?;
                    Ok(CommandOutput::ok(format!(
                        "permanently removed {session_id} from the trash"
                    )))
                }
            }
        }
    }
}

fn run_daemon(action: DaemonAction) -> Result<CommandOutput, String> {
    let store_root = paths::store_root();
    match action {
        DaemonAction::Run => {
            let store = Store::open(&store_root)?;
            println!("session-loom daemon running");
            SessionWatcher::new(
                store,
                vec![
                    WatchTarget {
                        source_tool: SourceTool::Codex,
                        root: paths::codex_sessions_root(),
                    },
                    WatchTarget {
                        source_tool: SourceTool::Claude,
                        root: paths::claude_sessions_root(),
                    },
                    WatchTarget {
                        source_tool: SourceTool::OpenCode,
                        root: paths::opencode_database(),
                    },
                    WatchTarget {
                        source_tool: SourceTool::Dsh,
                        root: paths::dsh_sessions_root(),
                    },
                    WatchTarget {
                        source_tool: SourceTool::Pi,
                        root: paths::pi_sessions_root(),
                    },
                ],
            )
            .run_forever(Duration::from_secs(2));
        }
        DaemonAction::Start => {
            let executable = std::env::current_exe().map_err(|error| error.to_string())?;
            let state = daemon::ensure_daemon_running(&store_root, &executable);
            if state.running {
                Ok(CommandOutput::ok(format!(
                    "daemon started (pid {})",
                    state.pid.unwrap_or_default()
                )))
            } else {
                Err("failed to start daemon".to_string())
            }
        }
        DaemonAction::Stop => daemon_stop_result(daemon::stop_daemon(&store_root)),
        DaemonAction::Status => {
            let state = daemon::daemon_state(&store_root);
            Ok(CommandOutput::ok(
                if state.running { "running" } else { "stopped" }.to_string(),
            ))
        }
    }
}

fn daemon_stop_result(state: daemon::DaemonState) -> Result<CommandOutput, String> {
    if state.running {
        return Err(match state.pid {
            Some(pid) => format!("failed to stop daemon (pid {pid})"),
            None => "failed to stop daemon".to_string(),
        });
    }
    Ok(CommandOutput::ok("stopped".to_string()))
}

fn migrate_error(json: bool, message: impl Into<String>) -> String {
    let message = message.into();
    if json {
        serde_json::json!({ "error": message }).to_string()
    } else {
        message
    }
}

fn run_migrate(action: MigrateAction) -> Result<CommandOutput, String> {
    match action {
        MigrateAction::Projects { json } => migrate_projects(json),
        MigrateAction::Prepare {
            project,
            sessions,
            dsh_profile,
            json,
        } => migrate_prepare(json, project, sessions, dsh_profile),
        MigrateAction::Capture { run, assets, json } => migrate_capture(json, run, assets),
        MigrateAction::Preview {
            run,
            to,
            project_root,
            decisions,
            opencode_version,
            pi_mcp,
            dsh_profile,
            project_trusted,
            environment_variables,
            json,
        } => migrate_preview(
            json,
            run,
            to,
            project_root,
            decisions,
            opencode_version,
            pi_mcp,
            dsh_profile,
            project_trusted,
            environment_variables,
        ),
        MigrateAction::Apply { run, preview, json } => migrate_apply(json, run, preview),
        MigrateAction::Status { run, json } => migrate_status(json, run),
    }
}

fn migrate_projects(json: bool) -> Result<CommandOutput, String> {
    let result = (|| -> Result<_, String> {
        let store = Store::from_environment()?;
        list_migration_projects(&store)
    })();
    match result {
        Ok(entries) if json => Ok(CommandOutput::ok(
            serde_json::to_string_pretty(&entries).map_err(|error| error.to_string())?,
        )),
        Ok(entries) => Ok(CommandOutput::ok(
            entries
                .into_iter()
                .map(|entry| {
                    format!(
                        "{}\t{}\t{}\t{}",
                        entry.project.id,
                        entry.project.root.display(),
                        entry.sessions.len(),
                        entry.project.accessible
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )),
        Err(error) => Err(migrate_error(json, error)),
    }
}

fn migrate_prepare(
    json: bool,
    project: String,
    sessions: Vec<String>,
    dsh_profile: Option<String>,
) -> Result<CommandOutput, String> {
    let result = (|| -> Result<_, String> {
        let store = Store::from_environment()?;
        let runs = RunStore::new(store.root())?;
        let roots = EnvironmentRoots::from_environment();
        prepare_migration(
            &store,
            &runs,
            &roots,
            &PrepareRequest {
                project,
                session_ids: sessions,
                dsh_profile,
            },
        )
    })();
    let prepared = match result {
        Ok(prepared) => prepared,
        Err(error) => return Err(migrate_error(json, error)),
    };
    let selected = prepared
        .report
        .assets
        .iter()
        .filter(|asset| asset.selected_by_default)
        .map(|asset| asset.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if json {
        let value = serde_json::json!({
            "runId": prepared.run.id,
            "revision": prepared.run.revision,
            "state": prepared.run.state,
            "project": prepared.run.project,
            "sessions": prepared
                .sessions
                .iter()
                .map(|session| &session.metadata)
                .collect::<Vec<_>>(),
            "assets": prepared
                .report
                .assets
                .iter()
                .map(|asset| {
                    serde_json::json!({
                        "id": asset.id,
                        "kind": asset.kind,
                        "name": asset.name,
                        "sourceTool": asset.source_tool,
                        "scope": asset.scope,
                        "format": asset.format,
                        "selected": selected.contains(asset.id.as_str()),
                        "selectedByDefault": asset.selected_by_default,
                        "sourceActive": asset.source_active,
                        "evidence": asset.evidence,
                        "diagnostics": asset.diagnostics,
                    })
                })
                .collect::<Vec<_>>(),
            "diagnostics": prepared.report.diagnostics,
        });
        return Ok(CommandOutput::ok(
            serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?,
        ));
    }
    let mut lines = vec![format!("run\t{}", prepared.run.id)];
    for session in &prepared.sessions {
        lines.push(format!(
            "session\t{}\t{}\t{}",
            session.metadata.session_id,
            session.metadata.source_tool.as_str(),
            session.metadata.cwd
        ));
    }
    for asset in &prepared.report.assets {
        lines.push(format!(
            "asset\t{}\t{:?}\t{:?}\t{}\t{}",
            asset.id,
            asset.kind,
            asset.scope,
            asset.name,
            if selected.contains(asset.id.as_str()) {
                "selected"
            } else {
                "available"
            }
        ));
    }
    Ok(CommandOutput::ok(lines.join("\n")))
}

#[allow(clippy::too_many_arguments)]
fn migrate_preview(
    json: bool,
    run_id: String,
    to: String,
    project_root: String,
    decisions: Option<String>,
    opencode_version: Option<String>,
    pi_mcp: Option<String>,
    dsh_profile: Option<String>,
    project_trusted: bool,
    environment_variables: Vec<String>,
) -> Result<CommandOutput, String> {
    let result = (|| -> Result<_, String> {
        let tool = SourceTool::from_str(&to)?;
        let project_root = std::path::PathBuf::from(project_root);
        if !project_root.is_absolute() {
            return Err("--project-root must be an absolute target path".to_string());
        }
        let opencode_version = match opencode_version.as_deref() {
            None | Some("auto") => None,
            Some("v1") => Some(OpenCodeConfigVersion::V1),
            Some("v2") => Some(OpenCodeConfigVersion::V2),
            Some("unknown") => Some(OpenCodeConfigVersion::Unknown),
            Some(value) => return Err(format!("unknown OpenCode version: {value}")),
        };
        let pi_mcp = match pi_mcp.as_deref() {
            None => None,
            Some("supported") => Some(VersionSupport::Supported),
            Some("unsupported") => Some(VersionSupport::Unsupported),
            Some("unknown") => Some(VersionSupport::Unknown),
            Some(value) => return Err(format!("unknown Pi MCP support value: {value}")),
        };
        let decisions = read_decisions(decisions.as_deref())?;
        let store = Store::from_environment()?;
        let runs = RunStore::new(store.root())?;
        let run = runs.load(&run_id)?;
        preview_migration(
            &runs,
            &run,
            &MigrationTarget { tool, project_root },
            &TargetChoices {
                opencode_version,
                pi_mcp,
                dsh_profile,
                project_trusted,
                environment_variables,
            },
            &decisions,
        )
        .map(|(run, preview)| (run, preview.public()))
    })();
    let (run, preview) = match result {
        Ok(value) => value,
        Err(error) => return Err(migrate_error(json, error)),
    };
    if json {
        return Ok(CommandOutput::ok(
            serde_json::to_string_pretty(&preview).map_err(|error| error.to_string())?,
        ));
    }
    let unresolved = preview
        .conflicts
        .iter()
        .filter(|conflict| !conflict.resolved)
        .count();
    let mut lines = vec![format!(
        "preview\t{}\trun revision {}\tready={}\tunresolved conflicts={}\tfiles={}",
        preview.id,
        run.revision,
        preview.ready_to_apply,
        unresolved,
        preview.files.len()
    )];
    for conflict in &preview.conflicts {
        lines.push(format!(
            "conflict\t{}\t{}\t{}\tresolved={}",
            conflict.id,
            conflict.kind,
            conflict.target_path.display(),
            conflict.resolved
        ));
        for item in &conflict.items {
            lines.push(format!("decision\t{}\t{}", item.decision_id, item.item_id));
        }
    }
    for item in &preview.items {
        lines.push(format!(
            "item\t{}\t{:?}\t{:?}\t{}",
            item.item_id, item.kind, item.readiness, item.message
        ));
    }
    Ok(CommandOutput::ok(lines.join("\n")))
}

fn read_decisions(source: Option<&str>) -> Result<Vec<PreviewDecision>, String> {
    let Some(source) = source else {
        return Ok(vec![]);
    };
    let text = if source == "-" {
        let mut text = String::new();
        std::io::stdin()
            .read_to_string(&mut text)
            .map_err(|error| error.to_string())?;
        text
    } else {
        std::fs::read_to_string(source).map_err(|error| error.to_string())?
    };
    serde_json::from_str(&text).map_err(|error| format!("invalid decisions JSON: {error}"))
}

fn migrate_apply(
    json: bool,
    run_id: String,
    preview_id: Option<String>,
) -> Result<CommandOutput, String> {
    let result = (|| -> Result<_, String> {
        let store = Store::from_environment()?;
        let runs = RunStore::new(store.root())?;
        apply_migration(&store, &runs, &run_id, preview_id.as_deref())
    })();
    let report = match result {
        Ok(report) => report,
        Err(error) => return Err(migrate_error(json, error)),
    };
    let success = report.is_ok();
    if json {
        let text = serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?;
        return Ok(if success {
            CommandOutput::ok(text)
        } else {
            CommandOutput::partial(text)
        });
    }
    let mut lines = vec![format!(
        "apply\t{}\t{:?}\titems={}\tsessions={}",
        report.preview_id,
        report.state,
        report.items.len(),
        report.sessions.len()
    )];
    for item in &report.items {
        lines.push(format!(
            "item\t{}\t{:?}\t{}",
            item.item_id, item.readiness, item.message
        ));
        for step in &item.next_steps {
            lines.push(format!("next\t{}", step));
        }
    }
    for session in &report.sessions {
        lines.push(format!(
            "session\t{}\t{}\t{:?}\t{}",
            session.source_session_id,
            session.target_session_id,
            session.readiness,
            session.message
        ));
    }
    let text = lines.join("\n");
    Ok(if success {
        CommandOutput::ok(text)
    } else {
        CommandOutput::partial(text)
    })
}

fn migrate_status(json: bool, run_id: String) -> Result<CommandOutput, String> {
    let result = (|| -> Result<_, String> {
        let store = Store::from_environment()?;
        let runs = RunStore::new(store.root())?;
        runs.load(&run_id)
            .map_err(|error| format!("migration not found or unreadable: {error}"))
    })();
    let run = match result {
        Ok(run) => run,
        Err(error) => return Err(migrate_error(json, error)),
    };
    if json {
        return Ok(CommandOutput::ok(
            serde_json::to_string_pretty(&run).map_err(|error| error.to_string())?,
        ));
    }
    let mut lines = vec![format!(
        "run\t{}\t{:?}\trevision {}",
        run.id, run.state, run.revision
    )];
    if let Some(preview) = &run.preview {
        lines.push(format!(
            "preview\t{}\tready={}\tunresolved conflicts={}\tfiles={}",
            preview.id, preview.ready_to_apply, preview.unresolved_conflicts, preview.file_count
        ));
    }
    if let Some(report) = &run.apply_report {
        lines.push(format!("apply\t{:?}", report.state));
        for item in &report.items {
            lines.push(format!(
                "result\t{}\t{:?}\t{}",
                item.item_id, item.readiness, item.message
            ));
        }
    }
    Ok(CommandOutput::ok(lines.join("\n")))
}

fn migrate_capture(
    json: bool,
    run_id: String,
    assets: Vec<String>,
) -> Result<CommandOutput, String> {
    let result = (|| -> Result<_, String> {
        let store = Store::from_environment()?;
        let runs = RunStore::new(store.root())?;
        session_loom_core::project_migration::capture_migration(&store, &runs, &run_id, &assets)
    })();
    let run = match result {
        Ok(run) => run,
        Err(error) => return Err(migrate_error(json, error)),
    };
    if json {
        let value = serde_json::json!({
            "runId": run.id,
            "revision": run.revision,
            "state": run.state,
            "captured": run.assets.iter().map(|asset| serde_json::json!({
                "id": asset.asset.id,
                "kind": asset.asset.kind,
                "name": asset.asset.name,
                "scope": asset.asset.scope,
                "files": asset.files.len(),
            })).collect::<Vec<_>>(),
            "sessions": run.sessions.iter().map(|session| &session.metadata).collect::<Vec<_>>(),
            "diagnostics": run.diagnostics,
        });
        return Ok(CommandOutput::ok(
            serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?,
        ));
    }
    let mut lines = vec![format!("run\t{}\tcaptured={}", run.id, run.assets.len())];
    for asset in &run.assets {
        lines.push(format!(
            "asset\t{}\t{:?}\t{:?}\t{}",
            asset.asset.id, asset.asset.kind, asset.asset.scope, asset.asset.name
        ));
    }
    Ok(CommandOutput::ok(lines.join("\n")))
}

#[cfg(test)]
mod tests {
    use super::daemon_stop_result;
    use session_loom_core::daemon::DaemonState;

    #[test]
    fn daemon_stop_reports_a_process_that_is_still_running() {
        assert_eq!(
            daemon_stop_result(DaemonState {
                running: true,
                pid: Some(42)
            })
            .unwrap_err(),
            "failed to stop daemon (pid 42)"
        );
        assert_eq!(
            daemon_stop_result(DaemonState {
                running: false,
                pid: None
            })
            .unwrap()
            .text,
            "stopped"
        );
    }
}
