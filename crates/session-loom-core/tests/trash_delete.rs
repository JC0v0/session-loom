use session_loom_core::{
    adapters::{dsh, opencode, pi},
    canonical::{CanonicalSession, Message, Role, SourceTool, CANONICAL_SCHEMA_VERSION},
    delete::delete_session,
    restore::RestoreRoots,
    store::Store,
    trash::{restore_from_trash, Trash},
    watcher::{SessionWatcher, WatchTarget},
};
use std::{fs, path::Path, time::Duration};

#[path = "common/mod.rs"]
mod common;

fn sample(session_id: &str, source_tool: SourceTool) -> CanonicalSession {
    CanonicalSession {
        schema_version: CANONICAL_SCHEMA_VERSION,
        source_tool,
        session_id: session_id.to_string(),
        cwd: "C:/proj".to_string(),
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
        updated_at: "2026-01-01T00:00:01.000Z".to_string(),
        model_provider: None,
        model: None,
        messages: vec![Message {
            role: Role::User,
            text: "hello trash".to_string(),
            tool_calls: vec![],
        }],
    }
}

fn roots(codex: &Path, claude: &Path, opencode: &Path, dsh: &Path) -> RestoreRoots {
    RestoreRoots {
        codex: codex.to_path_buf(),
        codex_home: codex.to_path_buf(),
        claude: claude.to_path_buf(),
        opencode: opencode.to_path_buf(),
        dsh: dsh.to_path_buf(),
        pi: dsh.to_path_buf(),
    }
}

fn codex_fixture(root: &Path, session_id: &str) -> std::path::PathBuf {
    let directory = root.join("2026").join("01").join("01");
    fs::create_dir_all(&directory).unwrap();
    let file = directory.join(format!("rollout-2026-01-01T00-00-00-{session_id}.jsonl"));
    fs::write(
        &file,
        [
            serde_json::json!({"timestamp":"2026-01-01T00:00:00.000Z","type":"session_meta","payload":{"session_id":session_id,"cwd":"C:/proj","timestamp":"2026-01-01T00:00:00.000Z"}}),
            serde_json::json!({"timestamp":"2026-01-01T00:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello trash"}]}}),
        ]
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join("
"),
    )
    .unwrap();
    file
}

#[test]
fn store_records_and_keeps_source_path() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let source = temp.path().join("source.jsonl");
    let session = sample("s1", SourceTool::Codex);

    store.write_session_from(&session, Some(&source)).unwrap();
    assert_eq!(
        store.session_source_path("s1").as_deref(),
        Some(source.to_string_lossy().as_ref())
    );
    // A plain write without a path must not erase the known source path.
    store.write_session(&session).unwrap();
    assert_eq!(
        store.session_source_path("s1").as_deref(),
        Some(source.to_string_lossy().as_ref())
    );
}

#[test]
fn store_migrates_existing_databases_without_source_path_column() {
    let temp = tempfile::tempdir().unwrap();
    let connection = rusqlite::Connection::open(temp.path().join("sessions.db")).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE sessions (
                session_id TEXT PRIMARY KEY,
                source_tool TEXT NOT NULL,
                cwd TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                schema_version INTEGER NOT NULL,
                payload TEXT NOT NULL
            );
            CREATE TABLE session_summaries (
                session_id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                message_count INTEGER NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
            );",
        )
        .unwrap();
    drop(connection);

    let store = Store::open(temp.path()).unwrap();
    let source = temp.path().join("legacy.jsonl");
    store
        .write_session_from(&sample("s1", SourceTool::Codex), Some(&source))
        .unwrap();
    assert_eq!(
        store.session_source_path("s1").as_deref(),
        Some(source.to_string_lossy().as_ref())
    );
}

#[test]
fn trash_round_trips_and_purges_expired_entries() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let trash = Trash::new(store.root());
    let session = sample("s1", SourceTool::Codex);

    trash.add(&session, None).unwrap();
    assert_eq!(trash.list().unwrap().len(), 1);
    assert!(trash.contains("s1"));
    assert_eq!(trash.get("s1").unwrap().session.session_id, "s1");

    // A zero retention window purges every entry (age is always positive).
    assert_eq!(trash.purge_expired(Duration::ZERO).unwrap(), 1);
    assert!(!trash.contains("s1"));

    trash.add(&session, None).unwrap();
    let restored = restore_from_trash(&store, &trash, "s1").unwrap();
    assert_eq!(restored.session_id, "s1");
    assert_eq!(
        store.read_session("s1").unwrap().messages[0].text,
        "hello trash"
    );
    assert!(!trash.contains("s1"));
    assert_eq!(trash.list().unwrap().len(), 0);

    trash.remove("missing").unwrap();
    assert!(restore_from_trash(&store, &trash, "missing").is_err());
}

