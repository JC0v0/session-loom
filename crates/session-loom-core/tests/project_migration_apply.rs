use session_loom_core::{
    canonical::SourceTool,
    project_migration::{
        apply_preview, capture_selection, create_preview, discover_environment, resolve_project,
        select_project_sessions, AssetKind, DiscoveryOptions, EnvironmentRoots, MigrationRun,
        MigrationTarget, OpenCodeConfigVersion, Readiness, RunState, RunStore, SelectedSession,
        TargetEnvironment, VersionSupport,
    },
    restore::RestoreRoots,
    store::Store,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

#[path = "common/mod.rs"]
mod common;

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

struct Fixture {
    _temp: tempfile::TempDir,
    source_project: PathBuf,
    target_project: PathBuf,
    target_roots: EnvironmentRoots,
    restore_roots: RestoreRoots,
    sessions: Store,
    runs: RunStore,
    selected: Vec<SelectedSession>,
    report: session_loom_core::project_migration::DiscoveryReport,
    base_run: MigrationRun,
}

impl Fixture {
    fn new(source: SourceTool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let source_project = temp.path().join("source-project");
        let target_project = temp.path().join("target-project");
        let source_roots = EnvironmentRoots::isolated(&temp.path().join("source-roots"));
        let target_roots = EnvironmentRoots::isolated(&temp.path().join("target-roots"));
        write(
            &source_project.join("AGENTS.md"),
            "Synthetic project instructions.\n",
        );
        write(
            &source_project.join("MEMORY.md"),
            "Synthetic project memory.",
        );
        write(
            &source_project.join("package.json"),
            r#"{"scripts":{"check":"./scripts/check.sh"}}"#,
        );
        write(
            &source_project.join("scripts/check.sh"),
            "#!/bin/sh\nprintf 'fixture'\n",
        );
        match source {
            SourceTool::Codex => {
                write(
                    &source_project.join(".agents/skills/shared/SKILL.md"),
                    "---\nname: shared\ndescription: Source skill\n---\nSource body.\n",
                );
                write(
                    &source_project.join(".agents/skills/shared/references/details.md"),
                    "Synthetic details.\n",
                );
                write(
                    &source_project.join(".codex/config.toml"),
                    "[mcp_servers.database]\ncommand = \"fixture-mcp\"\n",
                );
            }
            _ => unimplemented!("this fixture currently covers the Codex source"),
        }
        let sessions = Store::open(&temp.path().join("store")).unwrap();
        sessions
            .write_session(&common::migration_session(
                "selected",
                &source_project,
                source,
            ))
            .unwrap();
        sessions
            .write_session(&common::migration_session(
                "unselected",
                &source_project,
                source,
            ))
            .unwrap();
        let identity = resolve_project(source_project.to_str().unwrap());
        let selected =
            select_project_sessions(&sessions, &identity.id, &["selected".into()]).unwrap();
        let report = discover_environment(
            &identity,
            &selected,
            &source_roots,
            &DiscoveryOptions::default(),
        )
        .unwrap();
        let mut base_run = MigrationRun::new(identity, vec!["selected".into()]).unwrap();
        base_run.discovered_assets = report.assets.clone();
        base_run.discovery_probes = report.source_probes.clone();
        let runs = RunStore::new(sessions.root()).unwrap();
        runs.create(&base_run).unwrap();
        let restore_roots = RestoreRoots {
            codex: temp.path().join("target-roots/codex-sessions"),
            codex_home: temp.path().join("target-roots/codex-home"),
            claude: temp.path().join("target-roots/claude"),
            opencode: temp.path().join("target-roots/opencode/opencode.db"),
            dsh: temp.path().join("target-roots/dsh-sessions"),
            pi: temp.path().join("target-roots/pi"),
        };
        Self {
            _temp: temp,
            source_project,
            target_project,
            target_roots,
            restore_roots,
            sessions,
            runs,
            selected,
            report,
            base_run,
        }
    }

    fn capture(&self) -> MigrationRun {
        let ids = self
            .report
            .assets
            .iter()
            .map(|asset| asset.id.clone())
            .collect::<Vec<_>>();
        capture_selection(
            &self.sessions,
            &self.runs,
            &self.base_run,
            &self.report,
            &self.selected,
            &ids,
        )
        .unwrap()
    }

    fn environment(&self, tool: SourceTool) -> TargetEnvironment {
        TargetEnvironment {
            target: MigrationTarget {
                tool,
                project_root: self.target_project.clone(),
            },
            roots: EnvironmentRoots::isolated(&self.target_roots.user_home),
            session_roots: Some(session_loom_core::project_migration::TargetSessionRoots {
                codex: self.restore_roots.codex.clone(),
                codex_home: self.restore_roots.codex_home.clone(),
                claude: self.restore_roots.claude.clone(),
                opencode: self.restore_roots.opencode.clone(),
                dsh: self.restore_roots.dsh.clone(),
                pi: self.restore_roots.pi.clone(),
            }),
            opencode_version: OpenCodeConfigVersion::V2,
            pi_mcp: VersionSupport::Supported,
            dsh_profile: Some("fixture".into()),
            project_trusted: true,
            executable_search_path: vec![self._temp.path().join("bin")],
            environment_variables: vec!["FIXTURE_TOKEN".into()],
        }
    }

    fn previewed(
        &self,
        tool: SourceTool,
    ) -> (
        MigrationRun,
        session_loom_core::project_migration::PreviewRevision,
        TargetEnvironment,
    ) {
        let environment = self.environment(tool);
        let captured = self.capture();
        let mut run = captured;
        run.target = Some(environment.target.clone());
        run.revision += 1;
        self.runs.update(&run, run.revision - 1).unwrap();
        let (run, preview) = create_preview(&self.runs, &run, &environment, &[]).unwrap();
        (run, preview, environment)
    }
}

fn claude_session_files(root: &Path) -> Vec<PathBuf> {
    let mut files = vec![];
    let projects = root.join("projects");
    let Ok(directories) = fs::read_dir(&projects) else {
        return files;
    };
    for directory in directories.flatten() {
        if let Ok(entries) = fs::read_dir(directory.path()) {
            for entry in entries.flatten() {
                if entry.path().extension().and_then(|value| value.to_str()) == Some("jsonl") {
                    files.push(entry.path());
                }
            }
        }
    }
    files
}

#[test]
fn apply_writes_environment_and_only_the_selected_session() {
    let fixture = Fixture::new(SourceTool::Codex);
    let (run, preview, _environment) = fixture.previewed(SourceTool::Claude);
    assert!(preview.ready_to_apply);
    let source_instruction = fs::read(fixture.source_project.join("AGENTS.md")).unwrap();

    let report = apply_preview(
        &fixture.sessions,
        &fixture.runs,
        &run.id,
        &preview.id,
        &fixture.restore_roots,
    )
    .unwrap();

    assert_eq!(report.state, RunState::Partial);
    assert_eq!(report.sessions.len(), 1);
    let session = &report.sessions[0];
    assert_eq!(session.readiness, Readiness::Available);
    let target_instructions = fixture.target_project.join("CLAUDE.md");
    let text = fs::read_to_string(&target_instructions).unwrap();
    assert!(text.contains("Synthetic project instructions."), "{text}");
    assert!(text.contains("Synthetic project memory."), "{text}");
    assert!(fixture
        .target_project
        .join(".claude/skills/shared/SKILL.md")
        .is_file());
    assert!(fixture
        .target_project
        .join(".claude/skills/shared/references/details.md")
        .is_file());
    assert!(fixture.target_project.join("scripts/check.sh").is_file());
    let files = claude_session_files(&fixture.restore_roots.claude);
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0]
        .file_stem()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value == session.target_session_id));
    let linked = fixture
        .sessions
        .read_session(&session.target_session_id)
        .unwrap();
    assert_eq!(linked.source_tool, SourceTool::Claude);
    assert_eq!(linked.cwd, fixture.target_project.to_string_lossy());
    assert_eq!(
        fs::read(fixture.source_project.join("AGENTS.md")).unwrap(),
        source_instruction
    );
    let stored = fixture.runs.load(&run.id).unwrap();
    assert_eq!(stored.state, RunState::Partial);
    assert!(stored.apply_report.is_some());

    let again = apply_preview(
        &fixture.sessions,
        &fixture.runs,
        &run.id,
        &preview.id,
        &fixture.restore_roots,
    )
    .unwrap();
    assert_eq!(
        again.sessions[0].target_session_id,
        session.target_session_id
    );
    assert_eq!(
        claude_session_files(&fixture.restore_roots.claude).len(),
        1,
        "a repeated apply must not create a second native session"
    );
}

