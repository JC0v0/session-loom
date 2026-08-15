use serde::Serialize;
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::Duration,
};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DaemonState {
    pub running: bool,
    pub pid: Option<u32>,
}

pub fn daemon_state(store_root: &Path) -> DaemonState {
    let pid_path = pid_file(store_root);
    let Some(pid) = read_pid(&pid_path) else {
        let _ = fs::remove_file(pid_path);
        return stopped();
    };
    if daemon_process_matches(pid) {
        DaemonState {
            running: true,
            pid: Some(pid),
        }
    } else {
        let _ = remove_pid_file_if_matches(&pid_path, pid);
        stopped()
    }
}

pub fn ensure_daemon_running(store_root: &Path, executable: &Path) -> DaemonState {
    let state = daemon_state(store_root);
    #[cfg(unix)]
    if let Some(pid) = state.pid.filter(|_| state.running) {
        let command = process_command(pid).unwrap_or_default();
        if is_rust_daemon_command(&command) {
            return state;
        }
        if is_legacy_daemon_command(&command) {
            if !terminate_daemon_process(pid) {
                return state;
            }
            let _ = remove_pid_file_if_matches(&pid_file(store_root), pid);
        } else {
            return state;
        }
    }
    #[cfg(not(unix))]
    if state.running {
        return state;
    }

    let mut command = Command::new(executable);
    command
        .args(["daemon", "run"])
        .env("SESSION_LOOM_STORE", store_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    no_window(&mut command);
    match command.spawn() {
        Ok(child) => record_daemon_child(child, store_root),
        Err(_) => stopped(),
    }
}

pub fn stop_daemon(store_root: &Path) -> DaemonState {
    let state = daemon_state(store_root);
    if let Some(pid) = state.pid.filter(|_| state.running) {
        if !terminate_daemon_process(pid) {
            return state;
        }
        let _ = remove_pid_file_if_matches(&pid_file(store_root), pid);
    }
    stopped()
}

fn stopped() -> DaemonState {
    DaemonState {
        running: false,
        pid: None,
    }
}

fn pid_file(store_root: &Path) -> PathBuf {
    store_root.join("daemon.pid")
}

fn read_pid(path: &Path) -> Option<u32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn record_daemon_child(mut child: Child, store_root: &Path) -> DaemonState {
    let pid = child.id();
    let pid_path = pid_file(store_root);
    if fs::create_dir_all(store_root).is_err() || fs::write(&pid_path, pid.to_string()).is_err() {
        let _ = child.kill();
        let _ = child.wait();
        return stopped();
    }
    thread::spawn(move || {
        let _ = child.wait();
        let _ = remove_pid_file_if_matches(&pid_path, pid);
    });
    DaemonState {
        running: true,
        pid: Some(pid),
    }
}

fn remove_pid_file_if_matches(path: &Path, pid: u32) -> bool {
    if read_pid(path) != Some(pid) {
        return false;
    }
    fs::remove_file(path).is_ok()
}

fn daemon_process_matches(pid: u32) -> bool {
    if !pid_alive(pid) {
        return false;
    }
    #[cfg(unix)]
    {
        process_command(pid)
            .as_deref()
            .map(is_session_loom_daemon_command)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    true
}

#[cfg(unix)]
fn is_session_loom_daemon_command(command: &str) -> bool {
    is_rust_daemon_command(command) || is_legacy_daemon_command(command)
}

#[cfg(any(unix, test))]
fn is_rust_daemon_command(command: &str) -> bool {
    let normalized = command.replace('\\', "/");
    normalized == "ssl daemon run"
        || normalized == "ssl.exe daemon run"
        || normalized.contains("/ssl daemon run")
        || normalized.contains("/ssl.exe daemon run")
}

#[cfg(any(unix, test))]
fn is_legacy_daemon_command(command: &str) -> bool {
    let normalized = command.replace('\\', "/");
    (normalized.contains("/dist-cli/cli.mjs") || normalized.contains("/src/cli.ts"))
        && has_daemon_run_args(&normalized)
}

#[cfg(any(unix, test))]
fn has_daemon_run_args(command: &str) -> bool {
    command
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .any(|pair| pair == ["daemon", "run"])
}

fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        return Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
    }
    #[cfg(windows)]
    {
        let mut command = Command::new("tasklist");
        command.args(["/FI", &format!("PID eq {pid}"), "/NH"]);
        no_window(&mut command);
        return command
            .output()
            .map(|output| String::from_utf8_lossy(&output.stdout).contains(&pid.to_string()))
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

fn terminate_daemon_process(pid: u32) -> bool {
    if !daemon_process_matches(pid) {
        return false;
    }
    #[cfg(unix)]
    let result = Command::new("kill").arg(pid.to_string()).status();
    #[cfg(windows)]
    let result = {
        let mut command = Command::new("taskkill");
        command.args(["/PID", &pid.to_string(), "/F"]);
        no_window(&mut command);
        command.status()
    };
    if !result.map(|status| status.success()).unwrap_or(false) {
        return false;
    }
    for _ in 0..25 {
        if !daemon_process_matches(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    !daemon_process_matches(pid)
}

fn no_window(_command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        _command.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS | CREATE_NO_WINDOW);
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::{daemon_process_matches, pid_alive, terminate_daemon_process};
    use super::{has_daemon_run_args, is_legacy_daemon_command, is_rust_daemon_command};
    #[cfg(unix)]
    use std::process::Command;

    #[test]
    fn recognizes_rust_and_legacy_daemon_commands() {
        assert!(is_rust_daemon_command(
            "/Applications/Session-Loom.app/Contents/Resources/ssl daemon run"
        ));
        assert!(is_rust_daemon_command(
            "/Users/example/My Projects/session-loom/target/debug/ssl daemon run"
        ));
        assert!(is_legacy_daemon_command(
            "/opt/homebrew/bin/node /Applications/Session-Loom.app/Contents/Resources/dist-cli/cli.mjs daemon run"
        ));
        assert!(!is_rust_daemon_command("/tmp/ssl daemon status"));
        assert!(!has_daemon_run_args("sleep 30"));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_to_terminate_an_unrelated_process() {
        let mut child = Command::new("sleep").arg("30").spawn().unwrap();
        assert!(pid_alive(child.id()));
        assert!(!terminate_daemon_process(child.id()));
        assert!(child.try_wait().unwrap().is_none());
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn reports_when_a_daemon_ignores_the_stop_signal() {
        use std::{fs, os::unix::fs::PermissionsExt, thread, time::Duration};

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("ssl");
        let ready = directory.path().join("ready");
        fs::write(
            &executable,
            "#!/bin/sh\ntrap '' TERM\n: > \"$READY_FILE\"\nwhile true; do sleep 1; done\n",
        )
        .unwrap();
        let mut permissions = fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&executable, permissions).unwrap();
        let mut child = Command::new(&executable)
            .args(["daemon", "run"])
            .env("READY_FILE", &ready)
            .spawn()
            .unwrap();

        for _ in 0..50 {
            if ready.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists());

        assert!(daemon_process_matches(child.id()));
        assert!(!terminate_daemon_process(child.id()));
        assert!(child.try_wait().unwrap().is_none());

        child.kill().unwrap();
        child.wait().unwrap();
    }
}
