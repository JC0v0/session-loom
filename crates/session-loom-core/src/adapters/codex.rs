use super::{attach_tool_outputs, json_argument, json_text, push_tool_call};
use crate::canonical::{
    CanonicalSession, Message, Role, SourceTool, ToolCall, CANONICAL_SCHEMA_VERSION,
};
use serde_json::{json, Value};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::Duration,
};
use uuid::Uuid;

pub fn parse_session(jsonl: &str) -> Result<CanonicalSession, String> {
    let mut messages = vec![];
    let mut outputs = HashMap::new();
    let mut session_id = String::new();
    let mut cwd = String::new();
    let mut created_at = String::new();
    let mut updated_at = String::new();
    let mut model_provider = None;
    let mut model = None;

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
            model_provider = payload
                .get("model_provider")
                .and_then(Value::as_str)
                .map(str::to_string);
            model = payload
                .pointer("/base_instructions/provenance/model")
                .and_then(Value::as_str)
                .map(str::to_string)
                .or_else(|| {
                    payload
                        .get("model")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                });
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
        model_provider,
        model,
        messages,
    })
}

pub fn configured_model_provider() -> Option<String> {
    let config = crate::paths::codex_home().join("config.toml");
    let contents = fs::read_to_string(config).ok()?;
    parse_config_string(&contents, "model_provider")
}

fn parse_config_string(contents: &str, key: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
            return None;
        }
        let (name, value) = line.split_once('=')?;
        if name.trim() != key {
            return None;
        }
        let value = value.split('#').next()?.trim();
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .or_else(|| {
                value
                    .strip_prefix('\'')
                    .and_then(|value| value.strip_suffix('\''))
            })?;
        (!value.is_empty()).then(|| value.to_string())
    })
}

pub fn register_thread(
    session: &CanonicalSession,
    session_id: &str,
    rollout_path: &Path,
    codex_home: &Path,
) -> Result<(), String> {
    use rusqlite::{params, Connection, OptionalExtension};

    let database = codex_home.join("state_5.sqlite");
    let connection = Connection::open(&database).map_err(|error| error.to_string())?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| error.to_string())?;

    let has_threads = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'threads'",
            [],
            |_| Ok(true),
        )
        .optional()
        .map_err(|error| error.to_string())?
        .unwrap_or(false);
    if !has_threads {
        return Ok(());
    }

    let mut columns = HashSet::new();
    {
        let mut statement = connection
            .prepare("PRAGMA table_info(threads)")
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|error| error.to_string())?;
        for name in rows {
            columns.insert(name.map_err(|error| error.to_string())?);
        }
    }
    if !columns.contains("preview") {
        return Ok(());
    }

    let first_user_message = session
        .messages
        .iter()
        .find(|message| message.role == Role::User && !message.text.trim().is_empty())
        .map(|message| message.text.trim().to_string())
        .unwrap_or_default();
    let title = first_user_message.chars().take(120).collect::<String>();
    let provider = session
        .model_provider
        .as_deref()
        .filter(|provider| !provider.is_empty())
        .unwrap_or("cpa-gui");
    let (created_at, created_at_ms) = timestamp_parts(&session.created_at);
    let (updated_at, updated_at_ms) = timestamp_parts(&session.updated_at);
    let has_user_event = i64::from(!first_user_message.is_empty());
    let model = session.model.as_deref().map(str::to_string);

    connection
        .execute(
            "INSERT OR REPLACE INTO threads (
                id, rollout_path, created_at, updated_at, source, model_provider, cwd, title,
                sandbox_policy, approval_mode, has_user_event, archived, cli_version,
                first_user_message, memory_mode, model, thread_source, preview,
                created_at_ms, updated_at_ms, recency_at, recency_at_ms, history_mode, is_pinned
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8,
                ?9, ?10, ?11, ?12, ?13,
                ?14, ?15, ?16, ?17, ?18,
                ?19, ?20, ?21, ?22, ?23, ?24
             )",
            params![
                session_id,
                rollout_path.to_string_lossy().to_string(),
                created_at,
                updated_at,
                "cli",
                provider,
                verbatim_path(&session.cwd),
                title,
                "{\"type\":\"disabled\"}",
                "never",
                has_user_event,
                0i64,
                "session-loom",
                first_user_message,
                "enabled",
                model,
                "user",
                preview_text(session),
                created_at_ms,
                updated_at_ms,
                updated_at,
                updated_at_ms,
                "legacy",
                0i64
            ],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn preview_text(session: &CanonicalSession) -> String {
    session
        .messages
        .iter()
        .find(|message| message.role == Role::User && !message.text.trim().is_empty())
        .map(|message| message.text.trim().to_string())
        .unwrap_or_default()
}

