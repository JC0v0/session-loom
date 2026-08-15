use super::{attach_tool_outputs, json_argument, push_tool_call};
use crate::canonical::{
    CanonicalSession, Message, Role, SourceTool, ToolCall, CANONICAL_SCHEMA_VERSION,
};
use chrono::{DateTime, SecondsFormat, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const SESSION_FORMAT_VERSION: u64 = 0;
const ZSTD_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];
const DEFAULT_PROVIDER: &str = "unknown";
const DEFAULT_MODEL: &str = "unknown";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DshWriteResult {
    pub session_id: String,
    pub log_file: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogCompression {
    Zstd,
    None,
}

/// Parses one DeepSeek Harness session log (plaintext .jsonl or
/// checksummed concatenated-frame .jsonl.zstd) into a canonical session.
/// Returns Ok(None) for subagent sessions, which are not mirrored.
pub fn parse_session_file(path: &Path) -> Result<Option<CanonicalSession>, String> {
    let bytes = fs::read(path).map_err(|error| error.to_string())?;
    let text = decode_log(&bytes)?;
    let mut lines = text.lines();
    let header_line = lines
        .next()
        .ok_or_else(|| format!("empty session log: {}", path.display()))?;
    let header: Value = serde_json::from_str(header_line).map_err(|error| error.to_string())?;
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return Err(format!(
            "first line is not a session header: {}",
            path.display()
        ));
    }
    if header.get("origin").and_then(Value::as_str) == Some("subagent") {
        return Ok(None);
    }

    let session_id = header
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let cwd = header
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let created_ms = header.get("createdAt").and_then(Value::as_i64).unwrap_or(0);
    let mut updated_ms = created_ms;
    let mut messages = vec![];
    let mut outputs = HashMap::new();

    for line in lines {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(time) = event.get("time").and_then(Value::as_i64) {
            updated_ms = updated_ms.max(time);
        }
        let data = event.get("data").unwrap_or(&Value::Null);
        match event.get("type").and_then(Value::as_str) {
            Some("user/message") => {
                if data
                    .get("source")
                    .and_then(|source| source.get("kind"))
                    .and_then(Value::as_str)
                    != Some("user")
                {
                    continue;
                }
                let text = concat_text_blocks(data.get("content"));
                if text.is_empty() {
                    continue;
                }
                messages.push(Message {
                    role: Role::User,
                    text,
                    tool_calls: vec![],
                });
            }
            Some("assistant/message") => {
                if !surface_append(&event) {
                    continue;
                }
                let text = concat_text_blocks(
                    data.get("message")
                        .and_then(|message| message.get("content")),
                );
                messages.push(Message {
                    role: Role::Assistant,
                    text,
                    tool_calls: vec![],
                });
            }
            Some("tool/call") => {
                push_tool_call(
                    &mut messages,
                    ToolCall {
                        id: data
                            .get("callId")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        name: data
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        input: json_argument(data.get("arguments")),
                        output: None,
                    },
                );
            }
            Some("tool/result") => {
                let message = data.get("message").unwrap_or(&Value::Null);
                let block = message
                    .get("content")
                    .and_then(Value::as_array)
                    .and_then(|content| content.first());
                let call_id = block
                    .and_then(|block| block.get("toolCallId"))
                    .and_then(Value::as_str)
                    .or_else(|| {
                        message
                            .get("source")
                            .and_then(|source| source.get("callId"))
                            .and_then(Value::as_str)
                    })
                    .unwrap_or_default()
                    .to_string();
                let mut output = block
                    .and_then(|block| concat_block_text(block.get("content")))
                    .unwrap_or_default();
                if output.is_empty() {
                    if let Some(error) = data.get("error") {
                        let name = error.get("name").and_then(Value::as_str).unwrap_or("error");
                        let code = error
                            .get("code")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown");
                        output = format!("[{name}: {code}]");
                    }
                }
                if !call_id.is_empty() && !output.is_empty() {
                    outputs.insert(call_id, output);
                }
            }
            _ => {}
        }
    }

    attach_tool_outputs(&mut messages, &outputs);
    Ok(Some(CanonicalSession {
        schema_version: CANONICAL_SCHEMA_VERSION,
        source_tool: SourceTool::Dsh,
        session_id,
        cwd,
        created_at: format_timestamp(created_ms),
        updated_at: format_timestamp(updated_ms.max(created_ms)),
        model_provider: None,
        model: None,
        messages,
    }))
}