#[test]
fn delete_codex_removes_rollout_and_tombstones_the_mirror() {
    let store_temp = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let opencode = tempfile::tempdir().unwrap();
    let dsh = tempfile::tempdir().unwrap();
    let store = Store::open(store_temp.path()).unwrap();
    let file = codex_fixture(codex.path(), "s1");
    store
        .write_session_from(&sample("s1", SourceTool::Codex), Some(&file))
        .unwrap();

    let result = delete_session(
        &store,
        "s1",
        &roots(codex.path(), claude.path(), opencode.path(), dsh.path()),
    );
    assert!(result.ok, "{}", result.message);
    assert!(result.source_deleted);
    assert!(!file.exists());
    assert!(store.read_session("s1").is_err());
    assert!(Trash::new(store.root()).contains("s1"));
}

#[test]
fn delete_codex_reconstructs_path_and_watcher_respects_tombstones() {
    let store_temp = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let opencode = tempfile::tempdir().unwrap();
    let dsh = tempfile::tempdir().unwrap();
    let store = Store::open(store_temp.path()).unwrap();
    let file = codex_fixture(codex.path(), "s1");
    // No source_path recorded: deletion must reconstruct the rollout path by id.
    store
        .write_session(&sample("s1", SourceTool::Codex))
        .unwrap();

    let result = delete_session(
        &store,
        "s1",
        &roots(codex.path(), claude.path(), opencode.path(), dsh.path()),
    );
    assert!(result.ok, "{}", result.message);
    assert!(!file.exists());

    // A source that cannot be deleted stays tombstoned: the watcher must not
    // mirror it back while the recycle-bin entry is retained.
    let file2 = codex_fixture(elsewhere.path(), "s2");
    // No source_path recorded: deletion reconstructs from the (broken) roots,
    // so the source stays on disk and the trash entry must keep the tombstone.
    store
        .write_session(&sample("s2", SourceTool::Codex))
        .unwrap();
    let broken_roots = roots(
        &tempfile::tempdir().unwrap().path().join("missing"),
        claude.path(),
        opencode.path(),
        dsh.path(),
    );
    let result = delete_session(&store, "s2", &broken_roots);
    assert!(result.ok, "{}", result.message);
    assert!(!result.source_deleted);
    assert!(file2.exists());
    assert!(store.read_session("s2").is_err());

    let mut watcher = SessionWatcher::new(
        store.clone(),
        vec![WatchTarget {
            source_tool: SourceTool::Codex,
            root: elsewhere.path().to_path_buf(),
        }],
    );
    watcher.scan_once();
    assert!(store.read_session("s2").is_err());
}

#[test]
fn delete_claude_removes_file_and_history_entry() {
    let store_temp = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let opencode = tempfile::tempdir().unwrap();
    let dsh = tempfile::tempdir().unwrap();
    let store = Store::open(store_temp.path()).unwrap();
    let project = claude.path().join("projects").join("C--proj");
    fs::create_dir_all(&project).unwrap();
    let file = project.join("s1.jsonl");
    fs::write(
        &file,
        "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":[]},\"sessionId\":\"s1\"}\n",
    )
    .unwrap();
    fs::write(
        claude.path().join("history.jsonl"),
        [
            serde_json::json!({"display":"s1 title","pastedContents":{},"timestamp":1,"project":"C:\\proj","sessionId":"s1"}),
            serde_json::json!({"display":"keep me","pastedContents":{},"timestamp":2,"project":"C:\\proj","sessionId":"other"}),
        ]
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join("
")
            + "
",
    )
    .unwrap();
    store
        .write_session_from(&sample("s1", SourceTool::Claude), Some(&file))
        .unwrap();

    let result = delete_session(
        &store,
        "s1",
        &roots(codex.path(), claude.path(), opencode.path(), dsh.path()),
    );
    assert!(result.ok, "{}", result.message);
    assert!(!file.exists());
    let history = fs::read_to_string(claude.path().join("history.jsonl")).unwrap();
    assert!(!history.contains("s1"));
    assert!(history.contains("keep me"));
    assert!(store.read_session("s1").is_err());
    assert!(Trash::new(store.root()).contains("s1"));
}

