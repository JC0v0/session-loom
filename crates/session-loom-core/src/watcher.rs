use crate::{
    adapters::{claude, codex, dsh, opencode, pi},
    canonical::SourceTool,
    store::Store,
    trash::{Trash, TRASH_RETENTION},
};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const TRASH_PURGE_INTERVAL: Duration = Duration::from_secs(60 * 60);
const ACTIVE_FILE_DEBOUNCE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone)]
pub struct WatchTarget {
    pub source_tool: SourceTool,
    pub root: PathBuf,
}

pub struct SessionWatcher {
    store: Store,
    targets: Vec<WatchTarget>,
    seen: HashMap<String, String>,
    last_trash_purge: Option<Instant>,
}

impl SessionWatcher {
    pub fn new(store: Store, targets: Vec<WatchTarget>) -> Self {
        Self {
            store,
            targets,
            seen: HashMap::new(),
            last_trash_purge: None,
        }
    }

    pub fn scan_once(&mut self) {
        self.scan_once_with_debounce(false);
    }

    fn scan_once_with_debounce(&mut self, debounce_active_files: bool) {
        // If the store was wiped (database deleted and recreated while this
        // watcher kept running), the in-memory signature map would otherwise
        // keep skipping unchanged source files forever. An empty store means
        // a fresh start: drop the seen signatures so every source file is
        // mirrored again. Tombstoned ids are still skipped by mirror_file.
        if !self.store.has_sessions().unwrap_or(true) {
            self.seen.clear();
        }
        let trash = Trash::new(self.store.root());
        let now = Instant::now();
        if should_purge_trash(self.last_trash_purge, now) {
            let _ = trash.purge_expired(TRASH_RETENTION);
            self.last_trash_purge = Some(now);
        }
        for target in &self.targets {
            for file in session_files(target) {
                let Ok(metadata) = fs::metadata(&file) else {
                    continue;
                };
                let modified_at = metadata.modified().ok();
                let modified = modified_at
                    .as_ref()
                    .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                    .map(|value| value.as_nanos())
                    .unwrap_or_default();
                let signature = format!("{modified}:{}", metadata.len());
                let key = format!("{}:{}", target.source_tool.as_str(), file.display());
                if self.seen.get(&key) == Some(&signature) {
                    continue;
                }
                if debounce_active_files
                    && self.seen.contains_key(&key)
                    && is_recently_modified(modified_at)
                {
                    continue;
                }
                if mirror_file(&self.store, target.source_tool, &file, &trash).is_ok() {
                    self.seen.insert(key, signature);
                }
            }
        }
    }

    pub fn run_forever(mut self, interval: Duration) -> ! {
        loop {
            // Beat once per tick so daemon_status can stay cheap: freshness
            // of this file answers "is the mirror alive" without spawning
            // any process-probing helper from the UI's status polls.
            crate::daemon::write_heartbeat(self.store.root());
            self.scan_once_with_debounce(true);
            thread::sleep(interval);
        }
    }
}

fn should_purge_trash(last_purge: Option<Instant>, now: Instant) -> bool {
    last_purge
        .map(|last_purge| now.duration_since(last_purge) >= TRASH_PURGE_INTERVAL)
        .unwrap_or(true)
}

fn is_recently_modified(modified_at: Option<SystemTime>) -> bool {
    modified_at
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .map(|age| age < ACTIVE_FILE_DEBOUNCE)
        .unwrap_or(false)
}

fn mirror_file(
    store: &Store,
    source_tool: SourceTool,
    file: &Path,
    trash: &Trash,
) -> Result<(), String> {
    match source_tool {
        SourceTool::OpenCode => {
            for session in opencode::parse_sessions(file)? {
                if !trash.contains(&session.session_id) {
                    store.write_session_from(&session, Some(file))?;
                }
            }
            Ok(())
        }
        SourceTool::Dsh => {
            if let Some(session) = dsh::parse_session_file(file)? {
                if !trash.contains(&session.session_id) {
                    store.write_session_from(&session, Some(file))?;
                }
            }
            Ok(())
        }
        SourceTool::Pi => {
            let payload = fs::read_to_string(file).map_err(|error| error.to_string())?;
            let session = pi::parse_session(&payload)?;
            if !trash.contains(&session.session_id) {
                store.write_session_from(&session, Some(file))?;
            }
            Ok(())
        }
        SourceTool::Codex | SourceTool::Claude => {
            let payload = fs::read_to_string(file).map_err(|error| error.to_string())?;
            let session = match source_tool {
                SourceTool::Codex => {
                    let mut session = codex::parse_session(&payload)?;
                    codex::attach_index_title(&mut session, &crate::paths::codex_home());
                    session
                }
                SourceTool::Claude => claude::parse_session_file(file)?,
                SourceTool::OpenCode | SourceTool::Dsh | SourceTool::Pi => unreachable!(),
            };
            if !trash.contains(&session.session_id) {
                store.write_session_from(&session, Some(file))?;
            }
            Ok(())
        }
    }
}

fn session_files(target: &WatchTarget) -> Vec<PathBuf> {
    match target.source_tool {
        SourceTool::OpenCode => opencode_session_files(&target.root),
        SourceTool::Dsh => dsh::session_log_files(&target.root),
        SourceTool::Codex | SourceTool::Claude | SourceTool::Pi => {
            let mut files = vec![];
            walk(&target.root, &mut files);
            files
        }
    }
}

fn opencode_session_files(root: &Path) -> Vec<PathBuf> {
    if root.is_file() {
        return vec![root.to_path_buf()];
    }
    let Ok(entries) = fs::read_dir(root) else {
        return vec![];
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.starts_with("opencode") && name.ends_with(".db"))
                .unwrap_or(false)
        })
        .collect()
}

fn walk(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            walk(&path, files);
        } else if file_type.is_file()
            && path.extension().and_then(|value| value.to_str()) == Some("jsonl")
        {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{is_recently_modified, should_purge_trash, ACTIVE_FILE_DEBOUNCE};
    use std::time::{Duration, Instant, SystemTime};

    #[test]
    fn trash_purge_is_due_only_once_per_interval() {
        let start = Instant::now();
        assert!(should_purge_trash(None, start));
        assert!(!should_purge_trash(
            Some(start),
            start + Duration::from_secs(1)
        ));
        assert!(should_purge_trash(
            Some(start),
            start + Duration::from_secs(60 * 60)
        ));
    }

    #[test]
    fn recently_modified_detection_only_debounces_the_active_window() {
        assert!(is_recently_modified(Some(SystemTime::now())));
        assert!(!is_recently_modified(Some(
            SystemTime::now() - ACTIVE_FILE_DEBOUNCE - Duration::from_millis(1)
        )));
        assert!(!is_recently_modified(None));
    }
}
