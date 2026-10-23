pub mod antigravity;
pub mod auth;
pub mod catalog;
pub mod client;
pub mod discovery;
pub mod kiro;
pub mod opencode;
pub mod supercode;
pub mod thought_signature;

pub use antigravity::{
    detect_antigravity_credentials, execute_antigravity_completion, extract_bearer_token,
    fetch_antigravity_models, refresh_antigravity_token, resolve_antigravity_email_strict,
    retrieve_antigravity_quota, AntigravityCredentials, AntigravityModelQuotaInfo,
    AntigravityQuotaBucket, AntigravityQuotaGroup, AntigravityQuotaSummary,
};
pub use auth::{CredentialEntry, CredentialManager, RotationStrategy};
pub use catalog::{
    find_provider, get_context_window, get_default_model, get_provider_catalog, known_provider_ids,
    resolve_descriptor, AuthHeaderFormat, ProviderDescriptor, DEFAULT_TIMEOUT_SECS,
};
pub use client::{build_http_client, GenericProviderClient};
pub use discovery::ModelDiscovery;
pub use opencode::{
    detect_opencode_credentials, ensure_compat_tools, get_or_create_opencode_session_id,
    new_opencode_session_id, DetectedOpencodeCredential,
};
