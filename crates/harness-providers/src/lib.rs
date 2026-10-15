pub mod auth;
pub mod catalog;
pub mod client;
pub mod discovery;

pub use auth::{CredentialEntry, CredentialManager, RotationStrategy};
pub use catalog::{
    find_provider, get_provider_catalog, known_provider_ids, resolve_descriptor, AuthHeaderFormat,
    ProviderDescriptor, DEFAULT_TIMEOUT_SECS,
};
pub use client::GenericProviderClient;
pub use discovery::ModelDiscovery;
