use super::{attach_tool_outputs, encode_claude_project, json_text};
use crate::canonical::{
    CanonicalSession, Message, Role, SourceTool, ToolCall, CANONICAL_SCHEMA_VERSION,
};
use chrono::DateTime;
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeWriteResult {
    pub session_file: PathBuf,
    pub session_id: String,
}

/// Parses a Claude session file and recovers its project from the encoded
/// project directory when older records do not carry a `cwd` field.
pub fn parse_session_file(path: &Path) -> Result<CanonicalSession, String> {
    let payload = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let mut session = parse_session(&payload)?;
    if session.cwd.is_empty() {
        if let Some(project) = path
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
        {
            session.cwd = super::decode_claude_project(project);
        }
    }
    if session.session_id.is_empty() {
        if let Some(session_id) = path.file_stem().and_then(|name| name.to_str()) {
            session.session_id = session_id.to_string();
        }
    }
    Ok(session)
}

pub fn parse_session(jsonl: &str) -> Result<CanonicalSession, String> {
    let mut messages = vec![];
    let mut outputs = HashMap::new();
    let mut session_id = String::new();
    let mut cwd = String::new();
    let mut created_at = String::new();
    let mut updated_at = String::new();

    for line in jsonl.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(value) = record.get("sessionId").and_then(Value::as_str) {
            session_id = value.to_string();
        }
        if let Some(value) = record.get("cwd").and_then(Value::as_str) {
            cwd = value.to_string();
        }
        if let Some(value) = record.get("timestamp").and_then(Value::as_str) {
            if created_at.is_empty() {
                created_at = value.to_string();
            }
            updated_at = value.to_string();
        }

        let record_type = record.get("type").and_then(Value::as_str);
        if !matches!(record_type, Some("user" | "assistant")) {
            continue;
        }
        let Some(content) = record
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        let role = if record
            .get("message")
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str)
            == Some("assistant")
        {
            Role::Assistant
        } else {
            Role::User
        };
        let mut message = Message {
            role,
            text: String::new(),
            tool_calls: vec![],
        };

        for block in content {
            match block.get("type").and_then(Value::as_str) {
                Some("text") => message.text.push_str(
                    block
                        .get("text")
                        .and_then(Value::as_str)
                        .unwrap_or_default(),
                ),
                Some("tool_use") => message.tool_calls.push(ToolCall {
                    id: block
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    name: block
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                    input: block.get("input").cloned().unwrap_or(Value::Null),
                    output: None,
                }),
                Some("tool_result") => {
                    let call_id = block
                        .get("tool_use_id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    outputs.insert(call_id, json_text(block.get("content")));
                }
                _ => {}
            }
        }
        if !message.text.is_empty() || !message.tool_calls.is_empty() {
            messages.push(message);
        }
    }

    attach_tool_outputs(&mut messages, &outputs);
    Ok(CanonicalSession {
        schema_version: CANONICAL_SCHEMA_VERSION,
        source_tool: SourceTool::Claude,
        session_id,
        cwd,
        created_at,
        updated_at,
        model_provider: None,
        model: None,
        messages,
    })
}

pub fn write_session_to_root(
    session: &CanonicalSession,
    root: &Path,
) -> Result<ClaudeWriteResult, String> {
    let session_id = Uuid::new_v4().to_string();
    let project = encode_claude_project(&session.cwd);
    let directory = root.join("projects").join(project);
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
    let session_file = directory.join(format!("{session_id}.jsonl"));
    let output = render_session(session, &session_id)?;
    fs::write(&session_file, output).map_err(|error| error.to_string())?;
    if let Err(error) = append_history(root, session, &session_id) {
        return match fs::remove_file(&session_file) {
            Ok(()) => Err(error),
            Err(cleanup_error) => Err(format!(
                "{error}; failed to remove incomplete session {}: {cleanup_error}",
                session_file.display()
            )),
        };
    }
    Ok(ClaudeWriteResult {
        session_file,
        session_id,
    })
}

fn render_session(session: &CanonicalSession, session_id: &str) -> Result<String, String> {
    let mut records = vec![json!({ "type": "mode", "mode": "normal", "sessionId": session_id })];
    for message in &session.messages {
        let mut content = vec![];
        if !message.text.is_empty() {
            content.push(json!({ "type": "text", "text": message.text }));
        }
        for call in &message.tool_calls {
            content.push(json!({
                "type": "tool_use",
                "id": call.id,
                "name": call.name,
                "input": call.input
            }));
        }
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        records.push(json!({
            "type": role,
            "message": { "role": role, "content": content },
            "sessionId": session_id,
            "cwd": session.cwd,
            "timestamp": session.updated_at
        }));
        if message.role == Role::Assistant {
            for call in &message.tool_calls {
                if let Some(output) = &call.output {
                    records.push(json!({
                        "type": "user",
                        "message": {
                            "role": "user",
                            "content": [{
                                "type": "tool_result",
                                "tool_use_id": call.id,
                                "content": output
                            }]
                        },
                        "sessionId": session_id,
                        "cwd": session.cwd,
                        "timestamp": session.updated_at
                    }));
                }
            }
        }
    }
    let mut output = records
        .into_iter()
        .map(|record| serde_json::to_string(&record).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    output.push('\n');
    Ok(output)
}

fn append_history(root: &Path, session: &CanonicalSession, session_id: &str) -> Result<(), String> {
    let display = session
        .messages
        .iter()
        .find(|message| message.role == Role::User)
        .map(|message| message.text.chars().take(200).collect::<String>())
        .unwrap_or_default();
    let timestamp = DateTime::parse_from_rfc3339(&session.created_at)
        .map(|value| value.timestamp_millis())
        .unwrap_or_else(|_| chrono::Utc::now().timestamp_millis());
    let entry = json!({
        "display": display,
        "pastedContents": {},
        "timestamp": timestamp,
        "project": session.cwd,
        "sessionId": session_id
    });
    let mut history = OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("history.jsonl"))
        .map_err(|error| error.to_string())?;
    writeln!(
        history,
        "{}",
        serde_json::to_string(&entry).map_err(|error| error.to_string())?
    )
    .map_err(|error| error.to_string())
}
