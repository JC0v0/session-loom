use super::adapters::{parse_json, parse_toml, parse_yaml};
use jsonc_parser::{
    cst::{CstInputValue, CstRootNode},
    ParseOptions,
};
use serde_json::Value;
use toml_edit::{Array, Document, InlineTable, Item, Table, TableLike, Value as TomlValue};

/// Add or replace one value nested below object keys. JSONC comments,
/// whitespace, trailing commas and unrelated nodes remain in the syntax tree.
pub(crate) fn jsonc_set(
    text: &str,
    object_path: &[&str],
    key: &str,
    value: &Value,
) -> Result<String, String> {
    let root = CstRootNode::parse(text, &ParseOptions::default())
        .map_err(|error| format!("invalid JSONC: {error}"))?;
    let mut object = root
        .object_value_or_create()
        .ok_or_else(|| "JSONC root must be an object".to_string())?;
    for segment in object_path {
        object = object
            .object_value_or_create(segment)
            .ok_or_else(|| format!("JSONC path segment is not an object: {segment}"))?;
    }
    let value = json_to_cst(value)?;
    match object.get(key) {
        Some(property) => property.set_value(value),
        None => {
            object.append(key, value);
        }
    }
    let output = root.to_string();
    parse_json(&output)?;
    Ok(output)
}

fn json_to_cst(value: &Value) -> Result<CstInputValue, String> {
    Ok(match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(value) => CstInputValue::Bool(*value),
        Value::Number(value) => CstInputValue::Number(value.to_string()),
        Value::String(value) => CstInputValue::String(value.clone()),
        Value::Array(values) => CstInputValue::Array(
            values
                .iter()
                .map(json_to_cst)
                .collect::<Result<Vec<_>, _>>()?,
        ),
        Value::Object(values) => CstInputValue::Object(
            values
                .iter()
                .map(|(key, value)| Ok((key.clone(), json_to_cst(value)?)))
                .collect::<Result<Vec<_>, String>>()?,
        ),
    })
}

/// Add or replace one TOML value while retaining comments and unrelated
/// tables. `object_path` identifies table-like containers rather than dotted
/// string paths so quoted keys have natural TOML escaping.
pub(crate) fn toml_set(
    text: &str,
    object_path: &[&str],
    key: &str,
    value: &Value,
) -> Result<String, String> {
    let mut document = text
        .parse::<Document>()
        .map_err(|error| format!("invalid TOML: {error}"))?;
    let mut table: &mut dyn TableLike = document.as_table_mut();
    for segment in object_path {
        if !table.contains_key(segment) {
            table.insert(segment, Item::Table(Table::new()));
        }
        table = table
            .get_mut(segment)
            .and_then(Item::as_table_like_mut)
            .ok_or_else(|| format!("TOML path segment is not a table: {segment}"))?;
    }
    table.insert(key, Item::Value(json_to_toml(value)?));
    let output = document.to_string();
    parse_toml(&output)?;
    Ok(output)
}

fn json_to_toml(value: &Value) -> Result<TomlValue, String> {
    Ok(match value {
        Value::Null => return Err("TOML does not support null values".to_string()),
        Value::Bool(value) => TomlValue::from(*value),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                TomlValue::from(value)
            } else if let Some(value) = value.as_f64() {
                TomlValue::from(value)
            } else {
                return Err(format!("TOML number is out of range: {value}"));
            }
        }
        Value::String(value) => TomlValue::from(value.clone()),
        Value::Array(values) => {
            let mut array = Array::new();
            for value in values {
                array.push_formatted(json_to_toml(value)?);
            }
            TomlValue::from(array)
        }
        Value::Object(values) => {
            let mut table = InlineTable::new();
            for (key, value) in values {
                table.insert(key, json_to_toml(value)?);
            }
            TomlValue::from(table)
        }
    })
}

