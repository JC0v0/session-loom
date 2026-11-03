pub(crate) mod claude;
pub(crate) mod codex;
pub(crate) mod dsh;
pub(crate) mod opencode;
pub(crate) mod pi;

use serde_json::{Map, Value};
use std::{collections::BTreeMap, fs, io::Read, path::Path};
use yaml_rust2::{
    parser::{Event, EventReceiver, Parser, Tag},
    scanner::TScalarStyle,
    Yaml,
};

const MAX_CONFIG_BYTES: u64 = 8 * 1024 * 1024;

pub(crate) fn read_text(path: &Path) -> Result<Option<String>, String> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
        Ok(_) => {}
    }
    let metadata = fs::metadata(path).map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_CONFIG_BYTES {
        return Err("configuration is not a file or exceeds the 8 MiB text limit".to_string());
    }
    let mut bytes = vec![];
    fs::File::open(path)
        .map_err(|error| error.to_string())?
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err("configuration exceeds the 8 MiB text limit".to_string());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "configuration is not valid UTF-8".to_string())
}

pub(crate) fn parse_json(text: &str) -> Result<Value, String> {
    jsonc_parser::parse_to_serde_value(text, &Default::default())
        .map_err(|error| {
            format!(
                "{} at line {} column {}",
                error.kind(),
                error.line_display(),
                error.column_display()
            )
        })?
        .ok_or_else(|| "configuration document is empty".to_string())
}

pub(crate) fn parse_toml(text: &str) -> Result<Value, String> {
    toml_edit::de::from_str(text).map_err(|error: toml_edit::de::Error| {
        let prefix = text
            .get(..error.span().map(|span| span.start).unwrap_or(0))
            .unwrap_or("");
        let line = prefix.bytes().filter(|value| *value == b'\n').count() + 1;
        let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        format!(
            "invalid TOML at line {line} column {column}: {}",
            error.message()
        )
    })
}

enum Container {
    Sequence(Vec<Value>),
    Mapping(Map<String, Value>, Option<String>),
}

/// Keep executable YAML tags as explicit data. No JavaScript or shell
/// interpreter participates in configuration discovery.
#[derive(Default)]
struct YamlData {
    stack: Vec<(Container, Option<Tag>, usize)>,
    documents: Vec<Value>,
    anchors: BTreeMap<usize, Value>,
    root: Option<Value>,
    error: Option<String>,
    nodes: usize,
}

impl YamlData {
    fn push(&mut self, value: Value, anchor: usize) {
        self.nodes += 1;
        if self.nodes > 100_000 {
            self.error = Some("YAML document has too many values".to_string());
            return;
        }
        if anchor != 0 {
            self.anchors.insert(anchor, value.clone());
        }
        match self.stack.last_mut().map(|entry| &mut entry.0) {
            Some(Container::Sequence(values)) => values.push(value),
            Some(Container::Mapping(values, key)) => {
                if let Some(key) = key.take() {
                    if values.insert(key, value).is_some() {
                        self.error = Some("duplicate YAML mapping key".to_string());
                    }
                } else if let Value::String(value) = value {
                    *key = Some(value);
                } else {
                    self.error = Some("YAML configuration keys must be strings".to_string());
                }
            }
            None => self.root = Some(value),
        }
    }

    fn tagged(value: Value, tag: Option<Tag>) -> Value {
        match tag {
            Some(tag)
                if tag.handle != "tag:yaml.org,2002:"
                    || !matches!(
                        tag.suffix.as_str(),
                        "str" | "bool" | "int" | "float" | "null" | "map" | "seq"
                    ) =>
            {
                serde_json::json!({
                    "$yamlTag": format!("{}{}", tag.handle, tag.suffix),
                    "value": value,
                })
            }
            _ => value,
        }
    }
}

