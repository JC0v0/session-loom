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

pub fn codex_sessions_root() -> PathBuf {
    std::env::var("CODEX_SESSIONS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home_dir().join(".codex").join("sessions"))
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
