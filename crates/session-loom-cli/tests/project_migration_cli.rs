use rusqlite::Connection;
use session_loom_core::{
    canonical::{CanonicalSession, Message, Role, SourceTool, CANONICAL_SCHEMA_VERSION},
    store::Store,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

struct Env {
    _temp: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    target_project: PathBuf,
    store: PathBuf,
}

impl Env {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let project = root.join("project");
        let target_project = root.join("target-project");
        let store = root.join("store");
        fs::create_dir_all(project.join(".agents/skills/shared/references")).unwrap();
        fs::create_dir_all(project.join("scripts")).unwrap();
        fs::create_dir_all(project.join(".codex")).unwrap();
        fs::write(
            project.join("AGENTS.md"),
            "Synthetic CLI project instructions.\n",
        )
        .unwrap();
        fs::write(project.join("MEMORY.md"), "Synthetic CLI memory.").unwrap();
        fs::write(
            project.join("package.json"),
            r#"{"scripts":{"check":"./scripts/check.sh"}}"#,
        )
        .unwrap();
        fs::write(
            project.join("scripts/check.sh"),
            "#!/bin/sh\nprintf 'fixture'\n",
        )
        .unwrap();
        fs::write(
            project.join(".agents/skills/shared/SKILL.md"),
            "---\nname: shared\ndescription: CLI skill\n---\nSource body.\n",
        )
        .unwrap();
        fs::write(
            project.join(".agents/skills/shared/references/details.md"),
            "Synthetic details.\n",
        )
        .unwrap();
        fs::write(
            project.join(".codex/config.toml"),
            "[mcp_servers.database]\ncommand = \"fixture-mcp\"\n",
        )
        .unwrap();
        let codex_home = root.join("roots/codex");
        fs::create_dir_all(codex_home.join("memories")).unwrap();
        fs::write(
            codex_home.join("memories/global.md"),
            "Synthetic global memory selected by the user.\n",
        )
        .unwrap();
        let sessions_store = Store::open(&store).unwrap();
        for id in ["s1", "s2", "s3"] {
            sessions_store
                .write_session(&session(id, &project))
                .unwrap();
        }
        Self {
            _temp: temp,
            root,
            project,
            target_project,
            store,
        }
    }

    fn command(&self, binary: &str) -> Command {
        let mut command = Command::new(binary);
        command
            .env("HOME", self.root.join("home"))
            .env("SESSION_LOOM_STORE", &self.store)
            .env("CODEX_HOME", self.root.join("roots/codex"))
            .env(
                "CODEX_SESSIONS_ROOT",
                self.root.join("roots/codex-sessions"),
            )
            .env("CLAUDE_ROOT", self.root.join("roots/claude"))
            .env("CLAUDE_CONFIG_DIR", self.root.join("roots/claude"))
            .env("DSH_HOME", self.root.join("roots/dsh"))
            .env("DSH_AGENTS_HOME", self.root.join("home/.agents"))
            .env("DSH_SESSIONS_ROOT", self.root.join("roots/dsh-sessions"))
            .env("PI_CODING_AGENT_DIR", self.root.join("roots/pi"))
            .env(
                "PI_CODING_AGENT_SESSION_DIR",
                self.root.join("roots/pi-sessions"),
            )
            .env("XDG_CONFIG_HOME", self.root.join("roots/xdg"));
        command
    }
}

fn session(id: &str, cwd: &Path) -> CanonicalSession {
    CanonicalSession {
        schema_version: CANONICAL_SCHEMA_VERSION,
        source_tool: SourceTool::Codex,
        session_id: id.to_string(),
        cwd: cwd.to_string_lossy().to_string(),
        created_at: "2026-11-02T00:00:00Z".to_string(),
        updated_at: "2026-11-02T00:00:00Z".to_string(),
        model_provider: None,
        model: None,
        title: Some(id.to_string()),
        messages: vec![Message {
            role: Role::User,
            text: format!("synthetic {id}"),
            tool_calls: vec![],
        }],
    }
}

