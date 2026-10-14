pub mod auth;
pub mod catalog;
pub mod client;
pub mod discovery;

pub use auth::{CredentialEntry, CredentialManager, RotationStrategy};
pub use catalog::{find_provider, get_provider_catalog, AuthHeaderFormat, ProviderDescriptor};
pub use client::GenericProviderClient;
pub use discovery::ModelDiscovery;