#[test]
fn interrupted_link_is_recovered_without_duplicating_the_native_session() {
    let fixture = Fixture::new(SourceTool::Codex);
    let (run, preview, _environment) = fixture.previewed(SourceTool::Claude);
    let report = apply_preview(
        &fixture.sessions,
        &fixture.runs,
        &run.id,
        &preview.id,
        &fixture.restore_roots,
    )
    .unwrap();
    let target_id = report.sessions[0].target_session_id.clone();
    assert_eq!(claude_session_files(&fixture.restore_roots.claude).len(), 1);

    let connection = rusqlite::Connection::open(fixture.sessions.db_path()).unwrap();
    connection
        .execute("DELETE FROM sessions WHERE session_id = ?1", [&target_id])
        .unwrap();
    drop(connection);
    assert!(fixture.sessions.read_session(&target_id).is_err());

    let mut interrupted = fixture.runs.load(&run.id).unwrap();
    interrupted.revision += 1;
    interrupted.state = RunState::Partial;
    interrupted.apply_report = None;
    fixture
        .runs
        .update(&interrupted, interrupted.revision - 1)
        .unwrap();

    let recovered = apply_preview(
        &fixture.sessions,
        &fixture.runs,
        &run.id,
        &preview.id,
        &fixture.restore_roots,
    )
    .unwrap();
    assert_eq!(
        recovered.sessions[0].target_session_id, target_id,
        "{recovered:#?}"
    );
    assert_eq!(
        claude_session_files(&fixture.restore_roots.claude).len(),
        1,
        "recovery must reuse the pinned native session"
    );
    assert!(
        fixture.sessions.read_session(&target_id).is_ok(),
        "{:#?}",
        recovered.sessions
    );
}

