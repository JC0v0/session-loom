use session_loom_core::{
    canonical::SourceTool,
    project_migration::{
        capture_selection, create_preview, discover_environment, resolve_project,
        select_project_sessions, verify_preview, AssetKind, ConflictChoice, DiscoveryOptions,
        EnvironmentRoots, MigrationRun, MigrationTarget, OpenCodeConfigVersion, PreviewDecision,
        PreviewRevision, Readiness, RunStore, SelectedSession, TargetEnvironment, VersionSupport,
    },
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
    target_project: PathBuf,
    target_roots: EnvironmentRoots,
    sessions: Store,
    runs: RunStore,
    selected: Vec<SelectedSession>,
    report: session_loom_core::project_migration::DiscoveryReport,
    base_run: MigrationRun,
}

impl Fixture {
    fn new() -> Self {
        Self::build(false)
    }

    fn with_duplicate_skill() -> Self {
        Self::build(true)
    }

    fn build(duplicate_skill: bool) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("source-project");
        let target_project = temp.path().join("target-project");
        let source_roots = EnvironmentRoots::isolated(&temp.path().join("source-roots"));
        let target_roots = EnvironmentRoots::isolated(&temp.path().join("target-roots"));
        write(
            &project.join("AGENTS.md"),
            "Synthetic source instructions.\n",
        );
        write(
            &project.join("MEMORY.md"),
            "Synthetic source memory without a final newline",
        );
        write(
            &project.join(".agents/skills/shared/SKILL.md"),
            "---\nname: shared\ndescription: Source skill\n---\nSource body.\n",
        );
        write(
            &project.join(".agents/skills/shared/references/details.md"),
            "Synthetic source resource.\n",
        );
        fs::write(project.join(".agents/skills/shared/blob.bin"), [0x00, 0xff]).unwrap();
        if duplicate_skill {
            write(
                &project.join(".agents/skills/alias/SKILL.md"),
                "---\nname: shared\ndescription: Alias skill\n---\nAlias body.\n",
            );
        }
        write(
            &project.join("package.json"),
            r#"{"scripts":{"alpha":"echo source-alpha","beta":"echo source-beta"}}"#,
        );
        write(
            &project.join(".codex/config.toml"),
            "[mcp_servers.database]\ncommand = \"fixture-mcp\"\n",
        );
        let sessions = Store::open(&temp.path().join("store")).unwrap();
        sessions
            .write_session(&common::migration_session(
                "native",
                &project,
                SourceTool::Codex,
            ))
            .unwrap();
        let identity = resolve_project(project.to_str().unwrap());
        let selected =
            select_project_sessions(&sessions, &identity.id, &["native".into()]).unwrap();
        let report = discover_environment(
            &identity,
            &selected,
            &source_roots,
            &DiscoveryOptions::default(),
        )
        .unwrap();
        let mut base_run = MigrationRun::new(identity, vec!["native".into()]).unwrap();
        base_run.discovered_assets = report.assets.clone();
        base_run.discovery_probes = report.source_probes.clone();
        let runs = RunStore::new(sessions.root()).unwrap();
        runs.create(&base_run).unwrap();
        Self {
            _temp: temp,
            target_project,
            target_roots,
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
        let roots = EnvironmentRoots::isolated(&self.target_roots.user_home);
        let mut environment = TargetEnvironment {
            target: MigrationTarget {
                tool,
                project_root: self.target_project.clone(),
            },
            roots,
            session_roots: None,
            opencode_version: OpenCodeConfigVersion::V2,
            pi_mcp: VersionSupport::Supported,
            dsh_profile: Some("fixture".into()),
            project_trusted: true,
            executable_search_path: vec![self._temp.path().join("bin")],
            environment_variables: vec!["FIXTURE_TOKEN".into()],
        };
        let fake_bin = self._temp.path().join("bin");
        for command in ["fixture-mcp", "sh", "node", "npm"] {
            write(&fake_bin.join(command), "#!/bin/sh\n");
        }
        environment.executable_search_path = vec![fake_bin];
        environment
    }

    fn target_environment(
        &self,
        run: &MigrationRun,
        environment: &TargetEnvironment,
    ) -> MigrationRun {
        let mut run = run.clone();
        run.target = Some(environment.target.clone());
        run.revision += 1;
        self.runs.update(&run, run.revision - 1).unwrap();
        run
    }
}

