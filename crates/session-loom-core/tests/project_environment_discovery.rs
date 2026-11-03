use session_loom_core::{
    canonical::{SourceTool, ToolCall},
    project_migration::{
        discover_environment, resolve_project, select_project_sessions, AssetKind, AssetScope,
        DiscoveryOptions, EnvironmentRoots,
    },
    store::Store,
};
use std::{fs, path::Path};

#[path = "common/mod.rs"]
mod common;

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn skill(root: &Path, name: &str) {
    write(&root.join("SKILL.md"), &format!("---\nname: {name}\ndescription: Synthetic migration fixture\n---\nRead references/details.md.\n"));
    write(
        &root.join("references/details.md"),
        "Only synthetic project details.\n",
    );
    write(
        &root.join("scripts/check.sh"),
        "#!/bin/sh\nprintf 'fixture'\n",
    );
}

#[test]
fn each_native_channel_discovers_memory_skills_connections_and_commands() {
    for tool in [
        SourceTool::Codex,
        SourceTool::Claude,
        SourceTool::OpenCode,
        SourceTool::Dsh,
        SourceTool::Pi,
    ] {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let roots = EnvironmentRoots::isolated(&temp.path().join("agent-roots"));
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
            r#"{"scripts":{"check":"node scripts/check.cjs"}}"#,
        );
        write(
            &project.join("scripts/check.cjs"),
            "console.log('fixture');\n",
        );
        write(
            &project.join("scripts/support.cjs"),
            "module.exports = 'synthetic companion';\n",
        );
        let options = DiscoveryOptions {
            dsh_profile: Some("fixture".into()),
            ..Default::default()
        };
        match tool {
            SourceTool::Codex => {
                skill(
                    &project.join(".agents/skills/project-skill"),
                    "project-skill",
                );
                write(
                    &project.join(".codex/config.toml"),
                    include_str!("fixtures/project_migration/codex/config.toml"),
                );
            }
            SourceTool::Claude => {
                skill(
                    &project.join(".claude/skills/project-skill"),
                    "project-skill",
                );
                write(
                    &project.join(".mcp.json"),
                    include_str!("fixtures/project_migration/claude/mcp.json"),
                );
                let key = session_loom_core::adapters::encode_claude_project(
                    project.canonicalize().unwrap().to_str().unwrap(),
                );
                write(
                    &roots
                        .claude
                        .join("projects")
                        .join(key)
                        .join("memory/MEMORY.md"),
                    "Synthetic auto memory outside the project.\n",
                );
            }
            SourceTool::OpenCode => {
                skill(
                    &project.join(".opencode/skills/project-skill"),
                    "project-skill",
                );
                write(
                    &project.join("opencode.jsonc"),
                    include_str!("fixtures/project_migration/opencode/v2.jsonc"),
                );
            }
            SourceTool::Dsh => {
                skill(&project.join(".dsh/skills/project-skill"), "project-skill");
                write(
                    &roots.dsh.join("profiles/fixture/cordis.patch.yml"),
                    include_str!("fixtures/project_migration/dsh/cordis.patch.yml"),
                );
            }
            SourceTool::Pi => {
                skill(&project.join(".pi/skills/project-skill"), "project-skill");
                write(
                    &project.join(".pi/mcp.json"),
                    include_str!("fixtures/project_migration/pi/mcp.json"),
                );
            }
        }
        let store = Store::open(&temp.path().join("store")).unwrap();
        let mut session = common::migration_session("native", &project, tool);
        session.messages[0].tool_calls.push(ToolCall {
            id: "fixture-call".into(),
            name: "mcp__database__query".into(),
            input: serde_json::json!({}),
            output: None,
        });
        store.write_session(&session).unwrap();
        let identity = resolve_project(project.to_str().unwrap());
        let sessions = select_project_sessions(&store, &identity.id, &["native".into()]).unwrap();
        let report = discover_environment(&identity, &sessions, &roots, &options).unwrap();
        for kind in [
            AssetKind::Skill,
            AssetKind::Mcp,
            AssetKind::Script,
            AssetKind::Command,
        ] {
            assert!(
                report.assets.iter().any(|asset| asset.kind == kind),
                "{tool:?}: missing {kind:?}; {:?}",
                report.diagnostics
            );
        }
        assert!(report
            .assets
            .iter()
            .any(|asset| matches!(asset.kind, AssetKind::Memory | AssetKind::Instruction)));
        let connection = report
            .assets
            .iter()
            .find(|asset| asset.kind == AssetKind::Mcp)
            .unwrap();
        assert!(connection
            .evidence
            .iter()
            .any(|evidence| evidence.kind == "selectedSessionToolCall"));
        let bundle = report
            .assets
            .iter()
            .find(|asset| asset.kind == AssetKind::Skill)
            .unwrap();
        assert!(bundle
            .files
            .iter()
            .any(|file| file.relative_path == Path::new("scripts/check.sh")));
        assert!(bundle
            .files
            .iter()
            .any(|file| file.relative_path == Path::new("references/details.md")));
        let script = report
            .assets
            .iter()
            .find(|asset| asset.kind == AssetKind::Script)
            .unwrap();
        assert!(script
            .files
            .iter()
            .any(|file| file.relative_path == Path::new("support.cjs")));
        assert!(!serde_json::to_string(&report)
            .unwrap()
            .contains("fixture-only-do-not-copy"));
        assert!(report
            .assets
            .iter()
            .filter(|asset| asset.scope != AssetScope::Project)
            .all(|asset| !asset.selected_by_default));
    }
}