#[test]
fn delete_opencode_removes_database_rows() {
    let store_temp = tempfile::tempdir().unwrap();
    let opencode = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let dsh = tempfile::tempdir().unwrap();
    let store = Store::open(store_temp.path()).unwrap();
    let database = opencode.path().join("opencode.db");
    let mut session = sample("oc1", SourceTool::OpenCode);
    session.cwd = "C:/proj".to_string();
    opencode::init_database(&database).unwrap();
    let result = opencode::write_session_to_database(&session, &database).unwrap();
    let mirrored = opencode::parse_sessions(&database).unwrap();
    store
        .write_session_from(&mirrored[0], Some(&database))
        .unwrap();
    assert_eq!(
        store.read_session(&result.session_id).unwrap().session_id,
        result.session_id
    );

    let deleted = delete_session(
        &store,
        &result.session_id,
        &roots(codex.path(), claude.path(), &database, dsh.path()),
    );
    assert!(deleted.ok, "{}", deleted.message);
    assert!(deleted.source_deleted, "{}", deleted.message);
    assert!(opencode::parse_sessions(&database).unwrap().is_empty());
    assert!(Trash::new(store.root()).contains(&result.session_id));
}

#[test]
fn delete_dsh_removes_session_directory() {
    let store_temp = tempfile::tempdir().unwrap();
    let dsh = tempfile::tempdir().unwrap();
    let dsh_home = tempfile::tempdir().unwrap();
    let _dsh_home_guard = common::isolate_dsh_home(dsh_home.path());
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let opencode = tempfile::tempdir().unwrap();
    let store = Store::open(store_temp.path()).unwrap();
    let mut session = sample("dsh1", SourceTool::Dsh);
    session.cwd = "C:/proj".to_string();
    let result = dsh::write_session_to_root(&session, dsh.path()).unwrap();
    let logs = dsh::session_log_files(dsh.path());
    assert_eq!(logs.len(), 1);
    let mirrored = dsh::parse_session_file(&logs[0]).unwrap().unwrap();
    store.write_session_from(&mirrored, Some(&logs[0])).unwrap();

    let deleted = delete_session(
        &store,
        &result.session_id,
        &roots(codex.path(), claude.path(), opencode.path(), dsh.path()),
    );
    assert!(deleted.ok, "{}", deleted.message);
    assert!(dsh::session_log_files(dsh.path()).is_empty());
    assert!(Trash::new(store.root()).contains(&result.session_id));
}

#[test]
fn delete_pi_removes_session_file() {
    let store_temp = tempfile::tempdir().unwrap();
    let pi_root = tempfile::tempdir().unwrap();
    let codex = tempfile::tempdir().unwrap();
    let claude = tempfile::tempdir().unwrap();
    let opencode = tempfile::tempdir().unwrap();
    let dsh = tempfile::tempdir().unwrap();
    let store = Store::open(store_temp.path()).unwrap();
    let session = sample("pi1", SourceTool::Pi);
    let result = pi::write_session_to_root(&session, pi_root.path()).unwrap();
    let mirrored = pi::parse_session(&fs::read_to_string(&result.session_file).unwrap()).unwrap();
    store
        .write_session_from(&mirrored, Some(&result.session_file))
        .unwrap();

    let deleted = delete_session(
        &store,
        &result.session_id,
        &RestoreRoots {
            codex: codex.path().to_path_buf(),
            codex_home: codex.path().to_path_buf(),
            claude: claude.path().to_path_buf(),
            opencode: opencode.path().join("opencode.db"),
            dsh: dsh.path().to_path_buf(),
            pi: pi_root.path().to_path_buf(),
        },
    );
    assert!(deleted.ok, "{}", deleted.message);
    assert!(deleted.source_deleted, "{}", deleted.message);
    assert!(!result.session_file.exists());
    assert!(Trash::new(store.root()).contains(&result.session_id));
}
