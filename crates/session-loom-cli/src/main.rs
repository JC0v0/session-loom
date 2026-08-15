use clap::{Parser, Subcommand};
use session_loom_core::{
    canonical::SourceTool,
    daemon, delete, paths,
    restore::{restore_session, RestoreRoots},
    store::{ListFilter, Store},
    trash::{restore_from_trash, Trash},
    watcher::{SessionWatcher, WatchTarget},
};
use std::{process::ExitCode, str::FromStr, time::Duration};

#[derive(Parser)]
#[command(
    name = "ssl",
    version,
    about = "Mirror Claude Code, Codex, OpenCode, and DeepSeek Harness sessions into a durable canonical format"
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
        #[arg(long, value_parser = ["codex", "claude", "opencode", "dsh"])]
        to: String,
        session_id: Option<String>,
    },
    List {
        #[arg(long, value_parser = ["codex", "claude", "opencode", "dsh"])]
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

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(Some(output)) => {
            if !output.is_empty() {
                println!("{output}");
            }
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<Option<String>, String> {
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
                Ok(Some(result.message))
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
                .take(20)
                .map(|card| {
                    format!(
                        "{}\t{}\t{}\t{}",
                        card.session_id, card.source_tool, card.cwd, card.updated_at
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            Ok(Some(output))
        }
        Command::Search { query } => {
            let store = Store::from_environment()?;
            let output = store
                .search_sessions(&query.join(" "))?
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
            Ok(Some(output))
        }
        Command::Export { session_id } => {
            let store = Store::from_environment()?;
            Ok(Some(store.export_session(&session_id)?))
        }
        Command::Delete { session_id } => {
            let store = Store::from_environment()?;
            let result =
                delete::delete_session(&store, &session_id, &RestoreRoots::from_environment());
            if result.ok {
                Ok(Some(result.message))
            } else {
                Err(result.message)
            }
        }
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
                    Ok(Some(output))
                }
                TrashAction::Restore { session_id } => {
                    let session = restore_from_trash(&store, &trash, &session_id)?;
                    Ok(Some(format!(
                        "restored {} back to the session list",
                        session.session_id
                    )))
                }
                TrashAction::Delete { session_id } => {
                    trash.remove(&session_id)?;
                    Ok(Some(format!(
                        "permanently removed {session_id} from the trash"
                    )))
                }
            }
        }
    }
}

fn run_daemon(action: DaemonAction) -> Result<Option<String>, String> {
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
                ],
            )
            .run_forever(Duration::from_secs(2));
        }
        DaemonAction::Start => {
            let executable = std::env::current_exe().map_err(|error| error.to_string())?;
            let state = daemon::ensure_daemon_running(&store_root, &executable);
            if state.running {
                Ok(Some(format!(
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
            Ok(Some(
                if state.running { "running" } else { "stopped" }.to_string(),
            ))
        }
    }
}

fn daemon_stop_result(state: daemon::DaemonState) -> Result<Option<String>, String> {
    if state.running {
        return Err(match state.pid {
            Some(pid) => format!("failed to stop daemon (pid {pid})"),
            None => "failed to stop daemon".to_string(),
        });
    }
    Ok(Some("stopped".to_string()))
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
            .unwrap(),
            Some("stopped".to_string())
        );
    }
}
