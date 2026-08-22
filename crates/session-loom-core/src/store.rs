use crate::canonical::{CanonicalSession, SourceTool};
use rusqlite::{params, params_from_iter, Connection};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use uuid::Uuid;

const SUMMARY_MIGRATION_BATCH_SIZE: i64 = 20;

#[derive(Debug, Clone)]
pub struct Store {
    root: PathBuf,
    database: PathBuf,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ListFilter {
    pub tool: Option<SourceTool>,
    pub query: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionCard {
    pub conversation_id: String,
    pub session_id: String,
    pub source_tool: String,
    pub tools: Vec<String>,
    pub instance_count: i64,
    pub cwd: String,
    pub created_at: String,
    pub updated_at: String,
    pub title: String,
    pub message_count: i64,
}

impl Store {
    pub fn open(root: &Path) -> Result<Self, String> {
        fs::create_dir_all(root).map_err(|error| error.to_string())?;
        let store = Self {
            root: root.to_path_buf(),
            database: root.join("sessions.db"),
        };
        let mut connection = store.connection()?;
        create_schema(&connection)?;
        migrate_source_path_column(&connection)?;
        migrate_conversation_id_column(&connection)?;
        migrate_legacy_json(&mut connection, &store.root)?;
        migrate_conversations(&mut connection)?;
        migrate_summaries(&mut connection)?;
        Ok(store)
    }

    pub fn from_environment() -> Result<Self, String> {
        Self::open(&crate::paths::store_root())
    }

    pub fn db_path(&self) -> &Path {
        &self.database
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn write_session(&self, session: &CanonicalSession) -> Result<(), String> {
        self.write_session_from(session, None)
    }

    /// Writes a session and, when known, records the source artifact it was
    /// mirrored from. A write without a source keeps any previously recorded
    /// path so plain refreshes never erase the deletion hint.
    pub fn write_session_from(
        &self,
        session: &CanonicalSession,
        source: Option<&Path>,
    ) -> Result<(), String> {
        let mut connection = self.connection()?;
        let source_path = source.map(|path| path.to_string_lossy().to_string());
        let conversation_id =
            resolve_conversation_id(&connection, session, source_path.as_deref())?;
        write_session(
            &mut connection,
            session,
            source_path,
            Some(&conversation_id),
        )
    }

    /// Writes a restored native instance into the mirror while explicitly
    /// attaching it to an existing logical conversation.
    pub fn write_session_with_conversation(
        &self,
        session: &CanonicalSession,
        source: Option<&Path>,
        conversation_id: &str,
    ) -> Result<(), String> {
        if conversation_id.trim().is_empty() {
            return Err("conversation id is empty".to_string());
        }
        let mut connection = self.connection()?;
        write_session(
            &mut connection,
            session,
            source.map(|path| path.to_string_lossy().to_string()),
            Some(conversation_id),
        )
    }

    pub fn conversation_id(&self, session_id: &str) -> Result<String, String> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT COALESCE(NULLIF(conversation_id, ''), session_id)
                 FROM sessions WHERE session_id = ?",
                params![session_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| format!("session not found: {session_id}"))
    }

    /// The source artifact path recorded for a session, when one is known.
    pub fn session_source_path(&self, session_id: &str) -> Option<String> {
        let connection = self.connection().ok()?;
        connection
            .query_row(
                "SELECT source_path FROM sessions WHERE session_id = ?",
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .ok()
            .flatten()
    }

    pub fn read_session(&self, session_id: &str) -> Result<CanonicalSession, String> {
        let connection = self.connection()?;
        let payload = connection
            .query_row(
                "SELECT payload FROM sessions WHERE session_id = ?",
                params![session_id],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| format!("session not found: {session_id}"))?;
        CanonicalSession::from_json(&payload)
    }

    pub fn latest_session(&self) -> Result<CanonicalSession, String> {
        let connection = self.connection()?;
        let payload = connection
            .query_row(
                "SELECT payload FROM sessions ORDER BY updated_at DESC LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .map_err(|_| "no canonical sessions available".to_string())?;
        CanonicalSession::from_json(&payload)
    }

    /// Whether the store currently holds at least one session. The watcher
    /// uses this to notice a wiped store (database deleted and recreated
    /// while the daemon kept running) and re-mirror every source file.
    pub fn has_sessions(&self) -> Result<bool, String> {
        let connection = self.connection()?;
        let count = connection
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|error| error.to_string())?;
        Ok(count > 0)
    }

    pub fn list_sessions(
        &self,
        filter_tool: Option<SourceTool>,
    ) -> Result<Vec<CanonicalSession>, String> {
        let connection = self.connection()?;
        let mut sessions = vec![];
        if let Some(tool) = filter_tool {
            let mut statement = connection
                .prepare(
                    "SELECT payload FROM sessions WHERE source_tool = ? ORDER BY updated_at DESC",
                )
                .map_err(|error| error.to_string())?;
            let rows = statement
                .query_map(params![tool.as_str()], |row| row.get::<_, String>(0))
                .map_err(|error| error.to_string())?;
            for row in rows {
                sessions.push(CanonicalSession::from_json(
                    &row.map_err(|error| error.to_string())?,
                )?);
            }
        } else {
            let mut statement = connection
                .prepare("SELECT payload FROM sessions ORDER BY updated_at DESC")
                .map_err(|error| error.to_string())?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(|error| error.to_string())?;
            for row in rows {
                sessions.push(CanonicalSession::from_json(
                    &row.map_err(|error| error.to_string())?,
                )?);
            }
        }
        Ok(sessions)
    }

    pub fn search_sessions(&self, query: &str) -> Result<Vec<CanonicalSession>, String> {
        let connection = self.connection()?;
        let like = format!("%{query}%");
        let mut statement = connection
            .prepare(
                "SELECT payload FROM sessions
                 WHERE payload LIKE ? OR cwd LIKE ? OR source_tool LIKE ?
                 ORDER BY updated_at DESC",
            )
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params![like, like, like], |row| row.get::<_, String>(0))
            .map_err(|error| error.to_string())?;
        let mut sessions = vec![];
        for row in rows {
            sessions.push(CanonicalSession::from_json(
                &row.map_err(|error| error.to_string())?,
            )?);
        }
        Ok(sessions)
    }

    pub fn export_session(&self, session_id: &str) -> Result<String, String> {
        let connection = self.connection()?;
        connection
            .query_row(
                "SELECT payload FROM sessions WHERE session_id = ?",
                params![session_id],
                |row| row.get(0),
            )
            .map_err(|_| format!("session not found: {session_id}"))
    }

    pub fn delete_session(&self, session_id: &str) -> Result<(), String> {
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction()
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM session_summaries WHERE session_id = ?",
                params![session_id],
            )
            .map_err(|error| error.to_string())?;
        transaction
            .execute(
                "DELETE FROM sessions WHERE session_id = ?",
                params![session_id],
            )
            .map_err(|error| error.to_string())?;
        transaction.commit().map_err(|error| error.to_string())
    }

    pub fn list_cards(&self, filter: ListFilter) -> Result<Vec<SessionCard>, String> {
        let mut connection = self.connection()?;
        let mut sql = String::from(
            "SELECT s.conversation_id, s.session_id, s.source_tool, s.cwd,
                    s.created_at, s.updated_at,
                    summary.title, summary.message_count,
                    CASE WHEN summary.session_id IS NULL THEN s.payload END
             FROM sessions s
             LEFT JOIN session_summaries summary ON summary.session_id = s.session_id",
        );
        let mut clauses = vec![];
        let mut values = vec![];
        if let Some(tool) = filter.tool {
            clauses.push("s.source_tool = ?");
            values.push(tool.as_str().to_string());
        }
        if let Some(query) = filter.query.map(|query| query.trim().to_string()) {
            if !query.is_empty() {
                clauses.push("(s.payload LIKE ? OR s.cwd LIKE ?)");
                values.push(format!("%{query}%"));
                values.push(format!("%{query}%"));
            }
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY s.updated_at DESC");

        let mut statement = connection
            .prepare(&sql)
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params_from_iter(values.iter()), |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            })
            .map_err(|error| error.to_string())?;
        let mut cards: Vec<SessionCard> = vec![];
        let mut card_indices: HashMap<String, usize> = HashMap::new();
        let mut backfills = vec![];
        for row in rows {
            let (
                conversation_id,
                session_id,
                source_tool,
                cwd,
                created_at,
                updated_at,
                summary_title,
                summary_count,
                fallback_payload,
            ) = row.map_err(|error| error.to_string())?;
            let needs_backfill = summary_title.is_none() || summary_count.is_none();
            let (title, message_count) = match (summary_title, summary_count) {
                (Some(title), Some(message_count)) => (title, message_count),
                _ => fallback_payload
                    .as_deref()
                    .and_then(|payload| CanonicalSession::from_json(payload).ok())
                    .map(|session| session_summary(&session))
                    .unwrap_or_else(|| ("(无法解析)".to_string(), 0)),
            };
            if needs_backfill {
                backfills.push((session_id.clone(), title.clone(), message_count));
            }
            let conversation_id = conversation_id.unwrap_or_else(|| session_id.clone());
            if let Some(index) = card_indices.get(&conversation_id).copied() {
                let card = &mut cards[index];
                if !card.tools.iter().any(|tool| tool == &source_tool) {
                    card.tools.push(source_tool.clone());
                }
                card.instance_count += 1;
                if updated_at > card.updated_at {
                    card.session_id = session_id;
                    card.source_tool = source_tool;
                    card.cwd = cwd;
                    card.created_at = created_at;
                    card.updated_at = updated_at;
                    card.title = title;
                    card.message_count = message_count;
                }
            } else {
                card_indices.insert(conversation_id.clone(), cards.len());
                cards.push(SessionCard {
                    conversation_id,
                    session_id,
                    source_tool: source_tool.clone(),
                    tools: vec![source_tool],
                    instance_count: 1,
                    cwd,
                    created_at,
                    updated_at,
                    title,
                    message_count,
                });
            }
        }
        drop(statement);
        if !backfills.is_empty() {
            let _ = backfill_summaries(&mut connection, &backfills);
        }
        for card in &mut cards {
            let mut statement = connection
                .prepare(
                    "SELECT source_tool, COUNT(*) FROM sessions
                     WHERE conversation_id = ? OR (conversation_id IS NULL AND session_id = ?)
                     GROUP BY source_tool ORDER BY source_tool",
                )
                .map_err(|error| error.to_string())?;
            let rows = statement
                .query_map(params![card.conversation_id, card.session_id], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(|error| error.to_string())?;
            card.tools.clear();
            card.instance_count = 0;
            for row in rows {
                let (tool, count) = row.map_err(|error| error.to_string())?;
                card.tools.push(tool);
                card.instance_count += count;
            }
        }
        Ok(cards)
    }

    fn connection(&self) -> Result<Connection, String> {
        let connection =
            Connection::open(&self.database).map_err(|error| format!("open db failed: {error}"))?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(|error| error.to_string())?;
        // If the database file was deleted while a daemon kept running, a
        // bare Connection::open above recreates an empty file without the
        // schema. Re-create the schema so every operation self-heals.
        let has_sessions_table = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'sessions')",
                [],
                |row| row.get::<_, bool>(0),
            )
            .unwrap_or(false);
        if !has_sessions_table {
            create_schema(&connection)?;
        }
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|error| error.to_string())?;
        Ok(connection)
    }
}