#[test]
fn target_drift_after_preview_blocks_before_any_write() {
    let fixture = Fixture::new(SourceTool::Codex);
    let (run, preview, _environment) = fixture.previewed(SourceTool::Claude);
    let changed = fixture.target_project.join("CLAUDE.md");
    write(&changed, "target changed after preview\n");

    let error = apply_preview(
        &fixture.sessions,
        &fixture.runs,
        &run.id,
        &preview.id,
        &fixture.restore_roots,
    )
    .unwrap_err();
    assert!(error.contains("target changed"), "{error}");
    assert_eq!(
        fs::read_to_string(&changed).unwrap(),
        "target changed after preview\n"
    );
    assert!(!fixture
        .target_project
        .join(".claude/skills/shared/SKILL.md")
        .exists());
    assert!(claude_session_files(&fixture.restore_roots.claude).is_empty());
}

#[cfg(unix)]
#[test]
fn a_file_write_failure_marks_that_item_and_keeps_the_source() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = Fixture::new(SourceTool::Codex);
    let (run, preview, _environment) = fixture.previewed(SourceTool::Claude);
    let blocked = fixture.target_project.join(".claude/skills/shared");
    fs::create_dir_all(&blocked).unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o500)).unwrap();

    let report = apply_preview(
        &fixture.sessions,
        &fixture.runs,
        &run.id,
        &preview.id,
        &fixture.restore_roots,
    );
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    let report = report.unwrap();
    assert_eq!(report.state, RunState::Partial);
    assert!(report.items.iter().any(|item| {
        matches!(item.item_id.as_str(), value if preview.items.iter().any(|rendered| rendered.kind == AssetKind::Skill && rendered.item_id == value))
            && item.readiness == Readiness::Failed
    }));
    assert!(fixture.target_project.join("CLAUDE.md").is_file());
    assert_eq!(
        fs::read_to_string(
            fixture
                .source_project
                .join(".agents/skills/shared/SKILL.md")
        )
        .unwrap(),
        "---\nname: shared\ndescription: Source skill\n---\nSource body.\n"
    );
}

