use serde_json::json;
use session_loom_core::{
    adapters::{claude, codex},
    canonical::SourceTool,
    restore::{restore_session, RestoreRoots},
    store::Store,
};
use std::{fs, path::Path};

/// A realistic Codex rollout: injected context to filter out, a user turn, an
/// assistant turn carrying text plus two tool calls (string and object output).
fn codex_fixture() -> String {
    [
        json!({"timestamp":"2026-01-01T00:00:00.000Z","type":"session_meta","payload":{"session_id":"s1","id":"s1","cwd":r"C:\proj","timestamp":"2026-01-01T00:00:00.000Z","model_provider":"custom","base_instructions":{"provenance":{"type":"model","model":"deepseek-v4-pro"}}}}),
        json!({"timestamp":"2026-01-01T00:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<injected>skip me"}]}}),
        json!({"timestamp":"2026-01-01T00:00:02.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"帮我修一下这个 bug"}]}}),
        json!({"timestamp":"2026-01-01T00:00:03.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"先跑一下测试"}]}}),
        json!({"timestamp":"2026-01-01T00:00:04.000Z","type":"response_item","payload":{"type":"function_call","call_id":"c1","name":"shell","arguments":"{\"cmd\":\"cargo test\"}"}}),
        json!({"timestamp":"2026-01-01T00:00:05.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"test result: ok. 2 passed"}}),
        json!({"timestamp":"2026-01-01T00:00:06.000Z","type":"response_item","payload":{"type":"function_call","call_id":"c2","name":"read","arguments":"{\"path\":\"src/main.rs\"}"}}),
        json!({"timestamp":"2026-01-01T00:00:07.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c2","output":{"lines":["fn main() {}"]}}}),
        json!({"timestamp":"2026-01-01T00:00:08.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"修好了"}]}}),
    ]
    .into_iter()
    .map(|value| value.to_string())
    .collect::<Vec<_>>()
    .join("\n")
}

/// A realistic Claude Code session: mode and snapshot records to skip, a user
/// turn, an assistant turn with text plus two tool_use blocks, and matching
/// tool_result records.
fn claude_fixture() -> String {
    [
        json!({"type":"mode","mode":"normal","sessionId":"s2"}),
        json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":"帮我修一下这个 bug"}]},"sessionId":"s2","cwd":r"C:\proj","timestamp":"2026-01-01T00:00:00.000Z"}),
        json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"先跑一下测试"},{"type":"tool_use","id":"c1","name":"Bash","input":{"command":"cargo test"}},{"type":"tool_use","id":"c2","name":"Read","input":{"file_path":"src/main.rs"}}]},"sessionId":"s2","timestamp":"2026-01-01T00:00:01.000Z"}),
        json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c1","content":"test result: ok. 2 passed"}]},"sessionId":"s2","timestamp":"2026-01-01T00:00:02.000Z"}),
        json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c2","content":[{"type":"text","text":"fn main() {}"}]}]},"sessionId":"s2","timestamp":"2026-01-01T00:00:03.000Z"}),
        json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"修好了"}]},"sessionId":"s2","timestamp":"2026-01-01T00:00:04.000Z"}),
        json!({"type":"file-history-snapshot","messageId":"m","snapshot":{}}),
    ]
    .into_iter()
    .map(|value| value.to_string())
    .collect::<Vec<_>>()
    .join("\n")
}

fn walk_jsonl(directory: &Path) -> Vec<std::path::PathBuf> {
    let mut files = vec![];
    let Ok(entries) = fs::read_dir(directory) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk_jsonl(&path));
        } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    files
}

