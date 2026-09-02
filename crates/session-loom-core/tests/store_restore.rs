use rusqlite::{params, Connection};
use session_loom_core::{
    adapters::{dsh, opencode, pi},
    canonical::{CanonicalSession, Message, Role, SourceTool, CANONICAL_SCHEMA_VERSION},
    restore::{restore_session, sync_session, RestoreRoots},
    store::{ListFilter, Store},
};
use std::{fs, path::Path};

#[path = "common/mod.rs"]
mod common;

fn sample() -> CanonicalSession {
    CanonicalSession {
        schema_version: CANONICAL_SCHEMA_VERSION,
        source_tool: SourceTool::Codex,
        session_id: "s1".to_string(),
        cwd: r"C:\proj".to_string(),
        created_at: "2026-01-01T00:00:00.000Z".to_string(),
        updated_at: "2026-01-01T00:00:00.000Z".to_string(),
        model_provider: Some("custom".to_string()),
        model: Some("deepseek-v4-pro".to_string()),
        title: None,
        messages: vec![Message {
            role: Role::User,
            text: "帮我修一下这个 bug".to_string(),
            tool_calls: vec![],
        }],
    }
}

#[test]
fn store_writes_reads_lists_searches_and_exports_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let first = sample();
    let mut second = first.clone();
    second.session_id = "s2".to_string();
    second.source_tool = SourceTool::Claude;
    second.updated_at = "2026-01-02T00:00:00.000Z".to_string();

    store.write_session(&first).unwrap();
    store.write_session(&first).unwrap();
    store.write_session(&second).unwrap();

    assert_eq!(store.read_session("s1").unwrap(), first);
    assert_eq!(store.latest_session().unwrap().session_id, "s2");
    assert_eq!(
        store
            .list_sessions(None)
            .unwrap()
            .into_iter()
            .map(|session| session.session_id)
            .collect::<Vec<_>>(),
        vec!["s2", "s1"]
    );
    assert!(store
        .search_sessions("修一下")
        .unwrap()
        .iter()
        .any(|session| session.session_id == "s1"));
    assert!(store.search_sessions("不存在").unwrap().is_empty());
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&store.export_session("s1").unwrap()).unwrap()
            ["sessionId"],
        "s1"
    );
}

#[test]
fn storage_payloads_are_compact_while_exports_remain_pretty() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    store.write_session(&sample()).unwrap();

    let connection = Connection::open(store.db_path()).unwrap();
    let payload: String = connection
        .query_row(
            "SELECT payload FROM sessions WHERE session_id = 's1'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!payload.contains('\n'));
    drop(connection);

    assert!(store.export_session("s1").unwrap().contains('\n'));
    assert!(store
        .search_hits(r"C:\proj")
        .unwrap()
        .iter()
        .any(|hit| hit.session_id == "s1"));
}

#[test]
fn identical_cross_tool_sessions_share_one_card() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();

    for (session_id, source_tool) in [
        ("codex-1", SourceTool::Codex),
        ("claude-1", SourceTool::Claude),
        ("pi-1", SourceTool::Pi),
    ] {
        let mut session = sample();
        session.session_id = session_id.to_string();
        session.source_tool = source_tool;
        store.write_session(&session).unwrap();
    }

    let cards = store.list_cards(ListFilter::default()).unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].tools, vec!["claude", "codex", "pi"]);
    assert_eq!(cards[0].instance_count, 3);
}

#[test]
fn card_title_prefers_the_source_tool_title() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let mut session = sample();
    session.title = Some("源工具生成的标题".to_string());
    store.write_session(&session).unwrap();

    let cards = store.list_cards(ListFilter::default()).unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].title, "源工具生成的标题");
}

#[test]
fn reopening_a_migrated_store_records_the_schema_version() {
    let temp = tempfile::tempdir().unwrap();
    Store::open(temp.path()).unwrap();

    let connection = Connection::open(temp.path().join("sessions.db")).unwrap();
    let version: i32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, session_loom_core::store::SCHEMA_USER_VERSION);
}