/// Append a generated Cordis patch while retaining existing text, YAML tags
/// and JavaScript-looking expressions byte for byte. Any additions are JSON,
/// which is also valid YAML, then the combined document is parsed as data.
pub(crate) fn append_yaml_patch(text: &str, marker: &str, patch: &str) -> Result<String, String> {
    let marker = sanitize_comment(marker);
    let start = format!("# session-loom:{marker}:start");
    let end = format!("# session-loom:{marker}:end");
    if patch.contains(&start) || patch.contains(&end) {
        return Err("source YAML contains a conflicting managed marker".to_string());
    }
    let separator = if text.ends_with('\n') || text.is_empty() {
        ""
    } else {
        "\n"
    };
    let output = format!("{text}{separator}\n{start}\n{}\n{end}\n", patch.trim_end());
    parse_yaml(&output)?;
    Ok(output)
}

pub(crate) fn upsert_yaml_patch(text: &str, marker: &str, patch: &str) -> Result<String, String> {
    let marker = sanitize_comment(marker);
    let start = format!("# session-loom:{marker}:start");
    let end = format!("# session-loom:{marker}:end");
    let block = format!("{start}\n{}\n{end}", patch.trim_end());
    if let Some(start_index) = text.find(&start) {
        let end_offset = text[start_index..]
            .find(&end)
            .ok_or_else(|| "existing managed YAML section is unterminated".to_string())?;
        let end_index = start_index + end_offset + end.len();
        let mut output = String::with_capacity(text.len() + block.len());
        output.push_str(&text[..start_index]);
        output.push_str(&block);
        output.push_str(&text[end_index..]);
        parse_yaml(&output)?;
        return Ok(output);
    }
    let base = empty_flow_sequence_base(text).unwrap_or_else(|| text.to_string());
    append_yaml_patch(&base, &marker, patch)
}

/// DSH creates a fresh profile patch as comments followed by `[]`. Appending
/// a block sequence after that flow sequence is invalid YAML, so remove the
/// empty document value while keeping the file's comments.
fn empty_flow_sequence_base(text: &str) -> Option<String> {
    let value = parse_yaml(text).ok()?;
    if !matches!(&value, Value::Array(items) if items.is_empty()) {
        return None;
    }
    let mut removed = false;
    let mut lines = vec![];
    for line in text.lines() {
        if !removed && line.trim() == "[]" {
            removed = true;
            continue;
        }
        lines.push(line);
    }
    let mut output = lines.join("\n");
    if !output.is_empty() {
        output.push('\n');
    }
    Some(output)
}

pub(crate) fn yaml_patch_body(text: &str, marker: &str) -> Option<String> {
    let marker = sanitize_comment(marker);
    let start = format!("# session-loom:{marker}:start");
    let end = format!("# session-loom:{marker}:end");
    let start_index = text.find(&start)?;
    let body_start = start_index + start.len();
    let end_offset = text[body_start..].find(&end)?;
    Some(text[body_start..body_start + end_offset].trim().to_string())
}

fn sanitize_comment(value: &str) -> String {
    value
        .chars()
        .map(|value| {
            if value.is_ascii_alphanumeric() || matches!(value, '-' | '_') {
                value
            } else {
                '-'
            }
        })
        .collect()
}

pub(crate) fn managed_block(marker: &str, body: &str) -> Result<String, String> {
    let marker = sanitize_comment(marker);
    let start = format!("<!-- session-loom:{marker}:start -->");
    let end = format!("<!-- session-loom:{marker}:end -->");
    if body.contains(&start) || body.contains(&end) {
        return Err("source text contains a conflicting managed section marker".to_string());
    }
    Ok(format!("{start}\n{}\n{end}", body.trim_end()))
}

