//! OpenCode provider: native daemon HTTP/SSE transport, CLI transport, and shared helpers.
//!
//! Each concern has its own submodule; this facade re-exports the public surface so that
//! `harness_providers::opencode::*` keeps resolving to the same names as before the split.

mod binary;
mod cli;
mod credentials;
mod form;
mod ids;
mod models;
mod native;
mod native_complete;
mod prompt;
mod service;
mod tools;
mod ui;
mod usage;

#[cfg(test)]
mod tests;

pub use binary::find_opencode_binary;
pub use cli::{execute_opencode_completion_cli, stream_opencode_completion_cli};
pub use credentials::{
    detect_opencode_credentials, detect_opencode_credentials_in, DetectedOpencodeCredential,
};
pub use ids::{
    get_or_create_opencode_session_id, new_opencode_request_id, new_opencode_session_id,
};
pub use models::{
    fetch_opencode_cli_models, fetch_opencode_models, normalize_opencode_model,
    parse_opencode_model_ref, OPENCODE_FREE_MODELS,
};
pub use native::native_opencode_stream;
pub use native_complete::{
    execute_opencode_completion, native_opencode_complete, stream_opencode_completion,
};
pub use prompt::{format_messages_for_opencode, format_opencode_prompt, OpencodePrompt};
pub use service::{
    get_http_client, get_or_start_opencode_service, read_opencode_service_info, OpencodeServiceInfo,
};
pub use tools::{ensure_compat_tools, normalize_tool_arguments};
pub use ui::{get_ui_bridge, set_ui_bridge, OpencodeUiBridge};
