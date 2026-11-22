//! Configuration setting structs, enums, and defaults for Luna.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::approval::{ApprovalMode, DEFAULT_APPROVAL_TIMEOUT_SECS};
use crate::error::HarnessError;
use crate::types::ReasoningEffort;

pub use super::web::WebSettings;

/// Longest worker name accepted as a slash command (`/carbon` -> `carbon`).
pub const WORKER_NAME_MAX_CHARS: usize = 32;

/// A saved provider/model pair for a background worker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerProfile {
    pub provider: String,
    pub model: String,
    /// Reasoning effort to ask for. `None` inherits the session's current effort when the
    /// worker is invoked, so a worker profile does not silently turn thinking off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

/// Validate a worker name as a slash command: lowercase letters, digits, `-` and `_`, starting
/// with a letter, and short enough to fit the slash menu.
pub fn validate_worker_name(name: &str) -> Result<(), HarnessError> {
    if name.is_empty() {
        return Err(HarnessError::ConfigurationError(
            "worker name must not be empty".to_string(),
        ));
    }
    if name.chars().count() > WORKER_NAME_MAX_CHARS {
        return Err(HarnessError::ConfigurationError(format!(
            "worker name '{name}' is longer than {WORKER_NAME_MAX_CHARS} characters"
        )));
    }
    let mut chars = name.chars();
    if !chars.next().is_some_and(|c| c.is_ascii_lowercase()) {
        return Err(HarnessError::ConfigurationError(format!(
            "worker name '{name}' must start with a letter"
        )));
    }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        return Err(HarnessError::ConfigurationError(format!(
            "worker name '{name}' may only contain lowercase letters, digits, '-' and '_'"
        )));
    }
    Ok(())
}

/// Model Context Protocol (MCP) integration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpSettings {
    /// Master switch. Default `true`: servers only run because the user listed them.
    pub enabled: bool,
    /// Bound on the `initialize` handshake and `tools/list` at session start.
    pub startup_timeout_secs: u64,
    /// Bound on one `tools/call`. A hung server is reported as a tool error, not a stalled turn.
    pub call_timeout_secs: u64,
    pub servers: Vec<McpServerConfig>,
}

impl Default for McpSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            startup_timeout_secs: 20,
            call_timeout_secs: 120,
            servers: Vec::new(),
        }
    }
}

impl McpSettings {
    /// Reject a config that cannot produce a usable tool surface: a server with no name, no
    /// command, or a duplicate namespace would silently shadow another server's tools.
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.startup_timeout_secs == 0 {
            return Err(HarnessError::ConfigurationError(
                "mcp.startup_timeout_secs must be positive".to_string(),
            ));
        }
        if self.call_timeout_secs == 0 {
            return Err(HarnessError::ConfigurationError(
                "mcp.call_timeout_secs must be positive".to_string(),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for server in &self.servers {
            let name = server.name.trim();
            if name.is_empty() {
                return Err(HarnessError::ConfigurationError(
                    "mcp.servers[].name must not be empty".to_string(),
                ));
            }
            if server.command.trim().is_empty() {
                return Err(HarnessError::ConfigurationError(format!(
                    "mcp server '{name}' has an empty command"
                )));
            }
            if !seen.insert(name.to_string()) {
                return Err(HarnessError::ConfigurationError(format!(
                    "duplicate mcp server name '{name}': names are the tool namespace and must be unique"
                )));
            }
        }
        Ok(())
    }
}

/// One stdio MCP server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    /// Namespace for this server's tools. Must be non-empty and unique.
    pub name: String,
    /// Executable to spawn.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables for the child process.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Working directory; defaults to `paths.workspace_root`. `~` is expanded.
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Tool-name allowlist. Empty means every tool the server advertises.
    #[serde(default)]
    pub tools: Vec<String>,
}

fn default_true() -> bool {
    true
}

/// Messaging platform gateway settings (Telegram, Discord, WhatsApp).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GatewaySettings {
    /// Enable messaging platform gateway. Default `false`.
    pub enabled: bool,
    /// Active platform name: "telegram", "discord", "whatsapp", or "none".
    pub platform: String,
    /// Telegram Bot token from @BotFather.
    pub telegram_bot_token: String,
    /// Allowed Telegram user IDs or usernames (empty = allow all).
    pub telegram_allowed_users: Vec<String>,
    /// Discord Bot token from Discord Developer Portal.
    pub discord_bot_token: String,
    /// Default Discord channel ID to monitor.
    pub discord_channel_id: String,
    /// Allowed Discord user IDs (empty = allow all).
    pub discord_allowed_users: Vec<String>,
    /// WhatsApp Cloud API access token.
    pub whatsapp_token: String,
    /// WhatsApp Cloud Phone Number ID.
    pub whatsapp_phone_number_id: String,
    /// WhatsApp webhook verification token.
    pub whatsapp_verify_token: String,
    pub whatsapp_app_secret_env: String,
    /// Port for webhook servers (e.g. WhatsApp). Default 8080.
    pub port: u16,
    /// Whether gateway turns can execute mutating tool calls without asking.
    pub allow_mutating_tools: bool,
}

