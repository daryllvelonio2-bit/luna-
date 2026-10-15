pub mod agent;
pub mod approval;
pub mod config;
pub mod error;
pub mod registry;
pub mod traits;
pub mod types;

pub use agent::{Agent, AgentConfig};
pub use approval::{
    unified_diff, AllowAll, ApprovalDecision, ApprovalMode, ApprovalRequest, ApprovalSettings,
    Approver, DenyAll, FnApprover, NonInteractive, ToolPreview, DEFAULT_APPROVAL_TIMEOUT_SECS,
};
pub use config::{
    resolve, Config, Layers, ModelSettings, PathsSettings, ProviderOverride, Resolved, Source,
    ToolsSettings,
};
pub use error::HarnessError;
pub use registry::DefaultToolRegistry;
pub use traits::{
    EvolverEngine, MemoryEngine, ModelProvider, NotesEngine, Tool, ToolRegistry,
};
pub use types::*;