impl EventReceiver for YamlData {
    fn on_event(&mut self, event: Event) {
        if self.error.is_some() {
            return;
        }
        match event {
            Event::SequenceStart(anchor, tag) => {
                if self.stack.len() >= 64 {
                    self.error = Some("YAML nesting exceeds the configuration limit".to_string());
                    return;
                }
                self.stack.push((Container::Sequence(vec![]), tag, anchor));
            }
            Event::MappingStart(anchor, tag) => {
                if self.stack.len() >= 64 {
                    self.error = Some("YAML nesting exceeds the configuration limit".to_string());
                    return;
                }
                self.stack
                    .push((Container::Mapping(Map::new(), None), tag, anchor));
            }
            Event::SequenceEnd | Event::MappingEnd => {
                if let Some((container, tag, anchor)) = self.stack.pop() {
                    let value = match container {
                        Container::Sequence(values) => Value::Array(values),
                        Container::Mapping(values, None) => Value::Object(values),
                        Container::Mapping(_, Some(_)) => {
                            self.error = Some("YAML mapping has no value".to_string());
                            return;
                        }
                    };
                    self.push(Self::tagged(value, tag), anchor);
                }
            }
            Event::Scalar(text, style, anchor, tag) => {
                let core_type = tag
                    .as_ref()
                    .filter(|tag| tag.handle == "tag:yaml.org,2002:")
                    .map(|tag| tag.suffix.as_str());
                let typed_number = matches!(core_type, Some("int" | "float" | "bool" | "null"));
                let value = if core_type == Some("str")
                    || (!typed_number && (style != TScalarStyle::Plain || tag.is_some()))
                {
                    Value::String(text)
                } else {
                    match Yaml::from_str(&text) {
                        Yaml::Boolean(value) => Value::Bool(value),
                        Yaml::Integer(value) => Value::from(value),
                        Yaml::Real(value) => value
                            .parse::<serde_json::Number>()
                            .map(Value::Number)
                            .unwrap_or(Value::String(value)),
                        Yaml::Null => Value::Null,
                        _ => Value::String(text),
                    }
                };
                self.push(Self::tagged(value, tag), anchor);
            }
            Event::Alias(_) => {
                // Avoid expanding attacker-controlled alias graphs. Preserve
                // the source document for an explicit unsupported diagnostic.
                self.error = Some("YAML aliases need explicit configuration review".to_string());
            }
            Event::DocumentEnd => {
                if let Some(value) = self.root.take() {
                    self.documents.push(value);
                }
                self.anchors.clear();
            }
            _ => {}
        }
    }
}

pub(crate) fn parse_yaml(text: &str) -> Result<Value, String> {
    let mut data = YamlData::default();
    Parser::new_from_str(text)
        .load(&mut data, true)
        .map_err(|error| error.to_string())?;
    if let Some(error) = data.error {
        return Err(error);
    }
    if data.documents.len() != 1 {
        return Err("expected exactly one YAML configuration document".to_string());
    }
    Ok(data.documents.remove(0))
}

pub(crate) fn frontmatter(text: &str) -> Result<Value, String> {
    let normalized = text.replace("\r\n", "\n");
    let Some(rest) = normalized.strip_prefix("---\n") else {
        return Ok(Value::Object(Map::new()));
    };
    let end = rest
        .find("\n---")
        .ok_or_else(|| "unterminated Markdown frontmatter".to_string())?;
    if rest[..end].trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    let value = parse_yaml(&rest[..end])?;
    if !value.is_object() {
        return Err("Markdown frontmatter must be a mapping".to_string());
    }
    Ok(value)
}

pub(crate) fn variable_reference(value: &str) -> bool {
    (value.starts_with("${") && value.ends_with('}'))
        || (value.starts_with("{env:") && value.ends_with('}'))
}

