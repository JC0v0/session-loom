use crate::canonical::{
    CanonicalSession, Message, Role, SourceTool, ToolCall, CANONICAL_SCHEMA_VERSION,
};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

const SESSION_VERSION: &str = "v1";
const DEFAULT_AGENT: &str = "build";
const DEFAULT_PROVIDER: &str = "opencode";
const DEFAULT_MODEL: &str = "unknown";
const GLOBAL_PROJECT_ID: &str = "global";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeWriteResult {
    pub session_id: String,
    pub database: PathBuf,
}

/// Reads every session stored in an OpenCode SQLite database and converts
/// them into canonical sessions. Returns an empty list when the database
/// does not exist or has not been initialized by OpenCode yet.
pub fn parse_sessions(database: &Path) -> Result<Vec<CanonicalSession>, String> {
    if !database.exists() {
        return Ok(vec![]);
    }
    let connection = open_connection(database)?;
    if !table_exists(&connection, "session") || !table_exists(&connection, "message") {
        return Ok(vec![]);
    }

    let mut sessions = read_sessions(&connection)?;
    if sessions.is_empty() {
        return Ok(vec![]);
    }

    for message in read_messages(&connection)? {
        let Some(session) = sessions.get_mut(&message.session_id) else {
            continue;
        };
        session.messages.push(message);
    }

    let parts = read_parts(&connection)?;
    let mut canonical = vec![];
    for (session_id, session) in sessions {
        let mut messages = vec![];
        for raw in &session.messages {
            let part_rows = parts.get(&raw.id).map(Vec::as_slice).unwrap_or_default();
            if let Some((_, message)) = message_from_data(&raw.data, part_rows) {
                messages.push(message);
            }
        }
        canonical.push(CanonicalSession {
            schema_version: CANONICAL_SCHEMA_VERSION,
            source_tool: SourceTool::OpenCode,
            session_id,
            cwd: session.directory,
            created_at: format_timestamp(fallback_timestamp(
                session.created_ms,
                &session.messages,
                true,
            )),
            updated_at: format_timestamp(fallback_timestamp(
                session.updated_ms,
                &session.messages,
                false,
            )),
            messages,
        });
    }
    Ok(canonical)
}

/// Writes a canonical session into an OpenCode SQLite database so that
/// OpenCode can list and continue the conversation. Creates the database
/// (with the OpenCode table layout) when it does not exist yet.
pub fn write_session_to_database(
    session: &CanonicalSession,
    database: &Path,
) -> Result<OpenCodeWriteResult, String> {
    if let Some(parent) = database
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut connection = open_connection(database)?;
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .map_err(|error| error.to_string())?;
    ensure_schema(&connection)?;

    let created_ms = parse_timestamp_ms(&session.created_at).unwrap_or_else(now_ms);
    let updated_ms = parse_timestamp_ms(&session.updated_at).unwrap_or(created_ms);
    let directory = normalize_path(&session.cwd);
    let project_id = resolve_project_id(&connection, &session.cwd)?;
    ensure_project(&connection, &project_id, &session.cwd)?;

    let session_id = generate_id("ses", created_ms);
    let title = session_title(session, created_ms);
    let slug = random_slug();

    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    transaction
        .execute(
            "INSERT INTO session
               (id, project_id, slug, directory, title, version, cost,
                tokens_input, tokens_output, tokens_reasoning,
                tokens_cache_read, tokens_cache_write, time_created, time_updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, 0, 0, 0, 0, ?7, ?8)",
            params![
                session_id,
                project_id,
                slug,
                directory,
                title,
                SESSION_VERSION,
                created_ms,
                updated_ms
            ],
        )
        .map_err(|error| error.to_string())?;

    let mut message_ms = created_ms;
    let mut last_user_message: Option<String> = None;
    let mut last_message: Option<String> = None;
    for message in &session.messages {
        message_ms += 1;
        let message_id = generate_id("msg", message_ms);
        let parent_id = match message.role {
            Role::User => last_user_message.as_deref(),
            Role::Assistant => last_user_message
                .as_deref()
                .or(last_message.as_deref())
                .or(Some(&message_id)),
        }
        .unwrap_or_default()
        .to_string();
        let data = message_data(message, &session.cwd, &parent_id, message_ms);
        transaction
            .execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    message_id,
                    session_id,
                    message_ms,
                    message_ms,
                    serde_json::to_string(&data).map_err(|error| error.to_string())?
                ],
            )
            .map_err(|error| error.to_string())?;

        // Parts hydrate in (id, insertion) order; give each part its own
        // increasing timestamp so text stays ahead of tool parts on replay.
        let mut part_ms = message_ms;
        if !message.text.is_empty() {
            transaction
                .execute(
                    "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        generate_id("prt", part_ms),
                        message_id,
                        session_id,
                        part_ms,
                        part_ms,
                        serde_json::to_string(&json!({ "type": "text", "text": message.text }))
                            .map_err(|error| error.to_string())?
                    ],
                )
                .map_err(|error| error.to_string())?;
            part_ms += 1;
        }
        for call in &message.tool_calls {
            let call_id = if call.id.trim().is_empty() {
                generate_id("tool", message_ms)
            } else {
                call.id.clone()
            };
            let input = match &call.input {
                Value::Object(_) => call.input.clone(),
                other => json!({ "value": other }),
            };
            let state = match &call.output {
                Some(output) => json!({
                    "status": "completed",
                    "input": input,
                    "output": output,
                    "title": call.name,
                    "metadata": {},
                    "time": { "start": message_ms, "end": message_ms }
                }),
                None => json!({
                    "status": "error",
                    "input": input,
                    "error": "(output not captured)",
                    "metadata": {},
                    "time": { "start": message_ms, "end": message_ms }
                }),
            };
            transaction
                .execute(
                    "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        generate_id("prt", part_ms),
                        message_id,
                        session_id,
                        part_ms,
                        part_ms,
                        serde_json::to_string(
                            &json!({ "type": "tool", "callID": call_id, "tool": call.name, "state": state })
                        )
                        .map_err(|error| error.to_string())?
                    ],
                )
                .map_err(|error| error.to_string())?;
            part_ms += 1;
        }

        if message.role == Role::User {
            last_user_message = Some(message_id.clone());
        }
        last_message = Some(message_id);
    }

    transaction.commit().map_err(|error| error.to_string())?;
    Ok(OpenCodeWriteResult {
        session_id,
        database: database.to_path_buf(),
    })
}

