use thiserror::Error;

#[derive(Error, Debug, Clone)]
pub enum HarnessError {
    #[error("Authentication failed for provider '{provider}': {message}")]
    AuthenticationFailed {
        provider: String,
        message: String,
    },

    #[error("Adaptive failure detector triggered: {failures} consecutive non-working tool calls. Last failing tool: '{last_tool}'")]
    ConsecutiveFailuresExceeded {
        failures: usize,
        last_tool: String,
    },

    #[error("Rate limited by provider '{provider}'. Retry after: {retry_after_secs:?} seconds")]
    RateLimited {
        provider: String,
        retry_after_secs: Option<u64>,
    },

    #[error("Tool '{tool}' execution failed: {reason}")]
    ToolExecutionFailed {
        tool: String,
        reason: String,
    },

    #[error("Tool '{0}' not found in registry")]
    ToolNotFound(String),

    #[error("Operation was cooperatively cancelled")]
    Cancelled,

    #[error("Provider '{provider}' error: {message}")]
    ProviderError {
        provider: String,
        message: String,
    },

    #[error("Memory engine error: {0}")]
    MemoryError(String),

    #[error("Notes engine error: {0}")]
    NotesError(String),

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
