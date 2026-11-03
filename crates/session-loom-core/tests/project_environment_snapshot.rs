use session_loom_core::{
    canonical::SourceTool,
    project_migration::{
        capture_selection, discover_environment, resolve_project, select_project_sessions,
        AssetKind, AssetScope, DiscoveryOptions, DiscoveryReport, EnvironmentRoots, MigrationRun,
        RunStore, SelectedSession,
    },
    store::Store,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[path = "common/mod.rs"]
mod common;

fn write(path: &Path, bytes: &[u8]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, bytes).unwrap();
}

struct Fixture {
    _temp: tempfile::TempDir,
    project: PathBuf,
    roots: EnvironmentRoots,
    sessions_store: Store,
    runs: RunStore,
    selected: Vec<SelectedSession>,
    report: DiscoveryReport,
    run: MigrationRun,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let roots = EnvironmentRoots::isolated(&temp.path().join("roots"));
        write(
            &project.join("AGENTS.md"),
            b"Synthetic current project instructions.\n",
        );
        write(&project.join(".agents/skills/check/SKILL.md"), b"---\nname: check\ndescription: Synthetic check skill\n---\nRead references/rules.md.\n");
        write(
            &project.join(".agents/skills/check/references/rules.md"),
            b"Synthetic version one.\n",
        );
        write(
            &project.join(".agents/skills/check/scripts/check.sh"),
            b"#!/bin/sh\nprintf fixture\n",
        );
        write(
            &roots.codex.join("memories/selected.md"),
            b"Synthetic selected global memory.\n",
        );
        write(
            &roots.codex.join("memories/unselected.md"),
            b"Synthetic unselected global memory.\n",
        );
        let sessions_store = Store::open(&temp.path().join("store")).unwrap();
        let mut session = common::migration_session("chosen", &project, SourceTool::Codex);
        session.updated_at = "2026-10-01T00:00:00Z".into();
        sessions_store.write_session(&session).unwrap();
        sessions_store
            .write_session(&common::migration_session(
                "unchosen",
                &project,
                SourceTool::Claude,
            ))
            .unwrap();
        let identity = resolve_project(project.to_str().unwrap());
        let selected =
            select_project_sessions(&sessions_store, &identity.id, &["chosen".into()]).unwrap();
        let report =
            discover_environment(&identity, &selected, &roots, &DiscoveryOptions::default())
                .unwrap();
        let mut run = MigrationRun::new(identity, vec!["chosen".into()]).unwrap();
        run.discovered_assets = report.assets.clone();
        run.discovery_probes = report.source_probes.clone();
        let runs = RunStore::new(sessions_store.root()).unwrap();
        runs.create(&run).unwrap();
        Self {
            _temp: temp,
            project,
            roots,
            sessions_store,
            runs,
            selected,
            report,
            run,
        }
    }

    fn project_items(&self) -> Vec<String> {
        self.report
            .assets
            .iter()
            .filter(|asset| asset.scope == AssetScope::Project)
            .map(|asset| asset.id.clone())
            .collect()
    }

    fn rediscover_as(&mut self, tool: SourceTool) {
        let mut session = self.selected[0].session.clone();
        session.source_tool = tool;
        self.sessions_store.write_session(&session).unwrap();
        self.selected = select_project_sessions(
            &self.sessions_store,
            &self.run.project.id,
            &["chosen".into()],
        )
        .unwrap();
        self.report = discover_environment(
            &self.run.project,
            &self.selected,
            &self.roots,
            &Default::default(),
        )
        .unwrap();
        self.run = MigrationRun::new(self.run.project.clone(), vec!["chosen".into()]).unwrap();
        self.run.discovered_assets = self.report.assets.clone();
        self.run.discovery_probes = self.report.source_probes.clone();
        self.runs.create(&self.run).unwrap();
    }

    fn capture(&self, ids: &[String]) -> Result<MigrationRun, String> {
        capture_selection(
            &self.sessions_store,
            &self.runs,
            &self.run,
            &self.report,
            &self.selected,
            ids,
        )
    }
}

