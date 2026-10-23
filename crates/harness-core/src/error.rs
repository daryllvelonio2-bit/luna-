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

    #[error("Evolver self-improvement error: {0}")]
    EvolverError(String),

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

    #[error("Surface validation error: {0}")]
    SurfaceValidationError(String),

    #[error("Ledger budget error: {0}")]
    LedgerError(String),
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
