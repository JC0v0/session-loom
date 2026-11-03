use rusqlite::Connection;
use session_loom_core::{
    canonical::SourceTool,
    project_migration::{
        list_projects, resolve_project, select_project_sessions, validate_resource_path,
        EnvironmentRoots, MigrationRun,
    },
    store::Store,
};
use std::{fs, path::Path};

#[path = "common/mod.rs"]
mod common;

#[test]
fn project_listing_keeps_native_instances_and_never_parses_chat_payloads() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    fs::create_dir_all(root.join(".git")).unwrap();
    fs::create_dir_all(root.join("packages/server")).unwrap();
    let store = Store::open(&temp.path().join("store")).unwrap();
    for (index, tool) in [
        SourceTool::Codex,
        SourceTool::Claude,
        SourceTool::Pi,
        SourceTool::Dsh,
        SourceTool::OpenCode,
    ]
    .into_iter()
    .enumerate()
    {
        let cwd = if index == 1 {
            root.join("packages/server")
        } else {
            root.clone()
        };
        let session = common::migration_session(&format!("native-{index}"), &cwd, tool);
        store
            .write_session_with_conversation(&session, None, "logical-conversation")
            .unwrap();
    }
    Connection::open(store.db_path())
        .unwrap()
        .execute("UPDATE sessions SET payload = 'not-json'", [])
        .unwrap();

    let projects = list_projects(&store).unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].sessions.len(), 5);
    assert_eq!(projects[0].project.root, root.canonicalize().unwrap());
    assert!(projects[0]
        .sessions
        .iter()
        .all(|session| session.conversation_id == "logical-conversation"));
    assert!(projects[0]
        .sessions
        .iter()
        .any(|session| session.source_tool == SourceTool::Claude
            && session.cwd.ends_with("packages/server")));
}

#[test]
fn selection_contains_only_explicit_instances_and_rejects_other_projects() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("project");
    let other = temp.path().join("other");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&other).unwrap();
    let store = Store::open(&temp.path().join("store")).unwrap();
    for index in 0..5 {
        store
            .write_session(&common::migration_session(
                &format!("s{index}"),
                &root,
                SourceTool::Codex,
            ))
            .unwrap();
    }
    store
        .write_session(&common::migration_session(
            "other",
            &other,
            SourceTool::Claude,
        ))
        .unwrap();
    let project = resolve_project(root.to_str().unwrap());
    let selected =
        select_project_sessions(&store, &project.id, &["s3".into(), "s1".into()]).unwrap();
    assert_eq!(selected.len(), 2);
    assert_eq!(selected[0].session.session_id, "s3");
    assert_eq!(selected[1].session.session_id, "s1");
    assert_eq!(selected[0].content_hash.len(), 64);
    assert!(select_project_sessions(&store, &project.id, &[]).is_err());
    assert!(select_project_sessions(&store, &project.id, &["s1".into(), "s1".into()]).is_err());
    assert!(
        select_project_sessions(&store, &project.id, &["other".into()])
            .unwrap_err()
            .contains("not in the selected project")
    );
    assert_eq!(store.project_sessions().unwrap().len(), 6);
}

#[test]
fn worktrees_share_git_metadata_but_remain_distinct_migration_projects() {
    let temp = tempfile::tempdir().unwrap();
    let primary = temp.path().join("primary");
    let linked = temp.path().join("linked");
    let git_dir = primary.join(".git/worktrees/linked");
    fs::create_dir_all(&git_dir).unwrap();
    fs::create_dir_all(linked.join("src")).unwrap();
    fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", git_dir.display()),
    )
    .unwrap();
    fs::write(git_dir.join("commondir"), "../..\n").unwrap();
    let first = resolve_project(primary.to_str().unwrap());
    let second = resolve_project(linked.join("src").to_str().unwrap());
    assert!(first.accessible && second.accessible);
    assert_ne!(first.id, second.id);
    assert_eq!(first.git_common_dir, second.git_common_dir);
    assert_eq!(second.root, linked.canonicalize().unwrap());
}

#[test]
fn missing_directories_do_not_silently_select_an_ancestor_repository() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".git")).unwrap();
    let missing = temp.path().join("deleted-project");
    let project = resolve_project(missing.to_str().unwrap());
    assert!(!project.accessible);
    assert_eq!(project.root, missing);
    assert_eq!(project.diagnostics[0].code, "projectUnavailable");
    let first = resolve_project(temp.path().to_str().unwrap());
    let second = resolve_project(&format!("{}/", temp.path().display()));
    assert_eq!(first.id, second.id);
}

#[cfg(not(windows))]
#[test]
fn foreign_windows_paths_remain_logical_instead_of_scanning_the_current_directory() {
    let first = resolve_project(r"C:\Users\Example\project\");
    let second = resolve_project("c:/Users/Example/project");
    assert_eq!(first.id, second.id);
    assert!(!first.accessible);
    assert_eq!(first.root, Path::new("c:/Users/Example/project"));
    assert_eq!(first.diagnostics[0].code, "projectNotLocal");
}

#[test]
fn resources_cannot_escape_on_either_path_spelling() {
    for invalid in [
        "",
        ".",
        "./",
        "../other",
        "a/../../other",
        r"a\..\other",
        "/etc/file",
        r"C:\file",
        r"\\server\share",
    ] {
        assert!(
            validate_resource_path(Path::new(invalid)).is_err(),
            "{invalid}"
        );
    }
    assert!(validate_resource_path(Path::new("scripts/check.sh")).is_ok());
    assert!(validate_resource_path(Path::new("references/项目说明.md")).is_ok());
}

#[test]
fn explicit_roots_and_serialized_models_do_not_depend_on_the_real_home() {
    let temp = tempfile::tempdir().unwrap();
    let roots = EnvironmentRoots::isolated(temp.path());
    for tool in [
        SourceTool::Codex,
        SourceTool::Claude,
        SourceTool::OpenCode,
        SourceTool::Dsh,
        SourceTool::Pi,
    ] {
        assert!(roots.tool_home(tool).starts_with(temp.path()));
    }
    assert!(roots.agents.starts_with(temp.path()));
    assert!(roots.dsh_agents.starts_with(temp.path()));
    let project = resolve_project(temp.path().to_str().unwrap());
    assert!(MigrationRun::new(project.clone(), vec![]).is_err());
    assert!(MigrationRun::new(project.clone(), vec!["s1".into(), "s1".into()]).is_err());
    let run = MigrationRun::new(project, vec!["s1".into()]).unwrap();
    let value = serde_json::to_value(&run).unwrap();
    assert!(value.get("schemaVersion").is_some());
    assert_eq!(value["state"], "draft");
    assert!(value.get("session_ids").is_none());
}
