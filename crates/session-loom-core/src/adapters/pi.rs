use super::{attach_tool_outputs, json_argument};
use crate::canonical::{
    CanonicalSession, Message, Role, SourceTool, ToolCall, CANONICAL_SCHEMA_VERSION,
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const SESSION_VERSION: u64 = 3;
const DEFAULT_API: &str = "session-loom";
const DEFAULT_PROVIDER: &str = "session-loom";
const DEFAULT_MODEL: &str = "unknown";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PiWriteResult {
    pub session_file: PathBuf,
    pub session_id: String,
}

/// Parses the active branch of a Pi session into the canonical conversation.
/// Pi stores branches and control entries in the same JSONL file; canonical
/// sessions are linear, so only the current leaf path is materialized.
pub fn parse_session(jsonl: &str) -> Result<CanonicalSession, String> {
    let entries = jsonl
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect::<Vec<_>>();
    let header = entries
        .first()
        .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("session"))
        .ok_or_else(|| "invalid Pi session header".to_string())?;

    let session_id = header
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if session_id.is_empty() {
        return Err("Pi session header has no id".to_string());
    }
    let cwd = header
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let created_at = header
        .get("timestamp")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    let mut by_id = HashMap::new();
    let mut ordered_ids = vec![];
    let mut updated_at = created_at.clone();
    for entry in entries.iter().skip(1) {
        let Some(id) = entry.get("id").and_then(Value::as_str) else {
            continue;
        };
        if id.is_empty() {
            continue;
        }
        by_id.insert(id.to_string(), entry.clone());
        ordered_ids.push(id.to_string());
        if let Some(timestamp) = entry.get("timestamp").and_then(Value::as_str) {
            updated_at = timestamp.to_string();
        }
    }

    let mut path = vec![];
    let mut current = ordered_ids.last().cloned();
    let mut visited = HashSet::new();
    while let Some(id) = current {
        if !visited.insert(id.clone()) {
            break;
        }
        let Some(entry) = by_id.get(&id) else {
            break;
        };
        path.push(entry.clone());
        current = entry
            .get("parentId")
            .and_then(Value::as_str)
            .map(str::to_string);
    }
    path.reverse();

    let selected = active_entries(&path);
    let mut messages = vec![];
    let mut outputs = HashMap::new();
    let mut model_provider = None;
    let mut model = None;

    for entry in &selected {
        match entry.get("type").and_then(Value::as_str) {
            Some("message") => {
                let message = entry.get("message").unwrap_or(&Value::Null);
                update_model(message, &mut model_provider, &mut model);
                append_agent_message(message, &mut messages, &mut outputs);
            }
            Some("retained_message") => {
                let message = entry.get("message").unwrap_or(&Value::Null);
                update_model(message, &mut model_provider, &mut model);
                append_agent_message(message, &mut messages, &mut outputs);
            }
            Some("model_change") => {
                model_provider = entry
                    .get("provider")
                    .and_then(Value::as_str)
                    .map(str::to_string);
                model = entry
                    .get("modelId")
                    .and_then(Value::as_str)
                    .map(str::to_string);
            }
            Some("compaction") => {
                if let Some(summary) = entry.get("summary").and_then(Value::as_str) {
                    if !summary.is_empty() {
                        messages.push(Message {
                            role: Role::Assistant,
                            text: format!("[Pi compaction summary]\n{summary}"),
                            tool_calls: vec![],
                        });
                    }
                }
            }
            Some("branch_summary") => {
                if let Some(summary) = entry.get("summary").and_then(Value::as_str) {
                    if !summary.is_empty() {
                        messages.push(Message {
                            role: Role::Assistant,
                            text: format!("[Pi branch summary]\n{summary}"),
                            tool_calls: vec![],
                        });
                    }
                }
            }
            Some("bashExecution") => append_bash_execution(entry, &mut messages),
            _ => {}
        }
    }

    attach_tool_outputs(&mut messages, &outputs);
    Ok(CanonicalSession {
        schema_version: CANONICAL_SCHEMA_VERSION,
        source_tool: SourceTool::Pi,
        session_id,
        cwd,
        created_at,
        updated_at,
        model_provider,
        model,
        messages,
    })
}

