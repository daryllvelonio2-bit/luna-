//! Persistence of the onboarding result.
//!
//! The flow records its answers in the harness config file (`$LUNA_CONFIG`, else
//! `~/.luna/config.yaml`): who writes memory (`memory.mode`), where notes live
//! (`paths.notes_dir`), the auto-compact budget, the messaging gateway, whether the web tools are
//! registered (`tools.web.enabled`, `tools.web.search_backend`, `tools.web.search_api_key_env`),
//! and `first_run: false` to close the gate.
//!
//! The profile is not a config key. Bare `luna` always runs the built-in `default` profile, and a
//! named profile is selected by invocation (`luna terra`) — so onboarding has nothing to persist
//! for it and cannot pin a custom profile over the default.
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
/// The config key holding whether voice mode is enabled.
pub const VOICE_ENABLED_KEY: &str = "voice.enabled";
/// The config key holding which voice provider / backend is active.
pub const VOICE_PROVIDER_KEY: &str = "voice.provider";
/// The config key holding the wake word.
pub const VOICE_WAKE_WORD_KEY: &str = "voice.wake_word";
/// The config key holding the voice personality/name.
pub const VOICE_NAME_KEY: &str = "voice.voice_name";
/// The config key holding the speech-to-text model.
pub const VOICE_STT_MODEL_KEY: &str = "voice.stt_model";
/// The config key holding the text-to-speech model.
pub const VOICE_TTS_MODEL_KEY: &str = "voice.tts_model";
/// The config key holding custom base URL for voice endpoints.
pub const VOICE_BASE_URL_KEY: &str = "voice.base_url";
/// The config key holding the environment variable for voice API key.
pub const VOICE_API_KEY_ENV_KEY: &str = "voice.api_key_env";
/// The config key holding silence duration threshold in milliseconds.
pub const VOICE_SILENCE_THRESHOLD_MS_KEY: &str = "voice.silence_threshold_ms";
/// The config key holding whether speech barge-in is enabled.
pub const VOICE_ENABLE_BARGE_IN_KEY: &str = "voice.enable_barge_in";
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

/// The voice interface answer: whether voice mode is enabled, which provider backs it,
/// voice name, models, and latency / barge-in settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceSelection {
    pub enabled: bool,
    pub provider: String,
    pub wake_word: String,
    pub voice_name: String,
    pub stt_model: String,
    pub tts_model: String,
    pub base_url: Option<String>,
    pub api_key_env: Option<String>,
    pub silence_threshold_ms: u64,
    pub enable_barge_in: bool,
}

impl VoiceSelection {
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            provider: "none".to_string(),
            wake_word: "Luna".to_string(),
            voice_name: "none".to_string(),
            stt_model: String::new(),
            tts_model: String::new(),
            base_url: None,
            api_key_env: None,
            silence_threshold_ms: 800,
            enable_barge_in: false,
        }
    }

    pub fn antigravity(voice_name: Option<String>) -> Self {
        Self {
            enabled: true,
            provider: "antigravity".to_string(),
            wake_word: "Luna".to_string(),
            voice_name: voice_name.unwrap_or_else(|| "Aoede".to_string()),
            stt_model: String::new(),
            tts_model: String::new(),
            base_url: None,
            api_key_env: None,
            silence_threshold_ms: 800,
            enable_barge_in: true,
        }
    }

    pub fn openai(voice_name: Option<String>, api_key_env: Option<String>) -> Self {
        Self {
            enabled: true,
            provider: "openai".to_string(),
            wake_word: "Luna".to_string(),
            voice_name: voice_name.unwrap_or_else(|| "alloy".to_string()),
            stt_model: "whisper-1".to_string(),
            tts_model: "tts-1".to_string(),
            base_url: None,
            api_key_env: api_key_env.or_else(|| Some("OPENAI_API_KEY".to_string())),
            silence_threshold_ms: 800,
            enable_barge_in: true,
        }
    }

    pub fn groq(api_key_env: Option<String>) -> Self {
        Self {
            enabled: true,
            provider: "groq".to_string(),
            wake_word: "Luna".to_string(),
            voice_name: "en-US-EmmaNeural".to_string(),
            stt_model: "whisper-large-v3".to_string(),
            tts_model: "edge-tts".to_string(),
            base_url: None,
            api_key_env: api_key_env.or_else(|| Some("GROQ_API_KEY".to_string())),
            silence_threshold_ms: 800,
            enable_barge_in: true,
        }
    }
}

