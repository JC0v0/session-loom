use serde_json::json;
use session_loom_core::{
    canonical::SourceTool,
    store::Store,
    watcher::{SessionWatcher, WatchTarget},
};
use std::fs;

#[test]
fn watcher_mirrors_new_and_growing_session_files() {
    let source = tempfile::tempdir().unwrap();
    let store_root = tempfile::tempdir().unwrap();
    let store = Store::open(store_root.path()).unwrap();
    let file = source.path().join("s1.jsonl");
    fs::write(
        &file,
        [
            json!({"timestamp":"2026-01-01T00:00:00.000Z","type":"session_meta","payload":{"session_id":"s1","cwd":r"C:\proj","timestamp":"2026-01-01T00:00:00.000Z"}}),
            json!({"timestamp":"2026-01-01T00:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"hello"}]}}),
        ]
        .into_iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join("\n"),
    )
    .unwrap();
    let mut watcher = SessionWatcher::new(
        store.clone(),
        vec![WatchTarget {
            source_tool: SourceTool::Codex,
            root: source.path().to_path_buf(),
        }],
    );

    watcher.scan_once();
    assert_eq!(store.read_session("s1").unwrap().messages[0].text, "hello");

    let mut payload = fs::read_to_string(&file).unwrap();
    payload.push('\n');
    payload.push_str(
        &json!({"timestamp":"2026-01-01T00:00:02.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"world"}]}}).to_string(),
    );
    fs::write(&file, payload).unwrap();
    watcher.scan_once();

    assert!(store
        .read_session("s1")
        .unwrap()
        .messages
        .iter()
        .any(|message| message.text == "world"));
    assert_eq!(fs::read_dir(source.path()).unwrap().count(), 1);
}

#[test]
fn watcher_retries_files_that_do_not_have_session_metadata_yet() {
    let source = tempfile::tempdir().unwrap();
    let store_root = tempfile::tempdir().unwrap();
    let store = Store::open(store_root.path()).unwrap();
    let file = source.path().join("pending.jsonl");
    fs::write(&file, "").unwrap();
    let mut watcher = SessionWatcher::new(
        store.clone(),
        vec![WatchTarget {
            source_tool: SourceTool::Codex,
            root: source.path().to_path_buf(),
        }],
    );

    watcher.scan_once();
    assert!(store.list_sessions(None).unwrap().is_empty());

    fs::write(
        &file,
        json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {
                "session_id": "s1",
                "cwd": "/tmp/project",
                "timestamp": "2026-01-01T00:00:00.000Z"
            }
        })
        .to_string(),
    )
    .unwrap();
    watcher.scan_once();

    assert_eq!(store.read_session("s1").unwrap().session_id, "s1");
    assert_eq!(store.list_sessions(None).unwrap().len(), 1);
}

#[test]
fn watcher_remirrors_sources_after_the_store_is_wiped_and_keeps_tombstones() {
    use session_loom_core::{delete::delete_session, restore::RestoreRoots, trash::Trash};

    let source = tempfile::tempdir().unwrap();
    let store_root = tempfile::tempdir().unwrap();
    let store = Store::open(store_root.path()).unwrap();
    for (id, text) in [("s1", "hello"), ("s2", "keep me")] {
        fs::write(
            source.path().join(format!("{id}.jsonl")),
            [
                json!({"timestamp":"2026-01-01T00:00:00.000Z","type":"session_meta","payload":{"session_id":id,"cwd":r"C:\proj","timestamp":"2026-01-01T00:00:00.000Z"}}),
                json!({"timestamp":"2026-01-01T00:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":text}]}}),
            ]
            .into_iter()
            .map(|value| value.to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        )
        .unwrap();
    }
    let mut watcher = SessionWatcher::new(
        store.clone(),
        vec![WatchTarget {
            source_tool: SourceTool::Codex,
            root: source.path().to_path_buf(),
        }],
    );
    watcher.scan_once();
    assert!(store.read_session("s1").is_ok());
    assert!(store.read_session("s2").is_ok());

    // Tombstone s2: its source survives (the roots point at missing dirs)
    // while the mirror row is removed and a trash entry retained.
    let broken_roots = RestoreRoots {
        codex: store_root.path().join("missing-codex"),
        claude: store_root.path().join("missing-claude"),
        opencode: store_root.path().join("missing-opencode.db"),
        dsh: store_root.path().join("missing-dsh"),
    };
    let result = delete_session(&store, "s2", &broken_roots);
    assert!(result.ok, "{}", result.message);
    assert!(store.read_session("s2").is_err());
    assert!(Trash::new(store.root()).contains("s2"));

    // Wipe the database, like deleting the session data while the daemon
    // keeps running. The unchanged files must be mirrored again into the
    // fresh store, while the tombstoned session stays deleted.
    fs::remove_file(store.db_path()).unwrap();
    watcher.scan_once();

    assert_eq!(store.read_session("s1").unwrap().messages[0].text, "hello");
    assert!(store.read_session("s2").is_err());
    assert!(store.has_sessions().unwrap());
}