#[test]
fn schema_v3_backfills_content_hashes_and_compacts_legacy_payloads() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("sessions.db");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE sessions (
                session_id TEXT PRIMARY KEY,
                conversation_id TEXT,
                source_tool TEXT NOT NULL,
                cwd TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                schema_version INTEGER NOT NULL,
                payload TEXT NOT NULL,
                source_path TEXT
            );
            CREATE TABLE session_summaries (
                session_id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                message_count INTEGER NOT NULL
            );
            PRAGMA user_version = 2;",
        )
        .unwrap();

    for (session_id, source_tool) in [
        ("legacy-codex", SourceTool::Codex),
        ("legacy-claude", SourceTool::Claude),
    ] {
        let mut session = sample();
        session.session_id = session_id.to_string();
        session.source_tool = source_tool;
        connection
            .execute(
                "INSERT INTO sessions
                 (session_id, conversation_id, source_tool, cwd, created_at, updated_at,
                  schema_version, payload, source_path)
                 VALUES (?, NULL, ?, ?, ?, ?, ?, ?, NULL)",
                params![
                    session.session_id,
                    session.source_tool.as_str(),
                    session.cwd,
                    session.created_at,
                    session.updated_at,
                    session.schema_version,
                    session.to_json().unwrap(),
                ],
            )
            .unwrap();
    }
    drop(connection);

    let store = Store::open(temp.path()).unwrap();
    let connection = Connection::open(&database).unwrap();
    let hashed: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE content_hash IS NOT NULL",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(hashed, 2);
    let compact_payloads: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM sessions WHERE payload NOT LIKE '%' || char(10) || '%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(compact_payloads, 2);
    assert!(connection
        .query_row(
            "SELECT 1 FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_sessions_cwd_content_hash'",
            [],
            |_| Ok(()),
        )
        .is_ok());
    assert!(connection
        .query_row(
            "SELECT 1 FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_sessions_tool_source_path'",
            [],
            |_| Ok(()),
        )
        .is_ok());
    assert_eq!(store.list_cards(ListFilter::default()).unwrap().len(), 1);
}

#[test]
fn store_databases_use_wal_journaling_for_concurrent_access() {
    let temp = tempfile::tempdir().unwrap();
    Store::open(temp.path()).unwrap();

    let connection = Connection::open(temp.path().join("sessions.db")).unwrap();
    let mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
}

#[test]
fn reopened_store_still_heals_summaries_and_keeps_conversation_groups() {
    let temp = tempfile::tempdir().unwrap();
    {
        let store = Store::open(temp.path()).unwrap();
        store.write_session(&sample()).unwrap();
        let mut copy = sample();
        copy.session_id = "claude-copy".to_string();
        copy.source_tool = SourceTool::Claude;
        store.write_session(&copy).unwrap();
    }

    // Simulate a legacy row state and rely on the read path (not the
    // version-gated migration) to heal the missing summary on reopen.
    let connection = Connection::open(temp.path().join("sessions.db")).unwrap();
    connection
        .execute("DELETE FROM session_summaries", [])
        .unwrap();
    drop(connection);

    let store = Store::open(temp.path()).unwrap();
    let cards = store.list_cards(ListFilter::default()).unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].tools, vec!["claude", "codex"]);
    assert_eq!(cards[0].instance_count, 2);
    assert_eq!(cards[0].title, "帮我修一下这个 bug");
    assert_eq!(cards[0].message_count, 1);
}

#[test]
fn store_updates_summary_in_the_same_write_path() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let mut session = sample();
    store.write_session(&session).unwrap();
    session.messages = vec![
        Message {
            role: Role::Assistant,
            text: "先分析".to_string(),
            tool_calls: vec![],
        },
        Message {
            role: Role::User,
            text: "  请\n继续修复  ".to_string(),
            tool_calls: vec![],
        },
    ];
    store.write_session(&session).unwrap();

    let connection = Connection::open(store.db_path()).unwrap();
    let summary = connection
        .query_row(
            "SELECT title, message_count FROM session_summaries WHERE session_id = ?",
            params!["s1"],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
        )
        .unwrap();
    assert_eq!(summary, ("请 继续修复".to_string(), 2));
}

#[test]
fn store_rejects_sessions_without_an_id() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let mut session = sample();
    session.session_id.clear();

    assert_eq!(
        store.write_session(&session).unwrap_err(),
        "session id is empty"
    );
    assert!(store.list_sessions(None).unwrap().is_empty());
}

#[test]
fn store_backfills_summaries_from_an_older_database() {
    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("sessions.db");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE sessions (
                session_id TEXT PRIMARY KEY,
                source_tool TEXT NOT NULL,
                cwd TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                schema_version INTEGER NOT NULL,
                payload TEXT NOT NULL
            );",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO sessions VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![
                "s1",
                "codex",
                r"C:\proj",
                "2026-01-01T00:00:00.000Z",
                "2026-01-01T00:00:00.000Z",
                1,
                sample().to_json().unwrap()
            ],
        )
        .unwrap();
    drop(connection);

    let store = Store::open(temp.path()).unwrap();
    let cards = store.list_cards(ListFilter::default()).unwrap();

    assert_eq!(cards[0].title, "帮我修一下这个 bug");
    assert_eq!(cards[0].message_count, 1);
}

