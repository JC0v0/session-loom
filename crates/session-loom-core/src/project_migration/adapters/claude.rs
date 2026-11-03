use super::*;
use crate::{
    canonical::SourceTool,
    project_migration::{discovery::Scanner, AssetKind, AssetScope},
};

pub(crate) fn discover(scanner: &mut Scanner<'_>) {
    let tool = SourceTool::Claude;
    let home = scanner.roots.claude.clone();
    let project = scanner.project.root.clone();
    scanner.instruction_chain(
        tool,
        &[
            "CLAUDE.md".into(),
            ".claude/CLAUDE.md".into(),
            "AGENTS.md".into(),
        ],
        &["CLAUDE.local.md"],
    );
    scanner.file(
        tool,
        AssetKind::Instruction,
        AssetScope::User,
        &home.join("CLAUDE.md"),
        "instructions",
    );
    for directory in scanner.project_directories(tool) {
        scanner.markdown_directory(
            tool,
            AssetKind::Instruction,
            AssetScope::Project,
            &directory.join(".claude/rules"),
        );
        scanner.skills(
            tool,
            AssetScope::Project,
            &directory.join(".claude/skills"),
            false,
        );
        scanner.markdown_directory(
            tool,
            AssetKind::Command,
            AssetScope::Project,
            &directory.join(".claude/commands"),
        );
    }
    scanner.markdown_directory(
        tool,
        AssetKind::Instruction,
        AssetScope::User,
        &home.join("rules"),
    );
    scanner.skills(tool, AssetScope::User, &home.join("skills"), false);
    scanner.markdown_directory(
        tool,
        AssetKind::Command,
        AssetScope::User,
        &home.join("commands"),
    );
    let mcp = project.join(".mcp.json");
    if let Some((value, document)) = scanner.document(&mcp, "json") {
        scanner.mcp_map(
            tool,
            AssetScope::Project,
            &document,
            value.get("mcpServers"),
            "claude-project-mcp",
        );
    }
    let user_config = scanner.roots.claude_user_config.clone();
    let mut project_trusted = false;
    if let Some((value, document)) = scanner.document(&user_config, "json") {
        scanner.mcp_map(
            tool,
            AssetScope::User,
            &document,
            value.get("mcpServers"),
            "claude-user-mcp",
        );
        for (directory, settings) in value
            .get("projects")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if crate::project_migration::resolve_project(directory).id == scanner.project.id {
                project_trusted |= settings
                    .get("hasTrustDialogAccepted")
                    .and_then(Value::as_bool)
                    == Some(true);
                scanner.mcp_map(
                    tool,
                    AssetScope::Project,
                    &document,
                    settings.get("mcpServers"),
                    "claude-local-mcp",
                );
            }
        }
    }
    let mut custom = None;
    for (path, scope) in [
        (home.join("settings.json"), AssetScope::User),
        (project.join(".claude/settings.json"), AssetScope::Project),
        (
            project.join(".claude/settings.local.json"),
            AssetScope::Project,
        ),
    ] {
        if let Some((value, _)) = scanner.document(&path, "json") {
            if let Some(value) = value.get("autoMemoryDirectory").and_then(Value::as_str) {
                custom = Some((value.to_string(), scope, path));
            }
        }
    }
    if let Some((value, scope, source)) = custom {
        if !Path::new(&value).is_absolute() && !value.starts_with("~/") {
            scanner.problem(
                "memoryDirectoryInvalid",
                "custom memory directory must be absolute or start with ~/",
                &source,
            );
            return;
        }
        if let Some(directory) = scanner.expand_path(&value, &project) {
            let external = !directory
                .canonicalize()
                .is_ok_and(|directory| directory.starts_with(&project));
            let effective_scope = if scope == AssetScope::Project && external && !project_trusted {
                scanner.problem("externalMemoryNeedsSelection", "select external memory explicitly when source project trust is not established", &source);
                AssetScope::User
            } else {
                scope
            };
            scanner.markdown_directory(tool, AssetKind::Memory, effective_scope, &directory);
        } else {
            scanner.problem(
                "memoryDirectoryUnresolved",
                "custom memory directory cannot be resolved without evaluating configuration",
                &source,
            );
        }
    } else {
        let mut roots = vec![project.clone()];
        if let Some(common) = &scanner.project.git_common_dir {
            if common.file_name().is_some_and(|name| name == ".git") {
                if let Some(root) = common.parent() {
                    roots.push(root.to_path_buf());
                }
            }
        }
        for session in scanner
            .sessions
            .iter()
            .filter(|session| session.metadata.source_tool == tool)
        {
            for ancestor in Path::new(&session.metadata.cwd).ancestors() {
                if ancestor.canonicalize().is_ok_and(|path| path == project) {
                    roots.push(ancestor.to_path_buf());
                    break;
                }
            }
        }
        roots.sort();
        roots.dedup();
        for root in roots {
            let text = root.to_string_lossy();
            let old = crate::adapters::encode_claude_project(&text);
            let strict: String = text
                .chars()
                .map(|value| {
                    if value.is_ascii_alphanumeric() {
                        value
                    } else {
                        '-'
                    }
                })
                .collect();
            for encoded in [old, strict] {
                scanner.markdown_directory(
                    tool,
                    AssetKind::Memory,
                    AssetScope::Project,
                    &home.join("projects").join(encoded).join("memory"),
                );
            }
        }
    }
}