impl Default for GatewaySettings {
    fn default() -> Self {
        Self {
            enabled: false,
            platform: "none".to_string(),
            telegram_bot_token: String::new(),
            telegram_allowed_users: Vec::new(),
            discord_bot_token: String::new(),
            discord_channel_id: String::new(),
            discord_allowed_users: Vec::new(),
            whatsapp_token: String::new(),
            whatsapp_phone_number_id: String::new(),
            whatsapp_verify_token: String::new(),
            whatsapp_app_secret_env: String::new(),
            port: 8080,
            allow_mutating_tools: false,
        }
    }
}

/// Hands-free voice interface and audio settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VoiceSettings {
    /// Enable voice interface. Default `false`.
    pub enabled: bool,
    /// Active voice provider / engine: "antigravity", "openai", "groq", "custom", or "none".
    pub provider: String,
    /// Wake word to listen for in hands-free mode. Default "Luna".
    pub wake_word: String,
    /// Voice name / personality (e.g. "Aoede", "Puck", "Charon", "Fenrir", "Kore", "alloy", "echo", etc.).
    pub voice_name: String,
    /// Speech-to-text model override.
    pub stt_model: String,
    /// Text-to-speech model override.
    pub tts_model: String,
    /// Custom base URL for custom voice endpoints.
    pub base_url: Option<String>,
    /// Custom API key environment variable name.
    pub api_key_env: Option<String>,
    /// Silence detection threshold in milliseconds before ending a speech turn. Default 800.
    pub silence_threshold_ms: u64,
    /// Enable barge-in (interrupt playback when user starts speaking or says wake word). Default true.
    pub enable_barge_in: bool,
    /// Automatically submit transcribed voice input (`true`, default) or place in the input box to verify before sending (`false`).
    pub auto_send: bool,
    /// Audio input device name (None = default system microphone).
    pub input_device: Option<String>,
    /// Audio output device name (None = default system speaker).
    pub output_device: Option<String>,
}

impl Default for VoiceSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            provider: "antigravity".to_string(),
            wake_word: "Luna".to_string(),
            voice_name: "Aoede".to_string(),
            stt_model: String::new(),
            tts_model: String::new(),
            base_url: None,
            api_key_env: None,
            silence_threshold_ms: 800,
            enable_barge_in: true,
            auto_send: true,
            input_device: None,
            output_device: None,
        }
    }
}

impl VoiceSettings {
    pub fn validate(&self) -> Result<(), HarnessError> {
        if self.silence_threshold_ms < 100 || self.silence_threshold_ms > 10000 {
            return Err(HarnessError::ConfigurationError(format!(
                "voice.silence_threshold_ms must be between 100 and 10000, got {}",
                self.silence_threshold_ms
            )));
        }
        Ok(())
    }
}

/// Lifecycle-hook settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HooksSettings {
    /// Run hooks at all. Default `false`: an install that never opted in runs nothing.
    pub enabled: bool,
    /// Allow `<workspace>/.luna/hooks.json` (repository-supplied hooks) to run as well.
    pub trust_workspace: bool,
    /// A hook that has not finished within this window is killed. A gating event treats that as
    /// a denial; an observability event only records it.
    pub timeout_secs: u64,
    /// Event names to skip, e.g. `["post_tool_use"]`.
    pub disabled: Vec<String>,
}

impl Default for HooksSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            trust_workspace: false,
            timeout_secs: 30,
            disabled: Vec::new(),
        }
    }
}

