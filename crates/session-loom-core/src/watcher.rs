use crate::{
    adapters::{claude, codex, dsh, opencode},
    canonical::SourceTool,
    store::Store,
    trash::{Trash, TRASH_RETENTION},
};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    thread,
    time::{Duration, UNIX_EPOCH},
};

#[derive(Debug, Clone)]
pub struct WatchTarget {
    pub source_tool: SourceTool,
    pub root: PathBuf,
}

pub struct SessionWatcher {
    store: Store,
    targets: Vec<WatchTarget>,
    seen: HashMap<String, String>,
}

impl SessionWatcher {
    pub fn new(store: Store, targets: Vec<WatchTarget>) -> Self {
        Self {
            store,
            targets,
            seen: HashMap::new(),
        }
    }

    pub fn scan_once(&mut self) {
        let trash = Trash::new(self.store.root());
        let _ = trash.purge_expired(TRASH_RETENTION);
        for target in &self.targets {
            for file in session_files(target) {
                let Ok(metadata) = fs::metadata(&file) else {
                    continue;
                };
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
                    .map(|value| value.as_nanos())
                    .unwrap_or_default();
                let signature = format!("{modified}:{}", metadata.len());
                let key = format!("{}:{}", target.source_tool.as_str(), file.display());
                if self.seen.get(&key) == Some(&signature) {
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
            self.scan_once();
            thread::sleep(interval);
        }
    }
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
        SourceTool::Codex | SourceTool::Claude => {
            let payload = fs::read_to_string(file).map_err(|error| error.to_string())?;
            let session = match source_tool {
                SourceTool::Codex => codex::parse_session(&payload)?,
                SourceTool::Claude => claude::parse_session(&payload)?,
                SourceTool::OpenCode | SourceTool::Dsh => unreachable!(),
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
        SourceTool::Codex | SourceTool::Claude => {
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