impl Default for VoiceSelection {
    fn default() -> Self {
        Self::disabled()
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
    memory: MemoryMode,
    notes_dir: &std::path::Path,
    auto_compact: &AutoCompactSelection,
    gateway: &GatewaySelection,
    web: &WebSelection,
    voice: &VoiceSelection,
) -> Result<PathBuf, HarnessError> {
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
    config::set_in_file(
        layers,
        VOICE_ENABLED_KEY,
        if voice.enabled { "true" } else { "false" },
    )?;
    config::set_in_file(layers, VOICE_PROVIDER_KEY, &voice.provider)?;
    config::set_in_file(layers, VOICE_WAKE_WORD_KEY, &voice.wake_word)?;
    config::set_in_file(layers, VOICE_NAME_KEY, &voice.voice_name)?;
    config::set_in_file(layers, VOICE_STT_MODEL_KEY, &voice.stt_model)?;
    config::set_in_file(layers, VOICE_TTS_MODEL_KEY, &voice.tts_model)?;
    if let Some(base_url) = &voice.base_url {
        config::set_in_file(layers, VOICE_BASE_URL_KEY, base_url)?;
    }
    if let Some(env_var) = &voice.api_key_env {
        config::set_in_file(layers, VOICE_API_KEY_ENV_KEY, env_var)?;
    }
    config::set_in_file(
        layers,
        VOICE_SILENCE_THRESHOLD_MS_KEY,
        &voice.silence_threshold_ms.to_string(),
    )?;
    config::set_in_file(
        layers,
        VOICE_ENABLE_BARGE_IN_KEY,
        if voice.enable_barge_in {
            "true"
        } else {
            "false"
        },
    )?;
    config::set_in_file(layers, FIRST_RUN_KEY, "false")?;
    config_file(layers)
}

/// Write the voice selection, returning the config file path that was written.
pub fn persist_voice(layers: &Layers, voice: &VoiceSelection) -> Result<PathBuf, HarnessError> {
    config::set_in_file(
        layers,
        VOICE_ENABLED_KEY,
        if voice.enabled { "true" } else { "false" },
    )?;
    config::set_in_file(layers, VOICE_PROVIDER_KEY, &voice.provider)?;
    config::set_in_file(layers, VOICE_WAKE_WORD_KEY, &voice.wake_word)?;
    config::set_in_file(layers, VOICE_NAME_KEY, &voice.voice_name)?;
    config::set_in_file(layers, VOICE_STT_MODEL_KEY, &voice.stt_model)?;
    config::set_in_file(layers, VOICE_TTS_MODEL_KEY, &voice.tts_model)?;
    if let Some(base_url) = &voice.base_url {
        config::set_in_file(layers, VOICE_BASE_URL_KEY, base_url)?;
    }
    if let Some(env_var) = &voice.api_key_env {
        config::set_in_file(layers, VOICE_API_KEY_ENV_KEY, env_var)?;
    }
    config::set_in_file(
        layers,
        VOICE_SILENCE_THRESHOLD_MS_KEY,
        &voice.silence_threshold_ms.to_string(),
    )?;
    config::set_in_file(
        layers,
        VOICE_ENABLE_BARGE_IN_KEY,
        if voice.enable_barge_in {
            "true"
        } else {
            "false"
        },
    )?;
    config::set_in_file(layers, FIRST_RUN_KEY, "false")?;
    config_file(layers)
}

/// Write the memory mode selection, returning the config file path that was written.
pub fn persist_memory(layers: &Layers, memory: MemoryMode) -> Result<PathBuf, HarnessError> {
    config::set_in_file(layers, MEMORY_MODE_KEY, memory.as_str())?;
    config::set_in_file(layers, FIRST_RUN_KEY, "false")?;
    config_file(layers)
}

/// Write the notes storage directory selection, returning the config file path that was written.
pub fn persist_notes(
    layers: &Layers,
    notes_dir: &std::path::Path,
) -> Result<PathBuf, HarnessError> {
    config::set_in_file(layers, NOTES_DIR_KEY, &notes_dir.to_string_lossy())?;
    config::set_in_file(layers, FIRST_RUN_KEY, "false")?;
    config_file(layers)
}

/// Write the auto-compaction selection, returning the config file path that was written.
pub fn persist_auto_compact(
    layers: &Layers,
    auto_compact: &AutoCompactSelection,
) -> Result<PathBuf, HarnessError> {
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
    config::set_in_file(layers, FIRST_RUN_KEY, "false")?;
    config_file(layers)
}

/// Write the messaging gateway selection, returning the config file path that was written.
pub fn persist_gateway(
    layers: &Layers,
    gateway: &GatewaySelection,
) -> Result<PathBuf, HarnessError> {
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
    config::set_in_file(layers, FIRST_RUN_KEY, "false")?;
    config_file(layers)
}

/// Write the web access selection, returning the config file path that was written.
pub fn persist_web(layers: &Layers, web: &WebSelection) -> Result<PathBuf, HarnessError> {
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
