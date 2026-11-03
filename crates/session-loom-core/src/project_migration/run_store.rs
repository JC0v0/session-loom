use super::model::{MigrationRun, RunState};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use uuid::Uuid;

const MAX_MANIFEST_BYTES: u64 = 64 * 1024 * 1024;

/// Private, versioned run records; no migration tables in sessions.db.
#[derive(Debug, Clone)]
pub struct RunStore {
    directory: PathBuf,
}

impl RunStore {
    pub fn new(store_root: &Path) -> Result<Self, String> {
        if !store_root.is_dir() {
            return Err("migration store parent must be an existing directory".to_string());
        }
        let directory = store_root.join("migrations");
        private_directory(&directory)?;
        sync_directory(store_root)?;
        Ok(Self { directory })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn run_directory(&self, id: &str) -> Result<PathBuf, String> {
        validate_id(id)?;
        checked_directory(&self.directory)?;
        let directory = self.directory.join(id);
        checked_directory(&directory)?;
        Ok(directory)
    }

    pub fn create(&self, record: &MigrationRun) -> Result<(), String> {
        record.validate()?;
        if record.revision != 0 {
            return Err("new migration must start at revision zero".to_string());
        }
        checked_directory(&self.directory)?;
        let directory = self.directory.join(&record.id);
        fs::create_dir(&directory).map_err(|error| error.to_string())?;
        secure_directory(&directory)?;
        for child in ["snapshots", "staging", "backups", "revisions", "previews"] {
            private_directory(&directory.join(child))?;
        }
        let _lock = acquire_lock(&directory)?;
        persist_record(&directory, record)?;
        sync_directory(&self.directory)
    }

    pub fn load(&self, id: &str) -> Result<MigrationRun, String> {
        let directory = self.run_directory(id)?;
        let record = read_record(&directory.join("record.json"))?;
        if record.id != id {
            return Err("migration record identity does not match its directory".to_string());
        }
        Ok(record)
    }

    /// Compare-and-update under an OS lock. Locks release when the file closes,
    /// including process crashes; the lock inode is intentionally never removed.
    pub fn update(&self, record: &MigrationRun, expected_revision: u64) -> Result<(), String> {
        record.validate()?;
        let next = expected_revision
            .checked_add(1)
            .ok_or_else(|| "migration revision exhausted".to_string())?;
        if record.revision != next {
            return Err("migration revision must increase by one".to_string());
        }
        let directory = self.run_directory(&record.id)?;
        let _lock = acquire_lock(&directory)?;
        let current = self.load(&record.id)?;
        if current.revision != expected_revision {
            return Err("migration revision changed; reload the current preview".to_string());
        }
        if record.created_at != current.created_at || record.project != current.project {
            return Err("migration origin is immutable; create a new run".to_string());
        }
        persist_record(&directory, record)
    }

    pub fn revision(&self, id: &str, revision: u64) -> Result<MigrationRun, String> {
        let directory = self.run_directory(id)?;
        checked_directory(&directory.join("revisions"))?;
        let record = read_record(&directory.join("revisions").join(format!("{revision}.json")))?;
        if record.id != id || record.revision != revision {
            return Err("migration revision identity does not match".to_string());
        }
        Ok(record)
    }

    pub fn list(&self) -> Result<Vec<MigrationRun>, String> {
        checked_directory(&self.directory)?;
        let mut records = vec![];
        for entry in fs::read_dir(&self.directory).map_err(|error| error.to_string())? {
            let entry = entry.map_err(|error| error.to_string())?;
            let id = entry.file_name().to_string_lossy().to_string();
            if validate_id(&id).is_ok() {
                records.push(self.load(&id)?);
            }
        }
        records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
        Ok(records)
    }

    pub(crate) fn previews_directory(&self, run_id: &str) -> Result<PathBuf, String> {
        let directory = self.run_directory(run_id)?;
        let previews = directory.join("previews");
        private_directory(&previews)?;
        Ok(previews)
    }

    /// Remove snapshots, staging, backups, and preview bytes for completed
    /// runs older than the retention window. The run record and immutable
    /// revisions stay readable, target files are never touched, and
    /// unfinished runs are skipped so a recovery attempt keeps its inputs.
    pub fn cleanup_completed_runs(
        &self,
        now: chrono::DateTime<chrono::Utc>,
        retention_days: i64,
    ) -> Result<usize, String> {
        let cutoff = now - chrono::Duration::days(retention_days.max(0));
        let mut cleaned = 0;
        for record in self.list()? {
            if record.state != RunState::Completed {
                continue;
            }
            let Ok(updated_at) = chrono::DateTime::parse_from_rfc3339(&record.updated_at) else {
                continue;
            };
            if updated_at.with_timezone(&chrono::Utc) >= cutoff {
                continue;
            }
            let directory = self.run_directory(&record.id)?;
            let _lock = acquire_lock(&directory)?;
            let mut removed_any = false;
            for child in ["snapshots", "staging", "backups", "previews"] {
                let path = directory.join(child);
                match fs::symlink_metadata(&path) {
                    Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
                        return Err(format!(
                            "migration retention target is not a directory: {}",
                            path.display()
                        ));
                    }
                    Ok(_) => {
                        removed_any |= fs::read_dir(&path)
                            .map(|mut entries| entries.next().is_some())
                            .unwrap_or(false);
                        fs::remove_dir_all(&path).map_err(|error| error.to_string())?;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.to_string()),
                }
                private_directory(&path)?;
            }
            if removed_any {
                cleaned += 1;
            }
        }
        Ok(cleaned)
    }
}

