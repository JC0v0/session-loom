use session_loom_core::{
    canonical::SourceTool,
    project_migration::{
        capture_selection, discover_environment, render_environment, resolve_project,
        select_project_sessions, AssetKind, AssetScope, DiscoveryOptions, DiscoveryReport,
        EnvironmentRoots, MigrationRun, MigrationTarget, OpenCodeConfigVersion, Readiness,
        RenderPlan, RenderedFileBody, RunStore, SelectedSession, TargetEnvironment, TargetScope,
        VersionSupport,
    },
    store::Store,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

#[path = "common/mod.rs"]
mod common;

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

struct Fixture {
    _temp: tempfile::TempDir,
    project: PathBuf,
    sessions_store: Store,
    runs: RunStore,
    selected: Vec<SelectedSession>,
    report: DiscoveryReport,
    run: MigrationRun,
}

impl Fixture {
    fn new(tool: SourceTool) -> Self {
        Self::with_overrides(tool, None, None)
    }

    fn with_overrides(tool: SourceTool, script: Option<&str>, claude_mcp: Option<&str>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("source-project");
        let roots = EnvironmentRoots::isolated(&temp.path().join("source-roots"));
        write(
            &project.join("AGENTS.md"),
            "Synthetic project instructions.\n",
        );
        write(
            &project.join("MEMORY.md"),
            "Synthetic current project memory.\n",
        );
        write(
            &project.join("package.json"),
            r#"{"scripts":{"check":"./scripts/check.sh"},"fixture":true}"#,
        );
        write(
            &project.join("scripts/check.sh"),
            script.unwrap_or("#!/bin/sh\nprintf 'fixture'\n"),
        );
        match tool {
            SourceTool::Codex => {
                write(
                    &project.join(".agents/skills/project-skill/SKILL.md"),
                    "---\nname: project-skill\ndescription: Synthetic project skill\n---\nRead references/details.md.\n",
                );
                write(
                    &project.join(".agents/skills/project-skill/references/details.md"),
                    "Synthetic skill detail.\n",
                );
                write(
                    &project.join(".codex/config.toml"),
                    include_str!("fixtures/project_migration/codex/config.toml"),
                );
            }
            SourceTool::Claude => {
                write(
                    &project.join("CLAUDE.md"),
                    "@rules.md\nSynthetic project instructions.\n",
                );
                write(
                    &project.join("rules.md"),
                    "Synthetic imported project rules.\n",
                );
                write(
                    &project.join(".claude/skills/project-skill/SKILL.md"),
                    "---\nname: project-skill\ndescription: Synthetic project skill\n---\nRead references/details.md.\n",
                );
                write(
                    &project.join(".claude/skills/project-skill/references/details.md"),
                    "Synthetic skill detail.\n",
                );
                write(
                    &project.join(".mcp.json"),
                    claude_mcp
                        .unwrap_or(include_str!("fixtures/project_migration/claude/mcp.json")),
                );
            }
            SourceTool::OpenCode => {
                write(
                    &project.join(".opencode/skills/project-skill/SKILL.md"),
                    "---\nname: project-skill\ndescription: Synthetic project skill\n---\nRead references/details.md.\n",
                );
                write(
                    &project.join(".opencode/skills/project-skill/references/details.md"),
                    "Synthetic skill detail.\n",
                );
                write(
                    &project.join("opencode.jsonc"),
                    include_str!("fixtures/project_migration/opencode/v2.jsonc"),
                );
            }
            SourceTool::Dsh => {
                write(
                    &project.join(".dsh/skills/project-skill/SKILL.md"),
                    "---\nname: project-skill\ndescription: Synthetic project skill\n---\nRead references/details.md.\n",
                );
                write(
                    &project.join(".dsh/skills/project-skill/references/details.md"),
                    "Synthetic skill detail.\n",
                );
                write(
                    &roots.dsh.join("profiles/fixture/cordis.patch.yml"),
                    include_str!("fixtures/project_migration/dsh/cordis.patch.yml"),
                );
            }
            SourceTool::Pi => {
                write(
                    &project.join(".pi/skills/project-skill/SKILL.md"),
                    "---\nname: project-skill\ndescription: Synthetic project skill\n---\nRead references/details.md.\n",
                );
                write(
                    &project.join(".pi/skills/project-skill/references/details.md"),
                    "Synthetic skill detail.\n",
                );
                write(
                    &project.join(".pi/mcp.json"),
                    include_str!("fixtures/project_migration/pi/mcp.json"),
                );
            }
        }
        let sessions_store = Store::open(&temp.path().join("session-store")).unwrap();
        sessions_store
            .write_session(&common::migration_session("native", &project, tool))
            .unwrap();
        let identity = resolve_project(project.to_str().unwrap());
        let selected =
            select_project_sessions(&sessions_store, &identity.id, &["native".into()]).unwrap();
        let report = discover_environment(
            &identity,
            &selected,
            &roots,
            &DiscoveryOptions {
                dsh_profile: Some("fixture".into()),
                ..Default::default()
            },
        )
        .unwrap();
        let mut run = MigrationRun::new(identity, vec!["native".into()]).unwrap();
        run.discovered_assets = report.assets.clone();
        run.discovery_probes = report.source_probes.clone();
        let runs = RunStore::new(sessions_store.root()).unwrap();
        runs.create(&run).unwrap();
        Self {
            _temp: temp,
            project,
            sessions_store,
            runs,
            selected,
            report,
            run,
        }
    }

    fn selected_ids(&self) -> Vec<String> {
        self.report
            .assets
            .iter()
            .filter(|asset| asset.scope == AssetScope::Project || asset.kind == AssetKind::Mcp)
            .map(|asset| asset.id.clone())
            .collect()
    }

    fn target_root(&self, target_tool: SourceTool) -> PathBuf {
        self._temp
            .path()
            .join("target-roots")
            .join(target_tool.as_str())
    }

    fn target_project(&self, target_tool: SourceTool) -> PathBuf {
        self._temp
            .path()
            .join("target-projects")
            .join(target_tool.as_str())
    }

    fn render(&self, target_tool: SourceTool) -> (RenderPlan, TargetEnvironment) {
        self.render_with(
            target_tool,
            OpenCodeConfigVersion::V2,
            VersionSupport::Supported,
        )
    }

    fn render_with(
        &self,
        target_tool: SourceTool,
        opencode_version: OpenCodeConfigVersion,
        pi_mcp: VersionSupport,
    ) -> (RenderPlan, TargetEnvironment) {
        let target_project = self.target_project(target_tool);
        let target_roots = EnvironmentRoots::isolated(&self.target_root(target_tool));
        self.render_at(
            target_tool,
            target_project,
            target_roots,
            opencode_version,
            pi_mcp,
        )
    }

    fn render_at(
        &self,
        target_tool: SourceTool,
        target_project: PathBuf,
        target_roots: EnvironmentRoots,
        opencode_version: OpenCodeConfigVersion,
        pi_mcp: VersionSupport,
    ) -> (RenderPlan, TargetEnvironment) {
        self.render_at_trusted(
            target_tool,
            target_project,
            target_roots,
            opencode_version,
            pi_mcp,
            true,
        )
    }

    fn render_at_trusted(
        &self,
        target_tool: SourceTool,
        target_project: PathBuf,
        target_roots: EnvironmentRoots,
        opencode_version: OpenCodeConfigVersion,
        pi_mcp: VersionSupport,
        project_trusted: bool,
    ) -> (RenderPlan, TargetEnvironment) {
        let ids = self.selected_ids();
        let mut captured = capture_selection(
            &self.sessions_store,
            &self.runs,
            &self.run,
            &self.report,
            &self.selected,
            &ids,
        )
        .unwrap();
        let target = MigrationTarget {
            tool: target_tool,
            project_root: target_project,
        };
        captured.target = Some(target.clone());
        captured.revision += 1;
        self.runs.update(&captured, captured.revision - 1).unwrap();
        let fake_bin = self
            ._temp
            .path()
            .join("fake-bin")
            .join(target_tool.as_str());
        for command in ["node", "sh", "npm", "fixture-mcp"] {
            write(&fake_bin.join(command), "#!/bin/sh\n");
        }
        let environment = TargetEnvironment {
            target,
            roots: target_roots,
            session_roots: None,
            opencode_version,
            pi_mcp,
            dsh_profile: Some("fixture".into()),
            project_trusted,
            executable_search_path: vec![fake_bin],
            environment_variables: vec!["FIXTURE_TOKEN".into()],
        };
        let plan = render_environment(&self.runs, &captured, &environment).unwrap();
        (plan, environment)
    }
}

#[test]
fn every_source_and_target_maps_sessions_with_memory_and_three_tool_categories() {
    let sources = [
        SourceTool::Codex,
        SourceTool::Claude,
        SourceTool::OpenCode,
        SourceTool::Dsh,
        SourceTool::Pi,
    ];
    let targets = sources;
    for source in sources {
        for target in targets {
            let fixture = Fixture::new(source);
            let (plan, environment) = fixture.render(target);
            let kinds: Vec<_> = plan.items.iter().map(|item| item.kind).collect();
            assert!(
                kinds
                    .iter()
                    .any(|kind| matches!(kind, AssetKind::Memory | AssetKind::Instruction)),
                "{source:?} -> {target:?} missing memory/instructions: {:#?}",
                plan.items
            );
            for kind in [
                AssetKind::Skill,
                AssetKind::Mcp,
                AssetKind::Script,
                AssetKind::Command,
            ] {
                assert!(
                    kinds.contains(&kind),
                    "{source:?} -> {target:?} missing {kind:?}: {:#?}",
                    plan.items
                );
            }
            assert_eq!(plan.target, environment.target);
            assert!(plan
                .files
                .iter()
                .any(|file| file.target_scope == TargetScope::Project));
            assert!(
                !environment.target.project_root.exists(),
                "rendering wrote target files"
            );
            assert!(plan
                .items
                .iter()
                .any(|item| item.readiness != Readiness::Available));
        }
    }
}

fn target_bytes<'a>(plan: &'a RenderPlan, path: &Path) -> &'a [u8] {
    plan.files
        .iter()
        .find(|file| file.target_path == path)
        .and_then(|file| match &file.content {
            RenderedFileBody::Bytes(bytes) => Some(bytes.as_slice()),
            RenderedFileBody::Copy { .. } => None,
        })
        .unwrap_or_else(|| panic!("missing generated target file: {}", path.display()))
}

