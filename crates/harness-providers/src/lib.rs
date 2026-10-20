pub mod antigravity;
pub mod auth;
pub mod catalog;
pub mod client;
pub mod discovery;
pub mod opencode;

pub use antigravity::{
    detect_antigravity_credentials, execute_antigravity_completion, extract_bearer_token,
    fetch_antigravity_models, refresh_antigravity_token, AntigravityCredentials,
};
pub use opencode::{
    detect_opencode_credentials, ensure_compat_tools, get_or_create_opencode_session_id,
    new_opencode_session_id, DetectedOpencodeCredential,
};
pub use auth::{CredentialEntry, CredentialManager, RotationStrategy};
pub use catalog::{
    find_provider, get_default_model, get_provider_catalog, get_context_window, known_provider_ids, resolve_descriptor,
    AuthHeaderFormat, ProviderDescriptor, DEFAULT_TIMEOUT_SECS,
};
pub use client::GenericProviderClient;
pub use discovery::ModelDiscovery;
