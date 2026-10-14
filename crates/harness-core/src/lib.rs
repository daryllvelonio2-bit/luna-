pub mod agent;
pub mod config;
pub mod error;
pub mod registry;
pub mod traits;
pub mod types;

pub use agent::{Agent, AgentConfig};
pub use config::{
    resolve, Config, Layers, ModelSettings, PathsSettings, ProviderOverride, Resolved, Source,
};
pub use error::HarnessError;
pub use registry::DefaultToolRegistry;
pub use traits::{
    EvolverEngine, MemoryEngine, ModelProvider, NotesEngine, Tool, ToolRegistry,
};
pub use types::*;
