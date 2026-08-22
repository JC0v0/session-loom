use crate::canonical::CanonicalSession;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf, time::Duration};

/// How long a deleted session stays recoverable in the recycle bin.
pub const TRASH_RETENTION: Duration = Duration::from_secs(30 * 24 * 60 * 60);

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashEntry {
    pub deleted_at: String,
    pub source_tool: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub session: CanonicalSession,
}

/// File-based recycle bin under <store_root>/trash. One JSON entry per
/// deleted session; entries older than 30 days are purged automatically.
pub struct Trash {
    directory: PathBuf,
}

impl Trash {
    pub fn new(root: &std::path::Path) -> Self {
        Self {
            directory: root.join("trash"),
        }
    }

    pub fn directory(&self) -> &std::path::Path {
        &self.directory
    }

    pub fn add(
        &self,
        session: &CanonicalSession,
        source_path: Option<&std::path::Path>,
    ) -> Result<(), String> {
        self.add_with_conversation(session, source_path, None)
    }

    pub fn add_with_conversation(
        &self,
        session: &CanonicalSession,
        source_path: Option<&std::path::Path>,
        conversation_id: Option<&str>,
    ) -> Result<(), String> {
        fs::create_dir_all(&self.directory).map_err(|error| error.to_string())?;
        let entry = TrashEntry {
            deleted_at: Utc::now().to_rfc3339(),
            source_tool: session.source_tool.as_str().to_string(),
            conversation_id: conversation_id.map(str::to_string),
            source_path: source_path.map(|path| path.to_string_lossy().to_string()),
            session: session.clone(),
        };
        let payload = serde_json::to_string_pretty(&entry).map_err(|error| error.to_string())?;
        fs::write(self.entry_path(&session.session_id), payload).map_err(|error| error.to_string())
    }

    pub fn list(&self) -> Result<Vec<TrashEntry>, String> {
        let Ok(entries) = fs::read_dir(&self.directory) else {
            return Ok(vec![]);
        };
        let mut result = vec![];
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let Ok(payload) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(trash) = serde_json::from_str::<TrashEntry>(&payload) else {
                continue;
            };
            result.push(trash);
        }
        result.sort_by(|left, right| right.deleted_at.cmp(&left.deleted_at));
        Ok(result)
    }

    pub fn get(&self, session_id: &str) -> Option<TrashEntry> {
        let payload = fs::read_to_string(self.entry_path(session_id)).ok()?;
        serde_json::from_str::<TrashEntry>(&payload).ok()
    }

    pub fn remove(&self, session_id: &str) -> Result<(), String> {
        match fs::remove_file(self.entry_path(session_id)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// Whether a session id is currently retained in the recycle bin. The
    /// watcher treats these ids as tombstones so a partially-deleted source
    /// is not mirrored back while the entry is retained.
    pub fn contains(&self, session_id: &str) -> bool {
        self.entry_path(session_id).exists()
    }

    /// Removes entries older than the retention window. Returns the number
    /// of purged entries.
    pub fn purge_expired(&self, retention: Duration) -> Result<usize, String> {
        let now_ms = Utc::now().timestamp_millis();
        let retention_ms = retention.as_millis() as i64;
        let mut purged = 0;
        for entry in self.list()? {
            let Ok(deleted_at) = DateTime::parse_from_rfc3339(&entry.deleted_at) else {
                continue;
            };
            let age_ms = now_ms - deleted_at.timestamp_millis();
            if age_ms > retention_ms {
                self.remove(&entry.session.session_id)?;
                purged += 1;
            }
        }
        Ok(purged)
    }

    fn entry_path(&self, session_id: &str) -> PathBuf {
        self.directory.join(format!("{session_id}.json"))
    }
}

/// Restores a session from the recycle bin back into the mirror store only
/// (the user can push it to a source tool with the regular restore flow).
pub fn restore_from_trash(
    store: &crate::store::Store,
    trash: &Trash,
    session_id: &str,
) -> Result<CanonicalSession, String> {
    let entry = trash
        .get(session_id)
        .ok_or_else(|| format!("回收站中找不到会话: {session_id}"))?;
    if let Some(conversation_id) = entry.conversation_id.as_deref() {
        store.write_session_with_conversation(&entry.session, None, conversation_id)?;
    } else {
        store.write_session(&entry.session)?;
    }
    trash.remove(session_id)?;
    Ok(entry.session)
}
