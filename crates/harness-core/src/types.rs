use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub call_id: String,
    pub content: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            content: content.into(),
            is_error: false,
        }
    }

    pub fn error(call_id: impl Into<String>, error_message: impl Into<String>) -> Self {
        Self {
            call_id: call_id.into(),
            content: error_message.into(),
            is_error: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default)]
    pub tool_result: Option<ToolResult>,
    /// Provider-reported usage for the request that produced this message (the response's `usage`
    /// object, or `StreamEvent::Completed.usage` when streaming). `None` for messages no provider
    /// produced, and for providers that do not report it. It is what lets
    /// [`crate::session::estimate_tokens`] measure the transcript the provider actually billed
    /// instead of guessing from characters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: Role::System,
            content: Some(content.into()),
            reasoning: None,
            tool_calls: vec![],
            tool_result: None,
            usage: None,
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: Some(content.into()),
            reasoning: None,
            tool_calls: vec![],
            tool_result: None,
            usage: None,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: Role::Assistant,
            content: Some(content.into()),
            reasoning: None,
            tool_calls: vec![],
            tool_result: None,
            usage: None,
        }
    }

    pub fn assistant_with_tools(tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: Role::Assistant,
            content: None,
            reasoning: None,
            tool_calls,
            tool_result: None,
            usage: None,
        }
    }

    pub fn tool(result: ToolResult) -> Self {
        Self {
            role: Role::Tool,
            content: Some(result.content.clone()),
            reasoning: None,
            tool_calls: vec![],
            tool_result: Some(result),
            usage: None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TokenUsage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StreamEvent {
    ContentDelta(String),
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: Option<String>,
    },
    Completed {
        finish_reason: Option<String>,
        usage: Option<TokenUsage>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ReasoningEffort {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    pub fn from_str_loose(s: &str) -> Option<Self> {
        match s.trim().to_lowercase().as_str() {
            "minimal" | "min" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" | "med" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" | "extra-high" | "extra_high" => Some(Self::Xhigh),
            "max" | "maximum" => Some(Self::Max),
            _ => None,
        }
    }
}

impl std::fmt::Display for ReasoningEffort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CompletionOptions {
    pub model: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub top_p: Option<f32>,
    pub stop: Vec<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl Default for CompletionOptions {
    fn default() -> Self {
        Self {
            model: None,
            temperature: Some(0.7),
            max_tokens: Some(4096),
            top_p: Some(1.0),
            stop: vec![],
            reasoning_effort: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCategory {
    UserProfile,
    CodePattern,
    WorkflowRule,
    Fact,
    NotePointer,
    HarnessImprovement,
}

impl std::fmt::Display for MemoryCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UserProfile => write!(f, "user_profile"),
            Self::CodePattern => write!(f, "code_pattern"),
            Self::WorkflowRule => write!(f, "workflow_rule"),
            Self::Fact => write!(f, "fact"),
            Self::NotePointer => write!(f, "note_pointer"),
            Self::HarnessImprovement => write!(f, "harness_improvement"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub raw_content: String,
    pub category: MemoryCategory,
    pub confidence: f32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub is_active: bool,
    pub superseded_by: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub title: String,
    pub content: String,
    pub summary_pointer: String,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