/// Upsert one source-owned instruction block without replacing unrelated
/// target instructions.
pub(crate) fn upsert_managed_block(
    existing: &str,
    marker: &str,
    body: &str,
) -> Result<String, String> {
    let marker = sanitize_comment(marker);
    let start = format!("<!-- session-loom:{marker}:start -->");
    let end = format!("<!-- session-loom:{marker}:end -->");
    let block = managed_block(&marker, body)?;
    if let Some(start_index) = existing.find(&start) {
        let end_offset = existing[start_index..]
            .find(&end)
            .ok_or_else(|| "existing managed instruction section is unterminated".to_string())?;
        let end_index = start_index + end_offset + end.len();
        let mut output = String::with_capacity(existing.len() + block.len());
        output.push_str(&existing[..start_index]);
        output.push_str(&block);
        output.push_str(&existing[end_index..]);
        return Ok(output);
    }
    Ok(if existing.trim().is_empty() {
        format!("{block}\n")
    } else {
        format!("{}\n\n{block}\n", existing.trim_end())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonc_edit_keeps_comments_and_unrelated_values() {
        let text =
            "{\n  // keep me\n  \"unrelated\": {\"token\": \"target-owned\"},\n  \"mcp\": {},\n}\n";
        let value = serde_json::json!({"command": "fixture-mcp", "args": ["--mode", "test"]});
        let output = jsonc_set(text, &["mcp"], "database", &value).unwrap();
        assert!(output.contains("// keep me"));
        assert!(output.contains("\"token\": \"target-owned\""));
        assert!(output.contains("\"database\""));
    }

    #[test]
    fn toml_edit_keeps_comments_and_unrelated_tables() {
        let text = "# keep me\nmodel = \"fixture\"\n\n[other]\nvalue = true\n";
        let value = serde_json::json!({
            "command": "fixture-mcp",
            "args": ["--mode", "test"],
            "env": {"DETAIL": "fixture"}
        });
        let output = toml_set(text, &["mcp_servers"], "database", &value).unwrap();
        assert!(output.contains("# keep me"));
        assert!(output.contains("model = \"fixture\""));
        assert!(output.contains("[other]"));
        assert!(output.contains("database"));
    }

    #[test]
    fn yaml_patch_keeps_tags_and_appends_another_row() {
        let text =
            "- insert:\n    - name: existing\n      config:\n        cwd: !!js process.cwd()\n";
        let patch = "- insert:\n    - name: \"@deepseek-ai/dsh-mcp-client\"\n      config:\n        serverName: \"database\"\n";
        let output = append_yaml_patch(text, "fixture", patch).unwrap();
        assert!(output.starts_with(text));
        assert!(output.contains("# session-loom:fixture"));
        assert!(output.contains("@deepseek-ai/dsh-mcp-client"));
    }

    #[test]
    fn yaml_patch_replaces_a_fresh_profile_empty_array() {
        let text = "# Your patch layer for this dsh profile:\n# comments\n[]\n";
        let patch = "- insert:\n    - {\"name\":\"@deepseek-ai/dsh-mcp-client\",\"config\":{\"serverName\":\"database\"}}";
        let output = upsert_yaml_patch(text, "fixture", patch).unwrap();
        assert!(output.contains("# Your patch layer"));
        assert!(!output.lines().any(|line| line.trim() == "[]"));
        assert!(output.contains("@deepseek-ai/dsh-mcp-client"));
        let value = parse_yaml(&output).unwrap();
        assert_eq!(value.as_array().unwrap().len(), 1);
        let again = upsert_yaml_patch(&output, "fixture", patch)
            .unwrap_or_else(|error| panic!("second upsert failed: {error}\n---\n{output}\n---"));
        assert_eq!(again, output);
    }

    #[test]
    fn yaml_patch_appends_to_an_existing_block_sequence() {
        let text = "- id: existing\n  name: fixture\n";
        let patch = "- insert:\n    - {\"name\":\"@deepseek-ai/dsh-mcp-client\"}";
        let output = upsert_yaml_patch(text, "fixture", patch).unwrap();
        let value = parse_yaml(&output).unwrap();
        assert_eq!(value.as_array().unwrap().len(), 2);
    }

    #[test]
    fn managed_block_is_idempotent() {
        let first = upsert_managed_block("# Existing\n", "abc", "Migrated").unwrap();
        let second = upsert_managed_block(&first, "abc", "Updated").unwrap();
        assert!(second.contains("# Existing"));
        assert!(!second.contains("Migrated"));
        assert!(second.contains("Updated"));
        assert_eq!(second.matches("session-loom:abc:start").count(), 1);
    }
}
