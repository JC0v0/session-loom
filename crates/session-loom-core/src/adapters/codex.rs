use super::{attach_tool_outputs, json_argument, json_text, push_tool_call};
use crate::canonical::{
    CanonicalSession, Message, Role, SourceTool, ToolCall, CANONICAL_SCHEMA_VERSION,
};
use serde_json::{json, Value};
use std::collections::HashMap;

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
        let record_type = record.get("type").and_then(Value::as_str);
        let timestamp = record
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or_default();

        if record_type == Some("session_meta") {
            let payload = record.get("payload").unwrap_or(&Value::Null);
            session_id = payload
                .get("session_id")
                .or_else(|| payload.get("id"))
                .and_then(Value::as_str)
                .unwrap_or(&session_id)
                .to_string();
            cwd = payload
                .get("cwd")
                .and_then(Value::as_str)
                .unwrap_or(&cwd)
                .to_string();
            created_at = payload
                .get("timestamp")
                .and_then(Value::as_str)
                .unwrap_or(&created_at)
                .to_string();
            if !timestamp.is_empty() {
                updated_at = timestamp.to_string();
            }
            continue;
        }

        if record_type != Some("response_item") {
            continue;
        }
        let Some(item) = record.get("payload") else {
            continue;
        };
        if !timestamp.is_empty() {
            updated_at = timestamp.to_string();
        }

        match item.get("type").and_then(Value::as_str) {
            Some("function_call_output") => {
                let call_id = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                outputs.insert(call_id, json_text(item.get("output")));
            }
            Some("function_call") => {
                push_tool_call(
                    &mut messages,
                    ToolCall {
                        id: item
                            .get("call_id")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        name: item
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        input: json_argument(item.get("arguments")),
                        output: None,
                    },
                );
            }
            Some("message") => {
                let role = match item.get("role").and_then(Value::as_str) {
                    Some("user") => Role::User,
                    Some("assistant") => Role::Assistant,
                    _ => continue,
                };
                let text = item
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|block| {
                        matches!(
                            block.get("type").and_then(Value::as_str),
                            Some("input_text" | "output_text")
                        )
                    })
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .collect::<String>();
                if role == Role::User && text.trim_start().starts_with('<') {
                    continue;
                }
                messages.push(Message {
                    role,
                    text,
                    tool_calls: vec![],
                });
            }
            _ => {}
        }
    }

    attach_tool_outputs(&mut messages, &outputs);
    Ok(CanonicalSession {
        schema_version: CANONICAL_SCHEMA_VERSION,
        source_tool: SourceTool::Codex,
        session_id,
        cwd,
        created_at,
        updated_at,
        messages,
    })
}

pub fn write_session(session: &CanonicalSession) -> Result<String, String> {
    let mut records = vec![json!({
        "timestamp": session.updated_at,
        "type": "session_meta",
        "payload": {
            "session_id": session.session_id,
            "id": session.session_id,
            "timestamp": session.created_at,
            "cwd": session.cwd,
            "originator": "codex-tui",
            "source": "cli",
            "thread_source": "user",
            "cli_version": "session-loom",
            "history_mode": "legacy"
        }
    })];

    for message in &session.messages {
        records.push(json!({
            "timestamp": session.updated_at,
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": match message.role { Role::User => "user", Role::Assistant => "assistant" },
                "content": [{
                    "type": match message.role { Role::User => "input_text", Role::Assistant => "output_text" },
                    "text": message.text
                }]
            }
        }));
        for call in &message.tool_calls {
            let arguments = match &call.input {
                Value::String(value) => value.clone(),
                value => serde_json::to_string(value).map_err(|error| error.to_string())?,
            };
            records.push(json!({
                "timestamp": session.updated_at,
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "call_id": call.id,
                    "name": call.name,
                    "arguments": arguments
                }
            }));
            if let Some(output) = &call.output {
                records.push(json!({
                    "timestamp": session.updated_at,
                    "type": "response_item",
                    "payload": {
                        "type": "function_call_output",
                        "call_id": call.id,
                        "output": output
                    }
                }));
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
