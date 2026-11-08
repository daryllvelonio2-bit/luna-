//! Which writer owns durable memory, and how the CLI builds it.
//!
//! `memory.mode` has two values and both are real code paths:
//!
//! - `session` (the default) leaves the agent as the writer: it calls `save_memory` / `save_note`
//!   mid-turn, and the CLI registers those tools.
//! - `background` attaches an independent saver (`harness_memory::MemorySaver`) to the agent and
//!   withholds the *writing* tools, so the same store never has two writers. The saver reads each
//!   finished turn and writes the same records through the same tool.
//!
//! This module is the seam between the config file and those two behaviours: it resolves which
//! provider and model the saver bills to, and turns the `memory.*` keys into a `SaverPolicy`. It
//! lives in its own file because the mapping is worth testing on its own, without a terminal.

use std::sync::Arc;

use harness_core::config::Config;
use harness_core::{MemoryEngine, NotesEngine};
use harness_memory::{MemorySaver, SaverPolicy};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};

/// Build the independent memory writer, or `None` when the config keeps the session as the writer.
///
/// The saver gets its own provider client so it can be pointed at a cheaper model
/// (`memory.background_model`) than the session's, the same way compaction is configured. It bills
/// to that client, never to the turn's.
///
/// One caveat worth knowing: the client is resolved at startup, so `/model` changes the session
/// mid-conversation without moving the saver. Point `memory.background_model` at something cheap
/// and stable and the two never need to agree.
/// The saver gets its own provider client so it can be pointed at a cheaper model
/// (`memory.background_model`) than the session's, the same way compaction is configured. It bills
/// to that client, never to the turn's.
///
/// One caveat worth knowing: the client is resolved at startup, so `/model` changes the session
/// mid-conversation without moving the saver. Point `memory.background_model` at something cheap
/// and stable and the two never need to agree.
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
    let client = match GenericProviderClient::new(
        provider_id.clone(),
        model.clone(),
        cred_manager,
        discovery,
        config.providers.get(&provider_id),
    ) {
        Ok(client) => client,
        // Unusable saver config is a warning, not a startup failure: the session runs either way,
        // and the missing pass is visible in the log. The session keeps writing memory itself, so
        // the fallback is the mode the user already had.
        Err(error) => {
            eprintln!(
                "warning: the background memory saver is off (provider '{provider_id}', model '{model}'): {error}"
            );
            return None;
        }
    };

    let mut policy = SaverPolicy::new(model);
    policy.every_turns = config.memory.background_every_turns;
    policy.tail_messages = config.memory.background_tail_messages;
    policy.notes = config.memory.background_notes;
    policy.two_pass = config.memory.background_two_pass;
    policy.min_confidence = config.memory.background_min_confidence;
    policy.max_memory_writes = config.memory.background_max_memory_writes;
    policy.max_note_writes = config.memory.background_max_note_writes;
    policy.max_operations = config.memory.background_max_operations;
    Some(Arc::new(MemorySaver::new(
        Arc::new(client),
        memory.clone(),
        notes.clone(),
        policy,
    )))
}

/// The startup line that tells the user which writer is active.
///
/// A mode that changes where records come from is worth one line of notice, and the number of turns
/// is the part that surprises people ("why is nothing saved yet?").
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

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::BackgroundSaver;
    use harness_memory::SqliteMemoryEngine;
    use harness_notes::FileNotesEngine;

    fn temp_engines() -> (Arc<dyn MemoryEngine>, Arc<dyn NotesEngine>) {
        (
            Arc::new(SqliteMemoryEngine::in_memory().expect("memory db")),
            Arc::new(FileNotesEngine::in_memory().expect("notes db")),
        )
    }

    fn provider_parts() -> (Arc<CredentialManager>, Arc<ModelDiscovery>) {
        let dir = std::env::temp_dir().join(format!("luna-memory-wiring-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        (
            CredentialManager::new(Some(dir.join("credentials.json"))).expect("creds"),
            Arc::new(ModelDiscovery::new(
                Some(dir.join("models_cache.json")),
                24,
                Default::default(),
            )),
        )
    }

    fn build(config: &Config) -> Option<Arc<MemorySaver>> {
        let (memory, notes) = temp_engines();
        let (cred_manager, discovery) = provider_parts();
        build_saver(
            config,
            &memory,
            notes,
            "nvidia",
            "session-model",
            cred_manager,
            discovery,
        )
    }

    #[test]
    fn the_session_mode_builds_no_saver_at_all() {
        let config = Config::default();
        assert!(!config.memory.mode.is_background());
        assert!(build(&config).is_none());
        assert!(notice(&config).is_none());
    }

    #[test]
    fn the_background_mode_maps_every_memory_key_onto_the_policy() {
        let mut config = Config::default();
        config.memory.mode = harness_core::MemoryMode::Background;
        config.memory.background_every_turns = 3;
        config.memory.background_tail_messages = 40;
        config.memory.background_notes = false;
        config.memory.background_model = Some("cheap-model".to_string());
        config.memory.background_provider = Some("nvidia".to_string());

        let saver = build(&config).expect("background mode builds a saver");

        // The trait is the only window onto the policy, which is exactly what the agent sees.
        assert_eq!(saver.model(), "cheap-model");
        assert_eq!(saver.every_turns(), 3);
        assert_eq!(saver.tail_limit(), 40);
        assert_eq!(
            notice(&config).as_deref(),
            Some("memory: background saver on (cheap-model), after every 3 turn(s) — memories land after each reply")
        );
    }

    #[test]
    fn an_unset_background_model_falls_back_to_the_session_model() {
        let mut config = Config::default();
        config.memory.mode = harness_core::MemoryMode::Background;

        let saver = build(&config).expect("background mode builds a saver");

        assert_eq!(saver.model(), "session-model");
        assert_eq!(saver.every_turns(), 3);
        assert_eq!(saver.tail_limit(), 24);
        assert!(saver.every_turns() > 0, "a zero interval would disable it");
    }

    #[test]
    fn an_unknown_background_provider_disables_the_saver_instead_of_the_session() {
        let mut config = Config::default();
        config.memory.mode = harness_core::MemoryMode::Background;
        config.memory.background_provider = Some("not-a-real-provider".to_string());

        assert!(
            build(&config).is_none(),
            "an unusable provider must not stop the session from starting"
        );
    }

    #[test]
    fn a_zero_interval_builds_a_saver_that_never_runs() {
        let mut config = Config::default();
        config.memory.mode = harness_core::MemoryMode::Background;
        config.memory.background_every_turns = 0;

        let saver = build(&config).expect("the saver is still built");
        assert_eq!(saver.every_turns(), 0);
        // `SaverHook::due` treats 0 as "off"; the agent never dispatches it.
        assert!(!harness_core::SaverHook::from_saver(saver).due(1));
    }
}
