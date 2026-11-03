use super::*;
use crate::{
    canonical::SourceTool,
    project_migration::{discovery::Scanner, AssetKind, AssetScope},
};

pub(crate) fn discover(scanner: &mut Scanner<'_>) {
    let tool = SourceTool::Pi;
    let home = scanner.roots.pi.clone();
    let project = scanner.project.root.clone();
    let names: Vec<String> = [
        "AGENTS.override.md",
        "AGENTS.md",
        "AGENTS.MD",
        "CLAUDE.md",
        "CLAUDE.MD",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    scanner.instruction_chain(tool, &names, &[]);
    for name in &names {
        if scanner
            .file(
                tool,
                AssetKind::Instruction,
                AssetScope::User,
                &home.join(name),
                "instructions",
            )
            .is_some()
        {
            break;
        }
    }
    for directory in scanner.project_directories(tool) {
        scanner.skills(
            tool,
            AssetScope::Project,
            &directory.join(".agents/skills"),
            true,
        );
    }
    for (base, scope) in [
        (home, AssetScope::User),
        (project.join(".pi"), AssetScope::Project),
    ] {
        scanner.skills(tool, scope, &base.join("skills"), true);
        scanner.markdown_directory(tool, AssetKind::Command, scope, &base.join("prompts"));
        for name in ["SYSTEM.md", "APPEND_SYSTEM.md"] {
            scanner.file(
                tool,
                AssetKind::Instruction,
                scope,
                &base.join(name),
                if name == "SYSTEM.md" {
                    "pi-system-prompt"
                } else {
                    "instructions"
                },
            );
        }
        if let Some((value, document)) = scanner.document(&base.join("mcp.json"), "json") {
            scanner.mcp_map(tool, scope, &document, value.get("mcpServers"), "pi-mcp");
        }
        if let Some((value, _)) = scanner.document(&base.join("settings.json"), "json") {
            for key in ["skills", "prompts"] {
                if let Some(paths) = value.get(key).and_then(Value::as_array) {
                    for value in paths.iter().filter_map(Value::as_str) {
                        if let Some(path) = scanner.expand_path(value, &base) {
                            if key == "skills" {
                                scanner.skills(tool, scope, &path, true);
                            } else if path.is_file() {
                                scanner.file(
                                    tool,
                                    AssetKind::Command,
                                    scope,
                                    &path,
                                    "markdown-command",
                                );
                            } else {
                                scanner.markdown_directory(tool, AssetKind::Command, scope, &path);
                            }
                        }
                    }
                }
            }
        }
    }
    scanner.skills(
        tool,
        AssetScope::User,
        &scanner.roots.agents.join("skills"),
        true,
    );
    scanner.file(
        tool,
        AssetKind::Memory,
        AssetScope::Project,
        &project.join("MEMORY.md"),
        "markdown-memory",
    );
}