#[test]
fn source_changes_after_apply_do_not_update_the_target() {
    let fixture = Fixture::new(SourceTool::Codex);
    let (run, preview, _environment) = fixture.previewed(SourceTool::Claude);
    apply_preview(
        &fixture.sessions,
        &fixture.runs,
        &run.id,
        &preview.id,
        &fixture.restore_roots,
    )
    .unwrap();
    let target = fixture.target_project.join("CLAUDE.md");
    let before = fs::read(&target).unwrap();
    write(
        &fixture.source_project.join("AGENTS.md"),
        "changed source after migration\n",
    );
    assert_eq!(fs::read(&target).unwrap(), before);
}

#[test]
fn codex_target_registers_the_fixed_session_and_rollout_path() {
    let fixture = Fixture::new(SourceTool::Codex);
    let (run, preview, _environment) = fixture.previewed(SourceTool::Codex);
    let report = apply_preview(
        &fixture.sessions,
        &fixture.runs,
        &run.id,
        &preview.id,
        &fixture.restore_roots,
    )
    .unwrap();
    let session = &report.sessions[0];
    let path = session.target_path.clone().unwrap();
    assert!(path.starts_with(&fixture.restore_roots.codex));
    assert!(path.is_file());
    let index =
        fs::read_to_string(fixture.restore_roots.codex_home.join("session_index.jsonl")).unwrap();
    assert!(index.contains(&session.target_session_id));
    let payload = fs::read_to_string(&path).unwrap();
    assert!(payload.contains(&session.target_session_id));
    assert!(!payload.contains("\"selected\""));
    let again = apply_preview(
        &fixture.sessions,
        &fixture.runs,
        &run.id,
        &preview.id,
        &fixture.restore_roots,
    )
    .unwrap();
    assert_eq!(again.sessions[0].target_path, session.target_path);
    let entries = fs::read_dir(path.parent().unwrap()).unwrap().count();
    assert_eq!(entries, 1);
}

#[test]
fn every_native_target_uses_the_pinned_session_id() {
    let targets = [
        SourceTool::Claude,
        SourceTool::Codex,
        SourceTool::OpenCode,
        SourceTool::Dsh,
        SourceTool::Pi,
    ];
    for target in targets {
        let fixture = Fixture::new(SourceTool::Codex);
        if target == SourceTool::OpenCode {
            session_loom_core::adapters::opencode::init_database(&fixture.restore_roots.opencode)
                .unwrap();
        }
        let _dsh_home = (target == SourceTool::Dsh)
            .then(|| common::isolate_dsh_home(&fixture._temp.path().join("dsh-home")));
        let (run, preview, _environment) = fixture.previewed(target);
        let report = apply_preview(
            &fixture.sessions,
            &fixture.runs,
            &run.id,
            &preview.id,
            &fixture.restore_roots,
        )
        .unwrap();
        let session = &report.sessions[0];
        assert_eq!(
            session.readiness,
            Readiness::Available,
            "{target:?}: {session:?}"
        );
        match target {
            SourceTool::Claude => {
                let files = claude_session_files(&fixture.restore_roots.claude);
                assert_eq!(files.len(), 1, "{target:?}");
                assert!(files[0]
                    .to_string_lossy()
                    .contains(&session.target_session_id));
            }
            SourceTool::Codex => {
                let path = session.target_path.as_ref().unwrap();
                assert!(path.is_file(), "{target:?}");
                assert!(path.to_string_lossy().contains(&session.target_session_id));
            }
            SourceTool::OpenCode => {
                let sessions = session_loom_core::adapters::opencode::parse_sessions(
                    &fixture.restore_roots.opencode,
                )
                .unwrap();
                assert!(
                    sessions
                        .iter()
                        .any(|stored| stored.session_id == session.target_session_id),
                    "{target:?}: {sessions:?}"
                );
            }
            SourceTool::Dsh => {
                let logs =
                    session_loom_core::adapters::dsh::session_log_files(&fixture.restore_roots.dsh);
                assert_eq!(logs.len(), 1, "{target:?}");
                let parsed = session_loom_core::adapters::dsh::parse_session_file(&logs[0])
                    .unwrap()
                    .unwrap();
                assert_eq!(parsed.session_id, session.target_session_id);
            }
            SourceTool::Pi => {
                let file = session.target_path.as_ref().unwrap();
                assert!(file.is_file(), "{target:?}");
                assert!(file.to_string_lossy().contains(&session.target_session_id));
            }
        }
        let linked = fixture
            .sessions
            .read_session(&session.target_session_id)
            .unwrap();
        assert_eq!(linked.source_tool, target);
        assert_eq!(linked.cwd, fixture.target_project.to_string_lossy());
    }
}

