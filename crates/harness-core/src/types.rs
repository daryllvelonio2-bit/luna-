use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// What a provider hands back when the model spent its whole answer budget on reasoning
/// and wrote no text.
///
/// It is a marker, not content. A caller that needs real text — a compaction summary, say
/// — must not mistake it for an answer, and must not write it into the transcript as one.
pub const NO_TEXT_RESPONSE: &str = "(Completed reasoning with no text response)";

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
    /// Provider-issued opaque signature attached to this call (Gemini/Code Assist
    /// `thoughtSignature`). Captured verbatim from the response and replayed verbatim on the
    /// next request — Code Assist validates it against the model that issued it, so a call
    /// without one is sent without the field rather than with a fabricated value. `None` for
    /// every provider that has no such concept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
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
    /// How many of `prompt_tokens` the provider served from its prompt cache and billed at the
    /// cache-read rate. Zero when the provider reports no cache — which is not the same as "no
    /// cache was used", only as "nothing was said about it".
    ///
    /// Always a subset of `prompt_tokens`, whichever shape the wire used: a route that reports the
    /// cache read beside the input is normalized through [`TokenUsage::with_separate_cache`] before
    /// this struct is built.
    ///
    /// `#[serde(default)]` because this field postdates stored sessions and ledger rows: a
    /// transcript written before it existed must still load.
    #[serde(default)]
    pub cached_tokens: u32,
}

impl TokenUsage {
    /// A usage figure this crate computed itself (message text ÷ 4) instead of one a provider
    /// reported. No cache split: an estimate derived from characters knows nothing about which
    /// prefix the provider had already cached, and claiming it did would put a made-up number in
    /// the cost ledger.
    pub fn estimated(prompt_tokens: u32, completion_tokens: u32) -> Self {
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: prompt_tokens.saturating_add(completion_tokens),
            cached_tokens: 0,
        }
    }

    /// Build a report from a route that puts the cache read *beside* the prompt count.
    ///
    /// OpenCode (`tokens.input` plus `tokens.cache.read`) and Anthropic-shaped payloads
    /// (`cache_read_input_tokens`) list the cached tokens on top of the input, not inside it.
    /// `TokenUsage` promises `cached_tokens` is a subset of `prompt_tokens`, so the two are folded
    /// together here — once, at the edge — and cost, cache share and the context threshold all read
    /// the same shape instead of each guessing which provider wrote the row.
    pub fn with_separate_cache(
        prompt_tokens: u32,
        completion_tokens: u32,
        total_tokens: u32,
        cached_tokens: u32,
    ) -> Self {
        let prompt_tokens = prompt_tokens.saturating_add(cached_tokens);
        Self {
            prompt_tokens,
            completion_tokens,
            total_tokens: total_tokens.max(prompt_tokens.saturating_add(completion_tokens)),
            cached_tokens,
        }
    }
}

/// One increment of a streaming completion. Reasoning is a separate channel because providers
/// interleave it with (or emit it entirely before) the visible answer, and the UI renders the
/// two differently — reasoning dimmed, the answer normally.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StreamEvent {
    ContentDelta(String),
    ReasoningDelta(String),
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments_delta: Option<String>,
        /// The provider's signature for this call, when the wire format carries one.
        signature: Option<String>,
    },
    ToolExecuting {
        name: String,
        arguments: serde_json::Value,
    },
    ToolExecuted {
        name: String,
        output: String,
        is_error: bool,
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
}

impl std::fmt::Display for MemoryCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UserProfile => write!(f, "user_profile"),
            Self::CodePattern => write!(f, "code_pattern"),
            Self::WorkflowRule => write!(f, "workflow_rule"),
            Self::Fact => write!(f, "fact"),
            Self::NotePointer => write!(f, "note_pointer"),
        }
    }
}

/// One memory record: the index card of a single topic.
///
/// A record is *lean on purpose*. The agent reads every active record on every turn, so a record
/// that carries a paragraph costs a paragraph of tokens on every request. The three fields that
/// matter are therefore kept apart and short:
///
/// - `subject` — the topic key (`project.astra`). One active record per subject: the engine
///   supersedes the previous one, so a topic never fragments into several records.
/// - `object` — what the topic is about, in a handful of words.
/// - `name` / `path` — the display name and where it lives on disk.
///
/// Everything longer than that belongs in a note, which is read on demand instead of every turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryRecord {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    /// What the topic is about — a short phrase, not a paragraph.
    pub object: String,
    /// Display name of the topic (e.g. `Astra`).
    #[serde(default)]
    pub name: Option<String>,
    /// Where the topic lives on disk (repo root, file, directory).
    #[serde(default)]
    pub path: Option<String>,
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
    pub title: String,
    pub id: String,
    pub content: String,
    pub summary_pointer: String,
    pub tags: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarifyItem {
    pub question: String,
    #[serde(default)]
    pub choices: Vec<String>,
}

/// An incoming clarify request passed to the UI handler.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarifyRequest {
    pub questions: Vec<ClarifyItem>,
}

/// The user's answer to a single question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarifyAnswer {
    pub question: String,
    pub answer: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choice_index: Option<usize>,
}

/// Response returned to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarifyResponse {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choice_index: Option<usize>,
    pub answers: Vec<ClarifyAnswer>,
}