#[test]
fn snapshot_contains_only_chosen_sessions_and_explicit_global_items() {
    let fixture = Fixture::new();
    let mut ids = fixture.project_items();
    let global = fixture
        .report
        .assets
        .iter()
        .find(|asset| asset.scope == AssetScope::User && asset.name == "selected.md")
        .unwrap();
    ids.push(global.id.clone());
    let captured = fixture.capture(&ids).unwrap();
    assert_eq!(captured.assets.len(), ids.len());
    assert_eq!(captured.sessions.len(), 1);
    assert_eq!(captured.sessions[0].metadata.session_id, "chosen");
    assert_eq!(
        captured.sessions[0].metadata.updated_at,
        "2026-10-01T00:00:00Z"
    );
    assert!(!captured
        .assets
        .iter()
        .any(|asset| asset.asset.name == "unselected.md"));
    let directory = fixture.runs.run_directory(&captured.id).unwrap();
    let bundle = captured
        .assets
        .iter()
        .find(|asset| asset.asset.kind == AssetKind::Skill)
        .unwrap();
    let file = bundle
        .files
        .iter()
        .find(|file| file.relative_path == Path::new("references/rules.md"))
        .unwrap();
    assert_eq!(
        fs::read(directory.join(&file.snapshot_path)).unwrap(),
        b"Synthetic version one.\n"
    );
    assert_eq!(
        fs::read(fixture.project.join("AGENTS.md")).unwrap(),
        b"Synthetic current project instructions.\n"
    );
    assert_eq!(fixture.sessions_store.project_sessions().unwrap().len(), 2);
    assert_eq!(fixture.runs.revision(&captured.id, 0).unwrap(), fixture.run);
    assert_eq!(captured.revision, 1);
}

#[test]
fn changing_a_resource_after_discovery_rejects_the_entire_capture() {
    let fixture = Fixture::new();
    write(
        &fixture
            .project
            .join(".agents/skills/check/references/rules.md"),
        b"Synthetic version two.\n",
    );
    assert!(fixture
        .capture(&fixture.project_items())
        .unwrap_err()
        .contains("source changed"));
    assert_eq!(fixture.runs.load(&fixture.run.id).unwrap(), fixture.run);
    let directory = fixture.runs.run_directory(&fixture.run.id).unwrap();
    assert_eq!(
        fs::read_dir(directory.join("snapshots")).unwrap().count(),
        0
    );
    assert_eq!(fs::read_dir(directory.join("staging")).unwrap().count(), 0);
}

#[test]
fn adding_a_skill_resource_after_discovery_is_also_a_change() {
    let fixture = Fixture::new();
    write(
        &fixture
            .project
            .join(".agents/skills/check/references/new.md"),
        b"Synthetic added resource.\n",
    );
    assert!(fixture
        .capture(&fixture.project_items())
        .unwrap_err()
        .contains("resource tree changed"));
    assert_eq!(fixture.runs.load(&fixture.run.id).unwrap().revision, 0);
}

#[test]
fn snapshot_preserves_bytes_after_source_changes_without_starting_a_sync() {
    let fixture = Fixture::new();
    let captured = fixture.capture(&fixture.project_items()).unwrap();
    let directory = fixture.runs.run_directory(&captured.id).unwrap();
    let instruction = captured
        .assets
        .iter()
        .find(|asset| asset.asset.kind == AssetKind::Instruction)
        .unwrap();
    let path = directory.join(&instruction.files[0].snapshot_path);
    let original = fs::read(&path).unwrap();
    write(
        &fixture.project.join("AGENTS.md"),
        b"Changed after snapshot.\n",
    );
    assert_eq!(fs::read(path).unwrap(), original);
    assert_eq!(fixture.runs.load(&captured.id).unwrap(), captured);
}

#[test]
fn changed_chat_payload_or_forged_inventory_cannot_enter_the_snapshot() {
    let fixture = Fixture::new();
    let mut report = fixture.report.clone();
    report.assets[0].files.clear();
    assert!(capture_selection(
        &fixture.sessions_store,
        &fixture.runs,
        &fixture.run,
        &report,
        &fixture.selected,
        &fixture.project_items()
    )
    .unwrap_err()
    .contains("selection does not match"));
    let mut session = fixture.selected[0].session.clone();
    session.messages[0].text.push_str(" changed");
    fixture.sessions_store.write_session(&session).unwrap();
    assert!(fixture
        .capture(&fixture.project_items())
        .unwrap_err()
        .contains("selected session changed"));
    assert_eq!(fixture.runs.load(&fixture.run.id).unwrap().revision, 0);
}

#[test]
fn an_empty_environment_can_still_capture_the_chosen_session() {
    let fixture = Fixture::new();
    let captured = fixture.capture(&[]).unwrap();
    assert!(captured.assets.is_empty());
    assert_eq!(captured.sessions.len(), 1);
    let directory = fixture.runs.run_directory(&captured.id).unwrap();
    let bytes = fs::read(directory.join(&captured.sessions[0].snapshot_path)).unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["sessionId"], "chosen");
}