#[derive(Debug)]
struct RawSession {
    directory: String,
    created_ms: i64,
    updated_ms: i64,
    messages: Vec<RawMessage>,
}

#[derive(Debug)]
struct RawMessage {
    id: String,
    session_id: String,
    created_ms: i64,
    data: Value,
}

fn read_sessions(connection: &Connection) -> Result<HashMap<String, RawSession>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, directory, time_created, time_updated FROM session
             ORDER BY time_updated DESC",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let mut sessions = HashMap::new();
    for row in rows {
        let (id, directory, created_ms, updated_ms) = row.map_err(|error| error.to_string())?;
        if id.trim().is_empty() {
            continue;
        }
        sessions.insert(
            id,
            RawSession {
                directory,
                created_ms,
                updated_ms,
                messages: vec![],
            },
        );
    }
    Ok(sessions)
}

fn read_messages(connection: &Connection) -> Result<Vec<RawMessage>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, session_id, time_created, data FROM message
             ORDER BY session_id, time_created, id",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let mut messages = vec![];
    for row in rows {
        let (id, session_id, created_ms, data) = row.map_err(|error| error.to_string())?;
        let Ok(data) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        messages.push(RawMessage {
            id,
            session_id,
            created_ms,
            data,
        });
    }
    Ok(messages)
}

fn read_parts(connection: &Connection) -> Result<HashMap<String, Vec<Value>>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, message_id, data FROM part
             ORDER BY message_id, time_created, id",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let mut parts: HashMap<String, Vec<Value>> = HashMap::new();
    for row in rows {
        let (id, message_id, data) = row.map_err(|error| error.to_string())?;
        let Ok(mut data) = serde_json::from_str::<Value>(&data) else {
            continue;
        };
        if let Some(object) = data.as_object_mut() {
            object.insert("id".to_string(), json!(id));
        }
        parts.entry(message_id).or_default().push(data);
    }
    Ok(parts)
}

fn message_from_data(data: &Value, parts: &[Value]) -> Option<(Role, Message)> {
    let role = match data.get("role").and_then(Value::as_str) {
        Some("user") => Role::User,
        Some("assistant") => Role::Assistant,
        _ => return None,
    };
    let mut text = String::new();
    let mut tool_calls = vec![];
    for part in parts {
        match part.get("type").and_then(Value::as_str) {
            Some("text") => {
                if part.get("synthetic").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                if let Some(value) = part.get("text").and_then(Value::as_str) {
                    text.push_str(value);
                }
            }
            Some("tool") => {
                let state = part.get("state").unwrap_or(&Value::Null);
                let id = part
                    .get("callID")
                    .and_then(Value::as_str)
                    .or_else(|| part.get("id").and_then(Value::as_str))
                    .unwrap_or_default()
                    .to_string();
                let name = part
                    .get("tool")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let input = state.get("input").cloned().unwrap_or(Value::Null);
                let output = match state.get("status").and_then(Value::as_str) {
                    Some("completed") => state
                        .get("output")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    Some("error") => state
                        .get("error")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    _ => None,
                };
                tool_calls.push(ToolCall {
                    id,
                    name,
                    input,
                    output,
                });
            }
            _ => {}
        }
    }
    if text.is_empty() && tool_calls.is_empty() {
        return None;
    }
    Some((
        role,
        Message {
            role,
            text,
            tool_calls,
        },
    ))
}