fn create_schema(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                session_id TEXT PRIMARY KEY,
                conversation_id TEXT,
                source_tool TEXT NOT NULL,
                cwd TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                schema_version INTEGER NOT NULL,
                payload TEXT NOT NULL,
                source_path TEXT
            );
            CREATE TABLE IF NOT EXISTS session_summaries (
                session_id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                message_count INTEGER NOT NULL,
                FOREIGN KEY (session_id) REFERENCES sessions(session_id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_updated ON sessions(updated_at);
            CREATE INDEX IF NOT EXISTS idx_sessions_tool ON sessions(source_tool);
             CREATE INDEX IF NOT EXISTS idx_sessions_tool_updated ON sessions(source_tool, updated_at);",
        )
        .map_err(|error| error.to_string())
}

fn migrate_source_path_column(connection: &Connection) -> Result<(), String> {
    let mut statement = connection
        .prepare("PRAGMA table_info(sessions)")
        .map_err(|error| error.to_string())?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if !columns.iter().any(|column| column == "source_path") {
        connection
            .execute_batch("ALTER TABLE sessions ADD COLUMN source_path TEXT;")
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn migrate_conversation_id_column(connection: &Connection) -> Result<(), String> {
    let mut statement = connection
        .prepare("PRAGMA table_info(sessions)")
        .map_err(|error| error.to_string())?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| error.to_string())?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    if !columns.iter().any(|column| column == "conversation_id") {
        connection
            .execute_batch("ALTER TABLE sessions ADD COLUMN conversation_id TEXT;")
            .map_err(|error| error.to_string())?;
    }
    connection
        .execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_sessions_conversation ON sessions(conversation_id);",
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Gives legacy native instances a stable logical conversation id. Exact
/// content copies are grouped together so existing Codex -> Claude -> Pi
/// duplicates collapse on the first database open after this migration.
fn migrate_conversations(connection: &mut Connection) -> Result<(), String> {
    let rows = {
        let mut statement = connection
            .prepare(
                "SELECT session_id, payload, conversation_id FROM sessions
                 ORDER BY updated_at, session_id",
            )
            .map_err(|error| error.to_string())?;
        let collected = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })
            .map_err(|error| error.to_string())?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        collected
    };
    if rows.is_empty() {
        return Ok(());
    }

    let mut groups = HashMap::new();
    let mut existing_by_key = HashMap::new();
    for (_, payload, existing_id) in &rows {
        let Some(existing_id) = existing_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        if let Ok(session) = CanonicalSession::from_json(payload) {
            existing_by_key
                .entry(session_content_key(&session))
                .or_insert_with(|| existing_id.to_string());
        }
    }
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    for (session_id, payload, existing_id) in rows {
        let key = CanonicalSession::from_json(&payload)
            .ok()
            .map(|session| session_content_key(&session));
        let current_id = existing_id.filter(|value| !value.trim().is_empty());
        let conversation_id = current_id.clone().unwrap_or_else(|| {
            key.as_ref()
                .and_then(|key| existing_by_key.get(key).cloned())
                .or_else(|| key.as_ref().and_then(|key| groups.get(key).cloned()))
                .unwrap_or_else(|| Uuid::new_v4().to_string())
        });
        if let Some(key) = key {
            groups.entry(key).or_insert_with(|| conversation_id.clone());
        }
        if current_id.as_deref() != Some(conversation_id.as_str()) {
            transaction
                .execute(
                    "UPDATE sessions SET conversation_id = ? WHERE session_id = ?",
                    params![conversation_id, session_id],
                )
                .map_err(|error| error.to_string())?;
        }
    }
    transaction.commit().map_err(|error| error.to_string())
}

