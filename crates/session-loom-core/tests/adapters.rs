use serde_json::{json, Value};
use session_loom_core::{
    adapters::{claude, codex},
    canonical::{CanonicalSession, Message, Role, SourceTool, ToolCall, CANONICAL_SCHEMA_VERSION},
};

fn sample_session(source_tool: SourceTool) -> CanonicalSession {
    CanonicalSession {
        schema_version: CANONICAL_SCHEMA_VERSION,
        source_tool,
        session_id: "s1".to_string(),
        cwd: r"C:\proj".to_string(),
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
        updated_at: "2026-01-01T00:00:01.000Z".to_string(),
        messages: vec![
            Message {
                role: Role::User,
                text: "hello".to_string(),
                tool_calls: vec![],
            },
            Message {
                role: Role::Assistant,
                text: "running".to_string(),
                tool_calls: vec![ToolCall {
                    id: "c1".to_string(),
                    name: "shell".to_string(),
                    input: json!({ "cmd": "ls" }),
                    output: Some("file.txt".to_string()),
                }],
            },
        ],
    }
}

#[test]
fn canonical_json_round_trips_every_field() {
    let session = sample_session(SourceTool::Codex);
    let encoded = session.to_json().unwrap();
    assert_eq!(CanonicalSession::from_json(&encoded).unwrap(), session);
}

#[test]
fn canonical_json_rejects_unknown_schema_versions() {
    let mut value = serde_json::to_value(sample_session(SourceTool::Codex)).unwrap();
    value["schemaVersion"] = json!(99);
    assert!(CanonicalSession::from_json(&value.to_string())
        .unwrap_err()
        .contains("schema version"));
}

#[test]
fn parses_codex_messages_calls_and_filters_injected_context() {
    let fixture = [
        json!({"timestamp":"2026-01-01T00:00:00.000Z","type":"session_meta","payload":{"session_id":"s1","cwd":r"C:\proj","timestamp":"2026-01-01T00:00:00.000Z"}}),
        json!({"timestamp":"2026-01-01T00:00:01.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<recommended_plugins>ignored"}]}}),
        json!({"timestamp":"2026-01-01T00:00:02.000Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"do it"}]}}),
        json!({"timestamp":"2026-01-01T00:00:03.000Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}}),
        json!({"timestamp":"2026-01-01T00:00:04.000Z","type":"response_item","payload":{"type":"function_call","call_id":"c1","name":"shell","arguments":"{\"cmd\":\"ls\"}"}}),
        json!({"timestamp":"2026-01-01T00:00:05.000Z","type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"file.txt"}}),
    ]
    .into_iter()
    .map(|value| value.to_string())
    .collect::<Vec<_>>()
    .join("\n");

    let session = codex::parse_session(&fixture).unwrap();

    assert_eq!(session.source_tool, SourceTool::Codex);
    assert_eq!(session.session_id, "s1");
    assert_eq!(session.cwd, r"C:\proj");
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[0].text, "do it");
    assert_eq!(session.messages[1].tool_calls.len(), 1);
    assert_eq!(session.messages[1].tool_calls[0].input, json!({"cmd":"ls"}));
    assert_eq!(
        session.messages[1].tool_calls[0].output.as_deref(),
        Some("file.txt")
    );
}

#[test]
fn writes_codex_interactive_session_records() {
    let output = codex::write_session(&sample_session(SourceTool::Codex)).unwrap();
    let records = output
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();

    assert_eq!(records[0]["type"], "session_meta");
    assert_eq!(records[0]["payload"]["originator"], "codex-tui");
    assert_eq!(records[0]["payload"]["source"], "cli");
    assert_eq!(records[0]["payload"]["thread_source"], "user");
    assert!(records
        .iter()
        .any(|record| record["payload"]["type"] == "function_call"));
    assert!(records
        .iter()
        .any(|record| record["payload"]["type"] == "function_call_output"));
}

#[test]
fn parses_claude_messages_calls_and_tool_results() {
    let fixture = [
        json!({"type":"mode","mode":"normal","sessionId":"s2"}),
        json!({"type":"user","message":{"role":"user","content":[{"type":"text","text":"hello"}]},"sessionId":"s2","cwd":r"C:\proj","timestamp":"2026-01-01T00:00:00.000Z"}),
        json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"running"},{"type":"tool_use","id":"c2","name":"Bash","input":{"command":"ls"}}]},"sessionId":"s2","timestamp":"2026-01-01T00:00:01.000Z"}),
        json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"c2","content":"ok"}]},"sessionId":"s2","timestamp":"2026-01-01T00:00:02.000Z"}),
        json!({"type":"file-history-snapshot","messageId":"m","snapshot":{}}),
    ]
    .into_iter()
    .map(|value| value.to_string())
    .collect::<Vec<_>>()
    .join("\n");

    let session = claude::parse_session(&fixture).unwrap();

    assert_eq!(session.source_tool, SourceTool::Claude);
    assert_eq!(session.session_id, "s2");
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[1].text, "running");
    assert_eq!(
        session.messages[1].tool_calls[0].input,
        json!({"command":"ls"})
    );
    assert_eq!(
        session.messages[1].tool_calls[0].output.as_deref(),
        Some("ok")
    );
}

#[test]
fn writes_claude_session_file_and_history() {
    let temp = tempfile::tempdir().unwrap();
    let mut session = sample_session(SourceTool::Claude);
    session.cwd = r"F:\xiaoyou\xiaoyou".to_string();

    let result = claude::write_session_to_root(&session, temp.path()).unwrap();
    let history = std::fs::read_to_string(temp.path().join("history.jsonl")).unwrap();
    let history_entry: Value = serde_json::from_str(history.lines().next().unwrap()).unwrap();

    assert_eq!(history_entry["sessionId"], result.session_id);
    assert_eq!(history_entry["project"], r"F:\xiaoyou\xiaoyou");
    assert_eq!(history_entry["display"], "hello");
    assert!(result
        .session_file
        .to_string_lossy()
        .contains("F--xiaoyou-xiaoyou"));
}

#[test]
fn claude_restore_removes_the_session_when_history_cannot_be_updated() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::create_dir(temp.path().join("history.jsonl")).unwrap();

    let error = claude::write_session_to_root(&sample_session(SourceTool::Claude), temp.path())
        .unwrap_err();

    assert!(!error.is_empty());
    let project = temp.path().join("projects").join("C--proj");
    assert!(project.exists());
    assert!(std::fs::read_dir(project).unwrap().next().is_none());
}