fn json(output: &Output) -> serde_json::Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not JSON ({error}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn claude_sessions(root: &Path) -> usize {
    let mut count = 0;
    let Ok(projects) = fs::read_dir(root.join("projects")) else {
        return 0;
    };
    for directory in projects.flatten() {
        if let Ok(entries) = fs::read_dir(directory.path()) {
            count += entries
                .flatten()
                .filter(|entry| {
                    entry.path().extension().and_then(|value| value.to_str()) == Some("jsonl")
                })
                .count();
        }
    }
    count
}

#[test]
fn cli_prepares_captures_previews_and_applies_the_same_core_result() {
    let env = Env::new();
    let binary = env!("CARGO_BIN_EXE_ssl");

    let prepare = env
        .command(binary)
        .args([
            "migrate",
            "prepare",
            "--project",
            env.project.to_str().unwrap(),
            "--session",
            "s1",
            "--session",
            "s2",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        prepare.status.success(),
        "{}",
        String::from_utf8_lossy(&prepare.stderr)
    );
    let prepared = json(&prepare);
    let run_id = prepared["runId"].as_str().unwrap().to_string();
    assert_eq!(prepared["sessions"].as_array().unwrap().len(), 2);
    let global = prepared["assets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|asset| asset["name"] == "global.md" && asset["scope"] == "user")
        .expect("global memory should be discoverable")
        .clone();
    let global_id = global["id"].as_str().unwrap().to_string();
    assert_eq!(global["selected"], false, "{global}");

    let capture = env
        .command(binary)
        .args([
            "migrate", "capture", "--run", &run_id, "--asset", &global_id, "--json",
        ])
        .output()
        .unwrap();
    assert!(
        capture.status.success(),
        "{}",
        String::from_utf8_lossy(&capture.stderr)
    );
    let captured = json(&capture);
    assert!(captured["captured"]
        .as_array()
        .unwrap()
        .iter()
        .any(|asset| asset["name"] == "global.md"));

    let preview = env
        .command(binary)
        .args([
            "migrate",
            "preview",
            "--run",
            &run_id,
            "--to",
            "claude",
            "--project-root",
            env.target_project.to_str().unwrap(),
            "--env",
            "FIXTURE_TOKEN",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let preview = json(&preview);
    let preview_id = preview["id"].as_str().unwrap().to_string();
    assert_eq!(preview["readyToApply"], true);
    let kinds = preview["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["kind"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    for kind in ["instruction", "memory", "skill", "mcp", "script", "command"] {
        assert!(kinds.iter().any(|value| value == kind), "{kind}: {kinds:?}");
    }

    let apply = env
        .command(binary)
        .args([
            "migrate",
            "apply",
            "--run",
            &run_id,
            "--preview",
            &preview_id,
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!apply.status.success(), "partial results exit non-zero");
    let applied = json(&apply);
    assert_eq!(applied["state"], "partial");
    let target_session = applied["sessions"][0]["targetSessionId"]
        .as_str()
        .unwrap()
        .to_string();
    let target_path = applied["sessions"][0]["targetPath"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(Path::new(&target_path).is_file());
    assert_eq!(claude_sessions(&env.root.join("roots/claude")), 2);
    let store = Store::open(&env.store).unwrap();
    let linked = store.read_session(&target_session).unwrap();
    assert_eq!(linked.source_tool, SourceTool::Claude);
    assert_eq!(linked.cwd, env.target_project.to_string_lossy());

    let again = env
        .command(binary)
        .args([
            "migrate",
            "apply",
            "--run",
            &run_id,
            "--preview",
            &preview_id,
            "--json",
        ])
        .output()
        .unwrap();
    let again = json(&again);
    assert_eq!(again["sessions"][0]["targetSessionId"], target_session);
    assert_eq!(claude_sessions(&env.root.join("roots/claude")), 2);

    let status = env
        .command(binary)
        .args(["migrate", "status", "--run", &run_id, "--json"])
        .output()
        .unwrap();
    assert!(status.status.success());
    let status = json(&status);
    assert_eq!(status["state"], "partial");
    assert_eq!(status["preview"]["id"], preview_id);
}

#[test]
fn cli_conflict_decisions_are_required_and_applied_before_writing() {
    let env = Env::new();
    let binary = env!("CARGO_BIN_EXE_ssl");
    fs::create_dir_all(env.target_project.join(".claude/skills/shared")).unwrap();
    fs::write(
        env.target_project.join(".claude/skills/shared/SKILL.md"),
        "---\nname: shared\ndescription: Existing target\n---\nTarget body.\n",
    )
    .unwrap();

    let prepare = env
        .command(binary)
        .args([
            "migrate",
            "prepare",
            "--project",
            env.project.to_str().unwrap(),
            "--session",
            "s1",
            "--json",
        ])
        .output()
        .unwrap();
    let prepared = json(&prepare);
    let run_id = prepared["runId"].as_str().unwrap().to_string();
    let capture = env
        .command(binary)
        .args(["migrate", "capture", "--run", &run_id, "--json"])
        .output()
        .unwrap();
    assert!(capture.status.success());

    let first = env
        .command(binary)
        .args([
            "migrate",
            "preview",
            "--run",
            &run_id,
            "--to",
            "claude",
            "--project-root",
            env.target_project.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    let first = json(&first);
    assert_eq!(first["readyToApply"], false);
    let unresolved_apply = env
        .command(binary)
        .args([
            "migrate",
            "apply",
            "--run",
            &run_id,
            "--preview",
            first["id"].as_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!unresolved_apply.status.success());
    let error: serde_json::Value =
        serde_json::from_slice(&unresolved_apply.stderr).expect("stderr should be JSON");
    assert!(
        error["error"].as_str().unwrap().contains("unresolved"),
        "{error}"
    );
    let conflict = first["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|conflict| {
            conflict["targetPath"]
                .as_str()
                .unwrap()
                .ends_with("SKILL.md")
        })
        .unwrap();
    let decision = serde_json::json!([{
        "decisionId": conflict["items"][0]["decisionId"],
        "itemId": conflict["items"][0]["itemId"],
        "choice": "replace"
    }]);
    let decisions_path = env.root.join("decisions.json");
    fs::write(&decisions_path, serde_json::to_vec(&decision).unwrap()).unwrap();

    let second = env
        .command(binary)
        .args([
            "migrate",
            "preview",
            "--run",
            &run_id,
            "--to",
            "claude",
            "--project-root",
            env.target_project.to_str().unwrap(),
            "--decisions",
            decisions_path.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    let second = json(&second);
    assert_eq!(second["readyToApply"], true);
    assert_eq!(
        fs::read_to_string(env.target_project.join(".claude/skills/shared/SKILL.md")).unwrap(),
        "---\nname: shared\ndescription: Existing target\n---\nTarget body.\n",
        "preview must not modify the target"
    );

    let preview_id = second["id"].as_str().unwrap();
    let apply = env
        .command(binary)
        .args([
            "migrate",
            "apply",
            "--run",
            &run_id,
            "--preview",
            preview_id,
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!apply.status.success());
    assert_eq!(
        fs::read_to_string(env.target_project.join(".claude/skills/shared/SKILL.md")).unwrap(),
        "---\nname: shared\ndescription: CLI skill\n---\nSource body.\n"
    );
}

#[test]
fn cli_errors_are_structured_and_do_not_mix_progress_with_json() {
    let env = Env::new();
    let binary = env!("CARGO_BIN_EXE_ssl");

    let missing_run = env
        .command(binary)
        .args([
            "migrate",
            "status",
            "--run",
            "00000000-0000-4000-8000-000000000000",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!missing_run.status.success());
    assert!(missing_run.stdout.is_empty());
    let error: serde_json::Value =
        serde_json::from_slice(&missing_run.stderr).expect("stderr should be JSON");
    assert!(error["error"].as_str().unwrap().contains("migration"));

    let prepare = env
        .command(binary)
        .args([
            "migrate",
            "prepare",
            "--project",
            env.project.to_str().unwrap(),
            "--session",
            "s1",
            "--json",
        ])
        .output()
        .unwrap();
    let run_id = json(&prepare)["runId"].as_str().unwrap().to_string();

    let unknown_asset = env
        .command(binary)
        .args([
            "migrate",
            "capture",
            "--run",
            &run_id,
            "--asset",
            "forged-item",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!unknown_asset.status.success());
    let error: serde_json::Value = serde_json::from_slice(&unknown_asset.stderr).unwrap();
    assert!(error["error"].as_str().unwrap().contains("not discovered"));

    let relative_target = env
        .command(binary)
        .args([
            "migrate",
            "preview",
            "--run",
            &run_id,
            "--to",
            "claude",
            "--project-root",
            "relative/path",
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!relative_target.status.success());
    let error: serde_json::Value = serde_json::from_slice(&relative_target.stderr).unwrap();
    assert!(error["error"].as_str().unwrap().contains("absolute"));

    let invalid_decisions = env.root.join("invalid-decisions.json");
    fs::write(&invalid_decisions, "not json").unwrap();
    let invalid = env
        .command(binary)
        .args([
            "migrate",
            "preview",
            "--run",
            &run_id,
            "--to",
            "claude",
            "--project-root",
            env.target_project.to_str().unwrap(),
            "--decisions",
            invalid_decisions.to_str().unwrap(),
            "--json",
        ])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    let error: serde_json::Value = serde_json::from_slice(&invalid.stderr).unwrap();
    assert!(error["error"]
        .as_str()
        .unwrap()
        .contains("invalid decisions JSON"));

    // clap keeps usage errors on exit code 2.
    let usage = env
        .command(binary)
        .args(["migrate", "preview", "--run", "x"])
        .output()
        .unwrap();
    assert_eq!(usage.status.code(), Some(2));
    let _ = Connection::open(env.store.join("sessions.db"));
}
