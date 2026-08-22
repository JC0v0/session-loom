//! Shared helpers for integration tests that must not touch the machine's
//! real agent data directories.

#![allow(dead_code)]

use std::path::Path;
use std::sync::{Mutex, MutexGuard};

/// Serializes every test that mutates process-global environment variables;
/// Cargo runs one binary's tests on parallel threads.
static ENV_LOCK: Mutex<()> = Mutex::new(());

pub fn env_lock() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Redirects `DSH_HOME` to `root` until the returned guard is dropped.
///
/// `dsh::write_session_to_root` registers written sessions in
/// `$DSH_HOME/storages/workspace.json`, falling back to the given root's
/// parent only when the variable is unset. Without this guard the suite
/// appends fake workspaces to the real DeepSeek Harness index on machines
/// where `DSH_HOME` is configured.
pub struct DshHomeGuard {
    previous: Option<String>,
    _lock: MutexGuard<'static, ()>,
}

impl Drop for DshHomeGuard {
    fn drop(&mut self) {
        match &self.previous {
            Some(value) => std::env::set_var("DSH_HOME", value),
            None => std::env::remove_var("DSH_HOME"),
        }
    }
}

pub fn isolate_dsh_home(root: &Path) -> DshHomeGuard {
    let lock = env_lock();
    let previous = std::env::var("DSH_HOME").ok();
    std::env::set_var("DSH_HOME", root);
    DshHomeGuard {
        previous,
        _lock: lock,
    }
}