/// Per-turn file checkpoint settings (see [`crate::checkpoint`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CheckpointSettings {
    /// Snapshot files before a tool writes them.
    pub enabled: bool,
    /// Turns kept per session; the oldest are pruned.
    pub keep_turns: usize,
    /// Watch the workspace for changes no file tool recorded.
    pub track_external: bool,
    /// Directory names the external scan skips. Matched against any path component.
    pub ignore: Vec<String>,
}

impl Default for CheckpointSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            keep_turns: crate::checkpoint::DEFAULT_KEEP_TURNS,
            track_external: true,
            ignore: crate::checkpoint::DEFAULT_IGNORED_DIRS
                .iter()
                .map(|dir| dir.to_string())
                .collect(),
        }
    }
}

/// Which provider and model `luna` talks to.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelSettings {
    pub provider: String,
    pub default: String,
}

/// Who writes durable memory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryMode {
    /// The session's agent writes memories with its own tool calls (default).
    #[default]
    Session,
    /// An independent saver distils the conversation after the turn.
    Background,
}

impl MemoryMode {
    /// Every accepted spelling, for error messages and the setup flow.
    pub const VALUES: [&'static str; 2] = ["session", "background"];

    pub fn as_str(self) -> &'static str {
        match self {
            MemoryMode::Session => "session",
            MemoryMode::Background => "background",
        }
    }

    /// Whether an independent saver should be attached to the session.
    pub fn is_background(self) -> bool {
        matches!(self, MemoryMode::Background)
    }
}

impl std::fmt::Display for MemoryMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for MemoryMode {
    type Err = String;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "session" | "agent" | "main" | "inline" => Ok(MemoryMode::Session),
            "background" | "saver" | "independent" | "auto" => Ok(MemoryMode::Background),
            other => Err(format!(
                "unknown memory mode '{other}': expected one of {}",
                Self::VALUES.join(", ")
            )),
        }
    }
}

/// Memory and note persistence: who writes it, and with what budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemorySettings {
    pub mode: MemoryMode,
    pub background_notes: bool,
    pub background_every_turns: usize,
    pub background_tail_messages: usize,
    pub background_model: Option<String>,
    pub background_provider: Option<String>,
    pub background_two_pass: bool,
    pub background_min_confidence: f32,
    pub background_max_memory_writes: usize,
    pub background_max_note_writes: usize,
    pub background_max_operations: usize,
    pub background_idle_secs: u64,
    pub store: MemoryStoreSettings,
}

impl Default for MemorySettings {
    fn default() -> Self {
        Self {
            mode: MemoryMode::Session,
            background_notes: true,
            background_every_turns: 3,
            background_tail_messages: 24,
            background_model: None,
            background_provider: None,
            background_two_pass: true,
            background_min_confidence: 0.65,
            background_max_memory_writes: 3,
            background_max_note_writes: 2,
            background_max_operations: 6,
            background_idle_secs: 15,
            store: MemoryStoreSettings::default(),
        }
    }
}

/// Markdown memory store settings. See `docs/MEMORY_STORE.md`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryStoreSettings {
    pub enabled: bool,
    pub embedder: String,
    pub chunk_chars: usize,
    pub index_chars: usize,
    pub index_scan_chars: usize,
    pub hybrid_vector_weight: f32,
    pub hybrid_bm25_weight: f32,
    pub index_boost: f32,
    pub rerank: String,
    pub auto_mine: bool,
    pub hooks_auto_save: bool,
    pub hooks_save_interval: usize,
    pub write_routing: String,
    pub shard_after: usize,
}

impl Default for MemoryStoreSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            embedder: "hash".to_string(),
            chunk_chars: 800,
            index_chars: 1_500,
            index_scan_chars: 5_000,
            hybrid_vector_weight: 0.6,
            hybrid_bm25_weight: 0.4,
            index_boost: 0.1,
            rerank: "off".to_string(),
            auto_mine: false,
            hooks_auto_save: true,
            hooks_save_interval: 10,
            write_routing: "direct".to_string(),
            shard_after: 5_000,
        }
    }
}

