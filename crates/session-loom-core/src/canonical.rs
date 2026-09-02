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
    /// Source tool's own session title when the tool maintains one (for
    /// example Claude's summary lines or OpenCode's generated title). The
    /// mirror refreshes it together with the content on every re-mirror.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub messages: Vec<Message>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PortabilityReport {
    pub source_tool: SourceTool,
    pub target_tool: SourceTool,
    pub preserved: Vec<String>,
    pub degraded: Vec<String>,
}

impl PortabilityReport {
    pub fn is_degraded(&self) -> bool {
        !self.degraded.is_empty()
    }

    pub fn summary(&self) -> String {
        if self.source_tool == self.target_tool {
            return String::new();
        }

        let preserved = self.preserved.join("、");
        if self.degraded.is_empty() {
            return format!("可迁移内容：{preserved}");
        }
        format!(
            "可迁移内容：{preserved}；降级项：{}",
            self.degraded.join("；")
        )
    }
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

    pub fn portability_report(&self, target_tool: SourceTool) -> PortabilityReport {
        let cross_tool = self.source_tool != target_tool;
        let has_tool_calls = self
            .messages
            .iter()
            .any(|message| !message.tool_calls.is_empty());
        let mut preserved = vec!["用户和助手文本".to_string(), "工作目录和时间戳".to_string()];
        let mut degraded = vec![];

        if has_tool_calls {
            preserved.push("工具名称、参数和已捕获结果".to_string());
        }

        if cross_tool && has_tool_calls {
            degraded.push("历史工具调用只作为上下文保留，不会在目标工具中重新执行".to_string());
        }
        if cross_tool && self.source_tool == SourceTool::Codex {
            degraded.push("Codex 的内部推理、压缩、审批和 MCP 运行状态不会跨工具复制".to_string());
        }
        if cross_tool && (self.model_provider.is_some() || self.model.is_some()) {
            match target_tool {
                SourceTool::Codex => degraded.push(
                    "目标 Codex 会优先按自己的配置选择 provider，模型也可能被重新选择".to_string(),
                ),
                SourceTool::Claude | SourceTool::OpenCode | SourceTool::Dsh => {
                    degraded.push("目标工具不会继承源工具的模型和 provider 设置".to_string())
                }
                SourceTool::Pi => {}
            }
        }

        PortabilityReport {
            source_tool: self.source_tool,
            target_tool,
            preserved,
            degraded,
        }
    }
}
