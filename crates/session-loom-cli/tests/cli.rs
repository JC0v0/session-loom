use rusqlite::{params, Connection};
use session_loom_core::{
    canonical::{CanonicalSession, Message, Role, SourceTool, CANONICAL_SCHEMA_VERSION},
    store::Store,
};
use std::{fs, process::Command};

fn seed_store(root: &std::path::Path) {
    let store = Store::open(root).unwrap();
    store
        .write_session(&CanonicalSession {
            schema_version: CANONICAL_SCHEMA_VERSION,
            source_tool: SourceTool::Codex,
            session_id: "s1".to_string(),
            cwd: "/tmp/project".to_string(),
            created_at: "2026-01-01T00:00:00.000Z".to_string(),
            updated_at: "2026-01-01T00:00:01.000Z".to_string(),
            model_provider: Some("custom".to_string()),
            model: Some("deepseek-v4-pro".to_string()),
            messages: vec![Message {
                role: Role::User,
                text: "hello rust cli".to_string(),
                tool_calls: vec![],
            }],
        })
        .unwrap();
}

#[test]
fn rust_cli_lists_searches_and_exports_sessions() {
    let temp = tempfile::tempdir().unwrap();
    seed_store(temp.path());
    let binary = env!("CARGO_BIN_EXE_ssl");

    let connection = Connection::open(temp.path().join("sessions.db")).unwrap();
    connection
        .execute(
            "UPDATE sessions SET payload = ? WHERE session_id = ?",
            params!["not valid json", "s1"],
        )
        .unwrap();
    drop(connection);

    let list = Command::new(binary)
        .args(["list", "--tool", "codex"])
        .env("SESSION_LOOM_STORE", temp.path())
        .output()
        .unwrap();
    assert!(list.status.success());
    assert!(String::from_utf8_lossy(&list.stdout).contains("s1\tcodex\t/tmp/project"));

    seed_store(temp.path());

    let search = Command::new(binary)
        .args(["search", "rust", "cli"])
        .env("SESSION_LOOM_STORE", temp.path())
        .output()
        .unwrap();
    assert!(search.status.success());
    assert!(String::from_utf8_lossy(&search.stdout).contains("s1"));

    let export = Command::new(binary)
        .args(["export", "s1"])
        .env("SESSION_LOOM_STORE", temp.path())
        .output()
        .unwrap();
    assert!(export.status.success());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&export.stdout).unwrap()["sessionId"],
        "s1"
    );
}

#[test]
fn rust_cli_restores_and_reports_missing_sessions() {
    let store = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let pi = tempfile::tempdir().unwrap();
    seed_store(store.path());
    let binary = env!("CARGO_BIN_EXE_ssl");

    let restore = Command::new(binary)
        .args(["restore", "--to", "claude", "s1"])
        .env("SESSION_LOOM_STORE", store.path())
        .env("CODEX_SESSIONS_ROOT", codex.path())
        .env("CLAUDE_ROOT", claude.path())
        .output()
        .unwrap();
    assert!(restore.status.success());
    assert!(claude.path().join("history.jsonl").exists());

    let opencode = tempfile::tempdir().unwrap();
    let opencode_db = opencode.path().join("opencode.db");
    session_loom_core::adapters::opencode::init_database(&opencode_db).unwrap();
    let restore_opencode = Command::new(binary)
        .args(["restore", "--to", "opencode", "s1"])
        .env("SESSION_LOOM_STORE", store.path())
        .env("OPENCODE_DB", &opencode_db)
        .output()
        .unwrap();
    assert!(restore_opencode.status.success());
    let restored: i64 = Connection::open(&opencode_db)
        .unwrap()
        .query_row("SELECT COUNT(*) FROM session", [], |row| row.get(0))
        .unwrap();
    assert_eq!(restored, 1);

    let dsh = tempfile::tempdir().unwrap();
    let restore_dsh = Command::new(binary)
        .args(["restore", "--to", "dsh", "s1"])
        .env("SESSION_LOOM_STORE", store.path())
        .env("DSH_SESSIONS_ROOT", dsh.path())
        .output()
        .unwrap();
    assert!(restore_dsh.status.success());
    let dsh_logs = session_loom_core::adapters::dsh::session_log_files(dsh.path());
    assert_eq!(dsh_logs.len(), 1);
    assert!(dsh_logs[0].to_string_lossy().ends_with(".jsonl.zstd"));

    let restore_pi = Command::new(binary)
        .args(["restore", "--to", "pi", "s1"])
        .env("SESSION_LOOM_STORE", store.path())
        .env("PI_CODING_AGENT_SESSION_DIR", pi.path())
        .output()
        .unwrap();
    assert!(restore_pi.status.success());
    assert_eq!(
        fs::read_dir(pi.path())
            .unwrap()
            .filter_map(Result::ok)
            .count(),
        1
    );

    let missing = Command::new(binary)
        .args(["export", "missing"])
        .env("SESSION_LOOM_STORE", store.path())
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("session not found"));
}