#[test]
fn shared_skills_are_deduplicated_and_global_availability_is_not_usage_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let roots = EnvironmentRoots::isolated(&temp.path().join("agent-roots"));
    write(&project.join("AGENTS.md"), "Synthetic instructions.");
    skill(
        &project.join(".agents/skills/shared-project"),
        "shared-project",
    );
    skill(&roots.agents.join("skills/unused-global"), "unused-global");
    write(
        &roots.codex.join("config.toml"),
        "[mcp_servers.unused]\ncommand = 'fixture-mcp'\n",
    );
    write(
        &roots.codex.join("memories/MEMORY.md"),
        "Synthetic entries for several different projects.",
    );
    let store = Store::open(&temp.path().join("store")).unwrap();
    for (id, tool) in [
        ("codex", SourceTool::Codex),
        ("pi", SourceTool::Pi),
        ("opencode", SourceTool::OpenCode),
        ("dsh", SourceTool::Dsh),
    ] {
        store
            .write_session(&common::migration_session(id, &project, tool))
            .unwrap();
    }
    let identity = resolve_project(project.to_str().unwrap());
    let sessions = select_project_sessions(
        &store,
        &identity.id,
        &["codex".into(), "pi".into(), "opencode".into(), "dsh".into()],
    )
    .unwrap();
    let report = discover_environment(&identity, &sessions, &roots, &Default::default()).unwrap();
    assert_eq!(
        report
            .assets
            .iter()
            .filter(|asset| asset.kind == AssetKind::Skill && asset.name == "shared-project")
            .count(),
        1
    );
    let global = report
        .assets
        .iter()
        .find(|asset| asset.kind == AssetKind::Mcp && asset.name == "unused")
        .unwrap();
    assert!(!global.selected_by_default);
    assert_eq!(global.evidence[0].kind, "candidateOnly");
    assert!(report
        .assets
        .iter()
        .filter(|asset| asset.kind == AssetKind::Memory && asset.scope == AssetScope::User)
        .all(|asset| !asset.selected_by_default));
}

#[test]
fn claude_custom_project_memory_is_found_outside_the_project() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let memory = temp.path().join("memory-data");
    write(
        &memory.join("MEMORY.md"),
        "Synthetic custom project memory.",
    );
    write(
        &project.join(".claude/settings.json"),
        &serde_json::json!({"autoMemoryDirectory":memory}).to_string(),
    );
    let roots = EnvironmentRoots::isolated(&temp.path().join("agent-roots"));
    let mut projects = serde_json::Map::new();
    projects.insert(
        project
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string(),
        serde_json::json!({"hasTrustDialogAccepted":true}),
    );
    write(
        &roots.claude_user_config,
        &serde_json::json!({"projects":projects}).to_string(),
    );
    let store = Store::open(&temp.path().join("store")).unwrap();
    store
        .write_session(&common::migration_session(
            "claude",
            &project,
            SourceTool::Claude,
        ))
        .unwrap();
    let identity = resolve_project(project.to_str().unwrap());
    let selected = select_project_sessions(&store, &identity.id, &["claude".into()]).unwrap();
    let report = discover_environment(&identity, &selected, &roots, &Default::default()).unwrap();
    let asset = report
        .assets
        .iter()
        .find(|asset| asset.kind == AssetKind::Memory)
        .unwrap();
    assert_eq!(asset.scope, AssetScope::Project);
    assert!(asset.selected_by_default);
    assert!(asset.files[0]
        .path
        .starts_with(memory.canonicalize().unwrap()));
}

