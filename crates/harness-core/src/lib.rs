pub mod agent;
pub mod approval;
pub mod config;
pub mod error;
pub mod prompts;
pub mod registry;
pub mod session;
pub mod tasks;
pub mod tools;
pub mod traits;
pub mod types;

pub use agent::{
    Agent, AgentConfig, AgentEvent, AgentObserver, CompactionNotice, CompactionSetup, ReplayNotice,
    SessionBinding, TurnOutcome, TurnToolCall,
};
pub use approval::{
    unified_diff, AllowAll, ApprovalDecision, ApprovalMode, ApprovalRequest, ApprovalSettings,
    Approver, DenyAll, FnApprover, NonInteractive, ToolPreview, DEFAULT_APPROVAL_TIMEOUT_SECS,
};
pub use config::{
    resolve, Config, Layers, ModelSettings, PathsSettings, ProviderOverride, Resolved,
    SessionSettings, Source, ToolsSettings,
};
pub use error::HarnessError;
pub use registry::DefaultToolRegistry;
pub use tasks::BackgroundTaskQueue;
pub use session::{
    compact, estimate_tokens, fold_boundary, heuristic_tokens, summarize_session, CompactionOutcome, Estimate,
    EstimateSource, CHARS_PER_TOKEN, HEURISTIC_TOLERANCE, ReplayOutcome, SessionRecord,
    SessionStore, SqliteSessionStore, StoredMessage,
};
pub use traits::{
    EvolverEngine, MemoryEngine, ModelProvider, NotesEngine, Tool, ToolRegistry,
};
pub use types::*;