#[test]
fn an_index_registration_failure_recovers_without_a_second_rollout() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new(SourceTool::Codex);
    let (run, preview, _environment) = fixture.previewed(SourceTool::Codex);
    fs::create_dir_all(fixture.restore_roots.codex_home.join("session_index.jsonl")).unwrap();
    let report = apply_preview(
        &fixture.sessions,
        &fixture.runs,
        &run.id,
        &preview.id,
        &fixture.restore_roots,
    )
    .unwrap();
    assert_eq!(report.state, RunState::Partial);
    assert_eq!(report.sessions[0].readiness, Readiness::Failed);
    let rollout = report.sessions[0].target_path.clone().unwrap();
    assert!(rollout.is_file());
    let index = fixture.restore_roots.codex_home.join("session_index.jsonl");
    fs::remove_dir(&index).unwrap();
    std::fs::write(&index, "").unwrap();
    fs::set_permissions(&index, fs::Permissions::from_mode(0o600)).unwrap();

    let recovered = apply_preview(
        &fixture.sessions,
        &fixture.runs,
        &run.id,
        &preview.id,
        &fixture.restore_roots,
    )
    .unwrap();
    assert_eq!(recovered.sessions[0].readiness, Readiness::Available);
    assert_eq!(recovered.sessions[0].target_path.as_ref(), Some(&rollout));
    assert_eq!(
        fs::read_dir(rollout.parent().unwrap()).unwrap().count(),
        1,
        "recovery must not create a second rollout"
    );
    let contents = fs::read_to_string(&index).unwrap();
    assert!(contents.contains(&recovered.sessions[0].target_session_id));
}

