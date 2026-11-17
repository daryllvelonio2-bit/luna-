//! Headless application runtime for Luna.
//!
//! Extracts session wiring, turn execution, approval/clarify channels, feature toggles,
//! MCP registration, usage accounting, and structured event streaming out of `harness-cli`
//! so both the CLI/TUI and the optional native desktop GUI execute against one shared engine.

pub mod approval;
pub mod clarify;
pub mod context_window;
pub mod events;
pub mod features;
pub mod mcp;
pub mod memory_writer;
pub mod prompts;
pub mod runtime;
pub mod session;
pub mod usage;

pub use approval::{approval_settings, ChannelApprover};
pub use clarify::{ClarifyAnswer, ClarifyItem, ClarifyRequest, ClarifyResponse, ClarifyTool};
pub use context_window::{context_window, context_window_for};
pub use events::{AppEvent, AppEventSender};
pub use features::{Feature, FeatureToggles};
pub use mcp::{ensure_mcp_servers_registered, register_mcp_servers};
pub use prompts::ChannelSudoPrompter;
pub use runtime::{AppAgent, AppRuntime, AppSessionChannels, AppSessionOptions};
pub use session::{create_session, find_by_name, open_store, resolve_session, SessionSelection};
pub use usage::UsageRecorder;