fn message_data(message: &Message, cwd: &str, parent_id: &str, created_ms: i64) -> Value {
    match message.role {
        Role::User => json!({
            "role": "user",
            "time": { "created": created_ms },
            "agent": DEFAULT_AGENT,
            "model": { "providerID": DEFAULT_PROVIDER, "modelID": DEFAULT_MODEL }
        }),
        Role::Assistant => json!({
            "role": "assistant",
            "time": { "created": created_ms, "completed": created_ms },
            "parentID": parent_id,
            "modelID": DEFAULT_MODEL,
            "providerID": DEFAULT_PROVIDER,
            "mode": DEFAULT_AGENT,
            "agent": DEFAULT_AGENT,
            "path": { "cwd": normalize_path(cwd), "root": normalize_path(cwd) },
            "cost": 0,
            "tokens": {
                "input": 0,
                "output": 0,
                "reasoning": 0,
                "cache": { "read": 0, "write": 0 }
            }
        }),
    }
}

fn resolve_project_id(connection: &Connection, cwd: &str) -> Result<String, String> {
    let normalized = normalize_path(cwd);
    if !normalized.is_empty() {
        let existing = connection
            .query_row(
                "SELECT id FROM project WHERE worktree = ?1 LIMIT 1",
                params![normalized],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some(id) = existing {
            return Ok(id);
        }
        let existing = connection
            .query_row(
                "SELECT project_id FROM project_directory WHERE directory = ?1 LIMIT 1",
                params![normalized],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| error.to_string())?;
        if let Some(id) = existing {
            return Ok(id);
        }
    }
    if let Some(commit) = git_root_commit(cwd) {
        return Ok(commit);
    }
    Ok(GLOBAL_PROJECT_ID.to_string())
}

fn ensure_project(connection: &Connection, project_id: &str, cwd: &str) -> Result<(), String> {
    let now = now_ms();
    let in_git = is_git_repo(cwd);
    let worktree = if project_id == GLOBAL_PROJECT_ID {
        "/".to_string()
    } else if let Some(root) = git_worktree(cwd) {
        root
    } else {
        normalize_path(cwd)
    };
    connection
        .execute(
            "INSERT INTO project
               (id, worktree, vcs, time_created, time_updated, time_initialized, sandboxes)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, '[]')
             ON CONFLICT(id) DO NOTHING",
            params![
                project_id,
                worktree,
                if in_git {
                    Some("git".to_string())
                } else {
                    None::<String>
                },
                now,
                now,
                now
            ],
        )
        .map_err(|error| error.to_string())?;
    connection
        .execute(
            "INSERT INTO project_directory (project_id, directory, type, time_created)
             VALUES (?1, ?2, 'main', ?3)
             ON CONFLICT(project_id, directory) DO NOTHING",
            params![project_id, normalize_path(cwd), now],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn session_title(session: &CanonicalSession, created_ms: i64) -> String {
    let first_user = session
        .messages
        .iter()
        .find(|message| message.role == Role::User && !message.text.trim().is_empty())
        .map(|message| {
            message
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|title| !title.is_empty());
    match first_user {
        Some(title) if title.chars().count() <= 120 => title,
        Some(title) => format!("{}…", title.chars().take(120).collect::<String>()),
        None => format!("New session - {}", format_timestamp(created_ms)),
    }
}

fn random_slug() -> String {
    const ADJECTIVES: &[&str] = &[
        "brave", "calm", "clever", "cosmic", "crisp", "curious", "eager", "gentle", "glowing",
        "happy", "hidden", "jolly", "kind", "lucky", "mighty", "misty", "neon", "nimble",
        "playful", "proud", "quick", "quiet", "shiny", "silent", "stellar", "sunny", "swift",
        "tidy", "witty",
    ];
    const NOUNS: &[&str] = &[
        "cabin", "cactus", "canyon", "circuit", "comet", "eagle", "engine", "falcon", "forest",
        "garden", "harbor", "island", "knight", "lagoon", "meadow", "moon", "mountain", "nebula",
        "orchid", "otter", "panda", "pixel", "planet", "river", "rocket", "sailor", "squid",
        "star", "tiger", "wizard", "wolf",
    ];
    let bytes = Uuid::new_v4().as_bytes().to_vec();
    let adjective = ADJECTIVES[bytes[0] as usize % ADJECTIVES.len()];
    let noun = NOUNS[bytes[1] as usize % NOUNS.len()];
    format!("{adjective}-{noun}")
}

/// Generates an OpenCode-style identifier (ses_, msg_, prt_, tool_) with a
/// monotonic timestamp prefix followed by random entropy.
fn generate_id(prefix: &str, timestamp_ms: i64) -> String {
    const BASE32: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut value = timestamp_ms.max(0) as u64;
    let mut timestamp = String::with_capacity(10);
    for _ in 0..10 {
        timestamp.push(BASE32[(value % 32) as usize] as char);
        value /= 32;
    }
    let timestamp = timestamp.chars().rev().collect::<String>();
    let entropy = Uuid::new_v4().simple().to_string();
    format!("{prefix}_{timestamp}{}", &entropy[..16])
}

fn open_connection(database: &Path) -> Result<Connection, String> {
    let connection =
        Connection::open(database).map_err(|error| format!("open opencode db failed: {error}"))?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|error| error.to_string())?;
    Ok(connection)
}

fn table_exists(connection: &Connection, name: &str) -> bool {
    connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![name],
            |row| row.get::<_, i64>(0),
        )
        .map(|count| count > 0)
        .unwrap_or(false)
}

/// Creates the OpenCode table layout when the database file is brand new.
fn ensure_schema(connection: &Connection) -> Result<(), String> {
    if table_exists(connection, "session") {
        return Ok(());
    }
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS project (
                id TEXT PRIMARY KEY,
                worktree TEXT NOT NULL,
                vcs TEXT,
                name TEXT,
                icon_url TEXT,
                icon_url_override TEXT,
                icon_color TEXT,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                time_initialized INTEGER,
                sandboxes TEXT NOT NULL,
                commands TEXT
            );
            CREATE TABLE IF NOT EXISTS project_directory (
                project_id TEXT NOT NULL,
                directory TEXT NOT NULL,
                type TEXT,
                strategy TEXT,
                time_created INTEGER NOT NULL,
                PRIMARY KEY (project_id, directory)
            );
            CREATE TABLE IF NOT EXISTS session (
                id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                workspace_id TEXT,
                parent_id TEXT,
                slug TEXT NOT NULL,
                directory TEXT NOT NULL,
                path TEXT,
                title TEXT NOT NULL,
                version TEXT NOT NULL,
                share_url TEXT,
                summary_additions INTEGER,
                summary_deletions INTEGER,
                summary_files INTEGER,
                summary_diffs TEXT,
                metadata TEXT,
                cost REAL NOT NULL DEFAULT 0,
                tokens_input INTEGER NOT NULL DEFAULT 0,
                tokens_output INTEGER NOT NULL DEFAULT 0,
                tokens_reasoning INTEGER NOT NULL DEFAULT 0,
                tokens_cache_read INTEGER NOT NULL DEFAULT 0,
                tokens_cache_write INTEGER NOT NULL DEFAULT 0,
                revert TEXT,
                permission TEXT,
                agent TEXT,
                model TEXT,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                time_compacting INTEGER,
                time_archived INTEGER
            );
            CREATE TABLE IF NOT EXISTS message (
                id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS part (
                id TEXT PRIMARY KEY,
                message_id TEXT NOT NULL,
                session_id TEXT NOT NULL,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                data TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS session_project_idx ON session(project_id);
            CREATE INDEX IF NOT EXISTS message_session_time_created_id_idx
                ON message(session_id, time_created, id);
            CREATE INDEX IF NOT EXISTS part_message_id_id_idx ON part(message_id, id);
            CREATE INDEX IF NOT EXISTS part_session_idx ON part(session_id);",
        )
        .map_err(|error| error.to_string())
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
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

fn fallback_timestamp(value: i64, messages: &[RawMessage], oldest: bool) -> i64 {
    if value > 0 {
        return value;
    }
    let mut times = messages
        .iter()
        .map(|message| message.created_ms)
        .collect::<Vec<_>>();
    times.sort_unstable();
    if times.is_empty() {
        return now_ms();
    }
    if oldest {
        times[0]
    } else {
        times[times.len() - 1]
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn git_command(cwd: &str, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn git_root_commit(cwd: &str) -> Option<String> {
    let output = git_command(cwd, &["rev-list", "--max-parents=0", "--all"])?;
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .min()
        .map(str::to_string)
}

fn git_worktree(cwd: &str) -> Option<String> {
    git_command(cwd, &["rev-parse", "--show-toplevel"]).map(|root| normalize_path(&root))
}

fn is_git_repo(cwd: &str) -> bool {
    git_command(cwd, &["rev-parse", "--is-inside-work-tree"]).as_deref() == Some("true")
}