#[test]
fn cleaning_run_snapshots_does_not_break_persistent_target_references() {
    let fixture = Fixture::new(SourceTool::Codex);
    let (run, preview, _environment) = fixture.previewed(SourceTool::Claude);
    apply_preview(
        &fixture.sessions,
        &fixture.runs,
        &run.id,
        &preview.id,
        &fixture.restore_roots,
    )
    .unwrap();
    let instruction = fixture.target_project.join("CLAUDE.md");
    let text = fs::read_to_string(&instruction).unwrap();
    assert!(text.contains("Synthetic project instructions."), "{text}");
    let managed_root = fixture.target_project.join(".session-loom/instructions");
    let copies = fs::read_dir(&managed_root)
        .unwrap()
        .flat_map(|entry| {
            let path = entry.unwrap().path();
            fs::read_dir(path)
                .unwrap()
                .map(|file| file.unwrap().path())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    assert!(!copies.is_empty(), "no persistent instruction copy");
    let before = copies
        .iter()
        .map(|path| fs::read(path).unwrap())
        .collect::<Vec<_>>();
    let run_directory = fixture.runs.run_directory(&run.id).unwrap();
    fs::remove_dir_all(run_directory.join("snapshots")).unwrap();
    fs::remove_dir_all(run_directory.join("staging")).unwrap();
    assert_eq!(fs::read_to_string(&instruction).unwrap(), text);
    for (path, expected) in copies.iter().zip(before) {
        assert!(path.is_file(), "{}", path.display());
        assert_eq!(fs::read(path).unwrap(), expected);
    }
}

#[test]
fn retention_cleanup_keeps_target_files_and_unfinished_runs() {
    let fixture = Fixture::new(SourceTool::Codex);
    let (run, preview, _environment) = fixture.previewed(SourceTool::Claude);
    apply_preview(
        &fixture.sessions,
        &fixture.runs,
        &run.id,
        &preview.id,
        &fixture.restore_roots,
    )
    .unwrap();
    let target_instruction = fixture.target_project.join("CLAUDE.md");
    let target_bytes = fs::read(&target_instruction).unwrap();
    let session_files = claude_session_files(&fixture.restore_roots.claude);
    assert_eq!(session_files.len(), 1);
    let session_bytes = fs::read(&session_files[0]).unwrap();

    let mut completed = fixture.runs.load(&run.id).unwrap();
    completed.revision += 1;
    completed.state = RunState::Completed;
    completed.updated_at = "2026-01-01T00:00:00Z".to_string();
    fixture
        .runs
        .update(&completed, completed.revision - 1)
        .unwrap();

    let mut unfinished = session_loom_core::project_migration::MigrationRun::new(
        session_loom_core::project_migration::resolve_project(
            fixture.target_project.to_str().unwrap(),
        ),
        vec!["pending".into()],
    )
    .unwrap();
    unfinished.updated_at = "2026-01-01T00:00:00Z".to_string();
    fixture.runs.create(&unfinished).unwrap();
    let unfinished_directory = fixture.runs.run_directory(&unfinished.id).unwrap();
    fs::write(unfinished_directory.join("snapshots/keep.bin"), b"pending").unwrap();

    let cleaned = fixture
        .runs
        .cleanup_completed_runs(chrono::Utc::now(), 30)
        .unwrap();
    assert_eq!(cleaned, 1);
    let directory = fixture.runs.run_directory(&run.id).unwrap();
    for child in ["snapshots", "staging", "backups", "previews"] {
        assert_eq!(
            fs::read_dir(directory.join(child)).unwrap().count(),
            0,
            "{child} should be cleaned"
        );
    }
    assert_eq!(fs::read(&target_instruction).unwrap(), target_bytes);
    assert_eq!(fs::read(&session_files[0]).unwrap(), session_bytes);
    assert_eq!(
        fs::read(unfinished_directory.join("snapshots/keep.bin")).unwrap(),
        b"pending"
    );
    assert!(fixture.runs.load(&run.id).is_ok());
    assert_eq!(
        fixture
            .runs
            .cleanup_completed_runs(chrono::Utc::now(), 30)
            .unwrap(),
        0,
        "cleanup is idempotent"
    );
}

#[test]
fn an_unresolved_preview_cannot_be_applied() {
    let fixture = Fixture::new(SourceTool::Codex);
    write(
        &fixture
            .target_project
            .join(".claude/skills/shared/SKILL.md"),
        "---\nname: shared\ndescription: Existing target\n---\nTarget body.\n",
    );
    let (run, preview, _environment) = fixture.previewed(SourceTool::Claude);
    assert!(!preview.ready_to_apply);
    let error = apply_preview(
        &fixture.sessions,
        &fixture.runs,
        &run.id,
        &preview.id,
        &fixture.restore_roots,
    )
    .unwrap_err();
    assert!(error.contains("unresolved"), "{error}");
    assert_eq!(
        fs::read_to_string(
            fixture
                .target_project
                .join(".claude/skills/shared/SKILL.md")
        )
        .unwrap(),
        "---\nname: shared\ndescription: Existing target\n---\nTarget body.\n"
    );
    assert!(claude_session_files(&fixture.restore_roots.claude).is_empty());
    assert!(!fixture.target_project.join("CLAUDE.md").exists());
}
