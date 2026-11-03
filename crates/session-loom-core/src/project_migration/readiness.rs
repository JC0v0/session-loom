use super::model::{SnapshotFile, TargetEnvironment};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

const MAX_PROBE_BYTES: u64 = 4 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct CapabilityReport {
    pub missing_commands: Vec<String>,
    pub missing_variables: Vec<String>,
    pub dynamic_expressions: Vec<String>,
    pub unverified_dependencies: Vec<String>,
}

impl CapabilityReport {
    pub fn is_blocked(&self) -> bool {
        !self.missing_commands.is_empty()
            || !self.missing_variables.is_empty()
            || !self.dynamic_expressions.is_empty()
    }

    pub fn next_steps(&self) -> Vec<String> {
        let mut steps = vec![];
        for command in &self.missing_commands {
            steps.push(format!("安装或配置运行依赖: {command}"));
        }
        for variable in &self.missing_variables {
            steps.push(format!("在目标环境中设置变量: {variable}"));
        }
        for expression in &self.dynamic_expressions {
            steps.push(format!("手工检查动态表达式并确认其安全含义: {expression}"));
        }
        if !self.unverified_dependencies.is_empty() {
            steps.push("安装项目依赖并检查配套资源是否完整".to_string());
        }
        steps.sort();
        steps.dedup();
        steps
    }

    pub fn evidence(&self) -> Vec<String> {
        let mut evidence = vec![];
        if !self.missing_commands.is_empty() {
            evidence.push(format!(
                "缺少运行命令: {}",
                self.missing_commands.join(", ")
            ));
        }
        if !self.missing_variables.is_empty() {
            evidence.push(format!(
                "缺少环境变量: {}",
                self.missing_variables.join(", ")
            ));
        }
        if !self.dynamic_expressions.is_empty() {
            evidence.push(format!(
                "存在未求值的动态表达式: {}",
                self.dynamic_expressions.join(", ")
            ));
        }
        if !self.unverified_dependencies.is_empty() {
            evidence.push(format!(
                "尚未确认的依赖: {}",
                self.unverified_dependencies.join(", ")
            ));
        }
        evidence
    }
}

pub(crate) fn configuration_requirements(
    value: &Value,
    environment: &TargetEnvironment,
) -> CapabilityReport {
    let mut report = CapabilityReport::default();
    let available: BTreeSet<_> = environment.environment_variables.iter().collect();
    visit(value, &available, &mut report);
    report
}

fn visit(value: &Value, available: &BTreeSet<&String>, report: &mut CapabilityReport) {
    match value {
        Value::String(value) => {
            if let Some(name) = variable_name(value) {
                if !available.contains(&name) && !report.missing_variables.contains(&name) {
                    report.missing_variables.push(name);
                }
            }
            if value.starts_with('!') && !report.dynamic_expressions.contains(value) {
                report.dynamic_expressions.push(value.clone());
            }
        }
        Value::Array(values) => {
            for value in values {
                visit(value, available, report);
            }
        }
        Value::Object(values) => {
            for (key, value) in values {
                if matches!(key.as_str(), "args" | "command") {
                    if let Some(items) = values.get(key).and_then(Value::as_array) {
                        for value in items {
                            visit(value, available, report);
                        }
                    }
                }
                visit(value, available, report);
            }
        }
        _ => {}
    }
}

fn variable_name(value: &str) -> Option<String> {
    let value = value.trim();
    if let Some(name) = value
        .strip_prefix("${")
        .and_then(|value| value.strip_suffix('}'))
    {
        if valid_variable(name) {
            return Some(name.to_string());
        }
    }
    if let Some(name) = value
        .strip_prefix("{env:")
        .and_then(|value| value.strip_suffix('}'))
    {
        if valid_variable(name) {
            return Some(name.to_string());
        }
    }
    let (scheme, rest) = value.split_once(' ')?;
    if matches!(scheme.to_ascii_lowercase().as_str(), "bearer" | "basic") {
        return variable_name(rest);
    }
    None
}

fn valid_variable(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|value| value.is_ascii_alphanumeric() || value == '_')
}

pub(crate) fn script_requirements(
    files: &[SnapshotFile],
    run_root: &Path,
    environment: &TargetEnvironment,
) -> CapabilityReport {
    let mut report = CapabilityReport::default();
    report
        .unverified_dependencies
        .push("脚本配套依赖尚未执行验证".to_string());
    for file in files {
        let interpreter = interpreter_for(file, &run_root.join(&file.snapshot_path));
        if let Some(command) = interpreter {
            if !program_available(&command, &environment.executable_search_path) {
                if Path::new(&command).is_absolute() {
                    if !Path::new(&command).is_file() {
                        report.missing_commands.push(command);
                    }
                } else {
                    report.missing_commands.push(command);
                }
            }
        }
    }
    dedup(&mut report.missing_commands);
    report
}

fn dedup(values: &mut Vec<String>) {
    values.sort();
    values.dedup();
}

pub(crate) fn program_available(command: &str, search_path: &[PathBuf]) -> bool {
    let path = Path::new(command);
    if path.is_absolute() || command.contains(['/', '\\']) {
        return path.is_file();
    }
    search_path.iter().any(|directory| {
        let direct = directory.join(command);
        if direct.is_file() {
            return true;
        }
        #[cfg(windows)]
        {
            let extensions = std::env::var_os("PATHEXT")
                .unwrap_or_else(|| ".EXE;.CMD;.BAT;.COM".into())
                .to_string_lossy()
                .to_string();
            return extensions
                .split(';')
                .filter(|extension| !extension.is_empty())
                .any(|extension| directory.join(format!("{command}{extension}")).is_file());
        }
        #[cfg(not(windows))]
        false
    })
}

fn interpreter_for(file: &SnapshotFile, path: &Path) -> Option<String> {
    if let Some(shebang) = read_shebang(path) {
        let mut words = shebang.split_whitespace();
        let executable = words.next()?;
        if executable.ends_with("/env") || executable.ends_with("\\env") {
            return words.next().map(str::to_string);
        }
        return Path::new(executable)
            .file_name()
            .map(|value| value.to_string_lossy().to_string());
    }
    let extension = file
        .relative_path
        .extension()
        .and_then(|value| value.to_str())?
        .to_ascii_lowercase();
    Some(
        match extension.as_str() {
            "sh" | "bash" | "zsh" => "sh",
            "py" => "python3",
            "js" | "cjs" | "mjs" | "ts" | "mts" | "cts" => "node",
            "rb" => "ruby",
            "ps1" => "pwsh",
            "pl" => "perl",
            "php" => "php",
            _ => return None,
        }
        .to_string(),
    )
}

fn read_shebang(path: &Path) -> Option<String> {
    let mut file = fs::File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > 64 * 1024 {
        return None;
    }
    file.seek(SeekFrom::Start(0)).ok()?;
    let mut bytes = vec![];
    file.take(MAX_PROBE_BYTES).read_to_end(&mut bytes).ok()?;
    let text = std::str::from_utf8(&bytes).ok()?;
    let first = text.lines().next()?.trim();
    first
        .strip_prefix("#!")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}
