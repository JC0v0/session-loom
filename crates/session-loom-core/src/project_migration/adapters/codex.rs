use super::*;
use crate::{
    canonical::SourceTool,
    project_migration::{discovery::Scanner, AssetKind, AssetScope},
};

pub(crate) fn discover(scanner: &mut Scanner<'_>) {
    let tool = SourceTool::Codex;
    let home = scanner.roots.codex.clone();
    let project = scanner.project.root.clone();
    let mut candidates = vec!["AGENTS.override.md".to_string(), "AGENTS.md".to_string()];
    for (path, scope) in [
        (home.join("config.toml"), AssetScope::User),
        (project.join(".codex/config.toml"), AssetScope::Project),
    ] {
        if let Some((config, document)) = scanner.document(&path, "toml") {
            if let Some(names) = config
                .get("project_doc_fallback_filenames")
                .and_then(Value::as_array)
            {
                candidates.extend(
                    names
                        .iter()
                        .filter_map(Value::as_str)
                        .filter(|name| !name.contains(['/', '\\']) && !name.contains(".."))
                        .map(str::to_string),
                );
            }
            scanner.mcp_map(
                tool,
                scope,
                &document,
                config.get("mcp_servers"),
                "codex-mcp",
            );
            if let Some(entries) = config.pointer("/skills/config").and_then(Value::as_array) {
                for entry in entries {
                    if let Some(value) = entry.get("path").and_then(Value::as_str) {
                        if let Some(path) =
                            scanner.expand_path(value, path.parent().unwrap_or(&home))
                        {
                            let root = if path.is_file() {
                                path.parent().unwrap_or(&path).to_path_buf()
                            } else {
                                path
                            };
                            scanner.skills(tool, scope, &root, false);
                        }
                    }
                }
            }
        }
    }
    scanner.instruction_chain(tool, &candidates, &[]);
    for name in ["AGENTS.override.md", "AGENTS.md"] {
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
            false,
        );
    }
    scanner.skills(
        tool,
        AssetScope::User,
        &scanner.roots.agents.join("skills"),
        false,
    );
    scanner.skills(tool, AssetScope::User, &home.join("skills"), false);
    scanner.markdown_directory(
        tool,
        AssetKind::Command,
        AssetScope::User,
        &home.join("prompts"),
    );
    scanner.markdown_directory(
        tool,
        AssetKind::Memory,
        AssetScope::User,
        &home.join("memories"),
    );
    scanner.file(
        tool,
        AssetKind::Memory,
        AssetScope::Project,
        &project.join("MEMORY.md"),
        "markdown-memory",
    );
}