fn preview_file_bytes(fixture: &Fixture, preview: &PreviewRevision, target_path: &Path) -> Vec<u8> {
    let file = preview
        .files
        .iter()
        .find(|file| file.target_path == target_path)
        .unwrap();
    fs::read(
        fixture
            .runs
            .run_directory(&preview.run_id)
            .unwrap()
            .join("previews")
            .join(&preview.id)
            .join(&file.private_path),
    )
    .unwrap()
}

fn skill_decision(preview: &PreviewRevision, run: &MigrationRun) -> PreviewDecision {
    let skill_id = run
        .assets
        .iter()
        .find(|asset| asset.asset.kind == AssetKind::Skill)
        .unwrap()
        .asset
        .id
        .clone();
    let item = preview
        .conflicts
        .iter()
        .find(|conflict| conflict.target_path.ends_with("SKILL.md"))
        .unwrap()
        .items
        .iter()
        .find(|item| item.item_id == skill_id)
        .unwrap();
    PreviewDecision {
        decision_id: item.decision_id.clone(),
        item_id: skill_id,
        choice: ConflictChoice::Replace,
        merged_content: None,
        merged_value: None,
    }
}

#[test]
fn keep_merge_replace_preview_target_bytes_without_modifying_target() {
    for (choice, expected) in [
        (
            ConflictChoice::Keep,
            "---\nname: shared\ndescription: Target skill\n---\nTarget body.\n",
        ),
        (
            ConflictChoice::Replace,
            "---\nname: shared\ndescription: Source skill\n---\nSource body.\n",
        ),
        (
            ConflictChoice::Merge,
            "---\nname: shared\ndescription: Merged skill\n---\nMerged body.\n",
        ),
    ] {
        let fixture = Fixture::new();
        write(
            &fixture
                .target_project
                .join(".agents/skills/shared/SKILL.md"),
            "---\nname: shared\ndescription: Target skill\n---\nTarget body.\n",
        );
        let environment = fixture.environment(SourceTool::Codex);
        let captured = fixture.capture();
        let run = fixture.target_environment(&captured, &environment);
        let (run, initial) = create_preview(&fixture.runs, &run, &environment, &[]).unwrap();
        assert!(!initial.ready_to_apply);
        let mut decision = skill_decision(&initial, &run);
        decision.choice = choice;
        if choice == ConflictChoice::Merge {
            decision.merged_content = Some(expected.to_string());
        }
        let (_, preview) = create_preview(&fixture.runs, &run, &environment, &[decision]).unwrap();
        assert!(preview.ready_to_apply, "{:#?}", preview.conflicts);
        let output = preview_file_bytes(
            &fixture,
            &preview,
            &fixture
                .target_project
                .join(".agents/skills/shared/SKILL.md"),
        );
        assert_eq!(std::str::from_utf8(&output).unwrap(), expected);
        assert_eq!(
            fs::read_to_string(
                fixture
                    .target_project
                    .join(".agents/skills/shared/SKILL.md")
            )
            .unwrap(),
            "---\nname: shared\ndescription: Target skill\n---\nTarget body.\n"
        );
    }
}

#[test]
fn one_config_file_aggregates_independent_keep_and_replace_decisions() {
    let fixture = Fixture::new();
    let package = fixture.target_project.join("package.json");
    write(
        &package,
        r#"{"scripts":{"alpha":"echo target-alpha","beta":"echo target-beta","gamma":"keep"}}"#,
    );
    let environment = fixture.environment(SourceTool::Codex);
    let captured = fixture.capture();
    let run = fixture.target_environment(&captured, &environment);
    let (run, initial) = create_preview(&fixture.runs, &run, &environment, &[]).unwrap();
    let command_ids = run
        .assets
        .iter()
        .filter(|asset| asset.asset.kind == AssetKind::Command)
        .map(|asset| (asset.asset.name.clone(), asset.asset.id.clone()))
        .collect::<std::collections::BTreeMap<_, _>>();
    let decisions = ["alpha", "beta"]
        .into_iter()
        .map(|name| {
            let item_id = command_ids[name].clone();
            let item = initial
                .conflicts
                .iter()
                .flat_map(|conflict| &conflict.items)
                .find(|item| item.item_id == item_id)
                .unwrap();
            PreviewDecision {
                decision_id: item.decision_id.clone(),
                item_id,
                choice: if name == "alpha" {
                    ConflictChoice::Keep
                } else {
                    ConflictChoice::Replace
                },
                merged_content: None,
                merged_value: None,
            }
        })
        .collect::<Vec<_>>();
    let (_, preview) = create_preview(&fixture.runs, &run, &environment, &decisions).unwrap();
    let output = preview_file_bytes(&fixture, &preview, &package);
    let package: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        package.pointer("/scripts/alpha").unwrap(),
        "echo target-alpha"
    );
    assert_eq!(
        package.pointer("/scripts/beta").unwrap(),
        "echo source-beta"
    );
    assert_eq!(package.pointer("/scripts/gamma").unwrap(), "keep");
}