fn resolve_conversation_id(
    connection: &Connection,
    session: &CanonicalSession,
    source_path: Option<&str>,
) -> Result<String, String> {
    if let Some(conversation_id) = connection
        .query_row(
            "SELECT conversation_id FROM sessions WHERE session_id = ?",
            params![session.session_id],
            |row| row.get::<_, Option<String>>(0),
        )
        .ok()
        .flatten()
        .filter(|value| !value.trim().is_empty())
    {
        return Ok(conversation_id);
    }

    // Native file paths are unique for all file-backed tools. OpenCode uses a
    // single SQLite path for many sessions, so its path is intentionally not
    // used as an identity key.
    if session.source_tool != SourceTool::OpenCode {
        if let Some(source_path) = source_path {
            if let Some(conversation_id) = connection
                .query_row(
                    "SELECT conversation_id FROM sessions
                     WHERE source_tool = ? AND source_path = ? LIMIT 1",
                    params![session.source_tool.as_str(), source_path],
                    |row| row.get::<_, Option<String>>(0),
                )
                .ok()
                .flatten()
                .filter(|value| !value.trim().is_empty())
            {
                return Ok(conversation_id);
            }
        }
    }

    // This fallback links copies produced before the explicit restore link was
    // recorded. Empty sessions are excluded because many tools create several
    // indistinguishable empty sessions legitimately.
    if !session.messages.is_empty() {
        let key = session_content_key(session);
        let mut statement = connection
            .prepare("SELECT conversation_id, payload FROM sessions WHERE cwd = ?")
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map(params![session.cwd], |row| {
                Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|error| error.to_string())?;
        for row in rows {
            let (conversation_id, payload) = row.map_err(|error| error.to_string())?;
            if let (Some(conversation_id), Ok(existing)) = (
                conversation_id.filter(|value| !value.trim().is_empty()),
                CanonicalSession::from_json(&payload),
            ) {
                if session_content_key(&existing) == key {
                    return Ok(conversation_id);
                }
            }
        }
    }

    Ok(Uuid::new_v4().to_string())
}

fn session_content_key(session: &CanonicalSession) -> String {
    let messages = session
        .messages
        .iter()
        .map(|message| {
            serde_json::json!({
                "role": match message.role {
                    crate::canonical::Role::User => "user",
                    crate::canonical::Role::Assistant => "assistant",
                },
                "text": message.text,
                "toolCalls": message.tool_calls.iter().map(|call| serde_json::json!({
                    "name": call.name,
                    "input": call.input,
                    "output": call.output,
                })).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&serde_json::json!({
        "cwd": session.cwd.replace('\\', "/"),
        "messages": messages,
    }))
    .unwrap_or_default()
}

fn write_session(
    connection: &mut Connection,
    session: &CanonicalSession,
    source_path: Option<String>,
    conversation_id: Option<&str>,
) -> Result<(), String> {
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    upsert_session(
        &transaction,
        session,
        source_path.as_deref(),
        conversation_id,
    )?;
    transaction.commit().map_err(|error| error.to_string())
}

fn upsert_session(
    connection: &Connection,
    session: &CanonicalSession,
    source_path: Option<&str>,
    conversation_id: Option<&str>,
) -> Result<(), String> {
    if session.session_id.trim().is_empty() {
        return Err("session id is empty".to_string());
    }
    let payload = session.to_json()?;
    let (title, message_count) = session_summary(session);
    connection
        .execute(
            "INSERT INTO sessions
               (session_id, conversation_id, source_tool, cwd, created_at, updated_at, schema_version, payload, source_path)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(session_id) DO UPDATE SET
               conversation_id = COALESCE(excluded.conversation_id, sessions.conversation_id),
               source_tool = excluded.source_tool,
               cwd = excluded.cwd,
               created_at = excluded.created_at,
               updated_at = excluded.updated_at,
               schema_version = excluded.schema_version,
               payload = excluded.payload,
               source_path = COALESCE(excluded.source_path, sessions.source_path)",
            params![
                session.session_id,
                conversation_id,
                session.source_tool.as_str(),
                session.cwd,
                session.created_at,
                session.updated_at,
                session.schema_version,
                payload,
                source_path
            ],
        )
        .map_err(|error| error.to_string())?;
    connection
        .execute(
            "INSERT INTO session_summaries (session_id, title, message_count)
             VALUES (?, ?, ?)
             ON CONFLICT(session_id) DO UPDATE SET
               title = excluded.title,
               message_count = excluded.message_count",
            params![session.session_id, title, message_count],
        )
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn session_summary(session: &CanonicalSession) -> (String, i64) {
    let title = session
        .messages
        .iter()
        .find(|message| {
            message.role == crate::canonical::Role::User && !message.text.trim().is_empty()
        })
        .map(|message| {
            message
                .text
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| "(空会话)".to_string());
    let title = if title.chars().count() > 80 {
        format!("{}…", title.chars().take(80).collect::<String>())
    } else {
        title
    };
    (title, session.messages.len() as i64)
}

fn migrate_summaries(connection: &mut Connection) -> Result<(), String> {
    let mut cursor: Option<String> = None;
    loop {
        let rows = {
            let (sql, values): (&str, Vec<String>) = match &cursor {
                Some(cursor) => (
                    "SELECT s.session_id, s.payload
                     FROM sessions s
                     LEFT JOIN session_summaries summary ON summary.session_id = s.session_id
                     WHERE summary.session_id IS NULL AND s.session_id > ?
                     ORDER BY s.session_id LIMIT ?",
                    vec![cursor.clone(), SUMMARY_MIGRATION_BATCH_SIZE.to_string()],
                ),
                None => (
                    "SELECT s.session_id, s.payload
                     FROM sessions s
                     LEFT JOIN session_summaries summary ON summary.session_id = s.session_id
                     WHERE summary.session_id IS NULL
                     ORDER BY s.session_id LIMIT ?",
                    vec![SUMMARY_MIGRATION_BATCH_SIZE.to_string()],
                ),
            };
            let mut statement = connection.prepare(sql).map_err(|error| error.to_string())?;
            let collected = statement
                .query_map(params_from_iter(values.iter()), |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .map_err(|error| error.to_string())?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            collected
        };
        if rows.is_empty() {
            return Ok(());
        }
        let summaries = rows
            .iter()
            .filter_map(|(session_id, payload)| {
                CanonicalSession::from_json(payload)
                    .ok()
                    .map(|session| (session_id.clone(), session_summary(&session)))
            })
            .map(|(session_id, (title, count))| (session_id, title, count))
            .collect::<Vec<_>>();
        if !summaries.is_empty() {
            backfill_summaries(connection, &summaries)?;
        }
        cursor = rows.last().map(|row| row.0.clone());
    }
}

fn backfill_summaries(
    connection: &mut Connection,
    summaries: &[(String, String, i64)],
) -> Result<(), String> {
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    {
        let mut insert = transaction
            .prepare(
                "INSERT INTO session_summaries (session_id, title, message_count)
                 VALUES (?, ?, ?) ON CONFLICT(session_id) DO NOTHING",
            )
            .map_err(|error| error.to_string())?;
        for (session_id, title, count) in summaries {
            insert
                .execute(params![session_id, title, count])
                .map_err(|error| error.to_string())?;
        }
    }
    transaction.commit().map_err(|error| error.to_string())
}

fn migrate_legacy_json(connection: &mut Connection, root: &Path) -> Result<(), String> {
    let legacy = root.join("store");
    if !legacy.exists() {
        return Ok(());
    }
    let count = connection
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| error.to_string())?;
    if count > 0 {
        return Ok(());
    }
    let mut sessions = vec![];
    for tool in [
        SourceTool::Codex,
        SourceTool::Claude,
        SourceTool::OpenCode,
        SourceTool::Dsh,
        SourceTool::Pi,
    ] {
        let directory = legacy.join(tool.as_str());
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Ok(payload) = fs::read_to_string(path) else {
                continue;
            };
            let Ok(session) = CanonicalSession::from_json(&payload) else {
                continue;
            };
            sessions.push(session);
        }
    }
    let transaction = connection
        .transaction()
        .map_err(|error| error.to_string())?;
    for session in &sessions {
        upsert_session(&transaction, session, None, None)?;
    }
    transaction.commit().map_err(|error| error.to_string())?;
    let _ = fs::rename(&legacy, root.join("store.legacy"));
    Ok(())
}
