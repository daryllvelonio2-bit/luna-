//! Background memory saver wiring shared across CLI and GUI.

use std::sync::Arc;

use harness_core::config::Config;
use harness_core::{HarnessError, MemoryEngine, NotesEngine};
use harness_memory::{MemorySaver, SaverPolicy};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};

#[allow(clippy::too_many_arguments)]
pub fn build_saver(
    config: &Config,
    memory: &Arc<dyn MemoryEngine>,
    notes: Arc<dyn NotesEngine>,
    session_provider: &str,
    session_model: &str,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
) -> Option<Arc<MemorySaver>> {
    if !config.memory.mode.is_background() {
        return None;
    }
    let provider_id = config
        .memory
        .background_provider
        .clone()
        .unwrap_or_else(|| session_provider.to_string());
    let model = config
        .memory
        .background_model
        .clone()
        .unwrap_or_else(|| session_model.to_string());
    match build_saver_for(
        config,
        memory,
        notes,
        &provider_id,
        &model,
        cred_manager,
        discovery,
    ) {
        Ok(saver) => Some(Arc::new(saver)),
        Err(error) => {
            tracing::warn!(
                "the background memory saver is off (provider '{}', model '{}'): {}",
                provider_id,
                model,
                error
            );
            None
        }
    }
}

pub fn build_saver_for(
    config: &Config,
    memory: &Arc<dyn MemoryEngine>,
    notes: Arc<dyn NotesEngine>,
    provider_id: &str,
    model: &str,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
) -> Result<MemorySaver, HarnessError> {
    let client = GenericProviderClient::new(
        provider_id.to_string(),
        model.to_string(),
        cred_manager,
        discovery,
        config.providers.get(provider_id),
    )?;

    let mut policy = SaverPolicy::new(model.to_string());
    policy.every_turns = config.memory.background_every_turns;
    policy.tail_messages = config.memory.background_tail_messages;
    policy.notes = config.memory.background_notes;
    policy.two_pass = config.memory.background_two_pass;
    policy.min_confidence = config.memory.background_min_confidence;
    policy.max_memory_writes = config.memory.background_max_memory_writes;
    policy.max_note_writes = config.memory.background_max_note_writes;
    policy.max_operations = config.memory.background_max_operations;
    Ok(MemorySaver::new(
        Arc::new(client),
        memory.clone(),
        notes,
        policy,
    ))
}

pub fn notice(config: &Config) -> Option<String> {
    if !config.memory.mode.is_background() {
        return None;
    }
    Some(format!(
        "memory: background saver on ({}), after every {} turn(s) — memories land after each reply",
        config
            .memory
            .background_model
            .clone()
            .unwrap_or_else(|| config.model.default.clone()),
        config.memory.background_every_turns
    ))
}
