use rusqlite::params;
use serde_json::{json, Value};
use session_loom_core::{
    adapters::{claude, codex, dsh, opencode, pi},
    canonical::{CanonicalSession, Message, Role, SourceTool, ToolCall, CANONICAL_SCHEMA_VERSION},
};

#[path = "common/mod.rs"]
mod common;

fn sample_session(source_tool: SourceTool) -> CanonicalSession {
    CanonicalSession {
        schema_version: CANONICAL_SCHEMA_VERSION,
        source_tool,
        session_id: "s1".to_string(),
        cwd: r"C:\proj".to_string(),
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
        updated_at: "2026-01-01T00:00:01.000Z".to_string(),
        model_provider: Some("custom".to_string()),
        model: Some("deepseek-v4-pro".to_string()),
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
        json!({"timestamp":"2026-01-01T00:00:00.000Z","type":"session_meta","payload":{"session_id":"s1","cwd":r"C:\proj","timestamp":"2026-01-01T00:00:00.000Z","model_provider":"custom","base_instructions":{"provenance":{"type":"model","model":"deepseek-v4-pro"}}}}),
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
    assert_eq!(session.model_provider.as_deref(), Some("custom"));
    assert_eq!(session.model.as_deref(), Some("deepseek-v4-pro"));
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
fn parses_codex_custom_tool_calls_into_portable_calls() {
    let fixture = [
        json!({
            "timestamp":"2026-01-01T00:00:00.000Z",
            "type":"session_meta",
            "payload":{
                "session_id":"s-custom",
                "cwd":r"C:\proj",
                "timestamp":"2026-01-01T00:00:00.000Z"
            }
        }),
        json!({
            "timestamp":"2026-01-01T00:00:01.000Z",
            "type":"response_item",
            "payload":{
                "type":"message",
                "role":"assistant",
                "content":[{"type":"output_text","text":"I will inspect the project."}]
            }
        }),
        json!({
            "timestamp":"2026-01-01T00:00:02.000Z",
            "type":"response_item",
            "payload":{
                "type":"custom_tool_call",
                "id":"ctc-1",
                "call_id":"call-custom-1",
                "name":"exec",
                "input":"{\"cmd\":\"pwd\"}",
                "status":"completed"
            }
        }),
        json!({
            "timestamp":"2026-01-01T00:00:03.000Z",
            "type":"response_item",
            "payload":{
                "type":"custom_tool_call_output",
                "call_id":"call-custom-1",
                "output":"C:\\proj"
            }
        }),
    ]
    .into_iter()
    .map(|value| value.to_string())
    .collect::<Vec<_>>()
    .join("\n");

    let session = codex::parse_session(&fixture).unwrap();

    assert_eq!(session.messages.len(), 1);
    assert_eq!(session.messages[0].tool_calls.len(), 1);
    assert_eq!(session.messages[0].tool_calls[0].id, "call-custom-1");
    assert_eq!(session.messages[0].tool_calls[0].name, "exec");
    assert_eq!(
        session.messages[0].tool_calls[0].input,
        json!({"cmd":"pwd"})
    );
    assert_eq!(
        session.messages[0].tool_calls[0].output.as_deref(),
        Some("C:\\proj")
    );

    let report = session.portability_report(SourceTool::Claude);
    assert!(report
        .preserved
        .iter()
        .any(|item| item.contains("工具名称")));
    assert!(report.is_degraded());
}

#[test]
fn codex_custom_tool_history_can_be_written_to_other_tool_formats() {
    let fixture = [
        json!({
            "timestamp":"2026-01-01T00:00:00.000Z",
            "type":"session_meta",
            "payload":{
                "session_id":"s-custom",
                "cwd":r"C:\proj",
                "timestamp":"2026-01-01T00:00:00.000Z"
            }
        }),
        json!({
            "timestamp":"2026-01-01T00:00:01.000Z",
            "type":"response_item",
            "payload":{
                "type":"message",
                "role":"assistant",
                "content":[{"type":"output_text","text":"done"}]
            }
        }),
        json!({
            "timestamp":"2026-01-01T00:00:02.000Z",
            "type":"response_item",
            "payload":{
                "type":"custom_tool_call",
                "id":"ctc-1",
                "call_id":"call-custom-1",
                "name":"exec",
                "input":{"cmd":"pwd"}
            }
        }),
        json!({
            "timestamp":"2026-01-01T00:00:03.000Z",
            "type":"response_item",
            "payload":{
                "type":"custom_tool_call_output",
                "call_id":"call-custom-1",
                "output":"C:\\proj"
            }
        }),
    ]
    .into_iter()
    .map(|value| value.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    let session = codex::parse_session(&fixture).unwrap();

    let claude_root = tempfile::tempdir().unwrap();
    let claude_result = claude::write_session_to_root(&session, claude_root.path()).unwrap();
    let claude = claude::parse_session_file(&claude_result.session_file).unwrap();
    assert_eq!(claude.messages[0].tool_calls[0].name, "exec");
    assert_eq!(claude.messages[0].tool_calls[0].input, json!({"cmd":"pwd"}));
    assert_eq!(
        claude.messages[0].tool_calls[0].output.as_deref(),
        Some("C:\\proj")
    );

    let pi_root = tempfile::tempdir().unwrap();
    let pi_result = pi::write_session_to_root(&session, pi_root.path()).unwrap();
    let pi = pi::parse_session(&std::fs::read_to_string(&pi_result.session_file).unwrap()).unwrap();
    assert_eq!(pi.messages[0].tool_calls[0].name, "exec");
    assert_eq!(pi.messages[0].tool_calls[0].input, json!({"cmd":"pwd"}));
    assert_eq!(
        pi.messages[0].tool_calls[0].output.as_deref(),
        Some("C:\\proj")
    );

    let opencode_root = tempfile::tempdir().unwrap();
    let opencode_db = opencode_root.path().join("opencode.db");
    opencode::init_database(&opencode_db).unwrap();
    opencode::write_session_to_database(&session, &opencode_db).unwrap();
    let opencode = opencode::parse_sessions(&opencode_db).unwrap();
    assert_eq!(opencode[0].messages[0].tool_calls[0].name, "exec");
    assert_eq!(
        opencode[0].messages[0].tool_calls[0].input,
        json!({"cmd":"pwd"})
    );
    assert_eq!(
        opencode[0].messages[0].tool_calls[0].output.as_deref(),
        Some("C:\\proj")
    );

    let dsh_home = tempfile::tempdir().unwrap();
    let _dsh_home_guard = common::isolate_dsh_home(dsh_home.path());
    let dsh_root = tempfile::tempdir().unwrap();
    let dsh_result = dsh::write_session_to_root(&session, dsh_root.path()).unwrap();
    let dsh = dsh::parse_session_file(&dsh_result.log_file)
        .unwrap()
        .unwrap();
    assert_eq!(dsh.messages[0].tool_calls[0].name, "exec");
    assert_eq!(dsh.messages[0].tool_calls[0].input, json!({"cmd":"pwd"}));
    assert_eq!(
        dsh.messages[0].tool_calls[0].output.as_deref(),
        Some("C:\\proj")
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
    assert_eq!(records[0]["payload"]["model_provider"], "custom");
    assert_eq!(records[0]["payload"]["cli_version"], "0.147.0");
    assert!(records[0]["payload"].get("base_instructions").is_some());
    assert!(records
        .iter()
        .any(|record| record["payload"]["type"] == "function_call"));
    assert!(records
        .iter()
        .any(|record| record["payload"]["type"] == "function_call_output"));
    assert!(records
        .iter()
        .any(|record| record["payload"]["type"] == "item_completed"));
}

#[test]
fn writes_codex_session_with_configured_provider_without_model_pinning() {
    // Restored sessions identify the target provider so current Codex versions can
    // resume them, while still leaving model and base instructions unpinned.
    let mut session = sample_session(SourceTool::Codex);
    session.model_provider = Some("cpa-gui".to_string());
    session.model = Some("some-model".to_string());

    let output = codex::write_session(&session).unwrap();
    let record: Value = serde_json::from_str(output.lines().next().unwrap()).unwrap();

    assert_eq!(record["payload"]["model_provider"], "cpa-gui");
    assert!(record["payload"].get("base_instructions").is_some());
    assert_eq!(record["payload"]["cli_version"], "0.147.0");
}

#[test]
fn registers_codex_thread_with_preview() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("state_5.sqlite");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE threads (
                id TEXT PRIMARY KEY,
                rollout_path TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                source TEXT NOT NULL,
                model_provider TEXT NOT NULL,
                cwd TEXT NOT NULL,
                title TEXT NOT NULL,
                sandbox_policy TEXT NOT NULL,
                approval_mode TEXT NOT NULL,
                has_user_event INTEGER NOT NULL,
                archived INTEGER NOT NULL,
                cli_version TEXT NOT NULL,
                first_user_message TEXT NOT NULL,
                memory_mode TEXT NOT NULL,
                model TEXT,
                thread_source TEXT,
                preview TEXT NOT NULL,
                created_at_ms INTEGER,
                updated_at_ms INTEGER,
                recency_at INTEGER NOT NULL,
                recency_at_ms INTEGER NOT NULL,
                history_mode TEXT NOT NULL,
                is_pinned INTEGER NOT NULL
            )",
        )
        .unwrap();

    let mut session = sample_session(SourceTool::Codex);
    session.cwd = r"F:\proj".to_string();
    session.model_provider = Some("cpa-gui".to_string());
    session.model = Some("gpt-5.6-luna".to_string());
    let rollout = temp.path().join("rollout-test.jsonl");

    codex::register_thread(&session, "s1", &rollout, temp.path()).unwrap();

    let row = connection
        .query_row(
            "SELECT model_provider, cwd, title, first_user_message, preview
             FROM threads WHERE id = 's1'",
            [],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .unwrap();

    assert_eq!(row.0, "cpa-gui");
    assert_eq!(row.1, r"\\?\F:\proj");
    assert_eq!(row.2, "hello");
    assert_eq!(row.3, "hello");
    assert_eq!(row.4, "hello");
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
fn claude_file_recovers_project_from_encoded_directory() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("projects").join("C--Users-Administrator");
    std::fs::create_dir_all(&project).unwrap();
    let file = project.join("session-from-file.jsonl");
    std::fs::write(
        &file,
        json!({
            "type": "user",
            "message": { "role": "user", "content": [{ "type": "text", "text": "hello" }] },
            "timestamp": "2026-01-01T00:00:00.000Z"
        })
        .to_string()
            + "\n",
    )
    .unwrap();

    let session = claude::parse_session_file(&file).unwrap();
    assert_eq!(session.session_id, "session-from-file");
    assert_eq!(session.cwd, r"C:\Users\Administrator");
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

#[test]
fn pi_jsonl_round_trips_messages_and_tool_calls() {
    let fixture = [
        json!({"type":"session","version":3,"id":"pi1","timestamp":"2026-01-01T00:00:00.000Z","cwd":r"C:\\proj"}),
        json!({"type":"model_change","id":"m1","parentId":null,"timestamp":"2026-01-01T00:00:00.000Z","provider":"custom","modelId":"deepseek-v4-pro"}),
        json!({"type":"message","id":"u1","parentId":"m1","timestamp":"2026-01-01T00:00:01.000Z","message":{"role":"user","content":"hello","timestamp":1767225601000_i64}}),
        json!({"type":"message","id":"a1","parentId":"u1","timestamp":"2026-01-01T00:00:02.000Z","message":{"role":"assistant","content":[{"type":"text","text":"running"},{"type":"toolCall","id":"c1","name":"shell","arguments":{"cmd":"ls"}}],"api":"custom","provider":"custom","model":"deepseek-v4-pro","usage":{},"stopReason":"toolUse","timestamp":1767225602000_i64}}),
        json!({"type":"message","id":"t1","parentId":"a1","timestamp":"2026-01-01T00:00:03.000Z","message":{"role":"toolResult","toolCallId":"c1","toolName":"shell","content":[{"type":"text","text":"file.txt"}],"isError":false,"timestamp":1767225603000_i64}}),
    ]
    .into_iter()
    .map(|value| value.to_string())
    .collect::<Vec<_>>()
    .join("\n");

    let parsed = pi::parse_session(&fixture).unwrap();
    assert_eq!(parsed.source_tool, SourceTool::Pi);
    assert_eq!(parsed.session_id, "pi1");
    assert_eq!(parsed.cwd, r"C:\\proj");
    assert_eq!(parsed.model_provider.as_deref(), Some("custom"));
    assert_eq!(parsed.model.as_deref(), Some("deepseek-v4-pro"));
    assert_eq!(parsed.messages.len(), 2);
    assert_eq!(parsed.messages[1].text, "running");
    assert_eq!(parsed.messages[1].tool_calls[0].input, json!({"cmd":"ls"}));
    assert_eq!(
        parsed.messages[1].tool_calls[0].output.as_deref(),
        Some("file.txt")
    );

    let temp = tempfile::tempdir().unwrap();
    let result = pi::write_session_to_root(&sample_session(SourceTool::Pi), temp.path()).unwrap();
    assert!(result
        .session_file
        .to_string_lossy()
        .contains("--C--proj--"));
    let restored =
        pi::parse_session(&std::fs::read_to_string(&result.session_file).unwrap()).unwrap();
    assert_eq!(restored.source_tool, SourceTool::Pi);
    assert_eq!(restored.session_id, result.session_id);
    assert_eq!(restored.messages, sample_session(SourceTool::Pi).messages);
}

#[test]
fn opencode_database_round_trips_messages_and_tool_calls() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("opencode.db");
    let mut session = sample_session(SourceTool::OpenCode);
    session.cwd = "F:/proj".to_string();

    opencode::init_database(&database).unwrap();
    let result = opencode::write_session_to_database(&session, &database).unwrap();
    assert!(result.session_id.starts_with("ses_"));

    let sessions = opencode::parse_sessions(&database).unwrap();
    assert_eq!(sessions.len(), 1);
    let restored = &sessions[0];
    assert_eq!(restored.source_tool, SourceTool::OpenCode);
    assert_eq!(restored.session_id, result.session_id);
    assert_eq!(restored.cwd, "F:/proj");
    assert_eq!(restored.created_at, "2026-01-01T00:00:00.000Z");
    assert_eq!(restored.messages.len(), 2);
    assert_eq!(restored.messages[0].text, "hello");
    assert_eq!(restored.messages[1].text, "running");
    assert_eq!(restored.messages[1].tool_calls.len(), 1);
    assert_eq!(restored.messages[1].tool_calls[0].name, "shell");
    assert_eq!(
        restored.messages[1].tool_calls[0].input,
        json!({"cmd": "ls"})
    );
    assert_eq!(
        restored.messages[1].tool_calls[0].output.as_deref(),
        Some("file.txt")
    );
}

#[test]
fn parses_opencode_database_rows() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("opencode.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE session (
                id TEXT PRIMARY KEY, project_id TEXT NOT NULL, slug TEXT NOT NULL,
                directory TEXT NOT NULL, title TEXT NOT NULL, version TEXT NOT NULL,
                cost REAL NOT NULL DEFAULT 0, tokens_input INTEGER NOT NULL DEFAULT 0,
                tokens_output INTEGER NOT NULL DEFAULT 0, tokens_reasoning INTEGER NOT NULL DEFAULT 0,
                tokens_cache_read INTEGER NOT NULL DEFAULT 0, tokens_cache_write INTEGER NOT NULL DEFAULT 0,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL);
             CREATE TABLE message (
                id TEXT PRIMARY KEY, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);
             CREATE TABLE part (
                id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL);",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session VALUES ('ses_1', 'global', 'brave-cabin', 'C:/proj', 'title',
              'v1', 0, 0, 0, 0, 0, 0, 1767225600000, 1767225602000)",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO message VALUES ('msg_1', 'ses_1', 1767225600000, 1767225600000, ?)",
            params![json!({"role": "user", "time": {"created": 1767225600000_i64}}).to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO message VALUES ('msg_2', 'ses_1', 1767225601000, 1767225601000, ?)",
            params![
                json!({"role": "assistant", "time": {"created": 1767225601000_i64}}).to_string()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO part VALUES ('prt_1', 'msg_1', 'ses_1', 1767225600000, 1767225600000, ?)",
            params![json!({"type": "text", "text": "hello"}).to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO part VALUES ('prt_2', 'msg_2', 'ses_1', 1767225601000, 1767225601000, ?)",
            params![json!({
                "type": "tool", "callID": "c1", "tool": "shell",
                "state": { "status": "completed", "input": {"cmd": "ls"}, "output": "file.txt" }
            })
            .to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO part VALUES ('prt_3', 'msg_2', 'ses_1', 1767225601000, 1767225601000, ?)",
            params![json!({"type": "reasoning", "text": "thinking"}).to_string()],
        )
        .unwrap();
    drop(connection);

    let sessions = opencode::parse_sessions(&database).unwrap();
    assert_eq!(sessions.len(), 1);
    let session = &sessions[0];
    assert_eq!(session.session_id, "ses_1");
    assert_eq!(session.cwd, "C:/proj");
    assert_eq!(session.created_at, "2026-01-01T00:00:00.000Z");
    assert_eq!(session.updated_at, "2026-01-01T00:00:02.000Z");
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[0].text, "hello");
    assert_eq!(session.messages[1].text, "");
    assert_eq!(session.messages[1].tool_calls[0].name, "shell");
    assert_eq!(
        session.messages[1].tool_calls[0].output.as_deref(),
        Some("file.txt")
    );
}

#[test]
fn parses_nothing_from_a_missing_opencode_database() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = opencode::parse_sessions(&temp.path().join("opencode.db")).unwrap();
    assert!(sessions.is_empty());
}
#[test]
fn dsh_log_round_trips_messages_and_tool_calls() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let mut session = sample_session(SourceTool::Pi);
    session.cwd = "F:/proj".to_string();
    let _dsh_home_guard = common::isolate_dsh_home(temp.path());

    let result = dsh::write_session_to_root(&session, &root).unwrap();
    assert!(result.session_id.starts_with("session-"));
    assert!(result.log_file.to_string_lossy().ends_with(".jsonl.zstd"));
    let workspace: Value = serde_json::from_str(
        &std::fs::read_to_string(temp.path().join("storages").join("workspace.json")).unwrap(),
    )
    .unwrap();
    let workspace_entry = workspace["tables"]["workspaces"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    assert_eq!(workspace_entry["path"], "F:/proj");
    assert!(workspace_entry["sessionIds"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value.as_str() == Some(&result.session_id)));

    let parsed = dsh::parse_session_file(&result.log_file).unwrap().unwrap();
    assert_eq!(parsed.source_tool, SourceTool::Dsh);
    assert_eq!(parsed.session_id, result.session_id);
    assert_eq!(parsed.cwd, "F:/proj");
    assert_eq!(parsed.messages.len(), 2);
    assert_eq!(parsed.messages[0].text, "hello");
    assert_eq!(parsed.messages[1].text, "running");
    assert_eq!(parsed.messages[1].tool_calls[0].name, "shell");
    assert_eq!(parsed.messages[1].tool_calls[0].input, json!({"cmd": "ls"}));
    assert_eq!(
        parsed.messages[1].tool_calls[0].output.as_deref(),
        Some("file.txt")
    );
}

#[test]
fn parses_dsh_session_logs() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("session.jsonl");
    let lines = [
        json!({"type":"session","version":0,"id":"session-abc","createdAt":1767225600000_i64,"cwd":"C:/proj","delegationDepth":0}),
        json!({"type":"permission/preset","seq":0,"time":1767225600001_i64,"data":{"preset":"workspace-write"}}),
        json!({"type":"turn/start","seq":1,"time":1767225600002_i64,"data":{"turn":1}}),
        json!({"type":"user/message","seq":2,"time":1767225600003_i64,"data":{"id":"m1","role":"user","content":[{"type":"text","text":"hello"}],"source":{"kind":"user"}},"surfaceOp":"append"}),
        json!({"type":"user/message","seq":3,"time":1767225600004_i64,"data":{"id":"m2","role":"user","content":[{"type":"text","text":"injected context"}],"source":{"kind":"plugin","plugin":"x"}},"surfaceOp":"append"}),
        json!({"type":"assistant/message","seq":4,"time":1767225600005_i64,"data":{"turn":1,"step":1,"message":{"id":"m3","role":"assistant","content":[{"type":"reasoning","text":"think"},{"type":"text","text":"running"}],"source":{"kind":"model","provider":"deepseek","model":"chat"}}},"surfaceOp":"append"}),
        json!({"type":"tool/call","seq":5,"time":1767225600006_i64,"data":{"turn":1,"step":1,"callId":"tool_1","name":"shell","arguments":"{\"cmd\":\"ls\"}"}}),
        json!({"type":"tool/result","seq":6,"time":1767225600007_i64,"data":{"turn":1,"step":1,"message":{"id":"m4","role":"user","content":[{"type":"tool-result","toolCallId":"tool_1","content":[{"type":"text","text":"file.txt"}]}],"source":{"kind":"tool","callId":"tool_1"}}},"surfaceOp":"append"}),
        json!({"type":"assistant/message","seq":7,"time":1767225600008_i64,"data":{"turn":1,"step":1,"message":{"id":"m5","role":"assistant","content":[{"type":"text","text":"compacted"}]}},"surfaceOp":{"op":"replace","start":0,"end":3}}),
        json!({"type":"step/end","seq":8,"time":1767225600009_i64,"data":{"turn":1,"step":1}}),
        json!({"type":"turn/end","seq":9,"time":1767225600010_i64,"data":{"turn":1,"reason":{"kind":"completed"}}}),
        json!({"type":"text-chunks","seq":10,"time":1767225600011_i64,"data":{"turn":1,"step":1,"index":0,"chunks":["a","b"]}}),
    ];
    let content = lines
        .iter()
        .map(|line| line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    std::fs::write(&file, content).unwrap();

    let session = dsh::parse_session_file(&file).unwrap().unwrap();
    assert_eq!(session.source_tool, SourceTool::Dsh);
    assert_eq!(session.session_id, "session-abc");
    assert_eq!(session.cwd, "C:/proj");
    assert_eq!(session.created_at, "2026-01-01T00:00:00.000Z");
    assert_eq!(session.updated_at, "2026-01-01T00:00:00.011Z");
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[0].text, "hello");
    assert_eq!(session.messages[1].text, "running");
    assert_eq!(
        session.messages[1].tool_calls[0].input,
        json!({"cmd": "ls"})
    );
    assert_eq!(
        session.messages[1].tool_calls[0].output.as_deref(),
        Some("file.txt")
    );
}

#[test]
fn dsh_skips_subagent_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("session.jsonl");
    std::fs::write(
        &file,
        json!({"type":"session","version":0,"id":"session-sub","createdAt":1767225600000_i64,"cwd":"C:/proj","origin":"subagent","delegationDepth":1})
            .to_string()
            + "\n",
    )
    .unwrap();

    assert!(dsh::parse_session_file(&file).unwrap().is_none());
}
#[test]
fn dsh_parse_tolerates_a_torn_final_frame() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let _dsh_home_guard = common::isolate_dsh_home(temp.path());
    let mut session = sample_session(SourceTool::Dsh);
    session.messages.push(Message {
        role: Role::User,
        text: "second turn".to_string(),
        tool_calls: vec![],
    });
    let result = dsh::write_session_to_root(&session, &root).unwrap();
    let mut bytes = std::fs::read(&result.log_file).unwrap();
    // Simulate a live append interrupted mid-frame: truncate the tail frame.
    bytes.truncate(bytes.len() - 6);
    std::fs::write(&result.log_file, bytes).unwrap();

    let parsed = dsh::parse_session_file(&result.log_file).unwrap().unwrap();
    assert_eq!(parsed.session_id, result.session_id);
    // The first turn's frame survives; the torn final turn is dropped.
    assert_eq!(parsed.messages.len(), 2);
    assert_eq!(parsed.messages[0].text, "hello");
    assert_eq!(parsed.messages[1].text, "running");
}
#[test]
fn dsh_restore_embeds_tool_call_blocks_in_assistant_messages() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("sessions");
    let _dsh_home_guard = common::isolate_dsh_home(temp.path());
    let result = dsh::write_session_to_root(&sample_session(SourceTool::Dsh), &root).unwrap();

    let bytes = std::fs::read(&result.log_file).unwrap();
    let text = String::from_utf8(zstd::stream::decode_all(bytes.as_slice()).unwrap()).unwrap();
    let events = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();

    let assistant = events
        .iter()
        .find(|event| event["type"] == "assistant/message")
        .unwrap();
    let content = assistant["data"]["message"]["content"].as_array().unwrap();
    let block = content
        .iter()
        .find(|block| block["type"] == "tool-call")
        .expect("assistant message must carry a tool-call block");
    assert_eq!(block["id"], "c1");
    assert_eq!(block["name"], "shell");
    assert_eq!(block["arguments"], json!({"cmd": "ls"}).to_string());

    let call = events
        .iter()
        .find(|event| event["type"] == "tool/call")
        .unwrap();
    let tool_result = events
        .iter()
        .find(|event| event["type"] == "tool/result")
        .unwrap();
    assert_eq!(call["data"]["callId"], "c1");
    assert_eq!(
        tool_result["data"]["message"]["content"][0]["toolCallId"],
        "c1"
    );
}