fn verbatim_path(path: &str) -> String {
    let normalized = path.replace('/', "\\");
    if normalized.starts_with("\\\\?\\") {
        return normalized;
    }
    if normalized.starts_with("\\\\") || (normalized.len() >= 2 && normalized.as_bytes()[1] == b':')
    {
        return format!("\\\\?\\{}", normalized);
    }
    normalized
}

fn timestamp_parts(value: &str) -> (i64, i64) {
    use chrono::{DateTime, Utc};
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        let millis = parsed.timestamp_millis();
        (millis / 1000, millis)
    } else {
        let now = Utc::now();
        (now.timestamp(), now.timestamp_millis())
    }
}

pub fn register_session_index(
    session: &CanonicalSession,
    session_id: &str,
    codex_home: &Path,
) -> Result<(), String> {
    let index = codex_home.join("session_index.jsonl");
    if let Ok(contents) = fs::read_to_string(&index) {
        let already_registered = contents.lines().any(|line| {
            serde_json::from_str::<Value>(line)
                .ok()
                .and_then(|entry| entry.get("id").and_then(Value::as_str).map(str::to_string))
                .as_deref()
                == Some(session_id)
        });
        if already_registered {
            return Ok(());
        }
    }
    fs::create_dir_all(codex_home).map_err(|error| error.to_string())?;
    let thread_name = session
        .messages
        .iter()
        .find(|message| message.role == Role::User && !message.text.trim().is_empty())
        .map(|message| message.text.trim().chars().take(120).collect::<String>())
        .unwrap_or_else(|| "Restored session".to_string());
    let entry = json!({
        "id": session_id,
        "thread_name": thread_name,
        "updated_at": session.updated_at
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(index)
        .map_err(|error| error.to_string())?;
    writeln!(
        file,
        "{}",
        serde_json::to_string(&entry).map_err(|error| error.to_string())?
    )
    .map_err(|error| error.to_string())
}

const CODEX_CLI_VERSION: &str = "0.147.0";

fn codex_item_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4())
}

fn codex_current_date() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn codex_stringify_input(input: &Value) -> Result<String, String> {
    match input {
        Value::String(value) => Ok(value.clone()),
        value => serde_json::to_string(value).map_err(|error| error.to_string()),
    }
}

fn codex_message_item(timestamp: &str, role: &str, text: &str) -> Value {
    json!({
        "timestamp": timestamp,
        "type": "response_item",
        "payload": {
            "type": "message",
            "id": codex_item_id("msg"),
            "role": role,
            "content": [{
                "type": if role == "user" { "input_text" } else { "output_text" },
                "text": text
            }]
        }
    })
}

fn codex_item_completed(
    timestamp: &str,
    thread_id: &str,
    turn_id: &str,
    item_type: &str,
    text: &str,
) -> Value {
    json!({
        "timestamp": timestamp,
        "type": "event_msg",
        "payload": {
            "type": "item_completed",
            "thread_id": thread_id,
            "turn_id": turn_id,
            "item": {
                "type": item_type,
                "id": codex_item_id("itm"),
                "content": [{ "type": "text", "text": text, "text_elements": [] }]
            },
            "started_at_ms": 0,
            "completed_at_ms": 0
        }
    })
}

fn codex_user_event(timestamp: &str, text: &str) -> Value {
    json!({
        "timestamp": timestamp,
        "type": "event_msg",
        "payload": {
            "type": "user_message",
            "client_id": codex_item_id("cli"),
            "message": text,
            "images": [],
            "local_images": [],
            "audio": [],
            "local_audio": [],
            "text_elements": []
        }
    })
}

