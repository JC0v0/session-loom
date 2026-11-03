use crate::canonical::{CanonicalSession, SourceTool};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use uuid::Uuid;

pub const MIGRATION_SCHEMA_VERSION: u32 = 1;

pub fn content_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Only relative resource names are accepted inside a snapshot or a package.
/// Check Windows spelling as well, even when inspecting it on another OS.
pub fn validate_resource_path(path: &Path) -> Result<(), String> {
    let value = path.to_string_lossy();
    if value.is_empty()
        || value.starts_with(['/', '\\'])
        || value.contains('\0')
        || value.contains(':')
        || value
            .split(['/', '\\'])
            .all(|part| part.is_empty() || part == ".")
        || value.split(['/', '\\']).any(|part| part == "..")
        || path.components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(format!("invalid resource path: {}", path.display()));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<PathBuf>,
}

impl Diagnostic {
    pub fn new(code: &str, message: impl Into<String>, path: Option<PathBuf>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            path,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectIdentity {
    pub id: String,
    pub root: PathBuf,
    pub accessible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_common_dir: Option<PathBuf>,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

/// A native instance, never the representative instance of a merged card.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSession {
    pub session_id: String,
    pub conversation_id: String,
    pub source_tool: SourceTool,
    pub cwd: String,
    pub title: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectEntry {
    pub project: ProjectIdentity,
    pub sessions: Vec<ProjectSession>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SelectedSession {
    pub metadata: ProjectSession,
    pub content_hash: String,
    pub session: CanonicalSession,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentRoots {
    pub user_home: PathBuf,
    pub codex: PathBuf,
    pub claude: PathBuf,
    pub claude_user_config: PathBuf,
    pub opencode: PathBuf,
    pub dsh: PathBuf,
    pub pi: PathBuf,
    pub agents: PathBuf,
    pub dsh_agents: PathBuf,
}

impl EnvironmentRoots {
    /// Explicit roots for tests, isolated workers, and caller-owned profiles.
    /// No discovery code needs to consult the process home after this point.
    pub fn isolated(root: &Path) -> Self {
        Self {
            user_home: root.to_path_buf(),
            codex: root.join("codex"),
            claude: root.join("claude"),
            claude_user_config: root.join("claude.json"),
            opencode: root.join("opencode"),
            dsh: root.join("dsh"),
            pi: root.join("pi"),
            agents: root.join("agents"),
            dsh_agents: root.join("agents"),
        }
    }

    pub fn from_environment() -> Self {
        let home = crate::paths::home_dir();
        let configured = |key: &str, fallback: PathBuf| {
            std::env::var_os(key).map(PathBuf::from).unwrap_or(fallback)
        };
        let agents = home.join(".agents");
        Self {
            user_home: home.clone(),
            codex: crate::paths::codex_home(),
            claude: configured("CLAUDE_CONFIG_DIR", crate::paths::claude_root()),
            claude_user_config: home.join(".claude.json"),
            opencode: configured("XDG_CONFIG_HOME", home.join(".config")).join("opencode"),
            dsh: configured("DSH_HOME", home.join(".dsh")),
            pi: crate::paths::pi_agent_dir(),
            dsh_agents: configured("DSH_AGENTS_HOME", agents.clone()),
            agents,
        }
    }

    pub fn tool_home(&self, tool: SourceTool) -> &Path {
        match tool {
            SourceTool::Codex => &self.codex,
            SourceTool::Claude => &self.claude,
            SourceTool::OpenCode => &self.opencode,
            SourceTool::Dsh => &self.dsh,
            SourceTool::Pi => &self.pi,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetKind {
    Memory,
    Instruction,
    Skill,
    Mcp,
    Script,
    Command,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AssetScope {
    Project,
    User,
    Profile,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UsageEvidence {
    pub kind: String,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceFile {
    pub path: PathBuf,
    pub relative_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceDocument {
    pub path: PathBuf,
    pub resolved_path: PathBuf,
    pub kind: SourceDocumentKind,
    pub content_hash: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SourceDocumentKind {
    File,
    Directory,
    Pattern,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceProbe {
    pub path: PathBuf,
    pub is_pattern: bool,
    pub document: Option<SourceDocument>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentAsset {
    pub id: String,
    pub kind: AssetKind,
    pub name: String,
    pub source_tool: SourceTool,
    pub scope: AssetScope,
    pub scope_root: PathBuf,
    pub format: String,
    pub files: Vec<SourceFile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource_root: Option<PathBuf>,
    #[serde(default)]
    pub source_documents: Vec<SourceDocument>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<serde_json::Value>,
    #[serde(default)]
    pub evidence: Vec<UsageEvidence>,
    pub selected_by_default: bool,
    #[serde(default)]
    pub source_priority: u32,
    #[serde(default = "source_active_default")]
    pub source_active: bool,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

fn source_active_default() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotFile {
    pub source: PathBuf,
    pub relative_path: PathBuf,
    pub snapshot_path: PathBuf,
    pub content_hash: String,
    pub size: u64,
    pub executable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AssetSnapshot {
    pub asset: EnvironmentAsset,
    pub files: Vec<SnapshotFile>,
    pub content_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub metadata: ProjectSession,
    pub snapshot_path: PathBuf,
    pub content_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_document: Option<SourceDocument>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ConflictChoice {
    Keep,
    Merge,
    Replace,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Readiness {
    Available,
    NeedsConfiguration,
    Unsupported,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ItemResult {
    pub item_id: String,
    pub action: String,
    pub readiness: Readiness,
    pub message: String,
    #[serde(default)]
    pub next_steps: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum RunState {
    Draft,
    Previewed,
    Applying,
    Completed,
    Partial,
    Interrupted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MigrationTarget {
    pub tool: SourceTool,
    pub project_root: PathBuf,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum OpenCodeConfigVersion {
    V1,
    V2,
    Unknown,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum VersionSupport {
    Supported,
    Unsupported,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TargetSessionRoots {
    pub codex: PathBuf,
    pub codex_home: PathBuf,
    pub claude: PathBuf,
    pub opencode: PathBuf,
    pub dsh: PathBuf,
    pub pi: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TargetEnvironment {
    pub target: MigrationTarget,
    pub roots: EnvironmentRoots,
    /// Explicit native session roots pinned together with the preview so a
    /// later environment change cannot redirect the applied sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_roots: Option<TargetSessionRoots>,
    pub opencode_version: OpenCodeConfigVersion,
    pub pi_mcp: VersionSupport,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dsh_profile: Option<String>,
    /// Trust is supplied by the caller after an explicit target-side check.
    /// Rendering never treats a newly written project file as trusted.
    #[serde(default)]
    pub project_trusted: bool,
    /// Explicit inputs let previews remain deterministic in tests and isolated
    /// workers. CLI/desktop callers fill these from their target-side probe.
    #[serde(default)]
    pub executable_search_path: Vec<PathBuf>,
    #[serde(default)]
    pub environment_variables: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TargetScope {
    Project,
    User,
    Profile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderedFileBody {
    Bytes(Vec<u8>),
    Copy {
        snapshot_path: PathBuf,
        content_hash: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ConfigEditFormat {
    Jsonc,
    Toml,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RenderContribution {
    WholeFile {
        body: RenderedFileBody,
        executable: bool,
    },
    ManagedBlock {
        marker: String,
        body: String,
    },
    ConfigValue {
        format: ConfigEditFormat,
        object_path: Vec<String>,
        key: String,
        value: serde_json::Value,
    },
    YamlPatch {
        marker: String,
        patch: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SourceConflict {
    pub id: String,
    pub target_path: PathBuf,
    pub kind: String,
    pub item_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderedFile {
    pub target_scope: TargetScope,
    pub scope_root: PathBuf,
    pub target_path: PathBuf,
    pub source_item_ids: Vec<String>,
    pub content: RenderedFileBody,
    pub contributions: Vec<(String, RenderContribution)>,
    pub executable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RenderedItem {
    pub item_id: String,
    pub kind: AssetKind,
    pub target_scope: TargetScope,
    pub format: String,
    pub readiness: Readiness,
    pub action: String,
    pub message: String,
    #[serde(default)]
    pub next_steps: Vec<String>,
    #[serde(default)]
    pub evidence: Vec<String>,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
    #[serde(default)]
    pub target_paths: Vec<PathBuf>,
}

impl RenderedItem {
    pub fn result(&self) -> ItemResult {
        ItemResult {
            item_id: self.item_id.clone(),
            action: self.action.clone(),
            readiness: self.readiness,
            message: self.message.clone(),
            next_steps: self.next_steps.clone(),
            evidence: self.evidence.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RenderPlan {
    pub target: MigrationTarget,
    pub files: Vec<RenderedFile>,
    pub items: Vec<RenderedItem>,
    pub conflicts: Vec<SourceConflict>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewDecision {
    pub decision_id: String,
    pub item_id: String,
    pub choice: ConflictChoice,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_value: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewConflictItem {
    pub decision_id: String,
    pub item_id: String,
    pub contribution_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_hash: Option<String>,
    pub proposed_hash: String,
    pub diff: super::diff::ContentDiff,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewConflict {
    pub id: String,
    pub kind: String,
    pub target_path: PathBuf,
    pub item_ids: Vec<String>,
    pub message: String,
    #[serde(default)]
    pub items: Vec<PreviewConflictItem>,
    pub resolved: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewFile {
    pub target_scope: TargetScope,
    pub scope_root: PathBuf,
    pub target_path: PathBuf,
    pub private_path: PathBuf,
    pub item_ids: Vec<String>,
    pub target_existed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_hash: Option<String>,
    pub output_hash: String,
    pub output_size: u64,
    pub executable: bool,
    pub changed: bool,
    pub diff: super::diff::ContentDiff,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewSummary {
    pub id: String,
    pub run_revision: u64,
    pub created_at: String,
    pub file_count: usize,
    pub unresolved_conflicts: usize,
    pub ready_to_apply: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewRevision {
    pub schema_version: u32,
    pub id: String,
    pub run_id: String,
    pub run_revision: u64,
    pub created_at: String,
    pub target: MigrationTarget,
    pub environment: TargetEnvironment,
    pub decisions: Vec<PreviewDecision>,
    pub conflicts: Vec<PreviewConflict>,
    pub items: Vec<RenderedItem>,
    pub files: Vec<PreviewFile>,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
    pub ready_to_apply: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewPublicFile {
    pub target_scope: TargetScope,
    pub scope_root: PathBuf,
    pub target_path: PathBuf,
    pub item_ids: Vec<String>,
    pub target_existed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_hash: Option<String>,
    pub output_hash: String,
    pub output_size: u64,
    pub executable: bool,
    pub changed: bool,
    pub diff: super::diff::ContentDiff,
}

/// Client-facing preview without private staging paths. The execution step
/// only accepts the preview id, so exposing these paths could not authorize a
/// write and only invites clients to construct their own target ranges.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PreviewPublic {
    pub id: String,
    pub run_id: String,
    pub run_revision: u64,
    pub created_at: String,
    pub target: MigrationTarget,
    pub environment: TargetEnvironment,
    pub decisions: Vec<PreviewDecision>,
    pub conflicts: Vec<PreviewConflict>,
    pub items: Vec<RenderedItem>,
    pub files: Vec<PreviewPublicFile>,
    pub diagnostics: Vec<Diagnostic>,
    pub ready_to_apply: bool,
}

impl PreviewRevision {
    pub fn public(&self) -> PreviewPublic {
        PreviewPublic {
            id: self.id.clone(),
            run_id: self.run_id.clone(),
            run_revision: self.run_revision,
            created_at: self.created_at.clone(),
            target: self.target.clone(),
            environment: self.environment.clone(),
            decisions: self.decisions.clone(),
            conflicts: self.conflicts.clone(),
            items: self.items.clone(),
            files: self
                .files
                .iter()
                .map(|file| PreviewPublicFile {
                    target_scope: file.target_scope,
                    scope_root: file.scope_root.clone(),
                    target_path: file.target_path.clone(),
                    item_ids: file.item_ids.clone(),
                    target_existed: file.target_existed,
                    target_hash: file.target_hash.clone(),
                    output_hash: file.output_hash.clone(),
                    output_size: file.output_size,
                    executable: file.executable,
                    changed: file.changed,
                    diff: file.diff.clone(),
                })
                .collect(),
            diagnostics: self.diagnostics.clone(),
            ready_to_apply: self.ready_to_apply,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SessionApplyResult {
    pub source_session_id: String,
    pub target_session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_path: Option<PathBuf>,
    pub action: String,
    pub readiness: Readiness,
    pub message: String,
    #[serde(default)]
    pub next_steps: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApplyReport {
    pub run_id: String,
    pub preview_id: String,
    pub state: RunState,
    pub items: Vec<ItemResult>,
    pub sessions: Vec<SessionApplyResult>,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

impl ApplyReport {
    pub fn is_ok(&self) -> bool {
        self.state == RunState::Completed
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RenderReport {
    pub target: MigrationTarget,
    pub items: Vec<RenderedItem>,
    pub conflicts: Vec<SourceConflict>,
    pub diagnostics: Vec<Diagnostic>,
}

impl RenderPlan {
    /// A report suitable for a client because it excludes generated and
    /// preserved configuration bytes. Existing target credentials therefore
    /// cannot leak through ordinary preview metadata.
    pub fn report(&self) -> RenderReport {
        RenderReport {
            target: self.target.clone(),
            items: self.items.clone(),
            conflicts: self.conflicts.clone(),
            diagnostics: self.diagnostics.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MigrationRun {
    pub schema_version: u32,
    pub id: String,
    pub revision: u64,
    pub created_at: String,
    pub updated_at: String,
    pub state: RunState,
    pub project: ProjectIdentity,
    pub session_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<MigrationTarget>,
    #[serde(default)]
    pub discovered_assets: Vec<EnvironmentAsset>,
    #[serde(default)]
    pub discovery_probes: Vec<SourceProbe>,
    #[serde(default)]
    pub sessions: Vec<SessionSnapshot>,
    #[serde(default)]
    pub assets: Vec<AssetSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<PreviewSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apply_report: Option<ApplyReport>,
    #[serde(default)]
    pub results: Vec<ItemResult>,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

impl MigrationRun {
    pub fn new(project: ProjectIdentity, session_ids: Vec<String>) -> Result<Self, String> {
        if session_ids.is_empty() {
            return Err("select at least one native session".to_string());
        }
        let now = Utc::now().to_rfc3339();
        let run = Self {
            schema_version: MIGRATION_SCHEMA_VERSION,
            id: Uuid::new_v4().to_string(),
            revision: 0,
            created_at: now.clone(),
            updated_at: now,
            state: RunState::Draft,
            project,
            session_ids,
            target: None,
            discovered_assets: vec![],
            discovery_probes: vec![],
            sessions: vec![],
            assets: vec![],
            preview: None,
            apply_report: None,
            results: vec![],
            diagnostics: vec![],
        };
        run.validate()?;
        Ok(run)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != MIGRATION_SCHEMA_VERSION {
            return Err(format!(
                "unsupported migration schema version: {}",
                self.schema_version
            ));
        }
        let id = Uuid::parse_str(&self.id).map_err(|_| "invalid migration id".to_string())?;
        if id.to_string() != self.id {
            return Err("invalid migration id".to_string());
        }
        if self.session_ids.is_empty() {
            return Err("select at least one native session".to_string());
        }
        let unique: std::collections::HashSet<_> = self.session_ids.iter().collect();
        if unique.len() != self.session_ids.len()
            || self.session_ids.iter().any(|id| id.trim().is_empty())
        {
            return Err("invalid or duplicate selected session id".to_string());
        }
        for asset in &self.assets {
            for file in &asset.files {
                validate_resource_path(&file.relative_path)?;
                validate_resource_path(&file.snapshot_path)?;
            }
        }
        for session in &self.sessions {
            validate_resource_path(&session.snapshot_path)?;
        }
        Ok(())
    }
}
