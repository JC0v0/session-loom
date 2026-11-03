use super::{
    discovery::{collect_resources, fingerprint, observe_source, DiscoveryReport},
    model::{
        content_hash, validate_resource_path, AssetSnapshot, EnvironmentAsset, MigrationRun,
        RunState, SelectedSession, SessionSnapshot, SnapshotFile, SourceDocument,
    },
    run_store::{checked_directory, private_directory, private_file, sync_directory, RunStore},
};
use crate::store::Store;
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};
use uuid::Uuid;

/// Capture only explicitly selected assets. All files are staged privately;
/// publication happens only after the full selection passes source checks.
pub fn capture_selection(
    sessions_store: &Store,
    runs: &RunStore,
    run: &MigrationRun,
    report: &DiscoveryReport,
    sessions: &[SelectedSession],
    asset_ids: &[String],
) -> Result<MigrationRun, String> {
    run.validate()?;
    if runs.load(&run.id)? != *run {
        return Err("migration changed; reload before capturing its context".to_string());
    }
    if !matches!(run.state, RunState::Draft | RunState::Previewed) {
        return Err("migration cannot change its snapshot in the current state".to_string());
    }
    if run.project != report.project
        || run.session_ids != report.session_ids
        || run.discovered_assets != report.assets
        || run.discovery_probes != report.source_probes
        || sessions
            .iter()
            .map(|session| &session.metadata.session_id)
            .collect::<Vec<_>>()
            != run.session_ids.iter().collect::<Vec<_>>()
    {
        return Err("snapshot selection does not match the migration origin".to_string());
    }
    if super::select_project_sessions(sessions_store, &run.project.id, &run.session_ids)?
        != sessions
    {
        return Err("selected session changed; refresh the migration".to_string());
    }
    validate_inventory(report)?;
    let unique: HashSet<_> = asset_ids.iter().collect();
    if unique.len() != asset_ids.len() {
        return Err("duplicate selected environment item".to_string());
    }
    let mut selected = vec![];
    for id in asset_ids {
        let asset = report
            .assets
            .iter()
            .find(|asset| &asset.id == id)
            .ok_or_else(|| "selected environment item was not discovered".to_string())?;
        if asset.id.len() != 64 || !asset.id.bytes().all(|value| value.is_ascii_hexdigit()) {
            return Err("invalid environment item identity".to_string());
        }
        validate_sources(asset)?;
        selected.push(asset);
    }
    let directory = runs.run_directory(&run.id)?;
    checked_directory(&directory.join("staging"))?;
    checked_directory(&directory.join("snapshots"))?;
    let snapshot_id = Uuid::new_v4().to_string();
    let staging = directory.join("staging").join(&snapshot_id);
    private_directory(&staging)?;
    let relative_root = PathBuf::from("snapshots").join(&snapshot_id);
    let result = (|| {
        let mut snapshots = vec![];
        for asset in selected {
            snapshots.push(capture_asset(asset, &staging, &relative_root)?);
        }
        let mut session_snapshots = vec![];
        let session_directory = staging.join("sessions");
        private_directory(&session_directory)?;
        for selected in sessions {
            let current = sessions_store.read_session(&selected.metadata.session_id)?;
            let bytes = current.to_storage_json()?.into_bytes();
            if content_hash(&bytes) != selected.content_hash {
                return Err(format!(
                    "selected session changed: {}",
                    selected.metadata.session_id
                ));
            }
            let file_name = format!(
                "{}.json",
                content_hash(selected.metadata.session_id.as_bytes())
            );
            let mut file = private_file(&session_directory.join(&file_name), true)?;
            file.write_all(&bytes).map_err(|error| error.to_string())?;
            file.sync_all().map_err(|error| error.to_string())?;
            let source_document = sessions_store
                .session_source_path(&selected.metadata.session_id)
                .map(PathBuf::from)
                .filter(|path| {
                    path.is_file()
                        && path.extension().and_then(|value| value.to_str()) != Some("db")
                })
                .map(|path| fingerprint(&path))
                .transpose()?;
            session_snapshots.push(SessionSnapshot {
                metadata: selected.metadata.clone(),
                snapshot_path: relative_root.join("sessions").join(file_name),
                content_hash: selected.content_hash.clone(),
                source_document,
            });
        }
        for asset in &snapshots {
            validate_sources(&asset.asset)?;
        }
        validate_inventory(report)?;
        for session in &session_snapshots {
            if let Some(document) = &session.source_document {
                validate_document(document)?;
            }
        }
        let published = directory.join(&relative_root);
        fs::rename(&staging, &published).map_err(|error| error.to_string())?;
        sync_directory(&directory.join("snapshots"))?;
        let mut updated = run.clone();
        updated.revision = run
            .revision
            .checked_add(1)
            .ok_or_else(|| "migration revision exhausted".to_string())?;
        updated.updated_at = Utc::now().to_rfc3339();
        updated.state = RunState::Draft;
        updated.preview = None;
        updated.apply_report = None;
        updated.results.clear();
        updated.discovered_assets = report.assets.clone();
        updated.diagnostics = report.diagnostics.clone();
        updated.assets = snapshots;
        updated.sessions = session_snapshots;
        if let Err(error) = runs.update(&updated, run.revision) {
            // Publication may have succeeded before a durability error. A
            // persisted revision can already reference these immutable bytes.
            return Err(format!(
                "snapshot {snapshot_id} was saved but its run update failed: {error}"
            ));
        }
        Ok(updated)
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    result
}

pub(crate) fn validate_inventory(report: &DiscoveryReport) -> Result<(), String> {
    for probe in &report.source_probes {
        if observe_source(&probe.path, probe.is_pattern)? != *probe {
            return Err(format!(
                "source configuration or inventory changed: {}",
                probe.path.display()
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_document(document: &SourceDocument) -> Result<(), String> {
    if fingerprint(&document.path)? != *document {
        return Err(format!(
            "source changed; refresh the migration: {}",
            document.path.display()
        ));
    }
    Ok(())
}

pub(crate) fn validate_sources(asset: &EnvironmentAsset) -> Result<(), String> {
    for document in &asset.source_documents {
        validate_document(document)?;
    }
    if asset
        .definition
        .as_ref()
        .and_then(|value| value.get("resourceBundle"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        let root = asset
            .resource_root
            .as_ref()
            .ok_or_else(|| "package has no resource root".to_string())?;
        let mut current = vec![];
        let mut diagnostics = vec![];
        collect_resources(
            root,
            root,
            &mut HashSet::new(),
            &mut current,
            &mut diagnostics,
            0,
        );
        if current != asset.files {
            return Err(format!("skill resource tree changed: {}", asset.name));
        }
    }
    Ok(())
}

fn private_parent(root: &Path, relative: &Path) -> Result<PathBuf, String> {
    validate_resource_path(relative)?;
    let mut parent = root.to_path_buf();
    if let Some(parts) = relative.parent() {
        for component in parts.components() {
            if let Component::Normal(part) = component {
                parent.push(part);
                private_directory(&parent)?;
            }
        }
    }
    Ok(parent)
}

fn capture_asset(
    asset: &EnvironmentAsset,
    staging: &Path,
    relative_root: &Path,
) -> Result<AssetSnapshot, String> {
    let target_root = staging.join(&asset.id);
    private_directory(&target_root)?;
    let allowed = asset
        .resource_root
        .as_ref()
        .map(|root| root.canonicalize().map_err(|error| error.to_string()))
        .transpose()?;
    let mut files = vec![];
    for source in &asset.files {
        private_parent(&target_root, &source.relative_path)?;
        let source_path = source
            .path
            .canonicalize()
            .map_err(|error| error.to_string())?;
        if !allowed
            .as_ref()
            .is_some_and(|root| source_path.starts_with(root))
        {
            return Err("resource source leaves its declared package".to_string());
        }
        let expected = asset
            .source_documents
            .iter()
            .find(|document| document.resolved_path == source_path)
            .ok_or_else(|| "resource has no discovery fingerprint".to_string())?;
        let mut input = fs::File::open(&source_path).map_err(|error| error.to_string())?;
        let metadata = input.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() {
            return Err("resource is not a regular file".to_string());
        }
        let mut output = private_file(&target_root.join(&source.relative_path), true)?;
        let mut hash = Sha256::new();
        let mut size = 0;
        let mut buffer = [0; 64 * 1024];
        loop {
            let count = input.read(&mut buffer).map_err(|error| error.to_string())?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
            output
                .write_all(&buffer[..count])
                .map_err(|error| error.to_string())?;
            size += count as u64;
        }
        output.sync_all().map_err(|error| error.to_string())?;
        let hash = format!("{:x}", hash.finalize());
        if hash != expected.content_hash {
            return Err(format!(
                "resource changed during snapshot: {}",
                source.path.display()
            ));
        }
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = false;
        files.push(SnapshotFile {
            source: source_path,
            relative_path: source.relative_path.clone(),
            snapshot_path: relative_root.join(&asset.id).join(&source.relative_path),
            content_hash: hash,
            size,
            executable,
        });
    }
    let combined = serde_json::to_vec(&(asset, &files)).map_err(|error| error.to_string())?;
    Ok(AssetSnapshot {
        asset: asset.clone(),
        files,
        content_hash: content_hash(&combined),
    })
}