/// Restores a canonical session into the DeepSeek Harness sessions root so
/// the harness lists and continues it. The log is written as checksummed
/// concatenated zstd frames, or plaintext when the root already holds
/// plaintext logs.
pub fn write_session_to_root(
    session: &CanonicalSession,
    root: &Path,
) -> Result<DshWriteResult, String> {
    fs::create_dir_all(root).map_err(|error| error.to_string())?;
    let compression = detect_compression(root);
    let session_id = format!("session-{}", Uuid::new_v4());
    let directory = root
        .join(project_key(&session.cwd))
        .join(encode_segment(&session_id));
    fs::create_dir_all(&directory).map_err(|error| error.to_string())?;

    let created_ms = parse_timestamp_ms(&session.created_at).unwrap_or_else(now_ms);
    let updated_ms = parse_timestamp_ms(&session.updated_at).unwrap_or(created_ms);
    let header = json!({
        "type": "session",
        "version": SESSION_FORMAT_VERSION,
        "id": session_id,
        "createdAt": created_ms,
        "cwd": session.cwd,
        "delegationDepth": 0
    });
    let events = render_events(session, created_ms.max(updated_ms));
    let header_bytes = format!(
        "{}\n",
        serde_json::to_string(&header).map_err(|error| error.to_string())?
    );
    let encoded = match compression {
        LogCompression::Zstd => {
            let mut bytes = compress_frame(header_bytes.as_bytes())?;
            for frame in event_frames(&events)? {
                bytes.extend_from_slice(&compress_frame(frame.as_bytes())?);
            }
            bytes
        }
        LogCompression::None => {
            let mut plaintext = header_bytes.clone();
            for event in &events {
                plaintext
                    .push_str(&serde_json::to_string(event).map_err(|error| error.to_string())?);
                plaintext.push('\n');
            }
            plaintext.into_bytes()
        }
    };
    let log_file = directory.join(match compression {
        LogCompression::Zstd => "session.jsonl.zstd",
        LogCompression::None => "session.jsonl",
    });
    fs::write(&log_file, encoded).map_err(|error| error.to_string())?;
    Ok(DshWriteResult {
        session_id,
        log_file,
    })
}

/// Groups rendered events into plaintext JSONL batches, one frame per
/// turn, mirroring the harness append-batch framing.
fn event_frames(events: &[Value]) -> Result<Vec<String>, String> {
    let mut frames = vec![];
    let mut current_turn: Option<i64> = None;
    let mut current = String::new();
    for event in events {
        let event_turn = event
            .get("data")
            .and_then(|data| data.get("turn"))
            .and_then(Value::as_i64);
        if let Some(turn) = event_turn {
            if current_turn != Some(turn) {
                if !current.is_empty() {
                    frames.push(std::mem::take(&mut current));
                }
                current_turn = Some(turn);
            }
        }
        current.push_str(&serde_json::to_string(event).map_err(|error| error.to_string())?);
        current.push('\n');
    }
    if !current.is_empty() {
        frames.push(current);
    }
    Ok(frames)
}

