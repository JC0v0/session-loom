use crate::{
    adapters::{claude, codex, dsh, opencode, pi},
    canonical::SourceTool,
    paths,
    store::Store,
};
use chrono::{Datelike, Local, Timelike};
use serde::Serialize;
use std::{fs, path::PathBuf};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct RestoreRoots {
    pub codex: PathBuf,
    pub codex_home: PathBuf,
    pub claude: PathBuf,
    pub opencode: PathBuf,
    pub dsh: PathBuf,
    pub pi: PathBuf,
}

impl RestoreRoots {
    pub fn from_environment() -> Self {
        Self {
            codex: paths::codex_sessions_root(),
            codex_home: paths::codex_home(),
            claude: paths::claude_root(),
            opencode: paths::opencode_database(),
            dsh: paths::dsh_sessions_root(),
            pi: paths::pi_sessions_root(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct RestoreResult {
    pub ok: bool,
    pub message: String,
}

struct RestoredArtifact {
    session_id: String,
    path: PathBuf,
}

pub fn restore_session(
    store: &Store,
    target: SourceTool,
    session_id: Option<&str>,
    roots: &RestoreRoots,
) -> RestoreResult {
    let session = match session_id {
        Some(session_id) => store.read_session(session_id),
        None => store.latest_session(),
    };
    let session = match session {
        Ok(session) => session,
        Err(message) => return RestoreResult { ok: false, message },
    };
    let conversation_id = match store.conversation_id(&session.session_id) {
        Ok(value) => value,
        Err(message) => return RestoreResult { ok: false, message },
    };

    let result: Result<RestoredArtifact, String> = match target {
        SourceTool::Dsh => {
            dsh::write_session_to_root(&session, &roots.dsh).map(|result| RestoredArtifact {
                session_id: result.session_id,
                path: result.log_file,
            })
        }
        SourceTool::OpenCode => {
            opencode::write_session_to_database(&session, &roots.opencode).map(|result| {
                RestoredArtifact {
                    session_id: result.session_id,
                    path: result.database,
                }
            })
        }
        SourceTool::Claude => {
            claude::write_session_to_root(&session, &roots.claude).map(|result| RestoredArtifact {
                session_id: result.session_id,
                path: result.session_file,
            })
        }
        SourceTool::Pi => {
            pi::write_session_to_root(&session, &roots.pi).map(|result| RestoredArtifact {
                session_id: result.session_id,
                path: result.session_file,
            })
        }
        SourceTool::Codex => {
            let now = Local::now();
            let session_id = Uuid::new_v4().to_string();
            let directory = roots
                .codex
                .join(format!("{:04}", now.year()))
                .join(format!("{:02}", now.month()))
                .join(format!("{:02}", now.day()));
            let file = directory.join(format!(
                "rollout-{:04}-{:02}-{:02}T{:02}-{:02}-{:02}-{session_id}.jsonl",
                now.year(),
                now.month(),
                now.day(),
                now.hour(),
                now.minute(),
                now.second()
            ));
            fs::create_dir_all(&directory)
                .map_err(|error| error.to_string())
                .and_then(|_| {
                    let mut restored = session.clone();
                    restored.session_id = session_id.clone();
                    // Resume resolves the provider from the target Codex config;
                    // fall back to the source metadata only when no target default exists.
                    restored.model_provider =
                        codex::configured_model_provider().or(restored.model_provider);
                    codex::write_session(&restored).map(|payload| (restored, payload))
                })
                .and_then(|(restored, payload)| {
                    fs::write(&file, payload)
                        .map_err(|error| error.to_string())
                        .and_then(|_| {
                            codex::register_session_index(
                                &restored,
                                &session_id,
                                &roots.codex_home,
                            )?;
                            codex::register_thread(&restored, &session_id, &file, &roots.codex_home)
                        })
                })
                .map(|_| RestoredArtifact {
                    session_id,
                    path: file,
                })
        }
    };

    match result {
        Ok(artifact) => {
            let mut linked = session.clone();
            linked.session_id = artifact.session_id.clone();
            linked.source_tool = target;
            if let Err(message) = store.write_session_with_conversation(
                &linked,
                Some(&artifact.path),
                &conversation_id,
            ) {
                return RestoreResult { ok: false, message };
            }
            RestoreResult {
                ok: true,
                message: format!(
                    "restored to {}: {} ({})",
                    target.as_str(),
                    artifact.session_id,
                    artifact.path.display()
                ) + &format_portability_suffix(&session, target),
            }
        }
        Err(message) => RestoreResult { ok: false, message },
    }
}

/// Manually synchronizes the selected snapshot to another native tool.
///
/// This intentionally creates a new target-tool session and leaves the old
/// native session untouched, so an active session is never overwritten.
pub fn sync_session(
    store: &Store,
    target: SourceTool,
    session_id: Option<&str>,
    roots: &RestoreRoots,
) -> RestoreResult {
    let mut result = restore_session(store, target, session_id, roots);
    if result.ok {
        result.message = format!(
            "手动同步完成（已创建目标工具新副本，原会话保留）：{}",
            result.message
        );
    }
    result
}

fn format_portability_suffix(
    session: &crate::canonical::CanonicalSession,
    target: SourceTool,
) -> String {
    let report = session.portability_report(target);
    let summary = report.summary();
    if summary.is_empty() {
        String::new()
    } else {
        format!("；{summary}")
    }
}
