//! The chat command: one session, one REPL, one non-interactive loop.
//!
//! This file is only the facade. The pieces live in focused submodules:
//!
//! - [`setup`] resolves the session, provider, tools and agent into a [`setup::ChatRuntime`].
//! - [`piped`] runs the read-a-line-and-answer loop used when stdout is not a terminal.
//! - [`interactive`] owns the attended REPL and its event loop.
//! - [`helpers`] holds the small shared routines (transcript redraw, MCP registration, ...).

use std::sync::Arc;

use harness_core::{Agent, Config, DefaultToolRegistry, HarnessError};
use harness_memory::MemoryEngineKind;
use harness_notes::FileNotesEngine;
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};

use crate::commands::sessions::SessionSelection;

pub mod commands;
pub(crate) mod connect;
pub(crate) mod helpers;
pub(crate) mod history;
pub(crate) mod interactive;
pub(crate) mod keys;
pub(crate) mod memory_writer;
pub(crate) mod modal;
pub(crate) mod observers;
pub(crate) mod piped;
pub(crate) mod prompts;
pub(crate) mod render;
pub(crate) mod setup;
pub(crate) mod status;
pub mod todo;
pub(crate) mod turn;
pub mod ui;
pub mod viewer;

#[cfg(test)]
mod tests;

// Kept on the facade so crate code and tests can name them by their old paths.
#[allow(unused_imports)]
pub(crate) use helpers::{
    finish_slash_command, key_keeps_landing, redraw_transcript, register_mcp_servers,
    IdleMemoryPass,
};
pub(crate) use history::clear_on_exit;
pub(crate) use keys::AppKey;
#[cfg(test)]
use turn::Submission;

pub(crate) type ChatAgent =
    Agent<GenericProviderClient, MemoryEngineKind, FileNotesEngine, DefaultToolRegistry>;

/// Run a chat session: set it up once, then hand it to the loop that fits the terminal.
pub async fn run_interactive_chat(
    config: Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    selection: SessionSelection,
    explicit_model: bool,
) -> Result<(), HarnessError> {
    let mut runtime =
        setup::prepare(config, cred_manager, discovery, selection, explicit_model).await?;
    if runtime.is_attended {
        interactive::run(&mut runtime).await
    } else {
        piped::run(&mut runtime).await
    }
}
