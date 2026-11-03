//! Explicit, project-scoped migration of sessions and their working context.
//!
//! Environment snapshots and execution records are separate from canonical
//! sessions so the session watcher never treats a context change as a new chat.

mod adapters;
pub mod apply;
mod config_edit;
pub mod diff;
pub mod discovery;
mod markdown;
pub mod model;
pub mod preview;
pub mod projects;
mod readiness;
pub mod render;
pub mod run_store;
pub mod service;
pub mod snapshot;

pub use apply::apply_preview;
pub use discovery::{discover_environment, DiscoveryOptions, DiscoveryReport};
pub use model::*;
pub use preview::{create_preview, load_preview, verify_preview};
pub use projects::{list_projects, resolve_project, select_project_sessions};
pub use render::render_environment;
pub use run_store::RunStore;
pub use service::{
    apply_migration, capture_clone_selection, capture_migration, detect_opencode_version,
    list_migration_projects, prepare_migration, prepare_session_clone, preview_migration,
    set_migration_target, target_environment, PrepareRequest, PreparedMigration, TargetChoices,
};
pub use snapshot::capture_selection;