#[test]
fn rust_cli_prints_no_rows_for_an_empty_list() {
    let store = tempfile::tempdir().unwrap();
    let binary = env!("CARGO_BIN_EXE_ssl");

    let list = Command::new(binary)
        .arg("list")
        .env("SESSION_LOOM_STORE", store.path())
        .output()
        .unwrap();

    assert!(list.status.success());
    assert!(list.stdout.is_empty());
    assert!(list.stderr.is_empty());
}

#[test]
fn rust_cli_deletes_sessions_and_manages_the_trash() {
    let store_dir = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    seed_store(store_dir.path());
    let binary = env!("CARGO_BIN_EXE_ssl");

    // Deleting a missing session fails with an error on stderr.
    let missing = Command::new(binary)
        .args(["delete", "missing"])
        .env("SESSION_LOOM_STORE", store_dir.path())
        .output()
        .unwrap();
    assert!(!missing.status.success());
    assert!(String::from_utf8_lossy(&missing.stderr).contains("session not found"));

    let delete = Command::new(binary)
        .args(["delete", "s1"])
        .env("SESSION_LOOM_STORE", store_dir.path())
        .env("CODEX_SESSIONS_ROOT", codex.path())
        .env("CLAUDE_ROOT", claude.path())
        .output()
        .unwrap();
    assert!(
        delete.status.success(),
        "{}",
        String::from_utf8_lossy(&delete.stderr)
    );
    let store = Store::open(store_dir.path()).unwrap();
    assert!(store.read_session("s1").is_err());

    let trash_list = Command::new(binary)
        .args(["trash", "list"])
        .env("SESSION_LOOM_STORE", store_dir.path())
        .output()
        .unwrap();
    assert!(trash_list.status.success());
    assert!(String::from_utf8_lossy(&trash_list.stdout).contains("s1\tcodex\t/tmp/project"));

    let trash_restore = Command::new(binary)
        .args(["trash", "restore", "s1"])
        .env("SESSION_LOOM_STORE", store_dir.path())
        .output()
        .unwrap();
    assert!(
        trash_restore.status.success(),
        "{}",
        String::from_utf8_lossy(&trash_restore.stderr)
    );
    let store = Store::open(store_dir.path()).unwrap();
    assert_eq!(store.read_session("s1").unwrap().session_id, "s1");

    let trash_delete = Command::new(binary)
        .args(["trash", "delete", "s1"])
        .env("SESSION_LOOM_STORE", store_dir.path())
        .output()
        .unwrap();
    assert!(trash_delete.status.success());

    let trash_list = Command::new(binary)
        .args(["trash", "list"])
        .env("SESSION_LOOM_STORE", store_dir.path())
        .output()
        .unwrap();
    assert!(trash_list.status.success());
    assert!(trash_list.stdout.is_empty());
}
