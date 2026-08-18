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
    pub claude: PathBuf,
    pub opencode: PathBuf,
    pub dsh: PathBuf,
    pub pi: PathBuf,
}

impl RestoreRoots {
    pub fn from_environment() -> Self {
        Self {
            codex: paths::codex_sessions_root(),
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

    let result = match target {
        SourceTool::Dsh => dsh::write_session_to_root(&session, &roots.dsh).map(|result| {
            format!(
                "restored to DeepSeek Harness: {} ({})",
                result.session_id,
                result.log_file.display()
            )
        }),
        SourceTool::OpenCode => {
            opencode::write_session_to_database(&session, &roots.opencode).map(|result| {
                format!(
                    "restored to OpenCode: {} ({})",
                    result.session_id,
                    result.database.display()
                )
            })
        }
        SourceTool::Claude => {
            claude::write_session_to_root(&session, &roots.claude).map(|result| {
                format!(
                    "restored to Claude Code: {} ({})",
                    result.session_id,
                    result.session_file.display()
                )
            })
        }
        SourceTool::Pi => pi::write_session_to_root(&session, &roots.pi).map(|result| {
            format!(
                "restored to Pi: {} ({})",
                result.session_id,
                result.session_file.display()
            )
        }),
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
                    codex::write_session(&restored)
                })
                .and_then(|payload| fs::write(&file, payload).map_err(|error| error.to_string()))
                .map(|_| format!("restored to Codex: {session_id} ({})", file.display()))
        }
    };

    match result {
        Ok(message) => RestoreResult { ok: true, message },
        Err(message) => RestoreResult { ok: false, message },
    }
}
