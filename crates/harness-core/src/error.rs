use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum HarnessError {
    #[error("Authentication failed for provider '{provider}': {message}")]
    AuthenticationFailed { provider: String, message: String },

    #[error("Adaptive failure detector triggered: {failures} consecutive non-working tool calls. Last failing tool: '{last_tool}'")]
    ConsecutiveFailuresExceeded { failures: usize, last_tool: String },

    #[error("Rate limited by provider '{provider}'. Retry after: {retry_after_secs:?} seconds")]
    RateLimited {
        provider: String,
        retry_after_secs: Option<u64>,
    },

    #[error("Tool '{tool}' execution failed: {reason}")]
    ToolExecutionFailed { tool: String, reason: String },

    #[error("Tool '{0}' not found in registry")]
    ToolNotFound(String),

    #[error("Operation was cooperatively cancelled")]
    Cancelled,

    #[error("Provider '{provider}' error: {message}")]
    ProviderError { provider: String, message: String },

    #[error("Memory engine error: {0}")]
    MemoryError(String),

    #[error("Notes engine error: {0}")]
    NotesError(String),

    #[error("Session store error: {0}")]
    SessionError(String),

    #[error("Plugin/Wasm execution error: {0}")]
    PluginError(String),

    #[error("Voice pipeline error: {0}")]
    VoiceError(String),

    #[error("Configuration error: {0}")]
    ConfigurationError(String),

    #[error("Serialization error: {0}")]
    SerializationError(String),

    #[error("I/O error: {0}")]
    Io(String),

    #[error("Human approval rejected: {0}")]
    ApprovalRejected(String),

    #[error("Strict mode failure: {0}")]
    StrictFailure(String),
}

impl From<std::io::Error> for HarnessError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.to_string())
    }
}

impl From<serde_json::Error> for HarnessError {
    fn from(err: serde_json::Error) -> Self {
        Self::SerializationError(err.to_string())
    }
}

impl HarnessError {
    /// The error for an answer that never started because the model spent the whole output budget.
    ///
    /// On routes where thinking is charged to the same `max_tokens` allowance (Gemini, and any
    /// reasoning model behind an OpenAI-compatible endpoint), a hard turn can spend every token in
    /// its head and return `finishReason=MAX_TOKENS` with no text, no tool call and — when thoughts
    /// are not echoed — no reasoning either. That body is indistinguishable from a transient empty
    /// response, so the turn loop retried it until the budget ran out. Naming the condition is what
    /// makes the second attempt unnecessary: the same request with the same cap will do it again.
    pub fn output_limit_exhausted(provider: &str, max_tokens: Option<u32>) -> Self {
        Self::ProviderError {
            provider: provider.to_string(),
            message: format!(
                "The model hit its output-token limit before writing an answer{}. Thinking counts \
                 against the same budget on this route, so retrying the same request will do it \
                 again: raise max_tokens or lower the reasoning effort.",
                max_tokens
                    .map(|tokens| format!(" (max_tokens={tokens})"))
                    .unwrap_or_default()
            ),
        }
    }

    /// Returns true if this error represents a rate limit, quota exhaustion, or overloaded model/server.
    pub fn is_overloaded(&self) -> bool {
        match self {
            Self::RateLimited { .. } => true,
            Self::ProviderError { message, .. } => {
                let lower = message.to_lowercase();
                lower.contains("overloaded")
                    || lower.contains("rate limit")
                    || lower.contains("too many requests")
                    || lower.contains("resource_exhausted")
                    || lower.contains("resource has been exhausted")
                    || lower.contains("quota")
                    || lower.contains("503")
                    || lower.contains("429")
                    || lower.contains("server_error")
                    || lower.contains("service unavailable")
                    || lower.contains("temporarily unavailable")
            }
            _ => false,
        }
    }

    /// Returns true if this error is transient and can be retried with exponential backoff.
    pub fn is_retryable(&self) -> bool {
        if self.is_overloaded() {
            return true;
        }
        match self {
            Self::ProviderError { message, .. } => {
                let lower = message.to_lowercase();
                lower.contains("timeout")
                    || lower.contains("connection reset")
                    || lower.contains("connection closed")
                    || lower.contains("broken pipe")
                    || lower.contains("empty response")
                    || lower.contains("returned no answer")
                    || lower.contains("sse stream error")
                    || lower.contains("transport error")
                    || lower.contains("error decoding response body")
                    || lower.contains("incomplete message")
                    || lower.contains("unexpected eof")
                    || lower.contains("client error (connect)")
                    || lower.contains("network error")
                    || lower.contains("stream error")
                    || lower.contains("502")
                    || lower.contains("504")
                    || lower.contains("500")
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HarnessError;

    /// An empty answer is a transient shape of a request that got nothing back, so the turn loop
    /// may retry it. The provider's message changed once and silently lost this property.
    #[test]
    fn an_empty_answer_is_retryable() {
        let empty = HarnessError::ProviderError {
            provider: "antigravity".to_string(),
            message: "Code Assist returned no answer: the stream ended after 0s without text, \
                      tool calls or reasoning"
                .to_string(),
        };
        assert!(empty.is_retryable(), "{empty}");
    }

    /// An answer that never started because thinking ate the output budget is not transient: the
    /// same request with the same cap produces the same nothing.
    #[test]
    fn an_exhausted_output_budget_is_not_retryable() {
        let exhausted = HarnessError::output_limit_exhausted("antigravity", Some(4096));
        assert!(!exhausted.is_retryable(), "{exhausted}");
        assert!(exhausted.to_string().contains("max_tokens=4096"));
        assert!(
            exhausted.to_string().contains("output-token limit"),
            "{exhausted}"
        );
    }
}