/// Writes a canonical session into Pi's native JSONL format. The resulting
/// file is placed under the cwd project directory by default, or directly in
/// the configured PI_CODING_AGENT_SESSION_DIR when that override is active.
pub fn write_session_to_root(
    session: &CanonicalSession,
    root: &Path,
) -> Result<PiWriteResult, String> {
    let session_id = Uuid::new_v4().to_string();
    let directory = if std::env::var_os("PI_CODING_AGENT_SESSION_DIR").is_some() {
        root.to_path_buf()
    } else {
        root.join(project_key(&session.cwd))
    };
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;

    let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
    let file_timestamp = timestamp.replace([':', '.'], "-");
    let session_file = directory.join(format!("{file_timestamp}_{session_id}.jsonl"));
    let payload = render_session(session, &session_id, &timestamp)?;
    fs::write(&session_file, payload).map_err(|error| error.to_string())?;
    Ok(PiWriteResult {
        session_file,
        session_id,
    })
}

fn active_entries(path: &[Value]) -> Vec<Value> {
    let Some(compaction_index) = path
        .iter()
        .rposition(|entry| entry.get("type").and_then(Value::as_str) == Some("compaction"))
    else {
        return path.to_vec();
    };

    let compaction = &path[compaction_index];
    let mut selected = vec![];
    if let Some(retained_tail) = compaction.get("retainedTail").and_then(Value::as_array) {
        selected.push(compaction.clone());
        let timestamp = compaction
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or_default();
        for message in retained_tail {
            selected.push(json!({
                "type": "retained_message",
                "message": message,
                "timestamp": timestamp
            }));
        }
        selected.extend(path.iter().skip(compaction_index + 1).cloned());
        return selected;
    }

    if let Some(first_kept_id) = compaction.get("firstKeptEntryId").and_then(Value::as_str) {
        if let Some(first_kept_index) = path
            .iter()
            .position(|entry| entry.get("id").and_then(Value::as_str) == Some(first_kept_id))
        {
            selected.extend(
                path.iter()
                    .skip(first_kept_index)
                    .take(compaction_index - first_kept_index)
                    .cloned(),
            );
        }
    }
    selected.extend(path.iter().skip(compaction_index).cloned());
    selected
}

fn update_model(message: &Value, provider: &mut Option<String>, model: &mut Option<String>) {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    if let Some(value) = message.get("provider").and_then(Value::as_str) {
        *provider = Some(value.to_string());
    }
    if let Some(value) = message.get("model").and_then(Value::as_str) {
        *model = Some(value.to_string());
    }
}

fn append_agent_message(
    message: &Value,
    messages: &mut Vec<Message>,
    outputs: &mut HashMap<String, String>,
) {
    match message.get("role").and_then(Value::as_str) {
        Some("user") => {
            let text = content_text(message.get("content"));
            if !text.is_empty() {
                messages.push(Message {
                    role: Role::User,
                    text,
                    tool_calls: vec![],
                });
            }
        }
        Some("assistant") => {
            let mut text = String::new();
            let mut tool_calls = vec![];
            if let Some(content) = message.get("content").and_then(Value::as_array) {
                for block in content {
                    match block.get("type").and_then(Value::as_str) {
                        Some("text") => text.push_str(
                            block
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default(),
                        ),
                        Some("toolCall") => tool_calls.push(ToolCall {
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
                            input: json_argument(block.get("arguments")),
                            output: None,
                        }),
                        _ => {}
                    }
                }
            } else if let Some(value) = message.get("content").and_then(Value::as_str) {
                text.push_str(value);
            }
            if !text.is_empty() || !tool_calls.is_empty() {
                messages.push(Message {
                    role: Role::Assistant,
                    text,
                    tool_calls,
                });
            }
        }
        Some("toolResult") => {
            let call_id = message
                .get("toolCallId")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !call_id.is_empty() {
                outputs.insert(call_id.to_string(), content_text(message.get("content")));
            }
        }
        _ => {}
    }
}