#[test]
fn claude_imports_include_local_dependencies_and_leave_external_ones_optional() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let external = temp.path().join("external.md");
    write(&external, "Synthetic external notes.");
    write(
        &project.join("CLAUDE.md"),
        &format!(
            "@rules.md\n@{}\n`@example.md`\n```text\n@not-an-import.md\n```\n",
            external.display()
        ),
    );
    write(
        &project.join("rules.md"),
        "Synthetic imported project rules.",
    );
    let roots = EnvironmentRoots::isolated(&temp.path().join("roots"));
    let store = Store::open(&temp.path().join("store")).unwrap();
    store
        .write_session(&common::migration_session(
            "native",
            &project,
            SourceTool::Claude,
        ))
        .unwrap();
    let identity = resolve_project(project.to_str().unwrap());
    let selected = select_project_sessions(&store, &identity.id, &["native".into()]).unwrap();
    let report = discover_environment(&identity, &selected, &roots, &Default::default()).unwrap();
    let main = report
        .assets
        .iter()
        .find(|asset| asset.name == "CLAUDE.md")
        .unwrap();
    assert_eq!(
        main.definition.as_ref().unwrap()["imports"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let local = report
        .assets
        .iter()
        .find(|asset| asset.name == "rules.md")
        .unwrap();
    assert!(local.selected_by_default && local.scope == AssetScope::Project);
    let outside = report
        .assets
        .iter()
        .find(|asset| asset.name == "external.md")
        .unwrap();
    assert!(!outside.selected_by_default && outside.scope == AssetScope::User);
    assert!(!report.source_probes.iter().any(
        |probe| probe.path.ends_with("example.md") || probe.path.ends_with("not-an-import.md")
    ));
}

#[test]
fn credential_files_are_not_snapshottable_skill_resources_or_instruction_imports() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let synthetic_value = format!("synthetic-{}", uuid::Uuid::new_v4());
    let environment = format!("FIXTURE_TOKEN={synthetic_value}\n");
    write(&project.join("CLAUDE.md"), "@.env\n@rules.md\n");
    write(&project.join("rules.md"), "@CLAUDE.md\n");
    write(&project.join(".env"), &environment);
    skill(&project.join(".claude/skills/fixture"), "fixture");
    write(&project.join(".claude/skills/fixture/.env"), &environment);
    let roots = EnvironmentRoots::isolated(&temp.path().join("roots"));
    let store = Store::open(&temp.path().join("store")).unwrap();
    store
        .write_session(&common::migration_session(
            "native",
            &project,
            SourceTool::Claude,
        ))
        .unwrap();
    let identity = resolve_project(project.to_str().unwrap());
    let selected = select_project_sessions(&store, &identity.id, &["native".into()]).unwrap();
    let report = discover_environment(&identity, &selected, &roots, &Default::default()).unwrap();
    assert!(report
        .assets
        .iter()
        .all(|asset| asset
            .files
            .iter()
            .all(|file| file.path.file_name().unwrap() != ".env")));
    assert!(report
        .assets
        .iter()
        .flat_map(|asset| &asset.diagnostics)
        .any(|diagnostic| diagnostic.code == "credentialResourceExcluded"));
    assert!(report
        .assets
        .iter()
        .flat_map(|asset| &asset.diagnostics)
        .any(|diagnostic| diagnostic.code == "instructionImportCycle"));
    assert!(!serde_json::to_string(&report)
        .unwrap()
        .contains(&synthetic_value));
}