fn materialize_plan(plan: &RenderPlan) -> Result<(), String> {
    for file in &plan.files {
        fs::create_dir_all(
            file.target_path
                .parent()
                .ok_or_else(|| "target file has no parent".to_string())?,
        )
        .map_err(|error| error.to_string())?;
        match &file.content {
            RenderedFileBody::Bytes(bytes) => {
                fs::write(&file.target_path, bytes).map_err(|error| error.to_string())?;
            }
            RenderedFileBody::Copy {
                snapshot_path,
                content_hash,
            } => {
                let bytes = fs::read(snapshot_path).map_err(|error| error.to_string())?;
                if session_loom_core::project_migration::content_hash(&bytes) != *content_hash {
                    return Err(format!(
                        "snapshot changed before native validation: {}",
                        snapshot_path.display()
                    ));
                }
                fs::write(&file.target_path, bytes).map_err(|error| error.to_string())?;
            }
        }
        #[cfg(unix)]
        if file.executable {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(&file.target_path)
                .map_err(|error| error.to_string())?
                .permissions();
            permissions.set_mode(permissions.mode() | 0o111);
            fs::set_permissions(&file.target_path, permissions)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

#[test]
fn generated_configuration_preserves_target_structure_and_keeps_secrets_out_of_reports() {
    let targets = [
        SourceTool::Codex,
        SourceTool::Claude,
        SourceTool::OpenCode,
        SourceTool::Dsh,
        SourceTool::Pi,
    ];
    for target in targets {
        let fixture = Fixture::new(SourceTool::Codex);
        let target_project = fixture.target_project(target);
        let target_roots = fixture.target_root(target);
        let roots = EnvironmentRoots::isolated(&target_roots);
        let path = match target {
            SourceTool::Codex => {
                let path = roots.codex.join("config.toml");
                write(
                    &path,
                    "# keep-target-comment\nmodel = \"fixture\"\n\n[unrelated]\nTARGET_ONLY_SECRET = \"keep-me\"\n",
                );
                path
            }
            SourceTool::Claude => {
                let path = target_project.join(".mcp.json");
                write(
                    &path,
                    "{\n  // keep-target-comment\n  \"targetOnly\": {\"TARGET_ONLY_SECRET\": \"keep-me\"}\n}\n",
                );
                path
            }
            SourceTool::OpenCode => {
                let path = target_project.join("opencode.jsonc");
                write(
                    &path,
                    "{\n  // keep-target-comment\n  \"theme\": \"fixture\",\n  \"targetOnly\": {\"TARGET_ONLY_SECRET\": \"keep-me\"}\n}\n",
                );
                path
            }
            SourceTool::Pi => {
                let path = target_project.join(".pi/mcp.json");
                write(
                    &path,
                    "{\n  // keep-target-comment\n  \"targetOnly\": {\"TARGET_ONLY_SECRET\": \"keep-me\"}\n}\n",
                );
                path
            }
            SourceTool::Dsh => {
                let path = roots.dsh.join("profiles/fixture/cordis.patch.yml");
                write(
                    &path,
                    "- insert:\n    - name: existing\n      config:\n        cwd: !!js process.cwd()\n# keep-target-comment\n# TARGET_ONLY_SECRET=keep-me\n",
                );
                path
            }
        };
        let (plan, _) = fixture.render(target);
        let bytes = target_bytes(&plan, &path);
        let text = std::str::from_utf8(bytes).unwrap();
        assert!(text.contains("keep-target-comment"), "{target:?}: {text}");
        assert!(text.contains("TARGET_ONLY_SECRET"), "{target:?}: {text}");
        assert!(text.contains("database"), "{target:?}: {text}");
        assert!(!serde_json::to_string(&plan.report())
            .unwrap()
            .contains("TARGET_ONLY_SECRET"));
        match target {
            SourceTool::Codex => {
                toml_edit::de::from_str::<serde_json::Value>(text).unwrap();
            }
            SourceTool::Claude | SourceTool::OpenCode | SourceTool::Pi => {
                jsonc_parser::parse_to_serde_value(text, &Default::default()).unwrap();
            }
            SourceTool::Dsh => {
                yaml_rust2::YamlLoader::load_from_str(text).unwrap();
                assert!(text.contains("!!js process.cwd()"));
            }
        }
    }
}

#[test]
fn opencode_versions_and_unsupported_transports_are_reported_before_eight_binaries_run() {
    for (version, expected_mcp, expected_command) in [
        (OpenCodeConfigVersion::V1, "mcp", "command"),
        (OpenCodeConfigVersion::V2, "servers", "commands"),
    ] {
        let fixture = Fixture::new(SourceTool::OpenCode);
        let (plan, environment) =
            fixture.render_with(SourceTool::OpenCode, version, VersionSupport::Supported);
        let path = environment.target.project_root.join("opencode.jsonc");
        let text = std::str::from_utf8(target_bytes(&plan, &path)).unwrap();
        let value = jsonc_parser::parse_to_serde_value(text, &Default::default())
            .unwrap()
            .unwrap();
        assert!(value.pointer("/mcp").is_some());
        assert!(value.get(expected_command).is_some());
        if expected_mcp == "servers" {
            assert!(value.pointer("/mcp/servers/database").is_some());
        } else {
            assert!(value.pointer("/mcp/database").is_some());
        }
        assert!(!environment.target.project_root.join(".bin").exists());
    }

    let unknown = Fixture::new(SourceTool::OpenCode);
    let (plan, _) = unknown.render_with(
        SourceTool::OpenCode,
        OpenCodeConfigVersion::Unknown,
        VersionSupport::Supported,
    );
    assert!(plan.items.iter().any(|item| {
        matches!(item.kind, AssetKind::Mcp | AssetKind::Command)
            && item.readiness == Readiness::Unsupported
    }));

    let legacy_pi = Fixture::new(SourceTool::Pi);
    let (plan, _) = legacy_pi.render_with(
        SourceTool::Pi,
        OpenCodeConfigVersion::V2,
        VersionSupport::Unsupported,
    );
    assert!(plan
        .items
        .iter()
        .any(|item| { item.kind == AssetKind::Mcp && item.readiness == Readiness::Unsupported }));
}

#[test]
fn missing_runtime_and_non_portable_transports_require_explicit_user_action() {
    let targets = [
        SourceTool::Codex,
        SourceTool::OpenCode,
        SourceTool::Dsh,
        SourceTool::Pi,
    ];
    for target in targets {
        let fixture = Fixture::with_overrides(
            SourceTool::Claude,
            Some("#!/definitely/missing/migration-runtime\n"),
            Some(r#"{"mcpServers":{"sse":{"type":"sse","url":"https://fixture.invalid/sse"}}}"#),
        );
        let (plan, _) = fixture.render(target);
        assert!(
            plan.items.iter().any(|item| {
                item.kind == AssetKind::Script
                    && item.readiness == Readiness::NeedsConfiguration
                    && item
                        .next_steps
                        .iter()
                        .any(|step| step.contains("migration-runtime"))
            }),
            "{target:?}: {:#?}",
            plan.items
        );
        assert!(plan.items.iter().any(|item| {
            item.kind == AssetKind::Mcp && item.readiness == Readiness::Unsupported
        }));
    }
    let claude = Fixture::with_overrides(
        SourceTool::Claude,
        Some("#!/definitely/missing/migration-runtime\n"),
        Some(r#"{"mcpServers":{"sse":{"type":"sse","url":"https://fixture.invalid/sse"}}}"#),
    );
    let (plan, _) = claude.render(SourceTool::Claude);
    assert!(plan.items.iter().any(|item| {
        item.kind == AssetKind::Mcp && item.readiness == Readiness::NeedsConfiguration
    }));
}

#[test]
fn claude_imports_are_rewritten_and_global_skill_shadowing_is_not_reported_as_available() {
    let claude = Fixture::new(SourceTool::Claude);
    let (plan, environment) = claude.render(SourceTool::Claude);
    let path = environment.target.project_root.join("CLAUDE.md");
    let instructions = std::str::from_utf8(target_bytes(&plan, &path)).unwrap();
    assert!(
        instructions.contains("@.session-loom/instructions/"),
        "{instructions}"
    );
    assert!(instructions.contains("/rules.md"));

    let fixture = Fixture::new(SourceTool::Codex);
    let global_skill = EnvironmentRoots::isolated(&fixture.target_root(SourceTool::Pi))
        .pi
        .join("skills/project-skill/SKILL.md");
    write(
        &global_skill,
        "---\nname: project-skill\ndescription: Existing global shadow\n---\n",
    );
    let (plan, _) = fixture.render(SourceTool::Pi);
    let skill = plan
        .items
        .iter()
        .find(|item| item.kind == AssetKind::Skill)
        .unwrap();
    assert_eq!(skill.readiness, Readiness::NeedsConfiguration);
    assert!(skill
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "targetGlobalSkillShadowing"));
}

#[test]
fn instruction_limits_and_source_target_overlap_require_review() {
    let fixture = Fixture::new(SourceTool::Codex);
    let target_config = fixture
        .target_project(SourceTool::Codex)
        .join(".codex/config.toml");
    write(&target_config, "project_doc_max_bytes = 1\n");
    let (plan, _) = fixture.render(SourceTool::Codex);
    assert!(plan.items.iter().any(|item| {
        matches!(item.kind, AssetKind::Instruction | AssetKind::Memory)
            && item.readiness == Readiness::NeedsConfiguration
            && item
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "targetInstructionLimit")
    }));

    let same_project = Fixture::new(SourceTool::Codex);
    let (plan, _) = same_project.render_at(
        SourceTool::Codex,
        same_project.project.clone(),
        EnvironmentRoots::isolated(&same_project.target_root(SourceTool::Codex)),
        OpenCodeConfigVersion::V2,
        VersionSupport::Supported,
    );
    assert!(plan
        .diagnostics
        .iter()
        .any(|diagnostic| { diagnostic.code == "sourceTargetOverlap" }));
    assert!(plan.items.iter().any(|item| {
        item.readiness == Readiness::NeedsConfiguration
            && item
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "sourceTargetOverlap")
    }));
}

#[test]
#[ignore = "requires the pinned native CLIs and performs isolated local parsing checks"]
fn native_target_parsers_accept_generated_samples() {
    for target in [
        SourceTool::Codex,
        SourceTool::Claude,
        SourceTool::OpenCode,
        SourceTool::Pi,
    ] {
        let fixture = Fixture::new(SourceTool::Codex);
        let (plan, environment) = fixture.render(target);
        materialize_plan(&plan).unwrap();
        for root in [
            &environment.roots.user_home,
            &environment.roots.codex,
            &environment.roots.claude,
            &environment.roots.opencode,
            &environment.roots.dsh,
            &environment.roots.pi,
        ] {
            fs::create_dir_all(root).unwrap();
        }
        let home = &environment.roots.user_home;
        let cwd = &environment.target.project_root;
        let trusted_path = cwd.canonicalize().unwrap();
        let mut trust = serde_json::Map::new();
        trust.insert(
            trusted_path.to_string_lossy().to_string(),
            serde_json::Value::Bool(true),
        );
        write(
            &environment.roots.pi.join("trust.json"),
            &serde_json::Value::Object(trust).to_string(),
        );
        let output = match target {
            SourceTool::Codex => Command::new("codex")
                .args(["mcp", "list", "--json"])
                .env("HOME", home)
                .env("CODEX_HOME", &environment.roots.codex)
                .current_dir(cwd)
                .output(),
            SourceTool::Claude => Command::new("claude")
                .args(["mcp", "get", "database"])
                .env("HOME", home)
                .env("CLAUDE_CONFIG_DIR", &environment.roots.claude)
                .env("DISABLE_AUTOUPDATER", "1")
                .current_dir(cwd)
                .output(),
            SourceTool::OpenCode => Command::new("opencode")
                .arg("debug")
                .arg("config")
                .env("HOME", home)
                .env(
                    "XDG_CONFIG_HOME",
                    environment
                        .roots
                        .opencode
                        .parent()
                        .expect("opencode root has a parent"),
                )
                .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
                .current_dir(cwd)
                .output(),
            SourceTool::Pi => Command::new("pi")
                .args(["mcp", "list", "--json"])
                .env("HOME", home)
                .env("PI_CODING_AGENT_DIR", &environment.roots.pi)
                .env("PI_OFFLINE", "1")
                .current_dir(cwd)
                .output(),
            SourceTool::Dsh => unreachable!("DSH is not part of this native probe"),
        }
        .unwrap_or_else(|error| panic!("{target:?} CLI failed to start: {error}"));
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            combined.contains("database"),
            "{target:?} did not read the generated connection: {}",
            combined
        );
    }
}

fn prepared_target(target: SourceTool) -> (Fixture, TargetEnvironment) {
    let fixture = Fixture::new(SourceTool::Codex);
    let (plan, environment) = fixture.render(target);
    materialize_plan(&plan).unwrap();
    for root in [
        &environment.roots.user_home,
        &environment.roots.codex,
        &environment.roots.claude,
        &environment.roots.opencode,
        &environment.roots.dsh,
        &environment.roots.pi,
    ] {
        fs::create_dir_all(root).unwrap();
    }
    (fixture, environment)
}

fn isolated_command(environment: &TargetEnvironment, program: &str) -> Command {
    let mut command = Command::new(program);
    command
        .env("HOME", &environment.roots.user_home)
        .env("CODEX_HOME", &environment.roots.codex)
        .env("CLAUDE_CONFIG_DIR", &environment.roots.claude)
        .env(
            "XDG_CONFIG_HOME",
            environment.roots.opencode.parent().unwrap(),
        )
        .env("DSH_HOME", &environment.roots.dsh)
        .env("DSH_AGENTS_HOME", &environment.roots.dsh_agents)
        .env("PI_CODING_AGENT_DIR", &environment.roots.pi)
        .env("PI_OFFLINE", "1")
        .env("PI_SKIP_VERSION_CHECK", "1")
        .env("DISABLE_AUTOUPDATER", "1")
        .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
        .current_dir(&environment.target.project_root);
    command
}

fn combined(output: &std::process::Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
#[ignore = "requires pinned native CLIs"]
fn native_codex_prompt_input_reads_generated_instructions_and_skill() {
    let (_fixture, environment) = prepared_target(SourceTool::Codex);
    let output = isolated_command(&environment, "codex")
        .args(["debug", "prompt-input", "ping"])
        .output()
        .unwrap();
    let text = combined(&output);
    assert!(text.contains("Synthetic project instructions."), "{text}");
    assert!(text.contains("project-skill"), "{text}");
}

#[test]
#[ignore = "requires pinned native CLIs"]
fn native_opencode_lists_the_generated_skill() {
    let (_fixture, environment) = prepared_target(SourceTool::OpenCode);
    let output = isolated_command(&environment, "opencode")
        .args(["debug", "skill"])
        .output()
        .unwrap();
    let text = combined(&output);
    assert!(text.contains("project-skill"), "{text}");
}

#[test]
#[ignore = "requires pinned native CLIs"]
fn native_pi_rpc_lists_the_generated_skill() {
    use std::io::Write;
    use std::process::Stdio;
    let (_fixture, environment) = prepared_target(SourceTool::Pi);
    let mut child = isolated_command(&environment, "pi")
        .args([
            "--mode",
            "rpc",
            "--offline",
            "--no-session",
            "--no-mcp",
            "-a",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "{{\"id\":\"1\",\"type\":\"get_commands\"}}").unwrap();
    }
    let output = child.wait_with_output().unwrap();
    let text = combined(&output);
    assert!(text.contains("skill:project-skill"), "{text}");
}

const CLAUDE_STUB: &str = r#"
const http = require('http');
const fs = require('fs');
const log = process.argv[2];
const server = http.createServer((req, res) => {
  let body = '';
  req.on('data', (chunk) => { body += chunk; });
  req.on('end', () => {
    fs.appendFileSync(log, JSON.stringify({ path: req.url, body }) + '\n');
    res.setHeader('content-type', 'application/json');
    if (req.url.includes('count_tokens')) {
      res.end(JSON.stringify({ input_tokens: 12 }));
      return;
    }
    if (req.url.includes('/v1/messages')) {
      let parsed = {};
      try { parsed = JSON.parse(body); } catch {}
      if (parsed.stream) {
        res.setHeader('content-type', 'text/event-stream');
        const events = [
          ['message_start', { type: 'message_start', message: { id: 'msg_fixture', type: 'message', role: 'assistant', model: 'claude-sonnet-4-5', content: [], stop_reason: null, stop_sequence: null, usage: { input_tokens: 12, output_tokens: 1 } } }],
          ['content_block_start', { type: 'content_block_start', index: 0, content_block: { type: 'text', text: '' } }],
          ['content_block_delta', { type: 'content_block_delta', index: 0, delta: { type: 'text_delta', text: 'pong' } }],
          ['content_block_stop', { type: 'content_block_stop', index: 0 }],
          ['message_delta', { type: 'message_delta', delta: { stop_reason: 'end_turn', stop_sequence: null }, usage: { output_tokens: 1 } }],
          ['message_stop', { type: 'message_stop' }],
        ];
        for (const [name, data] of events) res.write(`event: ${name}\ndata: ${JSON.stringify(data)}\n\n`);
        res.end();
        return;
      }
      res.end(JSON.stringify({ id: 'msg_fixture', type: 'message', role: 'assistant', model: 'claude-sonnet-4-5', content: [{ type: 'text', text: 'pong' }], stop_reason: 'end_turn', stop_sequence: null, usage: { input_tokens: 12, output_tokens: 1 } }));
      return;
    }
    res.end('{}');
  });
});
server.listen(0, '127.0.0.1', () => console.log(server.address().port));
"#;

#[test]
#[ignore = "requires pinned native CLIs"]
fn native_claude_request_reads_generated_instruction_and_skill() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let (fixture, environment) = prepared_target(SourceTool::Claude);
    let stub_path = fixture._temp.path().join("claude-stub.js");
    let log_path = fixture._temp.path().join("claude-requests.jsonl");
    fs::write(&stub_path, CLAUDE_STUB).unwrap();
    fs::write(
        environment.roots.user_home.join(".claude.json"),
        r#"{"hasCompletedOnboarding":true,"theme":"dark"}"#,
    )
    .unwrap();
    let mut stub = Command::new("node")
        .arg(&stub_path)
        .arg(&log_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut port = String::new();
    BufReader::new(stub.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    let port = port.trim().to_string();
    let output = isolated_command(&environment, "claude")
        .env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{port}"))
        .env("ANTHROPIC_API_KEY", "fixture-key")
        .env("ANTHROPIC_AUTH_TOKEN", "fixture-token")
        .args([
            "--print",
            "--model",
            "claude-sonnet-4-5",
            "--output-format",
            "text",
            "--permission-mode",
            "dontAsk",
            "ping",
        ])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = stub.kill();
    let _ = stub.wait();
    let text = combined(&output);
    assert!(text.contains("pong"), "{text}");
    let requests = fs::read_to_string(&log_path).unwrap();
    assert!(
        requests.contains("Synthetic project instructions."),
        "{requests}"
    );
    assert!(requests.contains("project-skill"), "{requests}");
    let _ = environment;
    let _ = fixture;
}

#[test]
#[ignore = "requires the pinned DSH CLI via DSH_BIN"]
fn native_dsh_composes_the_generated_profile_patch() {
    let dsh = std::env::var("DSH_BIN").unwrap_or_else(|_| "dsh".to_string());
    let fixture = Fixture::new(SourceTool::Codex);
    let dsh_home = fixture.target_root(SourceTool::Dsh).join("dsh");
    fs::create_dir_all(&dsh_home).unwrap();
    let init = Command::new(&dsh)
        .env("DSH_HOME", &dsh_home)
        .args([
            "--profile",
            "fixture",
            "--from-default-profile",
            "headless",
            "--dump-config",
        ])
        .output()
        .expect("DSH_BIN must point to the dsh launcher");
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let (plan, _environment) = fixture.render(SourceTool::Dsh);
    materialize_plan(&plan).unwrap();
    let patch_path = dsh_home.join("profiles/fixture/cordis.patch.yml");
    let patch = fs::read_to_string(&patch_path).unwrap();
    assert!(
        patch.contains("@deepseek-ai/dsh-mcp-client"),
        "generated patch is missing the MCP row:
{patch}"
    );
    let dump = Command::new(&dsh)
        .env("DSH_HOME", &dsh_home)
        .args(["--profile", "fixture", "--dump-config"])
        .output()
        .unwrap();
    let text = combined(&dump);
    assert!(
        dump.status.success(),
        "{}",
        String::from_utf8_lossy(&dump.stderr)
    );
    assert!(text.contains("@deepseek-ai/dsh-mcp-client"), "{text}");
    assert!(text.contains("database"), "{text}");
}

const MCP_STUB: &str = r#"
const readline = require('readline');
const rl = readline.createInterface({ input: process.stdin });
function send(value) { process.stdout.write(JSON.stringify(value) + '\n'); }
rl.on('line', (line) => {
  let message;
  try { message = JSON.parse(line); } catch { return; }
  if (message.method === 'initialize') {
    send({ jsonrpc: '2.0', id: message.id, result: { protocolVersion: '2024-11-05', capabilities: { tools: {} }, serverInfo: { name: 'fixture-mcp', version: '0.0.1' } } });
  } else if (message.method === 'notifications/initialized') {
  } else if (message.method === 'tools/list') {
    send({ jsonrpc: '2.0', id: message.id, result: { tools: [{ name: 'fixture_echo', description: 'Echo fixture', inputSchema: { type: 'object', properties: {}, additionalProperties: false } }] } });
  } else if (message.method === 'resources/list' || message.method === 'resources/templates/list' || message.method === 'prompts/list') {
    send({ jsonrpc: '2.0', id: message.id, result: {} });
  } else if (message.method === 'tools/call') {
    send({ jsonrpc: '2.0', id: message.id, result: { content: [{ type: 'text', text: 'fixture-ok' }] } });
  } else if (message.id !== undefined) {
    send({ jsonrpc: '2.0', id: message.id, error: { code: -32601, message: 'method not found' } });
  }
});
"#;

fn install_mcp_stub(root: &Path) -> PathBuf {
    let server = root.join("fixture-mcp.js");
    fs::write(&server, MCP_STUB).unwrap();
    let bin = root.join("fixture-bin");
    fs::create_dir_all(&bin).unwrap();
    let wrapper = bin.join("fixture-mcp");
    fs::write(
        &wrapper,
        format!("#!/bin/sh\nexec node \"{}\" \"$@\"\n", server.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

fn with_path(command: &mut Command, bin: &Path) {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut paths = vec![bin.to_path_buf()];
    paths.extend(std::env::split_paths(&path));
    command.env("PATH", std::env::join_paths(paths).unwrap());
}

#[test]
#[ignore = "requires pinned native CLIs and a local MCP fixture"]
fn native_generated_mcp_configs_connect_to_a_local_server() {
    // Pi: connects and discovers the fixture tool.
    let fixture = Fixture::new(SourceTool::Codex);
    let bin = install_mcp_stub(fixture._temp.path());
    let (plan, environment) = fixture.render(SourceTool::Pi);
    materialize_plan(&plan).unwrap();
    for root in [&environment.roots.user_home, &environment.roots.pi] {
        fs::create_dir_all(root).unwrap();
    }
    let trusted = environment.target.project_root.canonicalize().unwrap();
    let mut trust = serde_json::Map::new();
    trust.insert(
        trusted.to_string_lossy().to_string(),
        serde_json::Value::Bool(true),
    );
    fs::write(
        environment.roots.pi.join("trust.json"),
        serde_json::Value::Object(trust).to_string(),
    )
    .unwrap();
    let mut command = isolated_command(&environment, "pi");
    with_path(&mut command, &bin);
    let output = command.args(["mcp", "list", "--json"]).output().unwrap();
    let text = combined(&output);
    let value: serde_json::Value = serde_json::from_str(&String::from_utf8_lossy(&output.stdout))
        .unwrap_or_else(|_| panic!("pi mcp list did not return JSON: {text}"));
    assert_eq!(value["servers"][0]["name"], "database", "{text}");
    assert_eq!(value["servers"][0]["state"], "connected", "{text}");
    assert_eq!(value["servers"][0]["tools"][0], "fixture_echo", "{text}");

    // OpenCode 1.18 uses the V1 namespace; the installed CLI must connect with it.
    let fixture = Fixture::new(SourceTool::Codex);
    let bin = install_mcp_stub(fixture._temp.path());
    let (plan, environment) = fixture.render_with(
        SourceTool::OpenCode,
        OpenCodeConfigVersion::V1,
        VersionSupport::Supported,
    );
    materialize_plan(&plan).unwrap();
    for root in [&environment.roots.user_home, &environment.roots.opencode] {
        fs::create_dir_all(root).unwrap();
    }
    let mut command = isolated_command(&environment, "opencode");
    with_path(&mut command, &bin);
    let output = command.args(["mcp", "list"]).output().unwrap();
    let text = combined(&output);
    assert!(text.contains("database"), "{text}");
    assert!(text.contains("connected"), "{text}");

    // Claude: the model request must include the MCP tool discovered from the
    // generated project configuration.
    let fixture = Fixture::new(SourceTool::Codex);
    let bin = install_mcp_stub(fixture._temp.path());
    let (plan, environment) = fixture.render(SourceTool::Claude);
    materialize_plan(&plan).unwrap();
    for root in [&environment.roots.user_home, &environment.roots.claude] {
        fs::create_dir_all(root).unwrap();
    }
    fs::write(
        environment.roots.user_home.join(".claude.json"),
        r#"{"hasCompletedOnboarding":true,"theme":"dark"}"#,
    )
    .unwrap();
    let stub_path = fixture._temp.path().join("claude-mcp-stub.js");
    let log_path = fixture._temp.path().join("claude-mcp-requests.jsonl");
    fs::write(&stub_path, CLAUDE_STUB).unwrap();
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let mut stub = Command::new("node")
        .arg(&stub_path)
        .arg(&log_path)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut port = String::new();
    BufReader::new(stub.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    let port = port.trim().to_string();
    let mcp_config = environment.target.project_root.join(".mcp.json");
    let mut command = isolated_command(&environment, "claude");
    with_path(&mut command, &bin);
    let output = command
        .env("ANTHROPIC_BASE_URL", format!("http://127.0.0.1:{port}"))
        .env("ANTHROPIC_API_KEY", "fixture-key")
        .env("ANTHROPIC_AUTH_TOKEN", "fixture-token")
        .arg("--print")
        .arg("--model")
        .arg("claude-sonnet-4-5")
        .arg("--output-format")
        .arg("text")
        .arg("--permission-mode")
        .arg("dontAsk")
        .arg("--mcp-config")
        .arg(&mcp_config)
        .arg("--strict-mcp-config")
        .arg("ping")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let _ = stub.kill();
    let _ = stub.wait();
    let text = combined(&output);
    assert!(text.contains("pong"), "{text}");
    let requests = fs::read_to_string(&log_path).unwrap();
    assert!(requests.contains("fixture_echo"), "{requests}");
}

#[test]
#[ignore = "requires the pinned DSH CLI via DSH_BIN"]
fn native_dsh_resumes_a_generated_session() {
    use session_loom_core::{
        adapters::dsh,
        canonical::{CanonicalSession, Message, Role, SourceTool, CANONICAL_SCHEMA_VERSION},
    };
    let dsh_bin = std::env::var("DSH_BIN").unwrap_or_else(|_| "dsh".to_string());
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(temp.path().join("home")).unwrap();
    let dsh_home = temp.path().join("dsh");
    fs::create_dir_all(&dsh_home).unwrap();
    let _guard = common::isolate_dsh_home(&dsh_home);
    let sessions = dsh_home.join("sessions");
    fs::create_dir_all(&sessions).unwrap();
    let session = CanonicalSession {
        schema_version: CANONICAL_SCHEMA_VERSION,
        source_tool: SourceTool::Codex,
        session_id: "source".into(),
        cwd: project.to_string_lossy().to_string(),
        created_at: "2026-11-01T00:00:00Z".into(),
        updated_at: "2026-11-01T00:00:01Z".into(),
        model_provider: None,
        model: None,
        title: Some("Native DSH session title".into()),
        messages: vec![Message {
            role: Role::User,
            text: "Synthetic native resume fixture.".into(),
            tool_calls: vec![],
        }],
    };
    let written =
        dsh::write_session_to_root_with_id(&session, &sessions, "session-fixture-native").unwrap();
    assert!(written.log_file.is_file());
    let init = Command::new(&dsh_bin)
        .env("DSH_HOME", &dsh_home)
        .args([
            "--profile",
            "fixture",
            "--from-default-profile",
            "headless",
            "--dump-config",
        ])
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let output = Command::new(&dsh_bin)
        .env("DSH_HOME", &dsh_home)
        .env("HOME", temp.path().join("home"))
        .current_dir(&project)
        .args([
            "fixture",
            "--session-id",
            "session-fixture-native",
            "--json",
            "ping",
        ])
        .output()
        .unwrap();
    let text = combined(&output);
    assert!(text.contains("\"type\":\"session\""), "{text}");
    assert!(text.contains("session-fixture-native"), "{text}");
    assert!(!text.contains("failed to observe session"), "{text}");
}

#[test]
fn an_untrusted_target_marks_project_loading_as_needs_configuration() {
    let fixture = Fixture::new(SourceTool::Codex);
    let target_project = fixture.target_project(SourceTool::Claude);
    let target_roots = EnvironmentRoots::isolated(&fixture.target_root(SourceTool::Claude));
    let (plan, _) = fixture.render_at_trusted(
        SourceTool::Claude,
        target_project,
        target_roots,
        OpenCodeConfigVersion::V2,
        VersionSupport::Supported,
        false,
    );
    for kind in [AssetKind::Instruction, AssetKind::Skill, AssetKind::Mcp] {
        let item = plan.items.iter().find(|item| item.kind == kind).unwrap();
        assert_eq!(item.readiness, Readiness::NeedsConfiguration, "{kind:?}");
        assert!(
            item.message.contains("信任") || item.message.contains("trust"),
            "{kind:?}: {}",
            item.message
        );
    }
}

#[test]
fn a_source_that_is_also_the_target_keeps_equal_files_and_blocks_changes() {
    let fixture = Fixture::new(SourceTool::Codex);
    let (plan, _) = fixture.render_at(
        SourceTool::Codex,
        fixture.project.clone(),
        EnvironmentRoots::isolated(&fixture.target_root(SourceTool::Codex)),
        OpenCodeConfigVersion::V2,
        VersionSupport::Supported,
    );
    assert!(plan
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "sourceTargetOverlap"));
    let skill = plan
        .items
        .iter()
        .find(|item| item.kind == AssetKind::Skill)
        .unwrap();
    assert_eq!(skill.action, "keep-existing");
    assert!(!plan.files.iter().any(|file| file
        .target_path
        .ends_with(".agents/skills/project-skill/SKILL.md")));
    let instruction = plan
        .items
        .iter()
        .find(|item| item.kind == AssetKind::Instruction)
        .unwrap();
    assert_eq!(instruction.action, "keep-existing");
    assert!(instruction.message.contains("保留现有文件"));
    // Adding other selected content must still not modify a source entry.
    let memory = plan
        .items
        .iter()
        .find(|item| item.kind == AssetKind::Memory)
        .unwrap();
    assert_eq!(memory.readiness, Readiness::NeedsConfiguration);
    assert!(memory
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "sourceTargetOverlap"));
}
