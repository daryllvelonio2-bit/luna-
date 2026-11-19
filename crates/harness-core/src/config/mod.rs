//! The config file, the precedence order, and the flat key view behind `luna config`.
//!
//! Resolution order, lowest priority first:
//!
//! 1. built-in defaults ([`Config::default`]). The caller may overlay catalog-derived
//!    `providers.<id>.*` entries first ([`Layers::defaults`]) — that is how provider base URLs stay
//!    out of this module and out of the config file format.
//! 2. the config file: `$LUNA_CONFIG` when set, else `~/.luna/config.yaml` (YAML).
//! 3. `LUNA_*` environment variables: `model.default` is `LUNA_MODEL_DEFAULT`,
//!    `providers.nvidia.base_url` is `LUNA_PROVIDERS_NVIDIA_BASE_URL`.
//! 4. CLI flags, as dotted keys.
//!
//! A missing file is not an error. A malformed file *is*: it fails with the parse error and the
//! line number instead of silently falling back to defaults.
//!
//! [`Resolved`] keeps the effective value of every key plus the layer that produced it, so
//! `luna config get --show-source` reports fact rather than a guess.
//!
//! [`set_in_file`] rewrites the file from a YAML value tree, so comments and key order in a user's
//! file are not preserved. That is the trade for not adding a format-preserving editor; the
//! resulting file is validated against [`Config`] *before* it is written.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub mod layers;
pub mod ops;
pub mod types;
pub mod web;

use crate::agent::AgentConfig;
use crate::error::HarnessError;

pub use layers::{
    config_file_path, env_overrides, env_var_name, process_layers, resolve, Layers, Resolved,
    CONFIG_FILE_RELATIVE, CONFIG_PATH_ENV, ENV_PREFIX,
};
pub use ops::{
    coerce, remove_worker_from_file, set_in_file, set_worker_in_file, unset_in_file,
    DISTANCE_BUCKET, PROVIDER_FIELDS,
};
pub use types::{
    expand_tilde, home_dir, validate_worker_name, CheckpointSettings, DisplaySettings,
    FeatureSettings, GatewaySettings, GoalSettings, HooksSettings, McpServerConfig, McpSettings,
    MemoryMode, MemorySettings, MemoryStoreSettings, ModelSettings, PathsSettings,
    ProviderOverride, SessionSettings, Source, TelemetrySettings, ThemeMode, ToolsSettings,
    UpdateSettings, UsageSettings, VoiceSettings, WorkerProfile, WORKER_NAME_MAX_CHARS,
};
pub use web::{
    web_tools_blocked_reason, SearchBackend, WebSettings, DEFAULT_WEB_USER_AGENT, SEARCH_BACKENDS,
    WEB_CACHE_CEILING_SECS, WEB_FETCH_MAX_BYTES_CEILING, WEB_FETCH_MAX_CHARS_CEILING,
    WEB_FETCH_MIN_BYTES, WEB_FETCH_MIN_CHARS, WEB_MAX_REDIRECT_CEILING,
    WEB_SEARCH_MAX_RESULTS_CEILING, WEB_TIMEOUT_CEILING_SECS,
};

