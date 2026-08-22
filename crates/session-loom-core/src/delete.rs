use crate::{
    adapters::{dsh, encode_claude_project},
    canonical::{CanonicalSession, SourceTool},
    restore::RestoreRoots,
    store::Store,
    trash::Trash,
};
use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteResult {
    pub ok: bool,
    pub message: String,
    pub source_deleted: bool,
}

/// Deletes a session everywhere: the mirror row, the original artifact in the
/// source tool, and — first — archives the canonical payload in the recycle
/// bin (30-day retention). The source deletion is best-effort: when it fails
/// the mirror is still removed and the recycle bin still holds the data, so
/// nothing is ever lost by a partial failure.
pub fn delete_session(store: &Store, session_id: &str, roots: &RestoreRoots) -> DeleteResult {
    let session = match store.read_session(session_id) {
        Ok(session) => session,
        Err(message) => {
            return DeleteResult {
                ok: false,
                message,
                source_deleted: false,
            }
        }
    };
    let trash = Trash::new(store.root());
    let conversation_id = store.conversation_id(session_id).ok();
    if let Err(message) = trash.add_with_conversation(
        &session,
        store
            .session_source_path(session_id)
            .as_deref()
            .map(Path::new),
        conversation_id.as_deref(),
    ) {
        return DeleteResult {
            ok: false,
            message,
            source_deleted: false,
        };
    }
    let _ = trash.purge_expired(crate::trash::TRASH_RETENTION);

    let source_warning = delete_source(
        &session,
        store.session_source_path(session_id).as_deref(),
        roots,
    )
    .err();
    let source_deleted = source_warning.is_none();
    if let Err(message) = store.delete_session(session_id) {
        return DeleteResult {
            ok: false,
            message,
            source_deleted,
        };
    }
    match source_warning {
        Some(message) => DeleteResult {
            ok: true,
            message: format!("镜像已删除并移入回收站（保留30天），但原始会话删除失败: {message}"),
            source_deleted: false,
        },
        None => DeleteResult {
            ok: true,
            message: "会话已删除（含原始会话），可在回收站保留30天内恢复".to_string(),
            source_deleted: true,
        },
    }
}

/// Removes the original session artifact from the source tool. Best-effort:
/// errors bubble up as strings; absence is success.
fn delete_source(
    session: &CanonicalSession,
    known_path: Option<&str>,
    roots: &RestoreRoots,
) -> Result<(), String> {
    match session.source_tool {
        SourceTool::Codex => delete_codex_source(session, known_path, &roots.codex),
        SourceTool::Claude => delete_claude_source(session, known_path, &roots.claude),
        SourceTool::OpenCode => delete_opencode_source(session, &roots.opencode),
        SourceTool::Dsh => delete_dsh_source(session, known_path, &roots.dsh),
        SourceTool::Pi => delete_pi_source(session, known_path, &roots.pi),
    }
}

fn delete_codex_source(
    session: &CanonicalSession,
    known: Option<&str>,
    root: &Path,
) -> Result<(), String> {
    let path = match known {
        Some(path) => PathBuf::from(path),
        None => {
            let suffix = format!("-{}.jsonl", session.session_id);
            let mut found = vec![];
            collect_codex_files(root, &suffix, &mut found);
            found
                .into_iter()
                .next()
                .ok_or_else(|| "找不到 Codex rollout 文件".to_string())?
        }
    };
    remove_file_if_exists(&path)
}

fn collect_codex_files(directory: &Path, suffix: &str, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_codex_files(&path, suffix, found);
        } else if file_type.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.ends_with(suffix))
                .unwrap_or(false)
        {
            found.push(path);
        }
    }
}

fn delete_claude_source(
    session: &CanonicalSession,
    known: Option<&str>,
    root: &Path,
) -> Result<(), String> {
    let file = match known {
        Some(path) => PathBuf::from(path),
        None => root
            .join("projects")
            .join(encode_claude_project(&session.cwd))
            .join(format!("{}.jsonl", session.session_id)),
    };
    let file_existed = file.exists();
    remove_file_if_exists(&file)?;
    let history = root.join("history.jsonl");
    if history.exists() {
        let payload = fs::read_to_string(&history).map_err(|error| error.to_string())?;
        let kept = payload
            .lines()
            .filter(|line| {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                    return true;
                };
                value.get("sessionId").and_then(serde_json::Value::as_str)
                    != Some(&session.session_id)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut output = kept;
        if !output.is_empty() {
            output.push('\n');
        }
        fs::write(&history, output).map_err(|error| error.to_string())?;
    }
    if file_existed {
        Ok(())
    } else {
        Err("找不到 Claude 会话文件".to_string())
    }
}

fn delete_opencode_source(session: &CanonicalSession, database: &Path) -> Result<(), String> {
    if !database.exists() {
        return Err("找不到 OpenCode 数据库".to_string());
    }
    let connection = rusqlite::Connection::open(database)
        .map_err(|error| format!("open opencode db failed: {error}"))?;
    connection
        .busy_timeout(std::time::Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|error| error.to_string())?;
    let deleted = connection
        .execute(
            "DELETE FROM session WHERE id = ?1",
            rusqlite::params![session.session_id],
        )
        .map_err(|error| error.to_string())?;
    connection
        .execute(
            "DELETE FROM message WHERE session_id = ?1",
            rusqlite::params![session.session_id],
        )
        .map_err(|error| error.to_string())?;
    connection
        .execute(
            "DELETE FROM part WHERE session_id = ?1",
            rusqlite::params![session.session_id],
        )
        .map_err(|error| error.to_string())?;
    if deleted > 0 {
        Ok(())
    } else {
        Err("OpenCode 数据库中没有该会话".to_string())
    }
}

fn delete_pi_source(
    session: &CanonicalSession,
    known: Option<&str>,
    root: &Path,
) -> Result<(), String> {
    let path = match known {
        Some(path) => PathBuf::from(path),
        None => {
            let suffix = format!("_{}.jsonl", session.session_id);
            let mut found = vec![];
            collect_pi_files(root, &suffix, &mut found);
            found
                .into_iter()
                .next()
                .ok_or_else(|| "找不到 Pi 会话文件".to_string())?
        }
    };
    remove_file_if_exists(&path)
}

fn collect_pi_files(directory: &Path, suffix: &str, found: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_pi_files(&path, suffix, found);
        } else if file_type.is_file()
            && path.extension().and_then(|value| value.to_str()) == Some("jsonl")
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.ends_with(suffix))
                .unwrap_or(false)
        {
            found.push(path);
        }
    }
}

fn delete_dsh_source(
    session: &CanonicalSession,
    known: Option<&str>,
    root: &Path,
) -> Result<(), String> {
    let directory = match known {
        Some(path) => {
            let log = PathBuf::from(path);
            let dir = log.parent().unwrap_or(Path::new(".")).to_path_buf();
            if dir
                .file_name()
                .and_then(|name| name.to_str())
                .map(|name| name == dsh::encode_segment(&session.session_id))
                .unwrap_or(false)
            {
                dir
            } else {
                return Err("DSH 源路径与会话 id 不匹配".to_string());
            }
        }
        None => root
            .join(dsh::project_key(&session.cwd))
            .join(dsh::encode_segment(&session.session_id)),
    };
    if !directory.exists() {
        return Err("找不到 DSH 会话目录".to_string());
    }
    fs::remove_dir_all(&directory).map_err(|error| error.to_string())
}

fn remove_file_if_exists(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(format!("找不到文件: {}", path.display()))
        }
        Err(error) => Err(error.to_string()),
    }
}