fn append_bash_execution(entry: &Value, messages: &mut Vec<Message>) {
    let command = entry
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let output = entry
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if command.is_empty() && output.is_empty() {
        return;
    }
    let id = entry.get("id").and_then(Value::as_str).unwrap_or("bash");
    messages.push(Message {
        role: Role::Assistant,
        text: String::new(),
        tool_calls: vec![ToolCall {
            id: format!("pi-bash-{id}"),
            name: "bash".to_string(),
            input: json!({ "command": command }),
            output: Some(output.to_string()),
        }],
    });
}

fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(value)) => value.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

fn render_session(
    session: &CanonicalSession,
    session_id: &str,
    timestamp: &str,
) -> Result<String, String> {
    let header = json!({
        "type": "session",
        "version": SESSION_VERSION,
        "id": session_id,
        "timestamp": timestamp,
        "cwd": session.cwd
    });
    let message_timestamp = parse_timestamp_ms(&session.updated_at)
        .or_else(|| parse_timestamp_ms(&session.created_at))
        .unwrap_or_else(|| Utc::now().timestamp_millis());
    let provider = session
        .model_provider
        .as_deref()
        .unwrap_or(DEFAULT_PROVIDER);
    let model = session.model.as_deref().unwrap_or(DEFAULT_MODEL);
    let mut entries = vec![header];
    let mut parent_id: Option<String> = None;
    let mut used_ids = HashSet::new();

    for message in &session.messages {
        let entry_id = next_entry_id(&mut used_ids);
        let content = match message.role {
            Role::User => json!(message.text),
            Role::Assistant => {
                let mut blocks = vec![];
                if !message.text.is_empty() {
                    blocks.push(json!({ "type": "text", "text": message.text }));
                }
                for call in &message.tool_calls {
                    blocks.push(json!({
                        "type": "toolCall",
                        "id": call.id,
                        "name": call.name,
                        "arguments": call.input
                    }));
                }
                Value::Array(blocks)
            }
        };
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
        };
        let message_value = match message.role {
            Role::User => json!({
                "role": role,
                "content": content,
                "timestamp": message_timestamp
            }),
            Role::Assistant => json!({
                "role": role,
                "content": content,
                "api": DEFAULT_API,
                "provider": provider,
                "model": model,
                "usage": zero_usage(),
                "stopReason": if message.tool_calls.is_empty() { "stop" } else { "toolUse" },
                "timestamp": message_timestamp
            }),
        };
        entries.push(json!({
            "type": "message",
            "id": entry_id,
            "parentId": parent_id,
            "timestamp": timestamp,
            "message": message_value
        }));
        parent_id = Some(entry_id);

        if message.role == Role::Assistant {
            for call in &message.tool_calls {
                let result_id = next_entry_id(&mut used_ids);
                let output = call.output.as_deref().unwrap_or("(output not captured)");
                entries.push(json!({
                    "type": "message",
                    "id": result_id,
                    "parentId": parent_id,
                    "timestamp": timestamp,
                    "message": {
                        "role": "toolResult",
                        "toolCallId": call.id,
                        "toolName": call.name,
                        "content": [{ "type": "text", "text": output }],
                        "isError": call.output.is_none(),
                        "timestamp": message_timestamp
                    }
                }));
                parent_id = Some(result_id);
            }
        }
    }

    entries
        .into_iter()
        .map(|entry| serde_json::to_string(&entry).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()
        .map(|lines| format!("{}\n", lines.join("\n")))
}

fn zero_usage() -> Value {
    json!({
        "input": 0,
        "output": 0,
        "cacheRead": 0,
        "cacheWrite": 0,
        "totalTokens": 0,
        "cost": {
            "input": 0,
            "output": 0,
            "cacheRead": 0,
            "cacheWrite": 0,
            "total": 0
        }
    })
}

fn next_entry_id(used: &mut HashSet<String>) -> String {
    loop {
        let id = Uuid::new_v4().simple().to_string()[..8].to_string();
        if used.insert(id.clone()) {
            return id;
        }
    }
}

fn project_key(cwd: &str) -> String {
    let readable = cwd
        .chars()
        .map(|character| match character {
            ':' | '\\' | '/' => '-',
            other => other,
        })
        .collect::<String>();
    let readable = readable.trim_start_matches('-');
    let readable = if readable.is_empty() {
        "root"
    } else {
        readable
    };
    format!("--{readable}--")
}

fn parse_timestamp_ms(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.timestamp_millis())
}
