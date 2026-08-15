use crate::{
    adapters::{claude, codex},
    canonical::SourceTool,
    store::Store,
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
        for target in &self.targets {
            for file in session_files(&target.root) {
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
                if mirror_file(&self.store, target.source_tool, &file).is_ok() {
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

fn mirror_file(store: &Store, source_tool: SourceTool, file: &Path) -> Result<(), String> {
    let payload = fs::read_to_string(file).map_err(|error| error.to_string())?;
    let session = match source_tool {
        SourceTool::Codex => codex::parse_session(&payload)?,
        SourceTool::Claude => claude::parse_session(&payload)?,
    };
    store.write_session(&session)
}

fn session_files(root: &Path) -> Vec<PathBuf> {
    let mut files = vec![];
    walk(root, &mut files);
    files
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