fn codex_agent_event(timestamp: &str, text: &str, phase: &str) -> Value {
    json!({
        "timestamp": timestamp,
        "type": "event_msg",
        "payload": {
            "type": "agent_message",
            "message": text,
            "phase": phase,
            "memory_citation": null
        }
    })
}

fn codex_task_started(timestamp: &str, turn_id: &str, started_at: i64) -> Value {
    json!({
        "timestamp": timestamp,
        "type": "event_msg",
        "payload": {
            "type": "task_started",
            "turn_id": turn_id,
            "started_at": started_at,
            "model_context_window": 258400,
            "collaboration_mode_kind": "default"
        }
    })
}

fn codex_task_complete(
    timestamp: &str,
    turn_id: &str,
    last_agent_text: &str,
    started_at: i64,
) -> Value {
    json!({
        "timestamp": timestamp,
        "type": "event_msg",
        "payload": {
            "type": "task_complete",
            "turn_id": turn_id,
            "last_agent_message": last_agent_text,
            "started_at": started_at,
            "completed_at": started_at,
            "duration_ms": 0
        }
    })
}

fn codex_token_count(timestamp: &str) -> Value {
    json!({
        "timestamp": timestamp,
        "type": "event_msg",
        "payload": {
            "type": "token_count",
            "info": {
                "total_token_usage": {
                    "input_tokens": 0, "cached_input_tokens": 0, "cache_write_input_tokens": 0,
                    "output_tokens": 0, "reasoning_output_tokens": 0, "total_tokens": 0
                },
                "last_token_usage": {
                    "input_tokens": 0, "cached_input_tokens": 0, "cache_write_input_tokens": 0,
                    "output_tokens": 0, "reasoning_output_tokens": 0, "total_tokens": 0
                },
                "model_context_window": 258400
            },
            "rate_limits": null
        }
    })
}

fn codex_thread_settings(timestamp: &str, provider: &str, model: &str, cwd: &str) -> Value {
    json!({
        "timestamp": timestamp,
        "type": "event_msg",
        "payload": {
            "type": "thread_settings_applied",
            "thread_settings": {
                "model": model,
                "model_provider_id": provider,
                "service_tier": "default",
                "approval_policy": "never",
                "approvals_reviewer": "user",
                "permission_profile": { "type": "disabled" },
                "active_permission_profile": { "id": ":danger-full-access" },
                "cwd": cwd,
                "reasoning_effort": "max",
                "collaboration_mode": { "mode": "default" }
            }
        }
    })
}

fn codex_tool_records(timestamp: &str, call: &ToolCall) -> Result<Vec<Value>, String> {
    // Codex validates call ids against a 64 character cap; source tools
    // (notably DSH) can emit longer ids, so always synthesize a short one.
    let call_id = format!("call_{}", Uuid::new_v4());
    Ok(vec![
        json!({
            "timestamp": timestamp,
            "type": "response_item",
            "payload": {
                "type": "function_call",
                "id": codex_item_id("fc"),
                "call_id": call_id,
                "name": call.name,
                "arguments": codex_stringify_input(&call.input)?
            }
        }),
        json!({
            "timestamp": timestamp,
            "type": "response_item",
            "payload": {
                "type": "function_call_output",
                "id": codex_item_id("fco"),
                "call_id": call_id,
                "output": call.output.as_deref().unwrap_or("(output not captured)")
            }
        }),
    ])
}

