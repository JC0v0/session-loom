#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use rusqlite::{params, params_from_iter, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread::{self, JoinHandle},
};

fn no_window(_cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        _cmd.creation_flags(CREATE_NO_WINDOW);
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
                    && m.get("text")
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
    conn.execute(
        "DELETE FROM sessions WHERE session_id = ?",
        params![session_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn cli_invocation(args: &[&str]) -> Result<(String, Vec<String>), String> {
    let tail = || args.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let node = node_program()?;
    if let Ok(cli) = std::env::var("SESSION_LOOM_CLI") {
        return Ok((node, [vec![cli], tail()].concat()));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for candidate in [
                dir.join("dist-cli").join("cli.mjs"),
                dir.join("resources").join("dist-cli").join("cli.mjs"),
                dir.join("..")
                    .join("Resources")
                    .join("dist-cli")
                    .join("cli.mjs"),
            ] {
                if candidate.exists() {
                    return Ok((
                        node,
                        [vec![candidate.to_string_lossy().to_string()], tail()].concat(),
                    ));
                }
            }
        }
    }
    let dev_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
    let bundled = dev_root.join("dist-cli").join("cli.mjs");
    if bundled.exists() {
        return Ok((
            node,
            [vec![bundled.to_string_lossy().to_string()], tail()].concat(),
        ));
    }
    Ok((
        node,
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
    ))
}

fn node_program() -> Result<String, String> {
    if let Ok(node) = std::env::var("SESSION_LOOM_NODE") {
        if !node.trim().is_empty() {
            // An explicit override is authoritative, including custom launchers.
            return Ok(node);
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(node) = first_compatible_node(
            ["/opt/homebrew/bin/node", "/usr/local/bin/node"],
            node_supports_sqlite,
        ) {
            return Ok(node);
        }

        if let Some(node) = node_from_login_shell() {
            return Ok(node);
        }

        if node_supports_sqlite("node") {
            return Ok("node".to_string());
        }

        Err(
            "No compatible Node.js was found. Session-Loom requires Node.js with node:sqlite support. Set SESSION_LOOM_NODE to use a custom Node.js executable."
                .to_string(),
        )
    }

    #[cfg(not(target_os = "macos"))]
    Ok("node".to_string())
}

#[cfg(target_os = "macos")]
fn node_supports_sqlite(node: &str) -> bool {
    let mut cmd = Command::new(node);
    cmd.args(["--input-type=module", "--eval", "import 'node:sqlite'"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.status().map(|status| status.success()).unwrap_or(false)
}

#[cfg(target_os = "macos")]
fn first_compatible_node<'a, I, F>(candidates: I, mut supports_sqlite: F) -> Option<String>
where
    I: IntoIterator<Item = &'a str>,
    F: FnMut(&str) -> bool,
{
    candidates
        .into_iter()
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty() && Path::new(candidate).is_file())
        .find(|candidate| supports_sqlite(candidate))
        .map(str::to_string)
}

#[cfg(target_os = "macos")]
fn login_shell_args() -> [&'static str; 2] {
    ["-lic", "command -v node"]
}

#[cfg(target_os = "macos")]
fn node_from_login_shell() -> Option<String> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let output = Command::new(shell).args(login_shell_args()).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    first_compatible_node(stdout.lines(), node_supports_sqlite)
}

#[tauri::command]
fn sessions_restore(session_id: String, target: String) -> RestoreResult {
    if target != "claude" && target != "codex" {
        return RestoreResult {
            ok: false,
            message: format!("unknown target: {target}"),
        };
    }
    let (program, args) = match cli_invocation(&["restore", "--to", &target, &session_id]) {
        Ok(invocation) => invocation,
        Err(message) => return RestoreResult { ok: false, message },
    };
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
    #[cfg(windows)]
    {
        let mut cmd = Command::new("tasklist");
        cmd.args(["/FI", &format!("PID eq {pid}"), "/NH"]);
        no_window(&mut cmd);
        return cmd
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
            .unwrap_or(false);
    }

    #[cfg(unix)]
    {
        return Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
    }

    #[allow(unreachable_code)]
    false
}

#[cfg(unix)]
fn process_command(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-ww", "-p", &pid.to_string(), "-o", "command="])
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let command = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!command.is_empty()).then_some(command)
}

#[cfg(unix)]
fn is_session_loom_daemon_command(command: &str) -> bool {
    let normalized = command.replace('\\', "/");
    let is_cli = normalized.contains("/dist-cli/cli.mjs") || normalized.contains("/src/cli.ts");
    let args = normalized.split_whitespace().collect::<Vec<_>>();
    let runs_daemon = args
        .windows(2)
        .any(|pair| pair[0] == "daemon" && pair[1] == "run");
    is_cli && runs_daemon
}

fn daemon_process_matches(pid: u32) -> bool {
    #[cfg(windows)]
    {
        return pid_alive(pid);
    }

    #[cfg(unix)]
    {
        return pid_alive(pid)
            && process_command(pid)
                .as_deref()
                .map(is_session_loom_daemon_command)
                .unwrap_or(false);
    }

    #[allow(unreachable_code)]
    false
}

fn terminate_process(pid: u32) {
    #[cfg(windows)]
    {
        let mut cmd = Command::new("taskkill");
        cmd.args(["/PID", &pid.to_string(), "/F"]);
        no_window(&mut cmd);
        let _ = cmd.output();
    }

    #[cfg(unix)]
    {
        let _ = Command::new("kill").arg(pid.to_string()).output();
    }
}

fn terminate_daemon_process(pid: u32) -> bool {
    #[cfg(windows)]
    {
        terminate_process(pid);
        return true;
    }

    #[cfg(unix)]
    {
        if !daemon_process_matches(pid) {
            return false;
        }
        terminate_process(pid);
        return true;
    }

    #[allow(unreachable_code)]
    false
}

fn read_pid(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse::<u32>().ok()
}

fn remove_pid_file_if_matches(path: &Path, pid: u32) -> bool {
    if read_pid(path) != Some(pid) {
        return false;
    }
    fs::remove_file(path).is_ok()
}

fn reap_child(mut child: Child, pid: u32, path: PathBuf) -> JoinHandle<()> {
    thread::spawn(move || {
        let _ = child.wait();
        let _ = remove_pid_file_if_matches(&path, pid);
    })
}

fn record_daemon_child(mut child: Child, store: &Path, pid_path: PathBuf) -> Option<u32> {
    let pid = child.id();
    if fs::create_dir_all(store).is_err() || fs::write(&pid_path, pid.to_string()).is_err() {
        terminate_process(pid);
        let _ = child.wait();
        return None;
    }
    let _ = reap_child(child, pid, pid_path);
    Some(pid)
}

fn daemon_state() -> DaemonState {
    let path = pid_file();
    let Some(pid) = read_pid(&path) else {
        #[cfg(unix)]
        let _ = fs::remove_file(&path);
        return DaemonState {
            running: false,
            pid: None,
        };
    };
    if daemon_process_matches(pid) {
        DaemonState {
            running: true,
            pid: Some(pid),
        }
    } else {
        #[cfg(unix)]
        {
            let _ = remove_pid_file_if_matches(&path, pid);
            DaemonState {
                running: false,
                pid: None,
            }
        }

        #[cfg(not(unix))]
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
            let _ = terminate_daemon_process(pid);
            let _ = remove_pid_file_if_matches(&pid_file(), pid);
        }
        return DaemonState {
            running: false,
            pid: None,
        };
    }
    let Ok((program, args)) = cli_invocation(&["daemon", "run"]) else {
        return DaemonState {
            running: false,
            pid: None,
        };
    };
    let mut cmd = Command::new(&program);
    cmd.args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
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
            let store = store_dir();
            let path = store.join("daemon.pid");
            match record_daemon_child(child, &store, path) {
                Some(pid) => DaemonState {
                    running: true,
                    pid: Some(pid),
                },
                None => DaemonState {
                    running: false,
                    pid: None,
                },
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

#[cfg(all(test, unix))]
mod tests {
    use super::{
        daemon_process_matches, is_session_loom_daemon_command, pid_alive, reap_child,
        record_daemon_child, terminate_daemon_process, terminate_process,
    };
    #[cfg(target_os = "macos")]
    use super::{first_compatible_node, login_shell_args};
    use std::{
        fs,
        path::PathBuf,
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
        thread,
        time::Duration,
    };

    fn test_dir(name: &str) -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "session-loom-{name}-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn selects_the_first_compatible_node_candidate() {
        let dir = test_dir("node-candidates");
        fs::create_dir_all(&dir).unwrap();
        let incompatible = dir.join("node-incompatible");
        let compatible = dir.join("node-compatible");
        fs::write(&incompatible, "incompatible").unwrap();
        fs::write(&compatible, "compatible").unwrap();
        let candidates = [
            incompatible.to_string_lossy().to_string(),
            compatible.to_string_lossy().to_string(),
        ];

        let selected = first_compatible_node(candidates.iter().map(String::as_str), |candidate| {
            candidate == compatible.to_string_lossy()
        });

        assert_eq!(selected, Some(compatible.to_string_lossy().to_string()));
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn login_shell_reads_interactive_configuration() {
        assert_eq!(login_shell_args(), ["-lic", "command -v node"]);
    }

    #[test]
    fn recognizes_only_session_loom_daemon_commands() {
        assert!(is_session_loom_daemon_command(
            "/opt/homebrew/bin/node /Applications/Session-Loom.app/Contents/Resources/dist-cli/cli.mjs daemon run"
        ));
        assert!(is_session_loom_daemon_command(
            "/opt/homebrew/bin/node --import tsx /work/session-loom/src/cli.ts daemon run"
        ));
        assert!(!is_session_loom_daemon_command(
            "/opt/homebrew/bin/node /tmp/dist-cli/cli.mjs daemon status"
        ));
        assert!(!is_session_loom_daemon_command("sleep 30"));
    }

    #[test]
    fn refuses_to_terminate_an_unrelated_process_from_a_stale_pid() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        assert!(!daemon_process_matches(child.id()));
        assert!(!terminate_daemon_process(child.id()));
        assert!(child.try_wait().unwrap().is_none());

        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn reaper_removes_only_the_pid_file_owned_by_its_child() {
        let dir = test_dir("reaper");
        fs::create_dir_all(&dir).unwrap();

        let owned_pid_file = dir.join("owned.pid");
        let owned_child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        let owned_pid = owned_child.id();
        fs::write(&owned_pid_file, owned_pid.to_string()).unwrap();
        reap_child(owned_child, owned_pid, owned_pid_file.clone())
            .join()
            .unwrap();
        assert!(!owned_pid_file.exists());

        let replaced_pid_file = dir.join("replaced.pid");
        let replaced_child = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
        let replaced_pid = replaced_child.id();
        fs::write(&replaced_pid_file, (replaced_pid + 1).to_string()).unwrap();
        reap_child(replaced_child, replaced_pid, replaced_pid_file.clone())
            .join()
            .unwrap();
        assert_eq!(
            fs::read_to_string(&replaced_pid_file).unwrap(),
            (replaced_pid + 1).to_string()
        );

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pid_persistence_failure_terminates_and_waits_for_the_child() {
        let dir = test_dir("pid-write-failure");
        let store = dir.join("store");
        let pid_path = store.join("daemon.pid");
        fs::create_dir_all(&pid_path).unwrap();
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();

        assert_eq!(record_daemon_child(child, &store, pid_path), None);
        assert!(!pid_alive(pid));

        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn records_the_pid_and_reaps_a_successfully_started_child() {
        let dir = test_dir("pid-write-success");
        let store = dir.join("store");
        let pid_path = store.join("daemon.pid");
        let child = Command::new("sleep").arg("30").spawn().unwrap();
        let pid = child.id();

        assert_eq!(
            record_daemon_child(child, &store, pid_path.clone()),
            Some(pid)
        );
        assert_eq!(fs::read_to_string(&pid_path).unwrap(), pid.to_string());

        terminate_process(pid);
        for _ in 0..50 {
            if !pid_path.exists() {
                fs::remove_dir_all(dir).unwrap();
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }

        fs::remove_dir_all(dir).unwrap();
        panic!("reaper did not remove the recorded pid file");
    }

    #[test]
    fn detects_and_terminates_a_background_process() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        assert!(pid_alive(child.id()));

        terminate_process(child.id());
        for _ in 0..50 {
            if child.try_wait().unwrap().is_some() {
                assert!(!pid_alive(child.id()));
                return;
            }
            thread::sleep(Duration::from_millis(20));
        }

        let _ = child.kill();
        panic!("background process did not stop");
    }
}
