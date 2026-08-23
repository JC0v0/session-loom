use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CANONICAL_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SourceTool {
    Codex,
    Claude,
    OpenCode,
    Dsh,
    Pi,
}

impl SourceTool {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::OpenCode => "opencode",
            Self::Dsh => "dsh",
            Self::Pi => "pi",
        }
    }
}

impl std::str::FromStr for SourceTool {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "codex" => Ok(Self::Codex),
            "claude" => Ok(Self::Claude),
            "opencode" => Ok(Self::OpenCode),
            "dsh" => Ok(Self::Dsh),
            "pi" => Ok(Self::Pi),
            _ => Err(format!("unknown source tool: {value}")),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub role: Role,
    pub text: String,
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CanonicalSession {
    pub schema_version: u32,
    pub source_tool: SourceTool,
    pub session_id: String,
    pub cwd: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    pub messages: Vec<Message>,
}

impl CanonicalSession {
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string_pretty(self).map_err(|error| error.to_string())
    }

    /// Compact representation used for durable storage. Human-readable
    /// exports continue to use [`Self::to_json`].
    pub fn to_storage_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| error.to_string())
    }

    pub fn from_json(input: &str) -> Result<Self, String> {
        let session = serde_json::from_str::<Self>(input).map_err(|error| error.to_string())?;
        if session.schema_version != CANONICAL_SCHEMA_VERSION {
            return Err(format!(
                "Unsupported canonical schema version: {}",
                session.schema_version
            ));
        }
        Ok(session)
    }
}