/// Runtime state locations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PathsSettings {
    pub memory_db: PathBuf,
    pub memory_dir: PathBuf,
    pub notes_db: PathBuf,
    pub notes_dir: PathBuf,
    pub sessions_db: PathBuf,
    pub credentials: PathBuf,
    pub models_cache: PathBuf,
    pub workspace_root: PathBuf,
    pub jobs_dir: PathBuf,
    pub skills_dir: PathBuf,
    pub profiles_dir: PathBuf,
    pub checkpoints_dir: PathBuf,
    pub usage_db: PathBuf,
    pub todos_dir: PathBuf,
    pub extra_roots: Vec<PathBuf>,
}

impl Default for PathsSettings {
    fn default() -> Self {
        let luna = home_dir().join(".luna");
        Self {
            memory_db: luna.join("memory.db"),
            memory_dir: luna.join("memory"),
            notes_db: luna.join("notes.db"),
            notes_dir: luna.join("notes"),
            sessions_db: luna.join("sessions.db"),
            credentials: luna.join("credentials.json"),
            models_cache: luna.join("models_cache.json"),
            workspace_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            jobs_dir: luna.join("jobs"),
            skills_dir: luna.join("skills"),
            profiles_dir: luna.join("profiles"),
            checkpoints_dir: luna.join("checkpoints"),
            usage_db: luna.join("usage.db"),
            todos_dir: luna.join("todos"),
            extra_roots: Vec::new(),
        }
    }
}

/// Per-provider override of the built-in provider catalog (`harness_providers::catalog`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderOverride {
    pub base_url: Option<String>,
    pub models_endpoint: Option<String>,
    pub timeout_secs: Option<u64>,
    pub context_window: Option<u32>,
}

/// Tool-run settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolsSettings {
    pub approval: ApprovalMode,
    pub approval_timeout_secs: u64,
    pub approval_provider: Option<String>,
    pub approval_model: Option<String>,
    pub approval_reasoning_effort: Option<ReasoningEffort>,
    pub mutating_overrides: Vec<String>,
    pub network: crate::tools::netguard::NetworkPolicy,
    pub read_max_chars: usize,
    pub terminal_timeout_secs: u64,
    pub terminal_output_max_bytes: usize,
    pub terminal_prefix: Option<String>,
    pub terminal_allowlist: Option<Vec<String>>,
    pub rtk_rewrite: bool,
    pub enabled: Vec<String>,
    pub permissions: Vec<crate::permission::PermissionRule>,
    pub web: WebSettings,
}

impl Default for ToolsSettings {
    fn default() -> Self {
        Self {
            approval: ApprovalMode::Ask,
            approval_timeout_secs: DEFAULT_APPROVAL_TIMEOUT_SECS,
            approval_provider: None,
            approval_model: None,
            approval_reasoning_effort: None,
            mutating_overrides: Vec::new(),
            network: crate::tools::netguard::NetworkPolicy::Allow,
            read_max_chars: 100_000,
            terminal_timeout_secs: 300,
            terminal_output_max_bytes: 50_000,
            terminal_prefix: None,
            terminal_allowlist: None,
            rtk_rewrite: true,
            permissions: Vec::new(),
            web: WebSettings::default(),
            enabled: vec![
                "read_file".to_string(),
                "view_image".to_string(),
                "write_file".to_string(),
                "patch".to_string(),
                "search_files".to_string(),
                "list_files".to_string(),
                "todo_list".to_string(),
                "terminal".to_string(),
                "sleep".to_string(),
                "web_search".to_string(),
                "web_fetch".to_string(),
            ],
        }
    }
}

/// Tracing/telemetry settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TelemetrySettings {
    pub json: bool,
    pub level: String,
}

impl Default for TelemetrySettings {
    fn default() -> Self {
        Self {
            json: false,
            level: "harness=info,harness_cli=info".to_string(),
        }
    }
}

/// Turn accounting settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UsageSettings {
    pub enabled: bool,
    pub prices: BTreeMap<String, crate::usage::ModelPrice>,
}

impl Default for UsageSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            prices: BTreeMap::new(),
        }
    }
}

impl UsageSettings {
    pub fn price_table(&self) -> crate::usage::PriceTable {
        crate::usage::PriceTable::with_overrides(&self.prices)
    }
}

/// The launch-time build check.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateSettings {
    pub check: bool,
    pub check_interval_hours: u64,
    pub repo: String,
    pub branch: String,
    pub workflow: String,
    pub artifact: String,
}

