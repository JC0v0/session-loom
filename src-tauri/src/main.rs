#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use rusqlite::{params, params_from_iter, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fs, path::PathBuf, process::Command};

fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SessionCard {
    session_id: String,
    source_tool: String,
    cwd: String,
    created_at: String,
    updated_at: String,
    title: String,
    message_count: i64,
}

#[derive(Deserialize, Default)]
struct ListFilter {
    tool: Option<String>,
    query: Option<String>,
}

#[derive(Serialize)]
struct RestoreResult {
    ok: bool,
    message: String,
}

#[derive(Serialize)]
struct DaemonState {
    running: bool,
    pid: Option<u32>,
}

fn store_dir() -> PathBuf {
    if let Ok(dir) =
        std::env::var("SESSION_LOOM_STORE").or_else(|_| std::env::var("SESSION_BRIDGE_STORE"))
    {
        return PathBuf::from(dir);
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".session-loom")
}

fn db_file() -> PathBuf {
    store_dir().join("sessions.db")
}

fn pid_file() -> PathBuf {
    store_dir().join("daemon.pid")
}

fn open_db() -> Result<Connection, String> {
    Connection::open(db_file()).map_err(|e| format!("open db failed: {e}"))
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn session_title(payload: &str) -> (String, i64) {
    let Ok(v) = serde_json::from_str::<Value>(payload) else {
        return ("(无法解析)".to_string(), 0);
    };
    let messages = v.get("messages").and_then(Value::as_array);
    let count = messages.map(|m| m.len() as i64).unwrap_or(0);
    let title = messages
        .and_then(|ms| {
            ms.iter().find(|m| {
                m.get("role").and_then(Value::as_str) == Some("user")
                    && m
                        .get("text")
                        .and_then(Value::as_str)
                        .map(|t| !t.trim().is_empty())
                        .unwrap_or(false)
            })
        })
        .and_then(|m| m.get("text").and_then(Value::as_str))
        .map(|t| collapse_ws(t.trim()))
        .filter(|t| !t.is_empty())
        .map(|t| {
            if t.chars().count() > 80 {
                format!("{}…", t.chars().take(80).collect::<String>())
            } else {
                t
            }
        })
        .unwrap_or_else(|| "(空会话)".to_string());
    (title, count)
}

#[tauri::command]
fn sessions_list(filter: Option<ListFilter>) -> Result<Vec<SessionCard>, String> {
    if !db_file().exists() {
        return Ok(vec![]);
    }
    let filter = filter.unwrap_or_default();
    let mut sql = String::from(
        "SELECT session_id, source_tool, cwd, created_at, updated_at, payload FROM sessions",
    );
    let mut clauses: Vec<&str> = vec![];
    let mut values: Vec<String> = vec![];
    if let Some(tool) = filter.tool.filter(|t| t == "codex" || t == "claude") {
        clauses.push("source_tool = ?");
        values.push(tool);
    }
    if let Some(q) = filter
        .query
        .map(|q| q.trim().to_string())
        .filter(|q| !q.is_empty())
    {
        clauses.push("(payload LIKE ? OR cwd LIKE ?)");
        values.push(format!("%{q}%"));
        values.push(format!("%{q}%"));
    }
    if !clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&clauses.join(" AND "));
    }
    sql.push_str(" ORDER BY updated_at DESC");

    let conn = open_db()?;
    let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map(params_from_iter(values.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = vec![];
    for row in rows {
        let (session_id, source_tool, cwd, created_at, updated_at, payload) =
            row.map_err(|e| e.to_string())?;
        let (title, message_count) = session_title(&payload);
        out.push(SessionCard {
            session_id,
            source_tool,
            cwd,
            created_at,
            updated_at,
            title,
            message_count,
        });
    }
    Ok(out)
}

#[tauri::command]
fn sessions_get(session_id: String) -> Result<Value, String> {
    if !db_file().exists() {
        return Err("store not found".to_string());
    }
    let conn = open_db()?;
    let payload: String = conn
        .query_row(
            "SELECT payload FROM sessions WHERE session_id = ?",
            params![session_id],
            |r| r.get(0),
        )
        .map_err(|_| format!("session not found: {session_id}"))?;
    serde_json::from_str(&payload).map_err(|e| format!("bad payload: {e}"))
}

#[tauri::command]
fn sessions_delete(session_id: String) -> Result<(), String> {
    let conn = open_db()?;
    conn.execute("DELETE FROM sessions WHERE session_id = ?", params![session_id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn cli_invocation(args: &[&str]) -> (String, Vec<String>) {
    let tail = || args.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    if let Ok(cli) = std::env::var("SESSION_LOOM_CLI") {
        return ("node".into(), [vec![cli], tail()].concat());
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in [
                dir.join("dist-cli").join("cli.mjs"),
                dir.join("resources").join("dist-cli").join("cli.mjs"),
            ] {
                if candidate.exists() {
                    return (
                        "node".into(),
                        [vec![candidate.to_string_lossy().to_string()], tail()].concat(),
                    );
                }
            }
        }
    }
    let dev_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let bundled = dev_root.join("dist-cli").join("cli.mjs");
    if bundled.exists() {
        return (
            "node".into(),
            [vec![bundled.to_string_lossy().to_string()], tail()].concat(),
        );
    }
    (
        "node".into(),
        [
            vec![
                "--import".to_string(),
                "tsx".to_string(),
                dev_root
                    .join("src")
                    .join("cli.ts")
                    .to_string_lossy()
                    .to_string(),
            ],
            tail(),
        ]
        .concat(),
    )
}

#[tauri::command]
fn sessions_restore(session_id: String, target: String) -> RestoreResult {
    if target != "claude" && target != "codex" {
        return RestoreResult {
            ok: false,
            message: format!("unknown target: {target}"),
        };
    }
    let (program, args) = cli_invocation(&["restore", "--to", &target, &session_id]);
    let mut cmd = Command::new(&program);
    cmd.args(&args);
    no_window(&mut cmd);
    match cmd.output() {
        Ok(out) => {
            let mut msg = String::from_utf8_lossy(&out.stdout).to_string();
            msg.push_str(&String::from_utf8_lossy(&out.stderr));
            let msg = msg.trim().to_string();
            RestoreResult {
                ok: out.status.success(),
                message: if msg.is_empty() {
                    format!("exit code {:?}", out.status.code())
                } else {
                    msg
                },
            }
        }
        Err(e) => RestoreResult {
            ok: false,
            message: format!("spawn failed (is Node.js installed?): {e}"),
        },
    }
}

fn pid_alive(pid: u32) -> bool {
    let mut cmd = Command::new("tasklist");
    cmd.args(["/FI", &format!("PID eq {pid}"), "/NH"]);
    no_window(&mut cmd);
    cmd.output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

fn daemon_state() -> DaemonState {
    let Ok(content) = fs::read_to_string(pid_file()) else {
        return DaemonState {
            running: false,
            pid: None,
        };
    };
    let Ok(pid) = content.trim().parse::<u32>() else {
        return DaemonState {
            running: false,
            pid: None,
        };
    };
    if pid_alive(pid) {
        DaemonState {
            running: true,
            pid: Some(pid),
        }
    } else {
        DaemonState {
            running: false,
            pid: Some(pid),
        }
    }
}

#[tauri::command]
fn daemon_status() -> DaemonState {
    daemon_state()
}

#[tauri::command]
fn daemon_toggle() -> DaemonState {
    let state = daemon_state();
    if state.running {
        if let Some(pid) = state.pid {
            let mut cmd = Command::new("taskkill");
            cmd.args(["/PID", &pid.to_string(), "/F"]);
            no_window(&mut cmd);
            let _ = cmd.output();
        }
        let _ = fs::remove_file(pid_file());
        return DaemonState {
            running: false,
            pid: None,
        };
    }
    let (program, args) = cli_invocation(&["daemon", "run"]);
    let mut cmd = Command::new(&program);
    cmd.args(&args);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS | CREATE_NO_WINDOW);
    }
    match cmd.spawn() {
        Ok(child) => {
            let pid = child.id();
            let _ = fs::write(pid_file(), pid.to_string());
            DaemonState {
                running: true,
                pid: Some(pid),
            }
        }
        Err(_) => DaemonState {
            running: false,
            pid: None,
        },
    }
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![
            sessions_list,
            sessions_get,
            sessions_delete,
            sessions_restore,
            daemon_status,
            daemon_toggle
        ])
        .run(tauri::generate_context!())
        .expect("error while running session-loom");
}