pub(crate) fn strip_secrets(value: &mut Value) -> Vec<String> {
    fn sensitive_option(flag: &str) -> bool {
        matches!(
            flag.to_ascii_lowercase().as_str(),
            "--token"
                | "--api-key"
                | "--apikey"
                | "--password"
                | "--client-secret"
                | "--secret"
                | "--authorization"
                | "--auth-token"
                | "--access-token"
                | "--bearer"
        )
    }

    fn arguments(values: &mut Vec<Value>, prefix: &str, removed: &mut Vec<String>) {
        let mut result = vec![];
        let mut index = 0;
        while index < values.len() {
            if let Some(argument) = values[index].as_str() {
                if let Some((flag, value)) = argument.split_once('=') {
                    if sensitive_option(flag) && !variable_reference(value) {
                        removed.push(format!("{prefix}[{index}]"));
                        index += 1;
                        continue;
                    }
                }
                if sensitive_option(argument) {
                    if let Some(value) = values.get(index + 1).and_then(Value::as_str) {
                        if !variable_reference(value) {
                            removed.push(format!("{prefix}[{index}]"));
                            index += 2;
                            continue;
                        }
                    }
                }
            }
            result.push(values[index].clone());
            index += 1;
        }
        *values = result;
    }

    fn address(value: &str) -> Option<String> {
        let scheme = value.find("://")?;
        let start = scheme + 3;
        let end = value[start..]
            .find(['/', '?', '#'])
            .map(|offset| start + offset)
            .unwrap_or(value.len());
        let mut result = value.to_string();
        let mut changed = false;
        if let Some(userinfo) = value[start..end].rfind('@') {
            result.replace_range(start..start + userinfo + 1, "");
            changed = true;
        }
        let (base, fragment) = result
            .split_once('#')
            .map(|(base, fragment)| (base.to_string(), Some(fragment.to_string())))
            .unwrap_or((result.clone(), None));
        let (prefix, query) = base
            .split_once('?')
            .map(|(prefix, query)| (prefix.to_string(), Some(query.to_string())))
            .unwrap_or((base.clone(), None));
        let clean_pairs = |query: &str| {
            query
                .split('&')
                .filter(|pair| {
                    let Some((key, value)) = form_urlencoded::parse(pair.as_bytes()).next() else {
                        return true;
                    };
                    let key: String = key
                        .to_ascii_lowercase()
                        .chars()
                        .filter(|value| value.is_ascii_alphanumeric())
                        .collect();
                    let secret = key.contains("token")
                        || key.contains("secret")
                        || key.contains("password")
                        || key.contains("apikey")
                        || matches!(
                            key.as_str(),
                            "key" | "auth" | "authorization" | "sig" | "signature"
                        );
                    !secret || variable_reference(&value)
                })
                .collect::<Vec<_>>()
                .join("&")
        };
        let mut clean = prefix;
        if let Some(query) = query {
            let filtered = clean_pairs(&query);
            changed |= filtered != query;
            if !filtered.is_empty() {
                clean.push('?');
                clean.push_str(&filtered);
            }
        }
        if let Some(fragment) = fragment {
            let filtered = clean_pairs(&fragment);
            changed |= filtered != fragment;
            if !filtered.is_empty() {
                clean.push('#');
                clean.push_str(&filtered);
            }
        }
        changed.then_some(clean)
    }

    fn visit(value: &mut Value, prefix: &str, removed: &mut Vec<String>) {
        match value {
            Value::Object(values) => {
                let keys: Vec<_> = values.keys().cloned().collect();
                for key in keys {
                    let lower = key.to_ascii_lowercase();
                    let normalized: String = lower
                        .chars()
                        .filter(|value| value.is_ascii_alphanumeric())
                        .collect();
                    let sensitive = normalized.contains("secret")
                        || normalized.contains("password")
                        || normalized.contains("token")
                        || normalized.contains("apikey")
                        || normalized.contains("credential")
                        || matches!(normalized.as_str(), "authorization" | "auth" | "bearer");
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    if matches!(key.as_str(), "args" | "command") {
                        if let Some(values) = values.get_mut(&key).and_then(Value::as_array_mut) {
                            arguments(values, &path, removed);
                        }
                    }
                    if key == "url" {
                        if let Some(clean) = values[&key].as_str().and_then(address) {
                            values.insert(key.clone(), Value::String(clean));
                            removed.push(path.clone());
                        }
                    }
                    let reference = values[&key].as_str().is_some_and(|value| {
                        variable_reference(value)
                            || value.split_once(' ').is_some_and(|(scheme, value)| {
                                (scheme.eq_ignore_ascii_case("Bearer")
                                    || scheme.eq_ignore_ascii_case("Basic"))
                                    && variable_reference(value.trim())
                            })
                    });
                    // Preserve variable-name fields such as bearer_token_env_var.
                    if sensitive && !normalized.ends_with("envvar") && !reference {
                        values.remove(&key);
                        removed.push(path);
                    } else if let Some(value) = values.get_mut(&key) {
                        visit(value, &path, removed);
                    }
                }
            }
            Value::Array(values) => {
                for (index, value) in values.iter_mut().enumerate() {
                    visit(value, &format!("{prefix}[{index}]"), removed);
                }
            }
            _ => {}
        }
    }
    let mut removed = vec![];
    visit(value, "", &mut removed);
    removed
}
