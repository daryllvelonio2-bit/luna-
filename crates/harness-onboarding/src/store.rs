//! Persistence of the onboarding result.
//!
//! The flow records four keys in the harness config file (`$LUNA_CONFIG`, else
//! `~/.luna/config.yaml`): `persona.id`, `persona.name`, `memory.mode`, and `first_run: false`.
//!
//! What is deliberately *not* written is the persona's prompt block. Prompt text has exactly one
//! home — `crates/harness-core/prompts.md` — so an install stores the *choice* and the block is
//! resolved from that file at load time ([`crate::persona::Persona::prompt_block`]). Copying the
//! prose into every user's YAML would freeze it at the version they onboarded with, and editing a
//! persona's voice would then require migrating config files instead of shipping a rebuild.
//!
//! `memory.mode` is stored the same way: the mode names a behaviour the harness already implements
//! (`session` or `background`), not a copy of anything.
//!
//! Writes go through [`harness_core::config::set_in_file`], so each one is validated against the
//! config schema before it lands: a failed write leaves the previous file intact rather than a
//! half-updated one.

use std::path::PathBuf;

use harness_core::config::{self, Layers};
use harness_core::error::HarnessError;
use harness_core::MemoryMode;

use crate::persona::Persona;

/// The config key holding the selected persona id.
pub const PERSONA_ID_KEY: &str = "persona.id";
/// The config key holding the selected persona's display name.
pub const PERSONA_NAME_KEY: &str = "persona.name";
/// The config key holding who writes durable memory: `session` or `background`.
pub const MEMORY_MODE_KEY: &str = "memory.mode";
/// The config key holding whether auto-compaction is enabled.
pub const AUTO_COMPACT_ENABLED_KEY: &str = "auto_compact_enabled";
/// The config key holding auto-compaction threshold.
pub const AUTO_COMPACT_THRESHOLD_KEY: &str = "auto_compact_threshold";
/// The config key holding auto-compaction turns.
pub const AUTO_COMPACT_TURNS_KEY: &str = "auto_compact_turns";
/// The config key that switches the first-run flow off.
pub const FIRST_RUN_KEY: &str = "first_run";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoCompactSelection {
    pub enabled: bool,
    pub threshold: usize,
    pub turns: usize,
}

impl Default for AutoCompactSelection {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: harness_core::config::AUTO_COMPACT_DEFAULT_THRESHOLD,
            turns: harness_core::config::AUTO_COMPACT_DEFAULT_TURNS,
        }
    }
}

/// Write the selections, returning the path that was written.
pub fn persist(
    layers: &Layers,
    persona: &Persona,
    memory: MemoryMode,
    auto_compact: &AutoCompactSelection,
) -> Result<PathBuf, HarnessError> {
    config::set_in_file(layers, PERSONA_ID_KEY, &persona.id.to_string())?;
    config::set_in_file(layers, PERSONA_NAME_KEY, persona.name)?;
    config::set_in_file(layers, MEMORY_MODE_KEY, memory.as_str())?;
    config::set_in_file(
        layers,
        AUTO_COMPACT_ENABLED_KEY,
        if auto_compact.enabled { "true" } else { "false" },
    )?;
    config::set_in_file(
        layers,
        AUTO_COMPACT_THRESHOLD_KEY,
        &auto_compact.threshold.to_string(),
    )?;
    config::set_in_file(
        layers,
        AUTO_COMPACT_TURNS_KEY,
        &auto_compact.turns.to_string(),
    )?;
    config::set_in_file(layers, FIRST_RUN_KEY, "false")?;
    config_file(layers)
}

/// Where the config file lives, or an error when the process has neither `$LUNA_CONFIG` nor `$HOME`.
pub fn config_file(layers: &Layers) -> Result<PathBuf, HarnessError> {
    layers.file.clone().ok_or_else(|| {
        HarnessError::ConfigurationError(
            "no config file location: set LUNA_CONFIG or HOME".to_string(),
        )
    })
}
