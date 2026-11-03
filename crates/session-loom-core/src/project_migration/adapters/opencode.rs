use super::*;
use crate::{
    canonical::SourceTool,
    project_migration::{discovery::Scanner, AssetKind, AssetScope},
};

pub(crate) fn discover(scanner: &mut Scanner<'_>) {
    let tool = SourceTool::OpenCode;
    let home = scanner.roots.opencode.clone();
    let project = scanner.project.root.clone();
    scanner.instruction_chain(tool, &["AGENTS.md".into(), "CLAUDE.md".into()], &[]);
    if scanner
        .file(
            tool,
            AssetKind::Instruction,
            AssetScope::User,
            &home.join("AGENTS.md"),
            "instructions",
        )
        .is_none()
    {
        scanner.file(
            tool,
            AssetKind::Instruction,
            AssetScope::User,
            &scanner.roots.claude.join("CLAUDE.md"),
            "instructions",
        );
    }
    for directory in scanner.project_directories(tool) {
        for path in [".opencode/skills", ".claude/skills", ".agents/skills"] {
            scanner.skills(tool, AssetScope::Project, &directory.join(path), false);
        }
        scanner.markdown_directory(
            tool,
            AssetKind::Command,
            AssetScope::Project,
            &directory.join(".opencode/commands"),
        );
    }
    for path in [
        home.join("skills"),
        scanner.roots.claude.join("skills"),
        scanner.roots.agents.join("skills"),
    ] {
        scanner.skills(tool, AssetScope::User, &path, false);
    }
    scanner.markdown_directory(
        tool,
        AssetKind::Command,
        AssetScope::User,
        &home.join("commands"),
    );
    for (base, scope) in [
        (home, AssetScope::User),
        (project.clone(), AssetScope::Project),
    ] {
        for name in ["opencode.json", "opencode.jsonc"] {
            if let Some((value, document)) = scanner.document(&base.join(name), "json") {
                let v2 = value.pointer("/mcp/servers").is_some() || value.get("commands").is_some();
                let map = if v2 {
                    value.pointer("/mcp/servers")
                } else {
                    value.get("mcp")
                };
                scanner.mcp_map(
                    tool,
                    scope,
                    &document,
                    map,
                    if v2 {
                        "opencode-v2-mcp"
                    } else {
                        "opencode-v1-mcp"
                    },
                );
                scanner.command_map(
                    tool,
                    scope,
                    &document,
                    value.get(if v2 { "commands" } else { "command" }),
                    if v2 {
                        "opencode-v2-command"
                    } else {
                        "opencode-v1-command"
                    },
                );
                scanner.referenced_files(tool, scope, value.get("instructions"), &project);
                if let Some(roots) = value.get("skills").and_then(Value::as_array) {
                    for path in roots.iter().filter_map(Value::as_str) {
                        if let Some(path) = scanner.expand_path(path, &base) {
                            scanner.skills(tool, scope, &path, false);
                        }
                    }
                }
            }
        }
    }
}