#[test]
fn codex_session_converts_to_claude_with_full_content_fidelity() {
    let canonical = codex::parse_session(&codex_fixture()).unwrap();
    assert_eq!(canonical.messages.len(), 3);
    assert_eq!(canonical.messages[1].tool_calls.len(), 2);

    let temp = tempfile::tempdir().unwrap();
    let result = claude::write_session_to_root(&canonical, temp.path()).unwrap();

    let restored =
        claude::parse_session(&fs::read_to_string(&result.session_file).unwrap()).unwrap();

    // Claude assigns a fresh id; everything else survives verbatim.
    assert_ne!(restored.session_id, canonical.session_id);
    assert_eq!(restored.session_id, result.session_id);
    assert_eq!(restored.source_tool, SourceTool::Claude);
    assert_eq!(restored.cwd, canonical.cwd);
    assert_eq!(restored.messages, canonical.messages);
    assert_eq!(
        restored.messages[1].tool_calls[0].output.as_deref(),
        Some("test result: ok. 2 passed")
    );
    // Object tool output is serialized and survives as JSON text.
    assert_eq!(
        restored.messages[1].tool_calls[1].output.as_deref(),
        Some(r#"{"lines":["fn main() {}"]}"#)
    );
    assert!(temp.path().join("history.jsonl").exists());
}

#[test]
fn claude_session_converts_to_codex_with_full_content_fidelity() {
    let canonical = claude::parse_session(&claude_fixture()).unwrap();
    assert_eq!(canonical.messages.len(), 3);
    assert_eq!(canonical.messages[1].tool_calls.len(), 2);

    let output = codex::write_session(&canonical).unwrap();
    let restored = codex::parse_session(&output).unwrap();

    assert_eq!(restored.session_id, canonical.session_id);
    assert_eq!(restored.source_tool, SourceTool::Codex);
    assert_eq!(restored.cwd, canonical.cwd);
    assert_eq!(restored.created_at, canonical.created_at);
    assert_eq!(restored.messages, canonical.messages);
    assert_eq!(
        restored.messages[1].tool_calls[0].output.as_deref(),
        Some("test result: ok. 2 passed")
    );
    // Array tool_result content from Claude is kept as raw JSON text.
    assert_eq!(
        restored.messages[1].tool_calls[1].output.as_deref(),
        Some(r#"[{"text":"fn main() {}","type":"text"}]"#)
    );
}

#[test]
fn codex_and_claude_interchange_through_the_store_restore_path() {
    // The full restore pipeline: ingest a Codex session into the store, restore
    // it as Claude, then restore the same session as Codex, and verify both
    // artifacts parse back with the same conversation.
    let store_temp = tempfile::tempdir().unwrap();
    let codex_root = tempfile::tempdir().unwrap();
    let claude_root = tempfile::tempdir().unwrap();
    let opencode_db = tempfile::tempdir().unwrap().path().join("opencode.db");
    let dsh_root = tempfile::tempdir().unwrap();

    let store = Store::open(store_temp.path()).unwrap();
    store
        .write_session(&codex::parse_session(&codex_fixture()).unwrap())
        .unwrap();
    let roots = RestoreRoots {
        codex: codex_root.path().to_path_buf(),
        claude: claude_root.path().to_path_buf(),
        opencode: opencode_db,
        dsh: dsh_root.path().to_path_buf(),
    };

    assert!(restore_session(&store, SourceTool::Claude, Some("s1"), &roots).ok);
    let claude_files = walk_jsonl(&claude_root.path().join("projects"));
    assert_eq!(claude_files.len(), 1);
    let claude_copy =
        claude::parse_session(&fs::read_to_string(&claude_files[0]).unwrap()).unwrap();
    assert_eq!(claude_copy.source_tool, SourceTool::Claude);
    assert_eq!(claude_copy.cwd, r"C:\proj");
    assert_eq!(claude_copy.messages.len(), 3);
    assert_eq!(claude_copy.messages[0].text, "帮我修一下这个 bug");
    assert_eq!(claude_copy.messages[1].tool_calls.len(), 2);
    assert_eq!(claude_copy.messages[2].text, "修好了");

    assert!(restore_session(&store, SourceTool::Codex, Some("s1"), &roots).ok);
    let codex_files = walk_jsonl(codex_root.path());
    assert_eq!(codex_files.len(), 1);
    let codex_copy = codex::parse_session(&fs::read_to_string(&codex_files[0]).unwrap()).unwrap();
    assert_eq!(codex_copy.source_tool, SourceTool::Codex);
    assert_eq!(codex_copy.cwd, r"C:\proj");
    assert_eq!(codex_copy.messages, claude_copy.messages);
    assert_eq!(
        codex_copy.messages[1].tool_calls[0].input,
        json!({"cmd": "cargo test"})
    );
    assert_ne!(codex_copy.session_id, "s1");
}

#[test]
fn interchange_reassigns_the_source_tool_on_every_conversion() {
    // The canonical payload is provider-neutral; each parsed artifact pins the
    // tool of the format it lives in. A full Codex -> Claude -> Codex chain
    // must flip the flag at every hop and keep the conversation intact.
    let canonical = codex::parse_session(&codex_fixture()).unwrap();
    assert_eq!(canonical.source_tool, SourceTool::Codex);

    let temp = tempfile::tempdir().unwrap();
    let result = claude::write_session_to_root(&canonical, temp.path()).unwrap();
    let as_claude =
        claude::parse_session(&fs::read_to_string(&result.session_file).unwrap()).unwrap();
    assert_eq!(as_claude.source_tool, SourceTool::Claude);
    assert_eq!(as_claude.messages, canonical.messages);

    let as_codex = codex::parse_session(&codex::write_session(&as_claude).unwrap()).unwrap();
    assert_eq!(as_codex.source_tool, SourceTool::Codex);
    assert_eq!(as_codex.messages, canonical.messages);
}
