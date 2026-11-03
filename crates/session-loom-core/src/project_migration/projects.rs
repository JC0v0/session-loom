use super::model::{
    content_hash, Diagnostic, ProjectEntry, ProjectIdentity, ProjectSession, SelectedSession,
};
use crate::store::Store;
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
};

fn is_windows_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\'))
        || value.starts_with("\\\\")
}

fn normalized_path(value: &str) -> String {
    if is_windows_path(value) || cfg!(windows) {
        let mut value = value.replace('\\', "/");
        while value.ends_with('/') && value.len() > 3 {
            value.pop();
        }
        if value.as_bytes().get(1) == Some(&b':') {
            value.replace_range(..1, &value[..1].to_ascii_lowercase());
        }
        value
    } else {
        let trimmed = value.trim_end_matches('/');
        if trimmed.is_empty() && value.starts_with('/') {
            "/".to_string()
        } else {
            trimmed.to_string()
        }
    }
}

fn identity(root: PathBuf, accessible: bool, diagnostics: Vec<Diagnostic>) -> ProjectIdentity {
    let key = normalized_path(&root.to_string_lossy());
    ProjectIdentity {
        id: content_hash(key.as_bytes()),
        root,
        accessible,
        git_common_dir: None,
        diagnostics,
    }
}

fn common_git_directory(marker: &Path) -> Result<PathBuf, String> {
    let metadata = fs::metadata(marker).map_err(|error| error.to_string())?;
    let git_dir = if metadata.is_dir() {
        marker.to_path_buf()
    } else {
        let text = fs::read_to_string(marker).map_err(|error| error.to_string())?;
        let value = text
            .trim()
            .strip_prefix("gitdir:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "invalid Git worktree marker".to_string())?;
        let value = Path::new(value);
        if value.is_absolute() {
            value.to_path_buf()
        } else {
            marker
                .parent()
                .ok_or_else(|| "Git marker has no parent".to_string())?
                .join(value)
        }
    };
    let common = match fs::read_to_string(git_dir.join("commondir")) {
        Ok(value) => git_dir.join(value.trim()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => git_dir,
        Err(error) => return Err(error.to_string()),
    };
    common.canonicalize().map_err(|error| error.to_string())
}

/// Resolve the current working tree without executing Git or reading chats.
/// Missing/foreign paths remain visible as logical projects with diagnostics.
pub fn resolve_project(cwd: &str) -> ProjectIdentity {
    let logical = PathBuf::from(normalized_path(cwd));
    if cwd.is_empty() || (!cfg!(windows) && is_windows_path(cwd)) || !logical.is_absolute() {
        return identity(
            logical.clone(),
            false,
            vec![Diagnostic::new(
                "projectNotLocal",
                "project directory is not an absolute path on this platform",
                Some(logical),
            )],
        );
    }
    let directory = match logical.canonicalize() {
        Ok(path) if path.is_dir() => path,
        Ok(_) => {
            return identity(
                logical.clone(),
                false,
                vec![Diagnostic::new(
                    "projectNotDirectory",
                    "project path is not a directory",
                    Some(logical),
                )],
            );
        }
        Err(error) => {
            return identity(
                logical.clone(),
                false,
                vec![Diagnostic::new(
                    "projectUnavailable",
                    error.to_string(),
                    Some(logical),
                )],
            );
        }
    };

    for ancestor in directory.ancestors() {
        let marker = ancestor.join(".git");
        match fs::symlink_metadata(&marker) {
            Ok(_) => {
                let mut project = identity(ancestor.to_path_buf(), true, vec![]);
                match common_git_directory(&marker) {
                    Ok(common) => project.git_common_dir = Some(common),
                    Err(error) => project.diagnostics.push(Diagnostic::new(
                        "gitMetadataUnavailable",
                        error,
                        Some(marker),
                    )),
                }
                return project;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return identity(
                    directory,
                    false,
                    vec![Diagnostic::new(
                        "projectRootProbeFailed",
                        error.to_string(),
                        Some(marker),
                    )],
                );
            }
        }
    }
    identity(directory, true, vec![])
}

pub fn list_projects(store: &Store) -> Result<Vec<ProjectEntry>, String> {
    let mut projects: BTreeMap<String, ProjectEntry> = BTreeMap::new();
    let mut cwd_cache = HashMap::new();
    for session in store.project_sessions()? {
        let project = cwd_cache
            .entry(session.cwd.clone())
            .or_insert_with(|| resolve_project(&session.cwd));
        projects
            .entry(project.id.clone())
            .or_insert_with(|| ProjectEntry {
                project: project.clone(),
                sessions: vec![],
            })
            .sessions
            .push(session);
    }
    let mut entries: Vec<_> = projects.into_values().collect();
    entries.sort_by(|left, right| left.project.root.cmp(&right.project.root));
    Ok(entries)
}

/// Validate the complete selection before loading any conversation bodies.
pub fn select_project_sessions(
    store: &Store,
    project_id: &str,
    session_ids: &[String],
) -> Result<Vec<SelectedSession>, String> {
    if session_ids.is_empty() {
        return Err("select at least one native session".to_string());
    }
    if session_ids.iter().collect::<HashSet<_>>().len() != session_ids.len() {
        return Err("duplicate selected session id".to_string());
    }
    let project = list_projects(store)?
        .into_iter()
        .find(|entry| entry.project.id == project_id)
        .ok_or_else(|| "project not found".to_string())?;
    let metadata: HashMap<_, _> = project
        .sessions
        .into_iter()
        .map(|session| (session.session_id.clone(), session))
        .collect();
    for id in session_ids {
        if !metadata.contains_key(id) {
            return Err(format!("session is not in the selected project: {id}"));
        }
    }
    session_ids
        .iter()
        .map(|id| {
            let metadata: ProjectSession = metadata[id].clone();
            let session = store.read_session(id)?;
            if session.session_id != metadata.session_id
                || session.source_tool != metadata.source_tool
                || session.cwd != metadata.cwd
                || session.updated_at != metadata.updated_at
            {
                return Err(format!("session changed during selection: {id}"));
            }
            let content_hash = content_hash(session.to_storage_json()?.as_bytes());
            Ok(SelectedSession {
                metadata,
                content_hash,
                session,
            })
        })
        .collect()
}
