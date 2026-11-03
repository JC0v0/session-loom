use rusqlite::Connection;
use session_loom_core::{
    project_migration::{resolve_project, MigrationRun, RunState, RunStore},
    store::{Store, SCHEMA_USER_VERSION},
};
use std::fs;

#[test]
fn run_records_are_private_and_separate_from_the_session_database() {
    let temp = tempfile::tempdir().unwrap();
    let sessions = Store::open(temp.path()).unwrap();
    let store = RunStore::new(temp.path()).unwrap();
    let run = MigrationRun::new(
        resolve_project(temp.path().to_str().unwrap()),
        vec!["s1".into()],
    )
    .unwrap();
    store.create(&run).unwrap();
    assert_eq!(store.load(&run.id).unwrap(), run);
    assert_eq!(store.revision(&run.id, 0).unwrap(), run);
    assert_eq!(store.list().unwrap(), vec![run.clone()]);
    assert!(store.create(&run).is_err());
    let connection = Connection::open(sessions.db_path()).unwrap();
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, i32>(0))
            .unwrap(),
        SCHEMA_USER_VERSION
    );
    let migration_tables: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name LIKE '%migration%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(migration_tables, 0);
    assert!(sessions.project_sessions().unwrap().is_empty());
}

#[test]
fn updates_require_the_current_revision_and_preserve_old_revisions() {
    let temp = tempfile::tempdir().unwrap();
    let store = RunStore::new(temp.path()).unwrap();
    let original = MigrationRun::new(
        resolve_project(temp.path().to_str().unwrap()),
        vec!["s1".into()],
    )
    .unwrap();
    store.create(&original).unwrap();
    let mut changed = original.clone();
    changed.revision = 1;
    changed.state = RunState::Previewed;
    store.update(&changed, 0).unwrap();
    assert_eq!(store.load(&original.id).unwrap(), changed);
    assert_eq!(store.revision(&original.id, 0).unwrap(), original);
    assert_eq!(store.revision(&original.id, 1).unwrap(), changed);
    let mut stale = changed.clone();
    stale.state = RunState::Applying;
    assert!(store
        .update(&stale, 0)
        .unwrap_err()
        .contains("revision changed"));
    assert_eq!(store.load(&original.id).unwrap(), changed);
    changed.revision = 2;
    changed.project.id = "another-project".to_string();
    assert!(store
        .update(&changed, 1)
        .unwrap_err()
        .contains("origin is immutable"));
}

#[test]
fn unknown_versions_and_directory_identity_mismatches_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let store = RunStore::new(temp.path()).unwrap();
    let mut run = MigrationRun::new(
        resolve_project(temp.path().to_str().unwrap()),
        vec!["s1".into()],
    )
    .unwrap();
    store.create(&run).unwrap();
    run.schema_version += 1;
    let path = store.run_directory(&run.id).unwrap().join("record.json");
    fs::write(&path, serde_json::to_vec(&run).unwrap()).unwrap();
    assert!(store
        .load(&run.id)
        .unwrap_err()
        .contains("unsupported migration schema"));
    run.schema_version -= 1;
    let directory_id = run.id.clone();
    run.id = uuid::Uuid::new_v4().to_string();
    fs::write(&path, serde_json::to_vec(&run).unwrap()).unwrap();
    assert!(store.load(&directory_id).unwrap_err().contains("identity"));
    for id in ["../other", "/tmp/other", "", "invalid"] {
        assert!(store.load(id).is_err());
    }
}

#[test]
fn a_busy_run_rejects_updates_and_releases_the_lock_without_deleting_it() {
    let temp = tempfile::tempdir().unwrap();
    let store = RunStore::new(temp.path()).unwrap();
    let run = MigrationRun::new(
        resolve_project(temp.path().to_str().unwrap()),
        vec!["s1".into()],
    )
    .unwrap();
    store.create(&run).unwrap();
    let lock_path = store.run_directory(&run.id).unwrap().join("run.lock");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&lock_path)
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    let mut next = run;
    next.revision = 1;
    assert!(store.update(&next, 0).unwrap_err().contains("busy"));
    drop(lock);
    store.update(&next, 0).unwrap();
    assert!(lock_path.is_file());
}

#[test]
fn a_revision_saved_before_its_pointer_is_published_can_be_retried() {
    let temp = tempfile::tempdir().unwrap();
    let store = RunStore::new(temp.path()).unwrap();
    let original = MigrationRun::new(
        resolve_project(temp.path().to_str().unwrap()),
        vec!["s1".into()],
    )
    .unwrap();
    store.create(&original).unwrap();
    let mut next = original.clone();
    next.revision = 1;
    next.state = RunState::Previewed;
    let revision = store
        .run_directory(&original.id)
        .unwrap()
        .join("revisions/1.json");
    fs::write(&revision, serde_json::to_vec_pretty(&next).unwrap()).unwrap();
    store.update(&next, 0).unwrap();
    assert_eq!(store.load(&original.id).unwrap(), next);
    assert_eq!(store.revision(&original.id, 0).unwrap(), original);
}

#[test]
fn an_existing_different_revision_cannot_be_overwritten_or_published() {
    let temp = tempfile::tempdir().unwrap();
    let store = RunStore::new(temp.path()).unwrap();
    let original = MigrationRun::new(
        resolve_project(temp.path().to_str().unwrap()),
        vec!["s1".into()],
    )
    .unwrap();
    store.create(&original).unwrap();
    let mut next = original.clone();
    next.revision = 1;
    let mut other = next.clone();
    other.state = RunState::Partial;
    let revision = store
        .run_directory(&original.id)
        .unwrap()
        .join("revisions/1.json");
    let bytes = serde_json::to_vec_pretty(&other).unwrap();
    fs::write(&revision, &bytes).unwrap();
    assert!(store
        .update(&next, 0)
        .unwrap_err()
        .contains("already differs"));
    assert_eq!(store.load(&original.id).unwrap(), original);
    assert_eq!(fs::read(revision).unwrap(), bytes);
}

#[cfg(unix)]
#[test]
fn private_permissions_and_symlink_boundaries_are_enforced() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let temp = tempfile::tempdir().unwrap();
    let store = RunStore::new(temp.path()).unwrap();
    let run = MigrationRun::new(
        resolve_project(temp.path().to_str().unwrap()),
        vec!["s1".into()],
    )
    .unwrap();
    store.create(&run).unwrap();
    let directory = store.run_directory(&run.id).unwrap();
    assert_eq!(
        fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(directory.join("record.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let record = directory.join("record.json");
    let outside = temp.path().join("outside.json");
    fs::rename(&record, &outside).unwrap();
    symlink(&outside, &record).unwrap();
    assert!(store
        .load(&run.id)
        .unwrap_err()
        .contains("not a regular file"));

    let another = tempfile::tempdir().unwrap();
    symlink(temp.path(), another.path().join("migrations")).unwrap();
    assert!(RunStore::new(another.path()).is_err());
}