fn render_events(session: &CanonicalSession, base_ms: i64) -> Vec<Value> {
    let mut events = vec![];
    let mut seq = 0i64;
    let mut time = base_ms;
    let mut turn = 0i64;
    let mut step = 1i64;
    let mut turn_open = false;

    let mut push = |events: &mut Vec<Value>, event_type: &str, data: Value, surface: bool| {
        let mut event = json!({ "type": event_type, "seq": seq, "time": time, "data": data });
        if surface {
            event["surfaceOp"] = json!("append");
        }
        events.push(event);
        seq += 1;
        time += 1;
    };

    for message in &session.messages {
        match message.role {
            Role::User => {
                if turn_open {
                    push(
                        &mut events,
                        "turn/end",
                        json!({ "turn": turn, "reason": { "kind": "completed" } }),
                        false,
                    );
                }
                turn += 1;
                step = 1;
                push(&mut events, "turn/start", json!({ "turn": turn }), false);
                turn_open = true;
                let data = json!({
                    "id": Uuid::new_v4().to_string(),
                    "role": "user",
                    "content": [{ "type": "text", "text": message.text }],
                    "source": { "kind": "user" }
                });
                push(&mut events, "user/message", data, true);
            }
            Role::Assistant => {
                if !turn_open {
                    turn += 1;
                    step = 1;
                    push(&mut events, "turn/start", json!({ "turn": turn }), false);
                    turn_open = true;
                }
                push(
                    &mut events,
                    "step/start",
                    json!({ "turn": turn, "step": step }),
                    false,
                );
                let mut content = vec![];
                if !message.text.is_empty() {
                    content.push(json!({ "type": "text", "text": message.text }));
                }
                // Derived history rebuilds provider-level tool_calls from the
                // tool-call blocks of the assembled assistant message, so every
                // call must appear here AND as its own tool/call event.
                let mut call_ids = Vec::with_capacity(message.tool_calls.len());
                for call in &message.tool_calls {
                    let call_id = if call.id.trim().is_empty() {
                        format!("tool_{}", Uuid::new_v4().simple())
                    } else {
                        call.id.clone()
                    };
                    let arguments = match &call.input {
                        Value::String(value) => value.clone(),
                        value => {
                            serde_json::to_string(value).unwrap_or_else(|_| "null".to_string())
                        }
                    };
                    content.push(json!({ "type": "tool-call", "id": call_id, "name": call.name, "arguments": arguments }));
                    call_ids.push(call_id);
                }
                let data = json!({
                    "turn": turn,
                    "step": step,
                    "message": {
                        "id": Uuid::new_v4().to_string(),
                        "role": "assistant",
                        "content": content,
                        "source": { "kind": "model", "provider": DEFAULT_PROVIDER, "model": DEFAULT_MODEL }
                    }
                });
                push(&mut events, "assistant/message", data, true);
                for (call, call_id) in message.tool_calls.iter().zip(call_ids) {
                    push(
                        &mut events,
                        "tool/call",
                        json!({
                            "turn": turn,
                            "step": step,
                            "callId": call_id,
                            "name": call.name,
                            "arguments": match &call.input {
                                Value::String(value) => value.clone(),
                                value => serde_json::to_string(value)
                                    .unwrap_or_else(|_| "null".to_string()),
                            }
                        }),
                        false,
                    );
                    let (result_content, is_error, error) = match &call.output {
                        Some(output) => (
                            json!([{ "type": "text", "text": output }]),
                            false,
                            Value::Null,
                        ),
                        None => (
                            json!([{ "type": "text", "text": "(output not captured)" }]),
                            true,
                            json!({ "name": "SessionLoomRestore", "code": "NO_OUTPUT" }),
                        ),
                    };
                    let mut data = json!({
                        "turn": turn,
                        "step": step,
                        "message": {
                            "id": Uuid::new_v4().to_string(),
                            "role": "user",
                            "content": [{
                                "type": "tool-result",
                                "toolCallId": call_id,
                                "content": result_content,
                                "isError": is_error
                            }],
                            "source": { "kind": "tool", "callId": call_id }
                        }
                    });
                    if !error.is_null() {
                        data["error"] = error;
                    }
                    push(&mut events, "tool/result", data, true);
                }
                push(
                    &mut events,
                    "step/end",
                    json!({ "turn": turn, "step": step }),
                    false,
                );
                step += 1;
            }
        }
    }
    if turn_open {
        push(
            &mut events,
            "turn/end",
            json!({ "turn": turn, "reason": { "kind": "completed" } }),
            false,
        );
    }
    events
}

fn decode_log(bytes: &[u8]) -> Result<String, String> {
    if !bytes.starts_with(&ZSTD_MAGIC) {
        return Ok(String::from_utf8_lossy(bytes).into_owned());
    }
    // Decode frame by frame and stop at the last structurally complete
    // frame: a live harness appends batches as independent frames, so the
    // final frame is often torn while the session is being written.
    let mut text = String::new();
    let mut offset = 0usize;
    while offset < bytes.len() {
        let Some(end) = complete_frame_end(bytes, offset).map_err(|error| error.to_string())?
        else {
            break;
        };
        let decoded = zstd::stream::decode_all(&bytes[offset..end])
            .map_err(|error| format!("decode zstd session log frame failed: {error}"))?;
        text.push_str(&String::from_utf8_lossy(&decoded));
        offset = end;
    }
    if text.is_empty() {
        return Err("zstd session log has no complete frame".to_string());
    }
    Ok(text)
}

/// Returns the exclusive end offset of the structurally complete zstd frame
/// starting at `start`, or `None` when the buffer ends inside the frame.
fn complete_frame_end(bytes: &[u8], start: usize) -> Result<Option<usize>, String> {
    let mut offset = start;
    if bytes.len() - offset < 4 {
        return Ok(None);
    }
    if bytes[offset..offset + 4] != ZSTD_MAGIC {
        return Err(format!("invalid zstd frame magic at byte {offset}"));
    }
    offset += 4;
    if offset == bytes.len() {
        return Ok(None);
    }
    let descriptor = bytes[offset];
    offset += 1;
    if descriptor & 0x18 != 0 {
        return Err(format!(
            "reserved zstd frame-header bit at byte {}",
            offset - 1
        ));
    }
    let content_size_flag = descriptor >> 6;
    let single_segment = descriptor & 0x20 != 0;
    let checksum = descriptor & 0x04 != 0;
    let dictionary_flag = descriptor & 0x03;
    let dictionary_bytes = if dictionary_flag == 3 {
        4
    } else {
        dictionary_flag as usize
    };
    let content_size_bytes = if content_size_flag == 0 {
        if single_segment {
            1
        } else {
            0
        }
    } else {
        1usize << content_size_flag
    };
    let remaining_header =
        if single_segment { 0 } else { 1 } + dictionary_bytes + content_size_bytes;
    if bytes.len() - offset < remaining_header {
        return Ok(None);
    }
    offset += remaining_header;
    loop {
        if bytes.len() - offset < 3 {
            return Ok(None);
        }
        let block_header = u32::from(bytes[offset])
            | (u32::from(bytes[offset + 1]) << 8)
            | (u32::from(bytes[offset + 2]) << 16);
        offset += 3;
        let last_block = block_header & 1 != 0;
        let block_type = (block_header >> 1) & 0x03;
        let block_size = (block_header >> 3) as usize;
        if block_type == 3 {
            return Err(format!("reserved zstd block type at byte {}", offset - 3));
        }
        let payload_bytes = if block_type == 1 { 1 } else { block_size };
        if bytes.len() - offset < payload_bytes {
            return Ok(None);
        }
        offset += payload_bytes;
        if last_block {
            break;
        }
    }
    if checksum {
        if bytes.len() - offset < 4 {
            return Ok(None);
        }
        offset += 4;
    }
    Ok(Some(offset))
}