#[test]
fn target_and_snapshot_drift_are_rejected_after_preview() {
    let fixture = Fixture::new();
    let environment = fixture.environment(SourceTool::Claude);
    let captured = fixture.capture();
    let run = fixture.target_environment(&captured, &environment);
    let (run, preview) = create_preview(&fixture.runs, &run, &environment, &[]).unwrap();
    verify_preview(&fixture.runs, &run.id, &preview.id).unwrap();

    let changed_file = preview
        .files
        .iter()
        .find(|file| file.changed && !file.target_existed)
        .unwrap();
    write(&changed_file.target_path, "target changed after preview\n");
    assert!(verify_preview(&fixture.runs, &run.id, &preview.id)
        .unwrap_err()
        .contains("target changed"));

    let snapshot_file = run
        .assets
        .iter()
        .find(|asset| !asset.files.is_empty())
        .unwrap()
        .files[0]
        .clone();
    let snapshot_path = fixture
        .runs
        .run_directory(&run.id)
        .unwrap()
        .join(&snapshot_file.snapshot_path);
    fs::write(&snapshot_path, "forged snapshot\n").unwrap();
    assert!(verify_preview(&fixture.runs, &run.id, &preview.id)
        .unwrap_err()
        .contains("snapshot changed"));
}

#[test]
fn redacted_placeholders_cannot_become_final_merge_content() {
    let fixture = Fixture::new();
    write(
        &fixture
            .target_project
            .join(".agents/skills/shared/SKILL.md"),
        "---\nname: shared\ndescription: Target skill\n---\nTarget body.\n",
    );
    let environment = fixture.environment(SourceTool::Codex);
    let captured = fixture.capture();
    let run = fixture.target_environment(&captured, &environment);
    let (run, initial) = create_preview(&fixture.runs, &run, &environment, &[]).unwrap();
    let mut decision = skill_decision(&initial, &run);
    decision.choice = ConflictChoice::Merge;
    decision.merged_content = Some("prefix [redacted] suffix".to_string());
    assert!(
        create_preview(&fixture.runs, &run, &environment, &[decision])
            .unwrap_err()
            .contains("redacted")
    );
}

#[test]
fn two_sources_mapping_to_one_target_are_shown_before_any_winner_is_chosen() {
    let fixture = Fixture::with_duplicate_skill();
    let environment = fixture.environment(SourceTool::Codex);
    let captured = fixture.capture();
    let run = fixture.target_environment(&captured, &environment);
    let (run, initial) = create_preview(&fixture.runs, &run, &environment, &[]).unwrap();
    let conflict = initial
        .conflicts
        .iter()
        .find(|conflict| conflict.target_path.ends_with("SKILL.md") && conflict.item_ids.len() == 2)
        .unwrap();
    assert_eq!(conflict.kind, "sourceSource");
    assert!(!conflict.resolved);
    assert_eq!(conflict.items.len(), 2);
    assert!(!initial.ready_to_apply);
    let decisions = vec![
        PreviewDecision {
            decision_id: conflict.items[0].decision_id.clone(),
            item_id: conflict.items[0].item_id.clone(),
            choice: ConflictChoice::Keep,
            merged_content: None,
            merged_value: None,
        },
        PreviewDecision {
            decision_id: conflict.items[1].decision_id.clone(),
            item_id: conflict.items[1].item_id.clone(),
            choice: ConflictChoice::Replace,
            merged_content: None,
            merged_value: None,
        },
    ];
    let (_, resolved) = create_preview(&fixture.runs, &run, &environment, &decisions).unwrap();
    let conflict = resolved
        .conflicts
        .iter()
        .find(|conflict| conflict.item_ids.len() == 2)
        .unwrap();
    assert!(conflict.resolved);
    let output = preview_file_bytes(
        &fixture,
        &resolved,
        &fixture
            .target_project
            .join(".agents/skills/shared/SKILL.md"),
    );
    let text = std::str::from_utf8(&output).unwrap();
    assert!(
        text.contains("Alias body") || text.contains("Source body"),
        "{text}"
    );
}