pub fn write_session(session: &CanonicalSession) -> Result<String, String> {
    let provider = session
        .model_provider
        .as_deref()
        .filter(|provider| !provider.is_empty())
        .unwrap_or("cpa-gui");
    let model = session.model.as_deref().unwrap_or("gpt-5.6-luna");
    let timestamp = session.updated_at.as_str();
    let (started_at, _) = timestamp_parts(&session.created_at);
    let thread_id = session.session_id.as_str();

    let mut records = vec![json!({
        "timestamp": timestamp,
        "type": "session_meta",
        "payload": {
            "session_id": thread_id,
            "id": thread_id,
            "timestamp": session.created_at,
            "cwd": session.cwd,
            "originator": "codex-tui",
            "cli_version": CODEX_CLI_VERSION,
            "source": "cli",
            "thread_source": "user",
            "model_provider": provider,
            "base_instructions": { "text": "You are Codex, an AI coding agent." },
            "history_mode": "legacy"
        }
    })];

    let messages = &session.messages;
    let mut index = 0usize;

    // Some source tools record tool activity before the first user message.
    // Emit it as a self-contained leading turn so the work stays visible
    // without fabricating a user turn.
    if index < messages.len() && messages[index].role != Role::User {
        let lead_turn = codex_item_id("turn");
        records.push(codex_task_started(timestamp, &lead_turn, started_at));
        while index < messages.len() && messages[index].role != Role::User {
            let assistant = &messages[index];
            if !assistant.text.trim().is_empty() {
                records.push(codex_message_item(timestamp, "assistant", &assistant.text));
                records.push(codex_item_completed(
                    timestamp,
                    thread_id,
                    &lead_turn,
                    "AssistantMessage",
                    &assistant.text,
                ));
                records.push(codex_agent_event(timestamp, &assistant.text, "commentary"));
            }
            for call in &assistant.tool_calls {
                records.extend(codex_tool_records(timestamp, call)?);
            }
            index += 1;
        }
        records.push(codex_token_count(timestamp));
        records.push(codex_task_complete(timestamp, &lead_turn, "", started_at));
        records.push(codex_thread_settings(
            timestamp,
            provider,
            model,
            &session.cwd,
        ));
    }

    while index < messages.len() {
        // Each restored turn gets its own turn id and full event envelope so
        // Codex can rebuild the visible transcript from user_message and
        // agent_message events on resume.
        let turn_id = codex_item_id("turn");
        records.push(codex_task_started(timestamp, &turn_id, started_at));

        records.push(json!({
            "timestamp": timestamp,
            "type": "turn_context",
            "payload": {
                "turn_id": turn_id,
                "cwd": session.cwd,
                "workspace_roots": [session.cwd],
                "current_date": codex_current_date(),
                "timezone": "Asia/Shanghai",
                "approval_policy": "never",
                "approvals_reviewer": "user",
                "sandbox_policy": { "type": "danger-full-access" },
                "permission_profile": { "type": "disabled" },
                "model": model,
                "collaboration_mode": { "mode": "default" }
            }
        }));

        let user = &messages[index];
        records.push(codex_message_item(timestamp, "user", &user.text));
        records.push(codex_item_completed(
            timestamp,
            thread_id,
            &turn_id,
            "UserMessage",
            &user.text,
        ));
        records.push(codex_user_event(timestamp, &user.text));
        index += 1;

        let mut last_agent_text = String::new();
        while index < messages.len() && messages[index].role == Role::Assistant {
            let assistant = &messages[index];
            let is_last =
                index + 1 >= messages.len() || messages[index + 1].role != Role::Assistant;
            if !assistant.text.trim().is_empty() {
                let phase = if is_last {
                    "final_answer"
                } else {
                    "commentary"
                };
                records.push(codex_message_item(timestamp, "assistant", &assistant.text));
                records.push(codex_item_completed(
                    timestamp,
                    thread_id,
                    &turn_id,
                    "AssistantMessage",
                    &assistant.text,
                ));
                records.push(codex_agent_event(timestamp, &assistant.text, phase));
                last_agent_text = assistant.text.clone();
            }
            for call in &assistant.tool_calls {
                records.extend(codex_tool_records(timestamp, call)?);
            }
            index += 1;
        }

        records.push(codex_token_count(timestamp));
        records.push(codex_task_complete(
            timestamp,
            &turn_id,
            &last_agent_text,
            started_at,
        ));
        records.push(codex_thread_settings(
            timestamp,
            provider,
            model,
            &session.cwd,
        ));
    }

    let mut output = records
        .into_iter()
        .map(|record| serde_json::to_string(&record).map_err(|error| error.to_string()))
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    output.push('\n');
    Ok(output)
}