#[test]
fn unverified_project_configuration_cannot_auto_select_external_memory() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let external = temp.path().join("external-notes");
    write(&external.join("MEMORY.md"), "Synthetic external notes.");
    write(
        &project.join(".claude/settings.json"),
        &serde_json::json!({"autoMemoryDirectory":external}).to_string(),
    );
    let roots = EnvironmentRoots::isolated(&temp.path().join("roots"));
    let store = Store::open(&temp.path().join("store")).unwrap();
    store
        .write_session(&common::migration_session(
            "native",
            &project,
            SourceTool::Claude,
        ))
        .unwrap();
    let identity = resolve_project(project.to_str().unwrap());
    let selected = select_project_sessions(&store, &identity.id, &["native".into()]).unwrap();
    let report = discover_environment(&identity, &selected, &roots, &Default::default()).unwrap();
    let memory = report
        .assets
        .iter()
        .find(|asset| asset.kind == AssetKind::Memory)
        .unwrap();
    assert_eq!(memory.scope, AssetScope::User);
    assert!(!memory.selected_by_default);
    assert!(report
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "externalMemoryNeedsSelection"));
}

#[test]
fn source_precedence_is_native_and_shadowed_definitions_are_not_auto_selected() {
    for tool in [SourceTool::Claude, SourceTool::Pi] {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        let roots = EnvironmentRoots::isolated(&temp.path().join("roots"));
        let (project_dir, user_dir) = if tool == SourceTool::Claude {
            (
                project.join(".claude/skills/duplicate"),
                roots.claude.join("skills/duplicate"),
            )
        } else {
            (
                project.join(".pi/skills/duplicate"),
                roots.pi.join("skills/duplicate"),
            )
        };
        skill(&project_dir, "duplicate");
        skill(&user_dir, "duplicate");
        let store = Store::open(&temp.path().join("store")).unwrap();
        store
            .write_session(&common::migration_session("native", &project, tool))
            .unwrap();
        let identity = resolve_project(project.to_str().unwrap());
        let selected = select_project_sessions(&store, &identity.id, &["native".into()]).unwrap();
        let report =
            discover_environment(&identity, &selected, &roots, &Default::default()).unwrap();
        let user = report
            .assets
            .iter()
            .find(|asset| asset.kind == AssetKind::Skill && asset.scope == AssetScope::User)
            .unwrap();
        let local = report
            .assets
            .iter()
            .find(|asset| asset.kind == AssetKind::Skill && asset.scope == AssetScope::Project)
            .unwrap();
        assert!(user.source_active);
        assert!(!user.selected_by_default);
        assert!(!local.source_active && !local.selected_by_default);
        assert!(local
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "sourceShadowed"));
    }
}

#[test]
fn opencode_versions_use_distinct_mcp_and_command_namespaces() {
    for (contents, expected) in [
        (
            include_str!("fixtures/project_migration/opencode/v1.jsonc"),
            "opencode-v1-mcp",
        ),
        (
            include_str!("fixtures/project_migration/opencode/v2.jsonc"),
            "opencode-v2-mcp",
        ),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        write(&project.join("opencode.jsonc"), contents);
        let roots = EnvironmentRoots::isolated(&temp.path().join("roots"));
        let store = Store::open(&temp.path().join("store")).unwrap();
        store
            .write_session(&common::migration_session(
                "native",
                &project,
                SourceTool::OpenCode,
            ))
            .unwrap();
        let identity = resolve_project(project.to_str().unwrap());
        let selected = select_project_sessions(&store, &identity.id, &["native".into()]).unwrap();
        let report =
            discover_environment(&identity, &selected, &roots, &Default::default()).unwrap();
        let connection = report
            .assets
            .iter()
            .find(|asset| asset.kind == AssetKind::Mcp)
            .unwrap();
        assert_eq!(connection.name, "database");
        assert_eq!(connection.format, expected);
        assert!(report
            .assets
            .iter()
            .any(|asset| asset.kind == AssetKind::Command && asset.name == "review"));
    }
}