#[test]
fn watcher_does_not_mirror_claude_history_as_a_session() {
    // Restoring a session into a Claude root writes a transcript file plus a
    // history.jsonl entry sharing the same sessionId. The next scan must
    // mirror the transcript and ignore history.jsonl entirely: without this,
    // the history entry overwrites the session as an empty ghost.
    let source = tempfile::tempdir().unwrap();
    let store_root = tempfile::tempdir().unwrap();
    let store = Store::open(store_root.path()).unwrap();
    let projects = source.path().join("projects");
    fs::create_dir_all(&projects).unwrap();
    fs::write(
        projects.join("s2.jsonl"),
        [
            json!({"type":"mode","mode":"normal","sessionId":"s2"}),
            json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":"hello"}]},"sessionId":"s2","cwd":"/tmp/project","timestamp":"2026-01-01T00:00:00.000Z"}),
        ]
        .into_iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join("\n"),
    )
    .unwrap();
    fs::write(
        source.path().join("history.jsonl"),
        json!({
            "display": "hello",
            "pastedContents": {},
            "timestamp": 1767225600000_i64,
            "project": "/tmp/project",
            "sessionId": "s2"
        })
        .to_string()
            + "\n",
    )
    .unwrap();
    let mut watcher = SessionWatcher::new(
        store.clone(),
        vec![WatchTarget {
            source_tool: SourceTool::Claude,
            root: source.path().to_path_buf(),
        }],
    );

    watcher.scan_once();

    assert_eq!(store.list_sessions(None).unwrap().len(), 1);
    let session = store.read_session("s2").unwrap();
    assert_eq!(session.cwd, "/tmp/project");
    assert_eq!(session.messages[0].text, "hello");
}
#[cfg(unix)]
#[test]
fn watcher_does_not_follow_directory_symlink_cycles() {
    use std::os::unix::fs::symlink;

    let source = tempfile::tempdir().unwrap();
    let store_root = tempfile::tempdir().unwrap();
    let store = Store::open(store_root.path()).unwrap();
    let file = source.path().join("s1.jsonl");
    fs::write(
        &file,
        json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {
                "session_id": "s1",
                "cwd": "/tmp/project",
                "timestamp": "2026-01-01T00:00:00.000Z"
            }
        })
        .to_string(),
    )
    .unwrap();
    symlink(source.path(), source.path().join("loop")).unwrap();
    let mut watcher = SessionWatcher::new(
        store.clone(),
        vec![WatchTarget {
            source_tool: SourceTool::Codex,
            root: source.path().to_path_buf(),
        }],
    );

    watcher.scan_once();

    assert_eq!(store.read_session("s1").unwrap().session_id, "s1");
}
#[test]
fn watcher_mirrors_opencode_database_sessions() {
    use rusqlite::{params, Connection};
    use session_loom_core::{
        adapters::opencode,
        canonical::{CanonicalSession, Message, Role, CANONICAL_SCHEMA_VERSION},
    };

    let source = tempfile::tempdir().unwrap();
    let store_root = tempfile::tempdir().unwrap();
    let store = Store::open(store_root.path()).unwrap();
    let database = source.path().join("opencode.db");
    let session = CanonicalSession {
        schema_version: CANONICAL_SCHEMA_VERSION,
        source_tool: SourceTool::OpenCode,
        session_id: "s1".to_string(),
        cwd: "C:/proj".to_string(),
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
        updated_at: "2026-01-01T00:00:01.000Z".to_string(),
        model_provider: None,
        model: None,
        messages: vec![Message {
            role: Role::User,
            text: "hello".to_string(),
            tool_calls: vec![],
        }],
    };
    let result = opencode::write_session_to_database(&session, &database).unwrap();
    let mut watcher = SessionWatcher::new(
        store.clone(),
        vec![WatchTarget {
            source_tool: SourceTool::OpenCode,
            root: database.clone(),
        }],
    );

    watcher.scan_once();
    assert_eq!(
        store.read_session(&result.session_id).unwrap().messages[0].text,
        "hello"
    );

    let connection = Connection::open(&database).unwrap();
    connection
        .execute(
            "INSERT INTO message VALUES ('msg_2', ?, 1767225601000, 1767225601000, ?)",
            params![
                result.session_id,
                json!({ "role": "user", "time": { "created": 1767225601000_i64 } }).to_string()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO part VALUES ('prt_2', 'msg_2', ?, 1767225601000, 1767225601000, ?)",
            params![
                result.session_id,
                json!({"type": "text", "text": "world"}).to_string()
            ],
        )
        .unwrap();
    drop(connection);

    watcher.scan_once();
    assert!(store
        .read_session(&result.session_id)
        .unwrap()
        .messages
        .iter()
        .any(|message| message.text == "world"));
}
#[test]
fn watcher_mirrors_dsh_session_logs() {
    use session_loom_core::{
        adapters::dsh,
        canonical::{CanonicalSession, Message, Role, CANONICAL_SCHEMA_VERSION},
    };

    let source = tempfile::tempdir().unwrap();
    let store_root = tempfile::tempdir().unwrap();
    let store = Store::open(store_root.path()).unwrap();
    let project = source.path().join("--C--proj--");
    let dir = project.join("session-s1");
    fs::create_dir_all(&dir).unwrap();
    let file = dir.join("session.jsonl");
    fs::write(
        &file,
        [
            json!({"type":"session","version":0,"id":"session-s1","createdAt":1767225600000_i64,"cwd":"C:/proj","delegationDepth":0}),
            json!({"type":"user/message","seq":0,"time":1767225600001_i64,"data":{"id":"m1","role":"user","content":[{"type":"text","text":"hello"}],"source":{"kind":"user"}},"surfaceOp":"append"}),
            json!({"type":"assistant/message","seq":1,"time":1767225600002_i64,"data":{"turn":1,"step":1,"message":{"id":"m2","role":"assistant","content":[{"type":"text","text":"world"}],"source":{"kind":"model","provider":"x","model":"y"}}},"surfaceOp":"append"}),
        ]
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>()
        .join("\n")
            + "\n",
    )
    .unwrap();
    let mut watcher = SessionWatcher::new(
        store.clone(),
        vec![WatchTarget {
            source_tool: SourceTool::Dsh,
            root: source.path().to_path_buf(),
        }],
    );

    watcher.scan_once();
    assert_eq!(store.read_session("session-s1").unwrap().messages.len(), 2);

    let mut payload = fs::read_to_string(&file).unwrap();
    payload.push_str(
        &json!({"type":"user/message","seq":2,"time":1767225600003_i64,"data":{"id":"m3","role":"user","content":[{"type":"text","text":"more"}],"source":{"kind":"user"}},"surfaceOp":"append"})
            .to_string(),
    );
    payload.push('\n');
    fs::write(&file, payload).unwrap();
    watcher.scan_once();

    assert!(store
        .read_session("session-s1")
        .unwrap()
        .messages
        .iter()
        .any(|message| message.text == "more"));

    // a subagent log in the same root must not appear in the store
    let sub = source.path().join("--C--proj--").join("session-sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(
        sub.join("session.jsonl"),
        json!({"type":"session","version":0,"id":"session-sub","createdAt":1767225600000_i64,"cwd":"C:/proj","origin":"subagent","delegationDepth":1})
            .to_string()
            + "\n",
    )
    .unwrap();
    watcher.scan_once();
    assert!(store.read_session("session-sub").is_err());

    // restored zstd logs are mirrored too
    let session = CanonicalSession {
        schema_version: CANONICAL_SCHEMA_VERSION,
        source_tool: SourceTool::Dsh,
        session_id: "s2".to_string(),
        cwd: "C:/proj".to_string(),
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
        updated_at: "2026-01-01T00:00:01.000Z".to_string(),
        model_provider: None,
        model: None,
        messages: vec![Message {
            role: Role::User,
            text: "zstd hello".to_string(),
            tool_calls: vec![],
        }],
    };
    let result = dsh::write_session_to_root(&session, source.path()).unwrap();
    watcher.scan_once();
    assert_eq!(
        store.read_session(&result.session_id).unwrap().messages[0].text,
        "zstd hello"
    );
}
