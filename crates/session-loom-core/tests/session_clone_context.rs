use session_loom_core::{
    canonical::SourceTool,
    project_migration::{
        capture_clone_selection, prepare_session_clone, render_environment, AssetKind,
        EnvironmentRoots, OpenCodeConfigVersion, RunStore, TargetEnvironment, VersionSupport,
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

#[test]
fn clone_context_is_bound_to_the_clicked_native_session_and_tool() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    write(&project.join("AGENTS.md"), "Project instructions.\n");
    write(&project.join("MEMORY.md"), "Project memory.\n");
    let store = Store::open(&temp.path().join("store")).unwrap();
    for id in ["clicked", "other-fork"] {
        store
            .write_session(&common::migration_session(id, &project, SourceTool::Codex))
            .unwrap();
    }
    let runs = RunStore::new(store.root()).unwrap();
    let roots = EnvironmentRoots::isolated(&temp.path().join("agents"));
    let prepared =
        prepare_session_clone(&store, &runs, &roots, "clicked", SourceTool::Claude).unwrap();
    assert_eq!(prepared.run.session_ids, vec!["clicked"]);
    assert_eq!(prepared.sessions.len(), 1);
    let target = prepared.run.target.as_ref().unwrap();
    assert_eq!(target.tool, SourceTool::Claude);
    assert_eq!(target.project_root, project.canonicalize().unwrap());
    assert!(
        prepared.run.assets.is_empty(),
        "discovery does not implicitly select project content"
    );
    let chosen = prepared
        .report
        .assets
        .iter()
        .find(|asset| asset.kind == AssetKind::Memory)
        .unwrap();
    let captured = capture_clone_selection(
        &store,
        &runs,
        &prepared.run.id,
        std::slice::from_ref(&chosen.id),
    )
    .unwrap();
    assert_eq!(captured.assets.len(), 1);
    assert_eq!(captured.assets[0].asset.kind, AssetKind::Memory);
    assert_eq!(captured.sessions.len(), 1);
    assert_eq!(captured.sessions[0].metadata.session_id, "clicked");
    let environment = TargetEnvironment {
        target: target.clone(),
        roots,
        session_roots: None,
        opencode_version: OpenCodeConfigVersion::Unknown,
        pi_mcp: VersionSupport::Unknown,
        dsh_profile: None,
        project_trusted: false,
        executable_search_path: vec![],
        environment_variables: vec![],
    };
    let plan = render_environment(&runs, &captured, &environment).unwrap();
    assert!(plan
        .files
        .iter()
        .any(|file| file.target_path == project.canonicalize().unwrap().join("CLAUDE.md")));
    assert_eq!(
        fs::read_to_string(project.join("AGENTS.md")).unwrap(),
        "Project instructions.\n"
    );
}

#[test]
fn choosing_nothing_keeps_a_session_only_snapshot_and_invalid_asset_ids_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    write(&project.join("AGENTS.md"), "Project instructions.\n");
    let store = Store::open(&temp.path().join("store")).unwrap();
    store
        .write_session(&common::migration_session(
            "clicked",
            &project,
            SourceTool::Codex,
        ))
        .unwrap();
    let runs = RunStore::new(store.root()).unwrap();
    let roots = EnvironmentRoots::isolated(&temp.path().join("agents"));
    let prepared =
        prepare_session_clone(&store, &runs, &roots, "clicked", SourceTool::Claude).unwrap();
    assert!(capture_clone_selection(&store, &runs, &prepared.run.id, &["forged".into()]).is_err());
    let captured = capture_clone_selection(&store, &runs, &prepared.run.id, &[]).unwrap();
    assert!(captured.assets.is_empty());
    assert_eq!(captured.sessions.len(), 1);
    assert!(!project.join("CLAUDE.md").exists());
}

#[test]
fn cloning_with_memory_keeps_the_original_cwd_and_omits_unchecked_project_tools() {
    use session_loom_core::project_migration::{apply_preview, create_preview};
    use session_loom_core::restore::RestoreRoots;
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    write(&project.join("AGENTS.md"), "Project instructions.\n");
    write(&project.join("MEMORY.md"), "Memory selected by the user.\n");
    write(
        &project.join(".agents/skills/not-selected/SKILL.md"),
        "---\nname: not-selected\ndescription: Must stay at source\n---\nDo not transfer.\n",
    );
    let store = Store::open(&temp.path().join("store")).unwrap();
    for id in ["clicked", "not-selected"] {
        store
            .write_session(&common::migration_session(id, &project, SourceTool::Codex))
            .unwrap();
    }
    let runs = RunStore::new(store.root()).unwrap();
    let roots = EnvironmentRoots::isolated(&temp.path().join("agents"));
    let native = RestoreRoots {
        codex: roots.codex.join("sessions"),
        codex_home: roots.codex.clone(),
        claude: roots.claude.clone(),
        opencode: roots.opencode.join("opencode.db"),
        dsh: roots.dsh.join("sessions"),
        pi: roots.pi.join("sessions"),
    };
    let prepared =
        prepare_session_clone(&store, &runs, &roots, "clicked", SourceTool::Claude).unwrap();
    let memory = prepared
        .report
        .assets
        .iter()
        .find(|asset| asset.kind == AssetKind::Memory)
        .unwrap();
    let captured = capture_clone_selection(
        &store,
        &runs,
        &prepared.run.id,
        std::slice::from_ref(&memory.id),
    )
    .unwrap();
    let environment = TargetEnvironment {
        target: captured.target.clone().unwrap(),
        roots,
        session_roots: Some((&native).into()),
        opencode_version: OpenCodeConfigVersion::Unknown,
        pi_mcp: VersionSupport::Unknown,
        dsh_profile: None,
        project_trusted: false,
        executable_search_path: vec![],
        environment_variables: vec![],
    };
    let (run, preview) = create_preview(&runs, &captured, &environment, &[]).unwrap();
    let report = apply_preview(&store, &runs, &run.id, &preview.id, &native).unwrap();
    assert_eq!(report.sessions.len(), 1);
    let linked = store
        .read_session(&report.sessions[0].target_session_id)
        .unwrap();
    assert_eq!(linked.cwd, project.to_string_lossy());
    assert_eq!(
        store.conversation_id(&linked.session_id).unwrap(),
        store.conversation_id("clicked").unwrap()
    );
    assert!(!project.join(".claude/skills/not-selected").exists());
    assert!(fs::read_to_string(project.join("CLAUDE.md"))
        .unwrap()
        .contains("Memory selected by the user."));
    assert_eq!(
        fs::read_to_string(project.join("AGENTS.md")).unwrap(),
        "Project instructions.\n"
    );
    // The cloned session keeps its own title in the target's native format,
    // so the target tool lists it under the original name.
    let claude_file = std::fs::read_dir(native.claude.join("projects"))
        .unwrap()
        .flatten()
        .flat_map(|entry| std::fs::read_dir(entry.path()).unwrap().flatten())
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|value| value.to_str()) == Some("jsonl"))
        .expect("Claude session file");
    let native_session =
        session_loom_core::adapters::claude::parse_session_file(&claude_file).unwrap();
    assert_eq!(native_session.title.as_deref(), Some("clicked"));
    assert_eq!(linked.title.as_deref(), Some("clicked"));
}
