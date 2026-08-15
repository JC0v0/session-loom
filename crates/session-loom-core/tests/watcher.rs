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