#[test]
fn tagged_yaml_is_data_and_flat_skills_do_not_capture_other_skills() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let roots = EnvironmentRoots::isolated(&temp.path().join("roots"));
    fs::create_dir_all(&project).unwrap();
    write(
        &project.join(".dsh/skills/flat.md"),
        "---\nname: flat\ndescription: Synthetic flat skill\n---\nNo actions are executed.\n",
    );
    write(
        &project.join(".dsh/skills/another.md"),
        "---\nname: another\ndescription: Other synthetic skill\n---\nOther content.\n",
    );
    write(
        &roots.dsh.join("profiles/fixture/cordis.patch.yml"),
        include_str!("fixtures/project_migration/dsh/cordis.patch.yml"),
    );
    let store = Store::open(&temp.path().join("store")).unwrap();
    store
        .write_session(&common::migration_session(
            "native",
            &project,
            SourceTool::Dsh,
        ))
        .unwrap();
    let identity = resolve_project(project.to_str().unwrap());
    let selected = select_project_sessions(&store, &identity.id, &["native".into()]).unwrap();
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
    let flat = report
        .assets
        .iter()
        .find(|asset| asset.kind == AssetKind::Skill && asset.name == "flat")
        .unwrap();
    assert_eq!(flat.files.len(), 1);
    assert_eq!(flat.files[0].relative_path, Path::new("flat.md"));
    let connection = report
        .assets
        .iter()
        .find(|asset| asset.kind == AssetKind::Mcp)
        .unwrap();
    assert_eq!(
        connection
            .definition
            .as_ref()
            .unwrap()
            .pointer("/config/cwd/value")
            .unwrap(),
        "process.cwd()"
    );
    assert!(connection
        .definition
        .as_ref()
        .unwrap()
        .pointer("/config/cwd/$yamlTag")
        .is_some());
}

#[cfg(unix)]
#[test]
fn cycles_and_external_resource_links_are_reported_without_following_them() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let package = project.join(".agents/skills/fixture");
    skill(&package, "fixture");
    let outside = temp.path().join("outside.txt");
    write(&outside, "Not a selected resource.");
    symlink(&package, package.join("loop")).unwrap();
    symlink(&outside, package.join("outside.txt")).unwrap();
    let roots = EnvironmentRoots::isolated(&temp.path().join("roots"));
    let store = Store::open(&temp.path().join("store")).unwrap();
    store
        .write_session(&common::migration_session(
            "native",
            &project,
            SourceTool::Codex,
        ))
        .unwrap();
    let identity = resolve_project(project.to_str().unwrap());
    let selected = select_project_sessions(&store, &identity.id, &["native".into()]).unwrap();
    let report = discover_environment(&identity, &selected, &roots, &Default::default()).unwrap();
    let asset = report
        .assets
        .iter()
        .find(|asset| asset.kind == AssetKind::Skill)
        .unwrap();
    assert!(asset
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "resourceCycle"));
    assert!(asset
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "externalResource"));
    assert!(!asset.files.iter().any(|file| file.path == outside));
}

#[test]
fn invalid_and_non_utf8_configuration_has_a_visible_diagnostic() {
    for bytes in [
        b"not valid JSON".as_slice(),
        b"[]".as_slice(),
        &[0xff, 0xfe],
    ] {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join(".mcp.json"), bytes).unwrap();
        let roots = EnvironmentRoots::isolated(&temp.path().join("roots"));
        let store = Store::open(&temp.path().join("store")).unwrap();
        store
            .write_session(&common::migration_session(
                "native",
                &project,
                SourceTool::Claude,
            ))
            .unwrap();
        let identity = resolve_project(project.to_str().unwrap());
        let selected = select_project_sessions(&store, &identity.id, &["native".into()]).unwrap();
        let report =
            discover_environment(&identity, &selected, &roots, &Default::default()).unwrap();
        assert!(report.diagnostics.iter().any(|diagnostic| diagnostic
            .path
            .as_ref()
            .and_then(|path| path.canonicalize().ok())
            == Some(project.join(".mcp.json").canonicalize().unwrap())));
        assert!(!report
            .assets
            .iter()
            .any(|asset| asset.kind == AssetKind::Mcp));
    }
}

#[test]
fn parse_failures_do_not_echo_source_credential_lines() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    write(
        &project.join(".codex/config.toml"),
        "[mcp_servers.fixture.env]\nTOKEN = 'fixture-secret-must-not-be-echoed' trailing-junk\n",
    );
    let roots = EnvironmentRoots::isolated(&temp.path().join("roots"));
    let store = Store::open(&temp.path().join("store")).unwrap();
    store
        .write_session(&common::migration_session(
            "native",
            &project,
            SourceTool::Codex,
        ))
        .unwrap();
    let identity = resolve_project(project.to_str().unwrap());
    let selected = select_project_sessions(&store, &identity.id, &["native".into()]).unwrap();
    let report = discover_environment(&identity, &selected, &roots, &Default::default()).unwrap();
    assert!(report
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "configurationInvalid"
            && diagnostic.message.contains("line 2")));
    assert!(!serde_json::to_string(&report)
        .unwrap()
        .contains("fixture-secret-must-not-be-echoed"));
}