impl Default for UpdateSettings {
    fn default() -> Self {
        Self {
            check: true,
            check_interval_hours: 6,
            repo: "daryllvelonio2-bit/luna-".to_string(),
            branch: "main".to_string(),
            workflow: "release-build.yml".to_string(),
            artifact: "luna-binary".to_string(),
        }
    }
}

/// Session replay bounds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionSettings {
    pub resume_max_messages: usize,
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            resume_max_messages: 1000,
        }
    }
}

/// Color theme for the full-screen TUI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    #[default]
    Dark,
    Light,
}

impl ThemeMode {
    pub fn is_light(self) -> bool {
        matches!(self, Self::Light)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Dark,
        }
    }
}

impl std::fmt::Display for ThemeMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

impl std::str::FromStr for ThemeMode {
    type Err = HarnessError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "dark" => Ok(Self::Dark),
            "light" => Ok(Self::Light),
            other => Err(HarnessError::ConfigurationError(format!(
                "theme must be 'dark' or 'light', got '{other}'"
            ))),
        }
    }
}

/// Display preferences for the interactive CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DisplaySettings {
    pub block_limit: Option<usize>,
    pub auto_collapse: bool,
    pub auto_hide_thinking: bool,
    pub theme: ThemeMode,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            block_limit: None,
            auto_collapse: true,
            auto_hide_thinking: true,
            theme: ThemeMode::Dark,
        }
    }
}

/// Autonomous `/goal` and `luna goal` loop defaults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GoalSettings {
    pub max_iterations: usize,
}

impl Default for GoalSettings {
    fn default() -> Self {
        Self {
            max_iterations: crate::goal::DEFAULT_GOAL_MAX_ITERATIONS,
        }
    }
}

/// Persistent timed-disable schedules for optional features.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FeatureSettings {
    pub mcp_disabled_until: Option<u64>,
    pub skills_disabled_until: Option<u64>,
    pub memory_disabled_until: Option<u64>,
    pub notes_disabled_until: Option<u64>,
}

/// Which layer produced a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Default,
    File,
    Env,
    Flag,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Default => "default",
            Source::File => "file",
            Source::Env => "env",
            Source::Flag => "flag",
        }
    }
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Expand `~` at the start of a path to the user's home directory.
pub fn expand_tilde(value: &str) -> String {
    if value == "~" {
        return home_dir().to_string_lossy().into_owned();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home_dir().join(rest).to_string_lossy().into_owned();
    }
    value.to_string()
}

/// User's home directory.
pub fn home_dir() -> PathBuf {
    #[cfg(windows)]
    const CANDIDATES: [&str; 2] = ["USERPROFILE", "HOME"];
    #[cfg(not(windows))]
    const CANDIDATES: [&str; 2] = ["HOME", "USERPROFILE"];

    for key in CANDIDATES {
        if let Ok(home) = std::env::var(key) {
            if !home.trim().is_empty() {
                #[cfg(windows)]
                if key == "HOME" && home.starts_with('/') {
                    continue;
                }
                return PathBuf::from(home);
            }
        }
    }

    #[cfg(windows)]
    if let (Ok(drive), Ok(path)) = (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH")) {
        if !drive.is_empty() && !path.is_empty() {
            return PathBuf::from(format!("{drive}{path}"));
        }
    }

    PathBuf::from(".")
}

/// Expand `~` in every configured path.
pub(crate) fn expand_paths(paths: &mut PathsSettings) {
    let expand = |p: &PathBuf| -> PathBuf { PathBuf::from(expand_tilde(&p.to_string_lossy())) };
    paths.memory_db = expand(&paths.memory_db);
    paths.memory_dir = expand(&paths.memory_dir);
    paths.notes_db = expand(&paths.notes_db);
    paths.notes_dir = expand(&paths.notes_dir);
    paths.sessions_db = expand(&paths.sessions_db);
    paths.credentials = expand(&paths.credentials);
    paths.models_cache = expand(&paths.models_cache);
    paths.workspace_root = expand(&paths.workspace_root);
    paths.jobs_dir = expand(&paths.jobs_dir);
    paths.skills_dir = expand(&paths.skills_dir);
    paths.profiles_dir = expand(&paths.profiles_dir);
    paths.checkpoints_dir = expand(&paths.checkpoints_dir);
    paths.usage_db = expand(&paths.usage_db);
    paths.todos_dir = expand(&paths.todos_dir);
    paths.extra_roots = paths.extra_roots.iter().map(expand).collect();
}