#[test]
fn instructions_and_memory_share_one_entry_with_independent_managed_blocks() {
    let fixture = Fixture::new();
    let environment = fixture.environment(SourceTool::Codex);
    let captured = fixture.capture();
    let run = fixture.target_environment(&captured, &environment);
    let (_, preview) = create_preview(&fixture.runs, &run, &environment, &[]).unwrap();
    let instructions = preview_file_bytes(
        &fixture,
        &preview,
        &fixture.target_project.join("AGENTS.md"),
    );
    let text = std::str::from_utf8(&instructions).unwrap();
    assert_eq!(text.matches(":start -->").count(), 2, "{text}");
    assert!(text.contains("Synthetic source instructions."), "{text}");
    assert!(
        text.contains("Synthetic source memory without a final newline"),
        "{text}"
    );
    assert!(text.contains("<!-- session-loom:"), "{text}");
}

#[test]
fn diff_redacts_target_secrets_and_handles_binary_and_no_final_newline() {
    let fixture = Fixture::new();
    let secret = ["fixture", "target", "secret"].join("-");
    let target_codex = EnvironmentRoots::isolated(&fixture.target_roots.user_home).codex;
    write(
        &target_codex.join("config.toml"),
        &format!(
            "# target file\n[mcp_servers.database]\ncommand = \"target-mcp\"\n\n[mcp_servers.database.env]\nTOKEN = \"{secret}\"\n"
        ),
    );
    write(
        &fixture
            .target_project
            .join(".agents/skills/shared/references/details.md"),
        "old bytes",
    );
    fs::write(
        fixture
            .target_project
            .join(".agents/skills/shared/blob.bin"),
        [0xff, 0xfe, 0xfd],
    )
    .unwrap();
    let environment = fixture.environment(SourceTool::Codex);
    let captured = fixture.capture();
    let run = fixture.target_environment(&captured, &environment);
    let (_, preview) = create_preview(&fixture.runs, &run, &environment, &[]).unwrap();
    let serialized = serde_json::to_string(&preview).unwrap();
    if let Some(index) = serialized.find(&secret) {
        let start = index.saturating_sub(160);
        let end = (index + 160).min(serialized.len());
        panic!(
            "preview leaked a target secret near: {}",
            &serialized[start..end]
        );
    }
    let mcp = preview
        .conflicts
        .iter()
        .find(|conflict| conflict.target_path.ends_with("config.toml"))
        .unwrap();
    let diff = &mcp.items[0].diff;
    assert!(matches!(
        diff,
        session_loom_core::project_migration::diff::ContentDiff::Text { .. }
    ));
    let session_loom_core::project_migration::diff::ContentDiff::Text { diff } = diff else {
        unreachable!()
    };
    assert!(diff.redacted);
    let details = preview
        .files
        .iter()
        .find(|file| file.target_path.ends_with("details.md"))
        .unwrap();
    let session_loom_core::project_migration::diff::ContentDiff::Text { diff } = &details.diff
    else {
        panic!("details.md should be a text diff");
    };
    assert!(!diff.old_has_final_newline);
    let blob = preview
        .files
        .iter()
        .find(|file| file.target_path.ends_with("blob.bin"))
        .unwrap();
    assert!(matches!(
        blob.diff,
        session_loom_core::project_migration::diff::ContentDiff::Binary { .. }
    ));
}

#[test]
fn keep_marks_the_retained_target_content_as_unverified() {
    let fixture = Fixture::new();
    write(
        &fixture
            .target_project
            .join(".agents/skills/shared/SKILL.md"),
        "---\nname: shared\ndescription: Target skill\n---\nTarget body.\n",
    );
    let environment = fixture.environment(SourceTool::Codex);
    let captured = fixture.capture();
    let run = fixture.target_environment(&captured, &environment);
    let (run, initial) = create_preview(&fixture.runs, &run, &environment, &[]).unwrap();
    let mut decision = skill_decision(&initial, &run);
    decision.choice = ConflictChoice::Keep;
    let item_id = decision.item_id.clone();
    let (_, preview) = create_preview(&fixture.runs, &run, &environment, &[decision]).unwrap();
    let skill_item = preview
        .items
        .iter()
        .find(|item| item.item_id == item_id)
        .unwrap();
    assert_eq!(skill_item.readiness, Readiness::NeedsConfiguration);
    assert_eq!(skill_item.action, "keep-target");
    assert!(skill_item.message.contains("保留目标现有内容"));
}