fn compress_frame(input: &[u8]) -> Result<Vec<u8>, String> {
    let mut encoder =
        zstd::stream::write::Encoder::new(Vec::new(), 3).map_err(|error| error.to_string())?;
    encoder
        .include_checksum(true)
        .map_err(|error| error.to_string())?;
    encoder
        .write_all(input)
        .map_err(|error| error.to_string())?;
    encoder.finish().map_err(|error| error.to_string())
}

fn detect_compression(root: &Path) -> LogCompression {
    let mut plaintext = false;
    let mut zstd = false;
    collect_log_suffixes(root, &mut plaintext, &mut zstd);
    if plaintext {
        LogCompression::None
    } else {
        LogCompression::Zstd
    }
}

fn collect_log_suffixes(root: &Path, plaintext: &mut bool, zstd: &mut bool) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_log_suffixes(&path, plaintext, zstd);
        } else if file_type.is_file() {
            if path.ends_with("session.jsonl") {
                *plaintext = true;
            } else if path.ends_with("session.jsonl.zstd") {
                *zstd = true;
            }
        }
    }
}

/// Lists the session log files under a DeepSeek Harness sessions root.
pub fn session_log_files(root: &Path) -> Vec<PathBuf> {
    let mut files = vec![];
    collect_log_files(root, &mut files);
    files
}

fn collect_log_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            collect_log_files(&path, files);
        } else if file_type.is_file() && is_session_log(&path) {
            files.push(path);
        }
    }
}

pub fn is_session_log(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|name| name == "session.jsonl" || name == "session.jsonl.zstd")
        .unwrap_or(false)
}

/// Mirrors the harness project-directory key: separators collapse to -
/// and the readable slug is wrapped as --<slug>--.
pub(crate) fn project_key(cwd: &str) -> String {
    let mut readable = String::new();
    let mut separator_run = false;
    for ch in cwd.chars() {
        if ch == '/' || ch == '\\' || ch == ':' {
            if !separator_run {
                readable.push('-');
            }
            separator_run = true;
        } else if ch != '~' && (ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')) {
            readable.push(ch);
            separator_run = false;
        } else {
            readable.push_str(&format!("~{:04X}", ch as u32));
            separator_run = false;
        }
    }
    let slug = readable.trim_start_matches('-');
    let slug = if slug.is_empty() { "root" } else { slug };
    let slug: String = slug.chars().take(251).collect();
    format!("--{slug}--")
}

/// Mirrors the harness path-segment encoding for session ids.
pub(crate) fn encode_segment(raw: &str) -> String {
    if raw == "." {
        return "~002E".to_string();
    }
    if raw == ".." {
        return "~002E~002E".to_string();
    }
    let mut out = String::new();
    for ch in raw.chars() {
        if ch != '~' && (ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-')) {
            out.push(ch);
        } else {
            out.push_str(&format!("~{:04X}", ch as u32));
        }
    }
    out
}

fn concat_text_blocks(content: Option<&Value>) -> String {
    concat_block_text(content).unwrap_or_default()
}

fn concat_block_text(content: Option<&Value>) -> Option<String> {
    let blocks = content.and_then(Value::as_array)?;
    let mut text = String::new();
    for block in blocks {
        if block.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(value) = block.get("text").and_then(Value::as_str) {
                text.push_str(value);
            }
        }
    }
    Some(text)
}

fn surface_append(event: &Value) -> bool {
    match event.get("surfaceOp") {
        None | Some(Value::String(_)) => true,
        Some(Value::Object(object)) => object.get("op").and_then(Value::as_str) != Some("replace"),
        Some(_) => true,
    }
}

fn parse_timestamp_ms(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|value| value.timestamp_millis())
}

fn format_timestamp(millis: i64) -> String {
    DateTime::from_timestamp_millis(millis)
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_default()
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}