/// The whole effective configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub model: ModelSettings,
    pub paths: PathsSettings,
    pub providers: BTreeMap<String, ProviderOverride>,
    pub agent: AgentConfig,
    pub tools: ToolsSettings,
    pub hooks: HooksSettings,
    pub checkpoints: CheckpointSettings,
    pub session: SessionSettings,
    pub telemetry: TelemetrySettings,
    pub usage: UsageSettings,
    /// Whether a launch asks GitHub for a newer build of this binary. See [`UpdateSettings`].
    #[serde(default)]
    pub update: UpdateSettings,
    /// Who writes durable memory, and with what budget. See [`MemorySettings`].
    pub memory: MemorySettings,
    /// Deprecated persona field.
    #[serde(default, skip_serializing)]
    pub persona: serde_yaml::Value,
    /// `false` once the first-run onboarding has completed.
    pub first_run: bool,
    /// Whether automatic compaction is enabled when transcript crosses threshold.
    #[serde(default = "default_auto_compact_enabled")]
    pub auto_compact_enabled: bool,
    /// Token threshold at which automatic compaction triggers (100k - 700k).
    #[serde(default = "default_auto_compact_threshold")]
    pub auto_compact_threshold: usize,
    /// Percentage of transcript context to compact into a summary (1% - 90%).
    #[serde(default = "default_auto_compact_percent")]
    pub auto_compact_percent: Option<usize>,
    /// Number of newest turns each auto-compaction fold keeps verbatim.
    #[serde(default = "default_auto_compact_turns")]
    pub auto_compact_turns: usize,
    /// Messaging platform gateway (Telegram, Discord, WhatsApp).
    #[serde(default)]
    pub gateway: GatewaySettings,
    /// Model Context Protocol servers whose tools are exposed to the agent.
    #[serde(default)]
    pub mcp: McpSettings,
    /// Named background workers, invoked in chat as `/<name> <task>`.
    #[serde(default)]
    pub workers: BTreeMap<String, WorkerProfile>,
    /// CLI display preferences (live block limit, turn completion collapse).
    #[serde(default)]
    pub display: DisplaySettings,
    /// Autonomous `/goal` and `luna goal` loop defaults.
    #[serde(default)]
    pub goal: GoalSettings,
    /// Persistent timed-disable schedules for optional features (`mcp`, `skills`, `memory`, `notes`).
    #[serde(default)]
    pub features: FeatureSettings,
    /// Hands-free voice interface and audio settings.
    #[serde(default)]
    pub voice: VoiceSettings,
}

pub const AUTO_COMPACT_MIN_THRESHOLD: usize = 100_000;
pub const AUTO_COMPACT_MAX_THRESHOLD: usize = 700_000;
pub const AUTO_COMPACT_DEFAULT_THRESHOLD: usize = 150_000;
pub const AUTO_COMPACT_DEFAULT_TURNS: usize = 20;
pub const AUTO_COMPACT_DEFAULT_PERCENT: usize = 50;
pub const AUTO_COMPACT_MIN_PERCENT: usize = 1;
pub const AUTO_COMPACT_MAX_PERCENT: usize = 90;

fn default_auto_compact_enabled() -> bool {
    true
}

fn default_auto_compact_threshold() -> usize {
    AUTO_COMPACT_DEFAULT_THRESHOLD
}

fn default_auto_compact_percent() -> Option<usize> {
    Some(AUTO_COMPACT_DEFAULT_PERCENT)
}

fn default_auto_compact_turns() -> usize {
    AUTO_COMPACT_DEFAULT_TURNS
}

impl Default for Config {
    fn default() -> Self {
        Self {
            model: ModelSettings::default(),
            paths: PathsSettings::default(),
            providers: BTreeMap::new(),
            agent: AgentConfig::default(),
            tools: ToolsSettings::default(),
            hooks: HooksSettings::default(),
            checkpoints: CheckpointSettings::default(),
            session: SessionSettings::default(),
            telemetry: TelemetrySettings::default(),
            usage: UsageSettings::default(),
            update: UpdateSettings::default(),
            memory: MemorySettings::default(),
            persona: serde_yaml::Value::Null,
            first_run: true,
            auto_compact_enabled: true,
            auto_compact_threshold: AUTO_COMPACT_DEFAULT_THRESHOLD,
            auto_compact_percent: Some(AUTO_COMPACT_DEFAULT_PERCENT),
            auto_compact_turns: AUTO_COMPACT_DEFAULT_TURNS,
            gateway: GatewaySettings::default(),
            mcp: McpSettings::default(),
            workers: BTreeMap::new(),
            display: DisplaySettings::default(),
            goal: GoalSettings::default(),
            features: FeatureSettings::default(),
            voice: VoiceSettings::default(),
        }
    }
}

impl Config {
    /// Current wall-clock time as Unix epoch seconds.
    pub fn current_unix_secs() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    /// Whether MCP is effectively enabled right now (considering global flags and timed disable).
    pub fn mcp_effective_enabled(&self) -> bool {
        self.mcp_effective_enabled_at(Self::current_unix_secs())
    }

    pub fn mcp_effective_enabled_at(&self, now_secs: u64) -> bool {
        self.mcp.enabled
            && self.agent.mcp_enabled
            && !self
                .features
                .mcp_disabled_until
                .is_some_and(|until| now_secs < until)
    }

    /// Whether skills are effectively enabled right now (considering global flag and timed disable).
    pub fn skills_effective_enabled(&self) -> bool {
        self.skills_effective_enabled_at(Self::current_unix_secs())
    }

