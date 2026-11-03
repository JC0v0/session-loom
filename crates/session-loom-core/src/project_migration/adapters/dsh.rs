use super::*;
use crate::{
    canonical::SourceTool,
    project_migration::{discovery::Scanner, AssetKind, AssetScope, SourceDocument},
};

pub(crate) fn discover(scanner: &mut Scanner<'_>) {
    let tool = SourceTool::Dsh;
    let project = scanner.project.root.clone();
    let home = scanner.roots.dsh.clone();
    scanner.instruction_chain(
        tool,
        &[],
        &[
            "AGENTS.md",
            "CLAUDE.md",
            "AGENTS.local.md",
            "CLAUDE.local.md",
        ],
    );
    for root in [home.clone(), scanner.roots.dsh_agents.clone()] {
        scanner.file(
            tool,
            AssetKind::Instruction,
            AssetScope::User,
            &root.join("AGENTS.md"),
            "instructions",
        );
        scanner.skills(tool, AssetScope::User, &root.join("skills"), false);
    }
    for path in [".dsh/skills", ".agents/skills"] {
        scanner.skills(tool, AssetScope::Project, &project.join(path), false);
    }
    let path = home.join("cordis.patch.yml");
    if let Some((value, document)) = scanner.document(&path, "yaml") {
        rows(scanner, &value, &document, AssetScope::User);
    }
    if let Some(profile) = &scanner.options.dsh_profile {
        if profile.is_empty()
            || profile == "."
            || profile == ".."
            || !profile
                .chars()
                .all(|value| value.is_ascii_alphanumeric() || matches!(value, '-' | '_' | '.'))
        {
            scanner.problem(
                "profileInvalid",
                "DSH profile must be a single profile name",
                &home,
            );
        } else {
            let path = home.join("profiles").join(profile).join("cordis.patch.yml");
            if let Some((value, document)) = scanner.document(&path, "yaml") {
                rows(scanner, &value, &document, AssetScope::Profile);
            }
        }
    }
    scanner.file(
        tool,
        AssetKind::Memory,
        AssetScope::Project,
        &project.join("MEMORY.md"),
        "markdown-memory",
    );
}

fn rows(scanner: &mut Scanner<'_>, value: &Value, document: &SourceDocument, scope: AssetScope) {
    match value {
        Value::Array(values) => {
            for value in values {
                rows(scanner, value, document, scope);
            }
        }
        Value::Object(values) => {
            if values.get("name").and_then(Value::as_str) == Some("@deepseek-ai/dsh-mcp-client") {
                if let Some(name) = value.pointer("/config/serverName").and_then(Value::as_str) {
                    scanner.mcp(
                        SourceTool::Dsh,
                        scope,
                        document,
                        name,
                        value,
                        "dsh-cordis-mcp",
                    );
                } else {
                    scanner.problem(
                        "mcpNameMissing",
                        "DSH MCP row has no serverName",
                        &document.path,
                    );
                }
            } else {
                for key in ["insert", "patch", "config", "children"] {
                    if let Some(value) = values.get(key) {
                        rows(scanner, value, document, scope);
                    }
                }
            }
        }
        _ => {}
    }
}