#[test]
fn published_snapshot_bytes_are_preserved_when_the_run_cannot_be_updated() {
    let fixture = Fixture::new();
    let directory = fixture.runs.run_directory(&fixture.run.id).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(directory.join("run.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&lock).unwrap();
    assert!(fixture
        .capture(&fixture.project_items())
        .unwrap_err()
        .contains("was saved but its run update failed"));
    assert_eq!(
        fs::read_dir(directory.join("snapshots")).unwrap().count(),
        1
    );
    assert_eq!(fixture.runs.load(&fixture.run.id).unwrap(), fixture.run);
    drop(lock);
    let captured = fixture.capture(&fixture.project_items()).unwrap();
    assert_eq!(captured.revision, 1);
    assert_eq!(
        fs::read_dir(directory.join("snapshots")).unwrap().count(),
        2
    );
}

#[test]
fn adding_a_higher_priority_instruction_invalidates_the_inventory() {
    let fixture = Fixture::new();
    write(
        &fixture.project.join("AGENTS.override.md"),
        b"A newer instruction authority.\n",
    );
    assert!(fixture
        .capture(&fixture.project_items())
        .unwrap_err()
        .contains("inventory changed"));
    assert_eq!(fixture.runs.load(&fixture.run.id).unwrap(), fixture.run);
}

#[test]
fn a_changed_configuration_reference_rejects_an_unchanged_memory_file() {
    let mut fixture = Fixture::new();
    let memory = fixture.roots.user_home.join("custom-memory");
    write(
        &memory.join("MEMORY.md"),
        b"Synthetic memory remains unchanged.",
    );
    let settings = fixture.project.join(".claude/settings.json");
    write(
        &settings,
        serde_json::json!({"autoMemoryDirectory":memory})
            .to_string()
            .as_bytes(),
    );
    let mut projects = serde_json::Map::new();
    projects.insert(
        fixture
            .project
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string(),
        serde_json::json!({"hasTrustDialogAccepted":true}),
    );
    write(
        &fixture.roots.claude_user_config,
        serde_json::json!({"projects":projects})
            .to_string()
            .as_bytes(),
    );
    fixture.rediscover_as(SourceTool::Claude);
    assert!(fixture
        .report
        .assets
        .iter()
        .any(|asset| asset.kind == AssetKind::Memory && asset.scope == AssetScope::Project));
    write(
        &settings,
        b"{\"autoMemoryDirectory\":\"/missing-new-memory\"}",
    );
    assert!(fixture
        .capture(&fixture.project_items())
        .unwrap_err()
        .contains("inventory changed"));
}

#[test]
fn new_files_matching_an_instruction_glob_invalidate_its_inventory() {
    let mut fixture = Fixture::new();
    write(
        &fixture.project.join("opencode.json"),
        b"{\"instructions\":[\"rules/*.md\"]}",
    );
    write(
        &fixture.project.join("rules/first.md"),
        b"First synthetic rule.",
    );
    fixture.rediscover_as(SourceTool::OpenCode);
    write(
        &fixture.project.join("rules/second.md"),
        b"Newly introduced synthetic rule.",
    );
    assert!(fixture
        .capture(&fixture.project_items())
        .unwrap_err()
        .contains("inventory changed"));
}

#[cfg(unix)]
#[test]
fn executable_bits_are_recorded_but_staged_resources_are_not_executable() {
    use std::os::unix::fs::PermissionsExt;
    let mut fixture = Fixture::new();
    let script = fixture
        .project
        .join(".agents/skills/check/scripts/check.sh");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    // File bytes and resource identities are unchanged; permissions are
    // captured as data, never used to execute a source or staged resource.
    fixture.report = discover_environment(
        &fixture.run.project,
        &fixture.selected,
        &fixture.roots,
        &Default::default(),
    )
    .unwrap();
    let captured = fixture.capture(&fixture.project_items()).unwrap();
    let file = captured
        .assets
        .iter()
        .flat_map(|asset| &asset.files)
        .find(|file| file.relative_path == Path::new("scripts/check.sh"))
        .unwrap();
    assert!(file.executable);
    let staged = fixture
        .runs
        .run_directory(&captured.id)
        .unwrap()
        .join(&file.snapshot_path);
    assert_eq!(
        fs::metadata(staged).unwrap().permissions().mode() & 0o777,
        0o600
    );
}