    pub fn skills_effective_enabled_at(&self, now_secs: u64) -> bool {
        self.agent.skills_enabled
            && !self
                .features
                .skills_disabled_until
                .is_some_and(|until| now_secs < until)
    }

    /// Whether memory is effectively enabled right now (considering global flag and timed disable).
    pub fn memory_effective_enabled(&self) -> bool {
        self.memory_effective_enabled_at(Self::current_unix_secs())
    }

    pub fn memory_effective_enabled_at(&self, now_secs: u64) -> bool {
        self.agent.memory_enabled
            && !self
                .features
                .memory_disabled_until
                .is_some_and(|until| now_secs < until)
    }

    /// Whether notes are effectively enabled right now (considering global flag and timed disable).
    pub fn notes_effective_enabled(&self) -> bool {
        self.notes_effective_enabled_at(Self::current_unix_secs())
    }

    pub fn notes_effective_enabled_at(&self, now_secs: u64) -> bool {
        self.agent.notes_enabled
            && !self
                .features
                .notes_disabled_until
                .is_some_and(|until| now_secs < until)
    }

    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.auto_compact_threshold < AUTO_COMPACT_MIN_THRESHOLD
            || self.auto_compact_threshold > AUTO_COMPACT_MAX_THRESHOLD
        {
            return Err(HarnessError::ConfigurationError(format!(
                "auto_compact_threshold must be between {} and {}, got {}",
                AUTO_COMPACT_MIN_THRESHOLD, AUTO_COMPACT_MAX_THRESHOLD, self.auto_compact_threshold
            )));
        }
        if let Some(pct) = self.auto_compact_percent {
            if !(AUTO_COMPACT_MIN_PERCENT..=AUTO_COMPACT_MAX_PERCENT).contains(&pct) {
                return Err(HarnessError::ConfigurationError(format!(
                    "auto_compact_percent must be between {} and {}, got {}",
                    AUTO_COMPACT_MIN_PERCENT, AUTO_COMPACT_MAX_PERCENT, pct
                )));
            }
        }
        if self.auto_compact_turns == 0 {
            return Err(HarnessError::ConfigurationError(
                "auto_compact_turns must be positive, got 0".to_string(),
            ));
        }
        if self.agent.auto_compact_threshold < AUTO_COMPACT_MIN_THRESHOLD
            || self.agent.auto_compact_threshold > AUTO_COMPACT_MAX_THRESHOLD
        {
            return Err(HarnessError::ConfigurationError(format!(
                "agent.auto_compact_threshold must be between {} and {}, got {}",
                AUTO_COMPACT_MIN_THRESHOLD,
                AUTO_COMPACT_MAX_THRESHOLD,
                self.agent.auto_compact_threshold
            )));
        }
        if let Some(pct) = self.agent.auto_compact_percent {
            if !(AUTO_COMPACT_MIN_PERCENT..=AUTO_COMPACT_MAX_PERCENT).contains(&pct) {
                return Err(HarnessError::ConfigurationError(format!(
                    "agent.auto_compact_percent must be between {} and {}, got {}",
                    AUTO_COMPACT_MIN_PERCENT, AUTO_COMPACT_MAX_PERCENT, pct
                )));
            }
        }
        if self.agent.auto_compact_turns == 0 {
            return Err(HarnessError::ConfigurationError(
                "agent.auto_compact_turns must be positive, got 0".to_string(),
            ));
        }
        self.mcp.validate()?;
        self.tools.web.validate()?;
        for (name, worker) in &self.workers {
            validate_worker_name(name)?;
            if worker.provider.trim().is_empty() {
                return Err(HarnessError::ConfigurationError(format!(
                    "workers.{name}.provider must not be empty"
                )));
            }
            if worker.model.trim().is_empty() {
                return Err(HarnessError::ConfigurationError(format!(
                    "workers.{name}.model must not be empty"
                )));
            }
        }
        if let Some(limit) = self.display.block_limit {
            if !(1..=10).contains(&limit) {
                return Err(HarnessError::ConfigurationError(format!(
                    "display.block_limit must be between 1 and 10, got {limit}"
                )));
            }
        }
        crate::goal::validate_max_iterations(self.goal.max_iterations)?;
        self.voice.validate()?;
        Ok(())
    }
}