#[test]
fn legacy_migration_keeps_source_data_when_database_write_fails() {
    let temp = tempfile::tempdir().unwrap();
    let legacy = temp.path().join("store").join("codex");
    fs::create_dir_all(&legacy).unwrap();
    let mut first = sample();
    first.session_id = "s1".to_string();
    let mut second = sample();
    second.session_id = "s2".to_string();
    fs::write(legacy.join("s1.json"), first.to_json().unwrap()).unwrap();
    fs::write(legacy.join("s2.json"), second.to_json().unwrap()).unwrap();

    let database = temp.path().join("sessions.db");
    let connection = Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE sessions (
                session_id TEXT PRIMARY KEY,
                source_tool TEXT NOT NULL,
                cwd TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                schema_version INTEGER NOT NULL,
                payload TEXT NOT NULL
            );
            CREATE TRIGGER reject_second_session
            BEFORE INSERT ON sessions WHEN NEW.session_id = 's2'
            BEGIN
                SELECT RAISE(ABORT, 'simulated migration failure');
            END;",
        )
        .unwrap();
    drop(connection);

    assert!(Store::open(temp.path())
        .unwrap_err()
        .contains("simulated migration failure"));
    assert!(temp.path().join("store").exists());
    assert!(!temp.path().join("store.legacy").exists());
    let connection = Connection::open(database).unwrap();
    let count = connection
        .query_row("SELECT COUNT(*) FROM sessions", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn card_list_uses_summary_without_parsing_the_full_payload() {
    let temp = tempfile::tempdir().unwrap();
    let store = Store::open(temp.path()).unwrap();
    let connection = Connection::open(store.db_path()).unwrap();
    connection
        .execute(
            "INSERT INTO sessions
             (session_id, conversation_id, source_tool, cwd, created_at, updated_at,
              schema_version, payload, source_path)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                "broken",
                Option::<String>::None,
                "codex",
                "/tmp/project",
                "2026-01-01T00:00:00.000Z",
                "2026-01-01T00:00:00.000Z",
                1,
                "not valid json",
                Option::<String>::None
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session_summaries VALUES (?, ?, ?)",
            params!["broken", "摘要标题", 42],
        )
        .unwrap();
    drop(connection);

    let cards = store
        .list_cards(ListFilter {
            tool: Some(SourceTool::Codex),
            query: None,
        })
        .unwrap();

    assert_eq!(cards[0].title, "摘要标题");
    assert_eq!(cards[0].message_count, 42);
}

#[test]
fn restores_sessions_to_codex_claude_opencode_dsh_and_pi() {
    let store_temp = tempfile::tempdir().unwrap();
    let codex_temp = tempfile::tempdir().unwrap();
    let codex_home_temp = tempfile::tempdir().unwrap();
    let claude_temp = tempfile::tempdir().unwrap();
    let opencode_temp = tempfile::tempdir().unwrap();
    let opencode_db = opencode_temp.path().join("opencode.db");
    let dsh_temp = tempfile::tempdir().unwrap();
    let dsh_home_temp = tempfile::tempdir().unwrap();
    let _dsh_home_guard = common::isolate_dsh_home(dsh_home_temp.path());
    let pi_temp = tempfile::tempdir().unwrap();
    let store = Store::open(store_temp.path()).unwrap();
    store.write_session(&sample()).unwrap();
    let roots = RestoreRoots {
        codex: codex_temp.path().to_path_buf(),
        codex_home: codex_home_temp.path().to_path_buf(),
        claude: claude_temp.path().to_path_buf(),
        opencode: opencode_db.clone(),
        dsh: dsh_temp.path().to_path_buf(),
        pi: pi_temp.path().to_path_buf(),
    };

    assert!(restore_session(&store, SourceTool::Codex, Some("s1"), &roots).ok);
    assert_eq!(walk_jsonl(codex_temp.path()).len(), 1);
    let claude_restore = restore_session(&store, SourceTool::Claude, Some("s1"), &roots);
    assert!(claude_restore.ok);
    assert!(claude_restore.message.contains("可迁移内容"));
    assert!(claude_temp.path().join("history.jsonl").exists());

    opencode::init_database(&opencode_db).unwrap();
    assert!(restore_session(&store, SourceTool::OpenCode, Some("s1"), &roots).ok);
    let restored = opencode::parse_sessions(&opencode_db).unwrap();
    assert_eq!(restored.len(), 1);
    assert_eq!(restored[0].source_tool, SourceTool::OpenCode);
    assert_eq!(restored[0].cwd, "C:/proj");
    assert_eq!(restored[0].messages[0].text, "帮我修一下这个 bug");
    assert_ne!(restored[0].session_id, "s1");

    assert!(restore_session(&store, SourceTool::Dsh, Some("s1"), &roots).ok);
    let dsh_logs = dsh::session_log_files(dsh_temp.path());
    assert_eq!(dsh_logs.len(), 1);
    let dsh_restored = dsh::parse_session_file(&dsh_logs[0]).unwrap().unwrap();
    assert_eq!(dsh_restored.source_tool, SourceTool::Dsh);
    assert_eq!(dsh_restored.cwd, r"C:\proj");
    assert_eq!(dsh_restored.messages[0].text, "帮我修一下这个 bug");
    assert_ne!(dsh_restored.session_id, "s1");

    assert!(restore_session(&store, SourceTool::Pi, Some("s1"), &roots).ok);
    let pi_files = walk_jsonl(pi_temp.path());
    assert_eq!(pi_files.len(), 1);
    let pi_restored = pi::parse_session(&fs::read_to_string(&pi_files[0]).unwrap()).unwrap();
    assert_eq!(pi_restored.source_tool, SourceTool::Pi);
    assert_eq!(pi_restored.cwd, r"C:\proj");
    assert_eq!(pi_restored.messages[0].text, "帮我修一下这个 bug");
    assert_ne!(pi_restored.session_id, "s1");

    let cards = store.list_cards(ListFilter::default()).unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(
        cards[0].tools,
        vec!["claude", "codex", "dsh", "opencode", "pi"]
    );
    assert_eq!(cards[0].instance_count, 6);

    assert!(!restore_session(&store, SourceTool::Codex, Some("missing"), &roots).ok);
}

#[test]
fn manually_syncs_the_latest_snapshot_to_an_existing_pi_instance() {
    let store_temp = tempfile::tempdir().unwrap();
    let codex_temp = tempfile::tempdir().unwrap();
    let codex_home_temp = tempfile::tempdir().unwrap();
    let pi_temp = tempfile::tempdir().unwrap();
    let store = Store::open(store_temp.path()).unwrap();
    store.write_session(&sample()).unwrap();
    let roots = RestoreRoots {
        codex: codex_temp.path().to_path_buf(),
        codex_home: codex_home_temp.path().to_path_buf(),
        claude: store_temp.path().join("claude"),
        opencode: store_temp.path().join("opencode.db"),
        dsh: store_temp.path().join("dsh"),
        pi: pi_temp.path().to_path_buf(),
    };

    assert!(restore_session(&store, SourceTool::Pi, Some("s1"), &roots).ok);

    let mut latest = sample();
    latest.updated_at = "2026-01-01T00:00:02.000Z".to_string();
    latest.messages.push(Message {
        role: Role::Assistant,
        text: "Codex 后续回复".to_string(),
        tool_calls: vec![],
    });
    store.write_session(&latest).unwrap();

    let result = sync_session(&store, SourceTool::Pi, Some("s1"), &roots);
    assert!(result.ok, "{}", result.message);
    assert!(result.message.contains("手动同步完成"));

    let pi_sessions = walk_jsonl(pi_temp.path())
        .into_iter()
        .map(|path| pi::parse_session(&fs::read_to_string(path).unwrap()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(pi_sessions.len(), 2);
    assert!(pi_sessions.iter().any(|session| session
        .messages
        .iter()
        .any(|message| message.text == "Codex 后续回复")));

    let cards = store.list_cards(ListFilter::default()).unwrap();
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].tools, vec!["codex", "pi"]);
    assert_eq!(cards[0].instance_count, 3);
}

fn walk_jsonl(directory: &Path) -> Vec<std::path::PathBuf> {
    let mut files = vec![];
    let Ok(entries) = fs::read_dir(directory) else {
        return files;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files.extend(walk_jsonl(&path));
        } else if path.extension().and_then(|value| value.to_str()) == Some("jsonl") {
            files.push(path);
        }
    }
    files
}
