use std::path::PathBuf;

pub fn home_dir() -> PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

pub fn store_root() -> PathBuf {
    std::env::var("SESSION_LOOM_STORE")
        .or_else(|_| std::env::var("SESSION_BRIDGE_STORE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".session-loom"))
}

pub fn codex_home() -> PathBuf {
    std::env::var("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".codex"))
}

pub fn codex_sessions_root() -> PathBuf {
    std::env::var("CODEX_SESSIONS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| codex_home().join("sessions"))
}

pub fn claude_root() -> PathBuf {
    std::env::var("CLAUDE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".claude"))
}

pub fn claude_sessions_root() -> PathBuf {
    std::env::var("CLAUDE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".claude").join("projects"))
}

pub fn dsh_sessions_root() -> PathBuf {
    std::env::var("DSH_SESSIONS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("DSH_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| home_dir().join(".dsh"))
                .join("sessions")
        })
}

pub fn pi_agent_dir() -> PathBuf {
    std::env::var("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".pi").join("agent"))
}

pub fn pi_sessions_root() -> PathBuf {
    std::env::var("PI_CODING_AGENT_SESSION_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| pi_agent_dir().join("sessions"))
}

pub fn opencode_data_dir() -> PathBuf {
    std::env::var("OPENCODE_DATA_DIR")
        .or_else(|_| std::env::var("XDG_DATA_HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".local").join("share"))
        .join("opencode")
}

pub fn opencode_database() -> PathBuf {
    if let Ok(value) = std::env::var("OPENCODE_DB") {
        if value != ":memory:" {
            let path = PathBuf::from(&value);
            if path.is_absolute() {
                return path;
            }
            return opencode_data_dir().join(path);
        }
    }
    let data = opencode_data_dir();
    let default = data.join("opencode.db");
    if default.exists() {
        return default;
    }
    if let Ok(entries) = std::fs::read_dir(&data) {
        let mut candidates = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.starts_with("opencode") && name.ends_with(".db"))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        candidates.sort();
        if let Some(first) = candidates.into_iter().next() {
            return first;
        }
    }
    default
}