#[test]
fn credentials_in_arguments_headers_and_addresses_are_removed_but_refs_remain() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let secret = format!("synthetic-{}", uuid::Uuid::new_v4());
    let value = serde_json::json!({"mcpServers": {
        "arguments": {"command":"fixture-mcp","args":["--token",secret,"--mode","fixture"]},
        "address": {"type":"http","url":format!("https://user:{secret}@fixture.invalid/mcp?api%5fkey={secret}&mode=fixture"),"headers":{"X-API-Key":secret,"Authorization":"Bearer ${FIXTURE_TOKEN}"}}
    }});
    write(&project.join(".mcp.json"), &value.to_string());
    let roots = EnvironmentRoots::isolated(&temp.path().join("roots"));
    let store = Store::open(&temp.path().join("store")).unwrap();
    store
        .write_session(&common::migration_session(
            "native",
            &project,
            SourceTool::Claude,
        ))
        .unwrap();
    let identity = resolve_project(project.to_str().unwrap());
    let selected = select_project_sessions(&store, &identity.id, &["native".into()]).unwrap();
    let report = discover_environment(&identity, &selected, &roots, &Default::default()).unwrap();
    assert!(!serde_json::to_string(&report).unwrap().contains(&secret));
    let address = report
        .assets
        .iter()
        .find(|asset| asset.name == "address")
        .unwrap();
    assert_eq!(
        address.definition.as_ref().unwrap()["url"],
        "https://fixture.invalid/mcp?mode=fixture"
    );
    assert_eq!(
        address.definition.as_ref().unwrap()["headers"]["Authorization"],
        "Bearer ${FIXTURE_TOKEN}"
    );
    let arguments = report
        .assets
        .iter()
        .find(|asset| asset.name == "arguments")
        .unwrap();
    assert_eq!(
        arguments.definition.as_ref().unwrap()["args"],
        serde_json::json!(["--mode", "fixture"])
    );
    assert!(address
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.code == "credentialRequired"));
}

#[test]
fn a_direct_script_command_discovers_its_entry_and_companion_files() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    write(
        &project.join("package.json"),
        "{\"scripts\":{\"direct\":\"./scripts/check.sh\"}}",
    );
    write(
        &project.join("scripts/check.sh"),
        "#!/bin/sh\nprintf fixture\n",
    );
    write(
        &project.join("scripts/rules.txt"),
        "Synthetic companion data.",
    );
    let roots = EnvironmentRoots::isolated(&temp.path().join("roots"));
    let store = Store::open(&temp.path().join("store")).unwrap();
    store
        .write_session(&common::migration_session(
            "native",
            &project,
            SourceTool::Codex,
        ))
        .unwrap();
    let identity = resolve_project(project.to_str().unwrap());
    let selected = select_project_sessions(&store, &identity.id, &["native".into()]).unwrap();
    let report = discover_environment(&identity, &selected, &roots, &Default::default()).unwrap();
    let script = report
        .assets
        .iter()
        .find(|asset| asset.kind == AssetKind::Script)
        .unwrap();
    assert!(script
        .files
        .iter()
        .any(|file| file.relative_path == Path::new("check.sh")));
    assert!(script
        .files
        .iter()
        .any(|file| file.relative_path == Path::new("rules.txt")));
}

#[cfg(unix)]
#[test]
fn unreadable_global_skill_roots_are_reported_and_do_not_fall_back_to_real_roots() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    write(
        &project.join("CLAUDE.md"),
        "Synthetic project instructions.",
    );
    let roots = EnvironmentRoots::isolated(&temp.path().join("roots"));
    let blocked = roots.claude.join("skills");
    fs::create_dir_all(&blocked).unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000)).unwrap();
    let store = Store::open(&temp.path().join("store")).unwrap();
    store
        .write_session(&common::migration_session(
            "native",
            &project,
            SourceTool::Claude,
        ))
        .unwrap();
    let identity = resolve_project(project.to_str().unwrap());
    let selected = select_project_sessions(&store, &identity.id, &["native".into()]).unwrap();
    let result = discover_environment(&identity, &selected, &roots, &Default::default());
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o700)).unwrap();
    let report = result.unwrap();
    assert!(report
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.path.as_deref() == Some(&blocked)));
    assert!(report.assets.iter().all(|asset| asset
        .files
        .iter()
        .all(|file| file.path.starts_with(temp.path().canonicalize().unwrap()))));
}