fn validate_id(id: &str) -> Result<(), String> {
    let parsed = Uuid::parse_str(id).map_err(|_| "invalid migration id".to_string())?;
    if parsed.to_string() == id {
        Ok(())
    } else {
        Err("invalid migration id".to_string())
    }
}

pub(crate) fn checked_directory(path: &Path) -> Result<(), String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!(
            "not a regular migration directory: {}",
            path.display()
        ));
    }
    Ok(())
}

fn secure_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    let _ = path;
    Ok(())
}

pub(crate) fn private_directory(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(_) => checked_directory(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|error| error.to_string())?;
        }
        Err(error) => return Err(error.to_string()),
    }
    secure_directory(path)
}

pub(crate) fn private_file(path: &Path, create_new: bool) -> Result<File, String> {
    if !create_new {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err("migration file is not a regular file".to_string());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    if create_new {
        options.create_new(true);
    } else {
        options.create(true).truncate(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|error| error.to_string())
}

fn acquire_lock(directory: &Path) -> Result<File, String> {
    let file = private_file(&directory.join("run.lock"), false)?;
    fs2::FileExt::try_lock_exclusive(&file)
        .map_err(|error| format!("migration is busy or cannot be locked: {error}"))?;
    Ok(file)
}

fn read_record(path: &Path) -> Result<MigrationRun, String> {
    let metadata = fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err("migration record is not a regular file".to_string());
    }
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err("migration manifest is too large".to_string());
    }
    let mut bytes = vec![];
    File::open(path)
        .map_err(|error| error.to_string())?
        .take(MAX_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("migration manifest is too large".to_string());
    }
    let record: MigrationRun = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    record.validate()?;
    Ok(record)
}

fn persist_record(directory: &Path, record: &MigrationRun) -> Result<(), String> {
    checked_directory(&directory.join("revisions"))?;
    let bytes = serde_json::to_vec_pretty(record).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("migration manifest is too large".to_string());
    }
    let revision_path = directory
        .join("revisions")
        .join(format!("{}.json", record.revision));
    let pending_revision = directory.join(format!(".revision-{}.tmp", Uuid::new_v4()));
    let revision_result = (|| {
        let mut file = private_file(&pending_revision, true)?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);
        match fs::hard_link(&pending_revision, &revision_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                // A prior attempt may have persisted the immutable revision
                // before its pointer. Only those exact bytes can be retried.
                if read_record(&revision_path).ok().as_ref() != Some(record) {
                    return Err("immutable migration revision already differs".to_string());
                }
            }
            Err(error) => return Err(error.to_string()),
        }
        sync_directory(&directory.join("revisions"))
    })();
    let _ = fs::remove_file(&pending_revision);
    revision_result?;
    let temporary = directory.join(format!(".record-{}.tmp", Uuid::new_v4()));
    let result = (|| {
        let mut file = private_file(&temporary, true)?;
        file.write_all(&bytes).map_err(|error| error.to_string())?;
        file.sync_all().map_err(|error| error.to_string())?;
        drop(file);
        let current = directory.join("record.json");
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
                return Err("migration record is not a regular file".to_string());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        fs::rename(&temporary, current).map_err(|error| error.to_string())?;
        sync_directory(directory)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())?;
    let _ = path;
    Ok(())
}
