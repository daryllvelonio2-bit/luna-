//! Persistence of the onboarding result.
//!
//! The flow records its answers in the harness config file (`$LUNA_CONFIG`, else
//! `~/.luna/config.yaml`): the persona (`persona.id`, `persona.name`), who writes memory
//! (`memory.mode`), where notes live (`paths.notes_dir`), the auto-compact budget, the messaging
//! gateway, whether the web tools are registered (`tools.web.enabled`, `tools.web.search_backend`,
//! `tools.web.search_api_key_env`), and `first_run: false` to close the gate.
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

use harness_core::config::{self, Layers, SearchBackend};
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
/// The config key holding auto-compaction percent.
pub const AUTO_COMPACT_PERCENT_KEY: &str = "auto_compact_percent";
/// The config key holding auto-compaction turns.
pub const AUTO_COMPACT_TURNS_KEY: &str = "auto_compact_turns";
/// The config key holding whether the messaging gateway is enabled.
pub const GATEWAY_ENABLED_KEY: &str = "gateway.enabled";
/// The config key holding the messaging gateway platform.
pub const GATEWAY_PLATFORM_KEY: &str = "gateway.platform";
/// The config key holding the Telegram bot token.
pub const GATEWAY_TELEGRAM_BOT_TOKEN_KEY: &str = "gateway.telegram_bot_token";
/// The config key holding the Discord bot token.
pub const GATEWAY_DISCORD_BOT_TOKEN_KEY: &str = "gateway.discord_bot_token";
/// The config key holding the WhatsApp access token.
pub const GATEWAY_WHATSAPP_TOKEN_KEY: &str = "gateway.whatsapp_token";
/// The config key holding where markdown notes are stored.
pub const NOTES_DIR_KEY: &str = "paths.notes_dir";
/// The config key holding whether `web_search` / `web_fetch` are registered.
pub const WEB_ENABLED_KEY: &str = "tools.web.enabled";
/// The config key holding which provider backs `web_search`.
pub const WEB_BACKEND_KEY: &str = "tools.web.search_backend";
/// The config key holding the environment variable a key is read from. The key itself is never
/// written here — see the `WebSettings` documentation.
pub const WEB_API_KEY_ENV_KEY: &str = "tools.web.search_api_key_env";
/// The config key that switches the first-run flow off.
pub const FIRST_RUN_KEY: &str = "first_run";

/// The web-access answer: whether the pair is registered, which provider backs search, and which
/// environment variable holds a key for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSelection {
    pub enabled: bool,
    pub backend: SearchBackend,
    pub api_key_env: Option<String>,
}

impl WebSelection {
    /// Neither tool is registered.
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            backend: SearchBackend::Disabled,
            api_key_env: None,
        }
    }
}

impl Default for WebSelection {
    /// A working keyless search: what a user gets by pressing Enter, and what a script that ends
    /// before this step leaves behind.
    fn default() -> Self {
        Self {
            enabled: true,
            backend: SearchBackend::DuckDuckGo,
            api_key_env: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewaySelection {
    pub enabled: bool,
    pub platform: String,
    pub token: Option<String>,
}

impl Default for GatewaySelection {
    fn default() -> Self {
        Self {
            enabled: false,
            platform: "none".to_string(),
            token: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoCompactSelection {
    pub enabled: bool,
    pub threshold: usize,
    pub percent: usize,
    pub turns: usize,
}

impl Default for AutoCompactSelection {
    fn default() -> Self {
        Self {
            enabled: true,
            threshold: harness_core::config::AUTO_COMPACT_DEFAULT_THRESHOLD,
            percent: harness_core::config::AUTO_COMPACT_DEFAULT_PERCENT,
            turns: harness_core::config::AUTO_COMPACT_DEFAULT_TURNS,
        }
    }
}

/// Write the selections, returning the path that was written.
pub fn persist(
    layers: &Layers,
    persona: &Persona,
    memory: MemoryMode,
    notes_dir: &std::path::Path,
    auto_compact: &AutoCompactSelection,
    gateway: &GatewaySelection,
    web: &WebSelection,
) -> Result<PathBuf, HarnessError> {
    config::set_in_file(layers, PERSONA_ID_KEY, &persona.id.to_string())?;
    config::set_in_file(layers, PERSONA_NAME_KEY, persona.name)?;
    config::set_in_file(layers, MEMORY_MODE_KEY, memory.as_str())?;
    config::set_in_file(layers, NOTES_DIR_KEY, &notes_dir.to_string_lossy())?;
    config::set_in_file(
        layers,
        AUTO_COMPACT_ENABLED_KEY,
        if auto_compact.enabled {
            "true"
        } else {
            "false"
        },
    )?;
    config::set_in_file(
        layers,
        AUTO_COMPACT_THRESHOLD_KEY,
        &auto_compact.threshold.to_string(),
    )?;
    config::set_in_file(
        layers,
        AUTO_COMPACT_PERCENT_KEY,
        &auto_compact.percent.to_string(),
    )?;
    config::set_in_file(
        layers,
        AUTO_COMPACT_TURNS_KEY,
        &auto_compact.turns.to_string(),
    )?;
    config::set_in_file(
        layers,
        GATEWAY_ENABLED_KEY,
        if gateway.enabled { "true" } else { "false" },
    )?;
    config::set_in_file(layers, GATEWAY_PLATFORM_KEY, &gateway.platform)?;
    if let Some(token) = &gateway.token {
        if !token.is_empty() {
            let key = match gateway.platform.as_str() {
                "telegram" => GATEWAY_TELEGRAM_BOT_TOKEN_KEY,
                "discord" => GATEWAY_DISCORD_BOT_TOKEN_KEY,
                "whatsapp" => GATEWAY_WHATSAPP_TOKEN_KEY,
                _ => "",
            };
            if !key.is_empty() {
                config::set_in_file(layers, key, token)?;
            }
        }
    }
    config::set_in_file(
        layers,
        WEB_ENABLED_KEY,
        if web.enabled { "true" } else { "false" },
    )?;
    config::set_in_file(layers, WEB_BACKEND_KEY, web.backend.as_str())?;
    config::set_in_file(
        layers,
        WEB_API_KEY_ENV_KEY,
        web.api_key_env.as_deref().unwrap_or(""),
    )?;
    config::set_in_file(layers, FIRST_RUN_KEY, "false")?;
    config_file(layers)
}

/// Where the config file lives, or an error when the process has neither `$LUNA_CONFIG` nor `$HOME`.
pub fn config_file(layers: &Layers) -> Result<PathBuf, HarnessError> {
    layers.file.clone().ok_or_else(|| {
        HarnessError::ConfigurationError(
            "no config file location: set LUNA_CONFIG, HOME, or USERPROFILE".to_string(),
        )
    })
}
