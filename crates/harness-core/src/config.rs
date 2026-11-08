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
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};

pub mod web;

use crate::agent::AgentConfig;
use crate::approval::{ApprovalMode, DEFAULT_APPROVAL_TIMEOUT_SECS};
use crate::error::HarnessError;
use crate::types::ReasoningEffort;
pub use web::{
    web_tools_blocked_reason, SearchBackend, WebSettings, DEFAULT_WEB_USER_AGENT, SEARCH_BACKENDS,
    WEB_CACHE_CEILING_SECS, WEB_FETCH_MAX_BYTES_CEILING, WEB_FETCH_MAX_CHARS_CEILING,
    WEB_FETCH_MIN_BYTES, WEB_FETCH_MIN_CHARS, WEB_MAX_REDIRECT_CEILING,
    WEB_SEARCH_MAX_RESULTS_CEILING, WEB_TIMEOUT_CEILING_SECS,
};

/// Environment variable that relocates the config file.
pub const CONFIG_PATH_ENV: &str = "LUNA_CONFIG";
/// Home-relative location of the config file.
pub const CONFIG_FILE_RELATIVE: &str = ".luna/config.yaml";
/// Prefix of every environment override.
pub const ENV_PREFIX: &str = "LUNA_";
/// Fields that may appear under `providers.<id>`. Kept in sync with [`ProviderOverride`] by
/// `provider_fields_match_the_struct`.
const PROVIDER_FIELDS: [&str; 3] = ["base_url", "models_endpoint", "timeout_secs"];
/// Width of the edit-distance bucket used when suggesting a key for a typo (see
/// `nearest_key_error`).
const DISTANCE_BUCKET: usize = 2;

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
    /// Deprecated: the persona the old onboarding flow used to select. Accepted so a config
    /// written before profiles still loads, then ignored and dropped on the next write. Profile
    /// selection is invocation-based now, so nothing reads this.
    #[serde(default, skip_serializing)]
    pub persona: serde_yaml::Value,
    /// `false` once the first-run onboarding has completed.
    ///
    /// Defaults to `true`, so an install with no config file — and a config file that predates the
    /// onboarding — both take the tour. `LUNA_FIRST_RUN=false` skips it through the env layer
    /// without touching the file.
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
    ///
    /// A worker is a saved provider/model pair: `/worker carbon` writes one, and
    /// `/carbon fix this` runs it on a fork of the current conversation.
    #[serde(default)]
    pub workers: BTreeMap<String, WorkerProfile>,
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

/// Written by hand rather than derived so `first_run` starts at `true`. A derived `Default` would
/// report `false`, which would silently skip onboarding on a machine with no config file.
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
        }
    }
}

impl Config {
    /// Validate configuration invariants (e.g. thresholds, bounds).
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
        Ok(())
    }
}

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
    /// Name of the environment variable holding the Meta app secret, which is what signs webhook
    /// deliveries. A secret in a config file is a secret in a backup and a shell history, so the
    /// config names where to read it from instead. Defaults to `WHATSAPP_APP_SECRET`.
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

/// Model Context Protocol (MCP) integration.
///
/// Each entry under `servers` is a stdio MCP server the harness spawns once per session. The
/// tools it advertises are registered under `mcp__<server>__<tool>`, so two servers may expose the
/// same tool name without colliding. `enabled: false` (or an empty `servers` list) means no process
/// is ever spawned — a config file with no `mcp` section behaves exactly as before.
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

/// One stdio MCP server.
///
/// `name` is the namespace in the registered tool name (`mcp__<name>__<tool>`); `command` + `args`
/// are the process the harness spawns. `env` is added to the inherited environment rather than
/// replacing it, so a server that needs `PATH`/`HOME` still finds them.
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

/// Lifecycle-hook settings.
///
/// Hooks execute shell commands on harness events, so they are opt-in and split by trust: the
/// user file (`~/.luna/hooks.json`) is authored by the machine's owner and runs when `enabled`;
/// the project file (`<workspace>/.luna/hooks.json`) travels with a repository and only runs when
/// `trust_workspace` is also set. A cloned repo cannot configure code execution for itself.
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
///
/// Enabled by default: undo is a safety net, not an experiment. What it costs is bounded — only
/// files a `write_file`/`patch` call is about to touch are copied, and only the newest
/// `keep_turns` turns per session are kept.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CheckpointSettings {
    /// Snapshot files before a tool writes them.
    pub enabled: bool,
    /// Turns kept per session; the oldest are pruned.
    pub keep_turns: usize,
    /// Watch the workspace for changes no file tool recorded, so a turn that ran `git checkout`
    /// or `sed -i` is named in the review instead of being invisible.
    ///
    /// Metadata only (path, size, mtime) — no blobs, so this costs a directory walk and not disk.
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
///
/// `Session` is the original design: the agent decides, mid-turn, to call `save_memory` /
/// `save_note`, so a record exists exactly when the model chose to write one. `Background` hands
/// that job to a separate saver that reads the finished conversation and writes the same records
/// through the same tools — same output, decided by a second pass instead of by the turn.
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

    /// Accepts the canonical spellings plus the words people reach for first ("agent", "main",
    /// "auto"), because this value is typed at a prompt as often as it is written in a file.
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
///
/// The *engines* are chosen by `paths.memory_db` (memory records live in SQLite) and
/// `paths.notes_dir` (notes live as markdown files); these keys only decide who drives them. Both
/// modes write through the same `save_memory` / `save_note` code path, so switching modes changes
/// when a memory appears, not what a memory looks like.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemorySettings {
    /// `session` (default) | `background`.
    ///
    /// `background` is wired by the interactive session (`luna chat`), which attaches the saver and
    /// withholds `save_memory` / `save_note` so the store has one writer. A one-shot `luna exec`
    /// has no post-turn pass to hand writing to, so it always keeps the session's own tools.
    pub mode: MemoryMode,
    /// Also let the background saver write notes, not just memory records.
    pub background_notes: bool,
    /// Distil after every Nth completed turn. `3` is the default: overlapping tails re-derive the
    /// same facts, and `/remember` is there when the user wants a write immediately. `0` disables
    /// the saver.
    pub background_every_turns: usize,
    /// How many trailing messages the saver reads. A saver that sees the whole session would
    /// re-derive the same facts on every turn and pay for the tokens twice.
    pub background_tail_messages: usize,
    /// Model used by the saver; `None` means the session's model.
    pub background_model: Option<String>,
    /// Provider used by the saver; `None` means the session's provider.
    pub background_provider: Option<String>,
    /// Split the pass into extract + reconcile once the store holds more than ~20 items.
    ///
    /// One call is cheaper; two calls make better decisions because the curator sees only real
    /// candidates and a full inventory instead of doing both jobs in one prompt.
    pub background_two_pass: bool,
    /// Refuse an operation whose confidence is below this.
    pub background_min_confidence: f32,
    /// Per-pass cap on memory writes (adds, updates, merges).
    pub background_max_memory_writes: usize,
    /// Per-pass cap on note writes (creates, updates, merges).
    pub background_max_note_writes: usize,
    /// Per-pass cap on every operation, including deletes.
    pub background_max_operations: usize,
    /// Run one extra pass when the session has been idle this many seconds, so a short session
    /// still saves before the every-N-turns interval fires. `0` disables idle passes.
    pub background_idle_secs: u64,
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
            background_idle_secs: 90,
        }
    }
}

/// Runtime state locations. These are the same paths the engines used to compute for themselves,
/// so a missing config file resolves to exactly the previous behaviour.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PathsSettings {
    pub memory_db: PathBuf,
    /// Legacy note store. Notes are files now; this database is only read once, to import the
    /// notes an older install left behind.
    pub notes_db: PathBuf,
    /// Where notes are written: one markdown file per note, so the user can read them outside the
    /// harness. Chosen during onboarding and changeable at any time with
    /// `luna config set paths.notes_dir <path>`.
    pub notes_dir: PathBuf,
    pub sessions_db: PathBuf,
    pub credentials: PathBuf,
    pub models_cache: PathBuf,
    /// Scope every filesystem/shell tool is confined to. Defaults to the process working
    /// directory.
    pub workspace_root: PathBuf,
    pub jobs_dir: PathBuf,
    pub skills_dir: PathBuf,
    /// Where custom profiles live: one self-contained directory per profile.
    pub profiles_dir: PathBuf,
    /// Per-turn file snapshots that `/rewind` restores from.
    pub checkpoints_dir: PathBuf,
    /// One row per turn: tokens, model, duration, cost. See `usage.rs`.
    pub usage_db: PathBuf,
    /// One file per session: the todo list of that conversation. A list belongs to the
    /// session that made it, so two conversations in the same workspace never share one.
    pub todos_dir: PathBuf,
    /// Roots the filesystem and shell tools may reach *besides* `workspace_root`.
    ///
    /// Empty by default: the workspace is the sandbox. An operator adds e.g. `/tmp` or a scratch
    /// volume here, and every tool resolves through the same list (`tools::sandbox::Sandbox`).
    pub extra_roots: Vec<PathBuf>,
}

impl Default for PathsSettings {
    fn default() -> Self {
        let luna = home_dir().join(".luna");
        Self {
            memory_db: luna.join("memory.db"),
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
/// `None` means "leave the catalog value alone".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProviderOverride {
    pub base_url: Option<String>,
    pub models_endpoint: Option<String>,
    pub timeout_secs: Option<u64>,
    /// Context window to assume for this provider's models that no source can describe: the ones
    /// its catalog publishes no window for and the model-family heuristics do not recognise.
    ///
    /// The escape hatch for a model released after this build. Providers that publish their own
    /// windows (OpenRouter, Mistral) never reach this value, and neither does any model the
    /// heuristics know — so setting it is a statement about the provider's *unknown* models only,
    /// which is why it wants a window those models all share rather than the largest one.
    pub context_window: Option<u32>,
}

/// Tool-run settings.
///
/// `approval*` and `mutating_overrides` are the human-in-the-loop gate's knobs: they decide
/// *whether* a mutating call runs. They are deliberately separate from the sandbox's reach
/// (`paths.workspace_root`, and the sandbox mode a later card adds) — a one-keypress prompt is not
/// a boundary, and a boundary is not a human decision.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ToolsSettings {
    /// `ask` (prompt, the default) | `allow` (never prompt) | `deny` (never run a mutation).
    pub approval: ApprovalMode,
    /// No answer within this window counts as a denial.
    pub approval_timeout_secs: u64,
    /// Tool names the gate must treat as mutating even when the tool says otherwise.
    pub mutating_overrides: Vec<String>,
    /// Whether a `terminal` child may open sockets other than `AF_UNIX`.
    ///
    /// `deny` is enforced by seccomp inside the child (inherited by everything it spawns), not by
    /// inspecting the command string. See `tools::netguard`.
    pub network: crate::tools::netguard::NetworkPolicy,
    /// Maximum character budget for read_file before truncation.
    pub read_max_chars: usize,
    /// Terminal command execution timeout in seconds.
    pub terminal_timeout_secs: u64,
    /// Maximum output bytes captured from terminal.
    pub terminal_output_max_bytes: usize,
    /// Optional prefix binary or command to prepend to shell commands (e.g. `~/.local/bin/rtk`).
    pub terminal_prefix: Option<String>,
    /// Whether to automatically rewrite shell commands using RTK (`rtk rewrite`) when available.
    pub rtk_rewrite: bool,
    /// Enabled tool names.
    pub enabled: Vec<String>,
    /// Standing policy applied before the human gate: `deny` > `ask` > `allow`.
    pub permissions: Vec<crate::permission::PermissionRule>,
    /// Web access: the `web_search` / `web_fetch` pair. See [`WebSettings`].
    pub web: WebSettings,
}

impl Default for ToolsSettings {
    fn default() -> Self {
        Self {
            approval: ApprovalMode::Ask,
            approval_timeout_secs: DEFAULT_APPROVAL_TIMEOUT_SECS,
            mutating_overrides: Vec::new(),
            network: crate::tools::netguard::NetworkPolicy::Allow,
            read_max_chars: 100_000,
            terminal_timeout_secs: 300,
            terminal_output_max_bytes: 50_000,
            terminal_prefix: None,
            rtk_rewrite: true,
            permissions: Vec::new(),
            web: WebSettings::default(),
            enabled: vec![
                "read_file".to_string(),
                "write_file".to_string(),
                "patch".to_string(),
                "search_files".to_string(),
                "list_files".to_string(),
                "todo_list".to_string(),
                "terminal".to_string(),
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
    /// JSON log lines instead of compact ones. `--json` also forces this on.
    pub json: bool,
    /// A `tracing_subscriber::EnvFilter` directive string. The default is the filter this binary
    /// hardcoded before the key existed, so nothing changes for an absent config file.
    /// `RUST_LOG` still takes precedence, as it is the ecosystem-wide env layer.
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
///
/// `prices` layers over the built-in table in [`crate::usage::PriceTable`]; a model with no price
/// is recorded as unpriced rather than guessed at. `enabled: false` turns recording off entirely —
/// a machine that must not leave a spending trail can say so.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UsageSettings {
    /// Recording is on by default: a spending report you have to opt into is a spending report
    /// nobody has.
    pub enabled: bool,
    /// `provider/model` (or `provider/*`, `*/model`) → price per million tokens.
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
///
/// Builds are produced by a GitHub Actions workflow, so the newest published binary is the newest
/// successful run of that workflow. The check compares the commit the binary was built from (baked
/// in by `harness-cli/build.rs`) with that run's commit and prints one line when they differ.
///
/// It is deliberately cheap: the answer is cached for `check_interval_hours`, one request times
/// out after a couple of seconds, and any failure is silent. A launch notice must never stand
/// between the user and the composer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UpdateSettings {
    /// Master switch. `false` makes `luna` launch without any network call.
    pub check: bool,
    /// How long a cached answer is reused before asking GitHub again. `0` checks every launch.
    pub check_interval_hours: u64,
    /// The repository whose workflow publishes the binary, as `owner/repo`.
    pub repo: String,
    /// The branch the workflow builds.
    pub branch: String,
    /// Workflow file name (or numeric id) that produces the binary.
    pub workflow: String,
    /// Name of the uploaded artifact inside that workflow run.
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

/// Session replay bounds. Compaction thresholds live under `agent.*` because the agent loop
/// applies them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SessionSettings {
    /// Most messages `luna chat --session <id>` replays from the session transcript.
    pub resume_max_messages: usize,
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            resume_max_messages: 200,
        }
    }
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

/// The four inputs to [`resolve`]. Fields are public so tests — and callers that hold a provider
/// catalog — can state the exact layer set they mean.
#[derive(Debug, Clone)]
pub struct Layers {
    /// Layer 1. Overlay catalog-derived `providers.<id>.*` entries here.
    pub defaults: Config,
    /// Layer 2. `None` means the file layer does not exist at all.
    pub file: Option<PathBuf>,
    /// Layer 2b. A profile's own `config.yaml`, overlaid after the global file and before the
    /// environment. `None` for the built-in `default` profile.
    pub profile: Option<PathBuf>,
    /// Layer 3, already mapped to dotted keys (see [`env_overrides`]).
    pub env: BTreeMap<String, String>,
    /// Layer 4, as dotted keys.
    pub flags: BTreeMap<String, String>,
}

/// The layers a real process has: `defaults`, the `$LUNA_CONFIG` file, and every `LUNA_*` variable
/// that maps onto a key known to `defaults`.
pub fn process_layers(defaults: Config) -> Result<Layers, HarnessError> {
    let env = env_overrides(&defaults)?;
    Ok(Layers {
        defaults,
        file: Some(config_file_path()),
        profile: None,
        env,
        flags: BTreeMap::new(),
    })
}

/// `$LUNA_CONFIG` if it is set and non-empty, else `~/.luna/config.yaml`.
pub fn config_file_path() -> PathBuf {
    if let Ok(raw) = std::env::var(CONFIG_PATH_ENV) {
        if !raw.trim().is_empty() {
            return PathBuf::from(expand_tilde(raw.trim()));
        }
    }
    home_dir().join(CONFIG_FILE_RELATIVE)
}

/// `model.default` -> `LUNA_MODEL_DEFAULT`, `providers.nvidia.base_url` ->
/// `LUNA_PROVIDERS_NVIDIA_BASE_URL`.
pub fn env_var_name(key: &str) -> String {
    let mut name = String::from(ENV_PREFIX);
    for ch in key.chars() {
        match ch {
            '.' | '-' => name.push('_'),
            c => name.push(c.to_ascii_uppercase()),
        }
    }
    name
}

/// Every `LUNA_*` variable that maps onto a key of `defaults`. Provider ids are only known from
/// the catalog overlay the caller puts in `defaults`, which is why the CLI injects it before
/// resolving: that is what makes `LUNA_PROVIDERS_NVIDIA_BASE_URL` work.
pub fn env_overrides(defaults: &Config) -> Result<BTreeMap<String, String>, HarnessError> {
    let known = flatten_schema(&to_value(defaults)?);
    let mut out = BTreeMap::new();
    for key in known.keys() {
        if let Ok(value) = std::env::var(env_var_name(key)) {
            out.insert(key.clone(), value);
        }
    }
    Ok(out)
}

/// Apply the precedence order and report both values and sources.
pub fn resolve(layers: &Layers) -> Result<Resolved, HarnessError> {
    let defaults = to_value(&layers.defaults)?;
    let known = flatten_schema(&defaults);
    let empty = || Value::Mapping(Mapping::new());

    let file = match layers.file.as_deref() {
        Some(path) => read_file_value(path)?.unwrap_or_else(empty),
        None => empty(),
    };
    let profile = match layers.profile.as_deref() {
        Some(path) => read_file_value(path)?.unwrap_or_else(empty),
        None => empty(),
    };
    let env = overlay_value(&layers.env, &known)?;
    let flag = overlay_value(&layers.flags, &known)?;

    let mut merged = defaults;
    deep_merge(&mut merged, file.clone());
    deep_merge(&mut merged, profile.clone());
    deep_merge(&mut merged, env.clone());
    deep_merge(&mut merged, flag.clone());

    let mut config: Config = from_value(&merged)?;
    expand_paths(&mut config.paths);

    let mut from_file = flatten(&file);
    for (key, value) in flatten(&profile) {
        from_file.insert(key, value);
    }
    let from_env = flatten(&env);
    let from_flag = flatten(&flag);

    // Sync auto-compact config between top-level and agent section
    if from_flag.contains_key("auto_compact_enabled")
        || from_env.contains_key("auto_compact_enabled")
        || from_file.contains_key("auto_compact_enabled")
    {
        config.agent.auto_compact_enabled = config.auto_compact_enabled;
    } else if from_flag.contains_key("agent.auto_compact_enabled")
        || from_env.contains_key("agent.auto_compact_enabled")
        || from_file.contains_key("agent.auto_compact_enabled")
    {
        config.auto_compact_enabled = config.agent.auto_compact_enabled;
    } else {
        config.agent.auto_compact_enabled = config.auto_compact_enabled;
    }

    if from_flag.contains_key("auto_compact_threshold")
        || from_env.contains_key("auto_compact_threshold")
        || from_file.contains_key("auto_compact_threshold")
    {
        config.agent.auto_compact_threshold = config.auto_compact_threshold;
    } else if from_flag.contains_key("agent.auto_compact_threshold")
        || from_env.contains_key("agent.auto_compact_threshold")
        || from_file.contains_key("agent.auto_compact_threshold")
    {
        config.auto_compact_threshold = config.agent.auto_compact_threshold;
    } else {
        config.agent.auto_compact_threshold = config.auto_compact_threshold;
    }

    if from_flag.contains_key("auto_compact_percent")
        || from_env.contains_key("auto_compact_percent")
        || from_file.contains_key("auto_compact_percent")
    {
        config.agent.auto_compact_percent = config.auto_compact_percent;
    } else if from_flag.contains_key("agent.auto_compact_percent")
        || from_env.contains_key("agent.auto_compact_percent")
        || from_file.contains_key("agent.auto_compact_percent")
    {
        config.auto_compact_percent = config.agent.auto_compact_percent;
    } else {
        config.agent.auto_compact_percent = config.auto_compact_percent;
    }

    if from_flag.contains_key("auto_compact_turns")
        || from_env.contains_key("auto_compact_turns")
        || from_file.contains_key("auto_compact_turns")
    {
        config.agent.auto_compact_turns = config.auto_compact_turns;
    } else if from_flag.contains_key("agent.auto_compact_turns")
        || from_env.contains_key("agent.auto_compact_turns")
        || from_file.contains_key("agent.auto_compact_turns")
    {
        config.auto_compact_turns = config.agent.auto_compact_turns;
    } else {
        config.agent.auto_compact_turns = config.auto_compact_turns;
    }

    config.validate()?;

    let values: BTreeMap<String, String> = flatten(&to_value(&config)?)
        .into_iter()
        .map(|(key, value)| (key, scalar_to_string(&value)))
        .collect();

    let mut sources = BTreeMap::new();
    for key in values.keys() {
        let source = if from_flag.contains_key(key) {
            Source::Flag
        } else if from_env.contains_key(key) {
            Source::Env
        } else if from_file.contains_key(key) {
            Source::File
        } else {
            Source::Default
        };
        sources.insert(key.clone(), source);
    }

    Ok(Resolved {
        config,
        values,
        sources,
        known,
    })
}

/// Effective configuration plus provenance.
#[derive(Debug, Clone)]
pub struct Resolved {
    config: Config,
    values: BTreeMap<String, String>,
    sources: BTreeMap<String, Source>,
    known: BTreeMap<String, Value>,
}

impl Resolved {
    pub fn config(&self) -> &Config {
        &self.config
    }

    pub fn into_config(self) -> Config {
        self.config
    }

    /// The effective value of `key`; an unknown key is an error naming the nearest known one.
    pub fn get(&self, key: &str) -> Result<&str, HarnessError> {
        match self.values.get(key) {
            Some(value) => Ok(value.as_str()),
            None => {
                if is_known_key(key, &self.known) {
                    Ok("")
                } else {
                    Err(self.unknown_key(key))
                }
            }
        }
    }

    /// The layer that produced `key`.
    pub fn source(&self, key: &str) -> Result<Source, HarnessError> {
        match self.sources.get(key) {
            Some(source) => Ok(*source),
            None => {
                if is_known_key(key, &self.known) {
                    Ok(Source::Default)
                } else {
                    Err(self.unknown_key(key))
                }
            }
        }
    }

    /// Every effective key, sorted, with its value and layer.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &str, Source)> {
        self.values.iter().map(move |(key, value)| {
            let source = self.sources.get(key).copied().unwrap_or(Source::Default);
            (key.as_str(), value.as_str(), source)
        })
    }

    /// A key that no layer set is still "known" when the schema knows it, which is what
    /// `config set providers.<id>.timeout_secs` relies on.
    pub fn knows(&self, key: &str) -> bool {
        self.values.contains_key(key) || is_known_key(key, &self.known)
    }

    /// A key that no layer set: report the nearest known key rather than accepting a typo.
    fn unknown_key(&self, key: &str) -> HarnessError {
        let mut candidates: Vec<&str> = self.known.keys().map(String::as_str).collect();
        candidates.extend(self.values.keys().map(String::as_str));
        candidates.sort_unstable();
        candidates.dedup();
        nearest_key_error(key, &candidates)
    }
}

/// Write `key = raw` into the config file, creating parent directories and the file as needed.
/// The value is coerced to the key's type and the whole resulting file is validated before it is
/// written, so a `set` can never leave behind a file that `resolve` would reject.
/// Returns the path written.
pub fn set_in_file(layers: &Layers, key: &str, raw: &str) -> Result<PathBuf, HarnessError> {
    let known = flatten_schema(&to_value(&layers.defaults)?);
    if !is_known_key(key, &known) {
        let candidates: Vec<&str> = known.keys().map(String::as_str).collect();
        return Err(nearest_key_error(key, &candidates));
    }
    let value = coerce(key, raw, known.get(key))?;

    let path = layers.file.clone().ok_or_else(|| {
        HarnessError::ConfigurationError(
            "no config file location: set LUNA_CONFIG or HOME".to_string(),
        )
    })?;

    let mut root = read_file_value(&path)?.unwrap_or_else(|| Value::Mapping(Mapping::new()));
    set_path(&mut root, key, value)?;

    let mut check = to_value(&layers.defaults)?;
    deep_merge(&mut check, root.clone());
    // Reject anything that would not load (a type the key does not accept, a corrupted sibling
    // key) while the original file is still untouched.
    let check_cfg: Config = from_value(&check)?;
    check_cfg.validate()?;

    write_file_value(&path, &root)?;
    Ok(path)
}

/// Write one named worker profile into the config file.
///
/// Dynamic map keys (`workers.<name>.*`) are not part of the static flat schema behind
/// [`set_in_file`], so this writes the map entry directly and then validates the merged file as a
/// whole before touching it. A typo in a neighbouring key still fails the write.
pub fn set_worker_in_file(
    layers: &Layers,
    name: &str,
    profile: &WorkerProfile,
) -> Result<PathBuf, HarnessError> {
    validate_worker_name(name)?;
    let path = layers.file.clone().ok_or_else(|| {
        HarnessError::ConfigurationError(
            "no config file location: set LUNA_CONFIG or HOME".to_string(),
        )
    })?;

    let mut root = read_file_value(&path)?.unwrap_or_else(|| Value::Mapping(Mapping::new()));
    let value = to_value(profile)?;
    set_path(&mut root, &format!("workers.{name}"), value)?;

    let mut check = to_value(&layers.defaults)?;
    deep_merge(&mut check, root.clone());
    let check_cfg: Config = from_value(&check)?;
    check_cfg.validate()?;

    write_file_value(&path, &root)?;
    Ok(path)
}

/// Remove one named worker profile from the config file.
///
/// Returns `Some(path)` when an entry was removed and the file rewritten, `None` when the worker
/// was not present or the file does not exist.
pub fn remove_worker_from_file(
    layers: &Layers,
    name: &str,
) -> Result<Option<PathBuf>, HarnessError> {
    let path = match &layers.file {
        Some(path) => path.clone(),
        None => return Ok(None),
    };
    if !path.exists() {
        return Ok(None);
    }
    let mut root = match read_file_value(&path)? {
        Some(root) => root,
        None => return Ok(None),
    };
    if !remove_path(&mut root, &format!("workers.{name}")) {
        return Ok(None);
    }

    let mut check = to_value(&layers.defaults)?;
    deep_merge(&mut check, root.clone());
    let check_cfg: Config = from_value(&check)?;
    check_cfg.validate()?;

    write_file_value(&path, &root)?;
    Ok(Some(path))
}

/// Remove `key` from the config file if present.
/// The resulting configuration is validated against defaults before writing back.
/// Returns `Some(path)` if a key was removed and written, or `None` if the key was not present
/// or the file does not exist.
pub fn unset_in_file(layers: &Layers, key: &str) -> Result<Option<PathBuf>, HarnessError> {
    let path = match &layers.file {
        Some(p) => p.clone(),
        None => return Ok(None),
    };
    if !path.exists() {
        return Ok(None);
    }
    let mut root = match read_file_value(&path)? {
        Some(r) => r,
        None => return Ok(None),
    };
    if !remove_path(&mut root, key) {
        return Ok(None);
    }

    let mut check = to_value(&layers.defaults)?;
    deep_merge(&mut check, root.clone());
    let check_cfg: Config = from_value(&check)?;
    check_cfg.validate()?;

    write_file_value(&path, &root)?;
    Ok(Some(path))
}

fn remove_path(root: &mut Value, key: &str) -> bool {
    let mut segments = key.split('.');
    let first = segments.next().unwrap_or_default();
    if first.is_empty() {
        return false;
    }
    let rest: Vec<&str> = segments.collect();

    if let Some(mapping) = root.as_mapping_mut() {
        let key_val = Value::String(first.to_string());
        if rest.is_empty() {
            return mapping.remove(&key_val).is_some();
        } else if let Some(child) = mapping.get_mut(&key_val) {
            let removed = remove_path(child, &rest.join("."));
            let is_empty = child.as_mapping().is_some_and(|m| m.is_empty());
            if is_empty {
                mapping.remove(&key_val);
            }
            return removed;
        }
    }
    false
}

/// `~` and `~/…` against the platform home directory.
pub fn expand_tilde(value: &str) -> String {
    let home = home_dir();
    if value == "~" {
        return home.to_string_lossy().into_owned();
    }
    for prefix in ["~/", "~\\"] {
        if let Some(rest) = value.strip_prefix(prefix) {
            return home.join(rest).to_string_lossy().into_owned();
        }
    }
    value.to_string()
}

/// The platform's home directory.
///
/// On Windows that is `%USERPROFILE%` first, because a `HOME` inherited from Git Bash or MSYS is
/// usually a POSIX-style path (`/c/Users/me`) that `std::fs` cannot open; `%HOMEDRIVE%%HOMEPATH%`
/// is the last resort. Everywhere else it is `$HOME` (`$USERPROFILE` only if `HOME` is unset).
///
/// Every runtime path (`~/.luna`, the credential store, the session database) is derived from
/// this one function rather than from a literal `/home/...`, so the same build runs on Windows.
pub fn home_dir() -> PathBuf {
    #[cfg(windows)]
    const CANDIDATES: [&str; 2] = ["USERPROFILE", "HOME"];
    #[cfg(not(windows))]
    const CANDIDATES: [&str; 2] = ["HOME", "USERPROFILE"];

    for key in CANDIDATES {
        if let Ok(home) = std::env::var(key) {
            if !home.trim().is_empty() {
                // An MSYS-style `HOME` (`/c/Users/me`) is not a Windows path; fall through to
                // HOMEDRIVE/HOMEPATH rather than producing a path that does not exist.
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

/// A `providers.<id>.<field>` key, if that is what this is.
fn provider_field(key: &str) -> Option<&str> {
    let mut segments = key.split('.');
    if segments.next()? != "providers" {
        return None;
    }
    let id = segments.next()?;
    let field = segments.next()?;
    if id.is_empty() || segments.next().is_some() {
        return None;
    }
    PROVIDER_FIELDS.contains(&field).then_some(field)
}

fn is_known_key(key: &str, known: &BTreeMap<String, Value>) -> bool {
    known.contains_key(key) || provider_field(key).is_some()
}

/// Nearest known key to a typo.
///
/// Ranking: same section first, then a coarse distance bucket on the leaf names, then alphabetical
/// order for determinism. Distances on short keys are noisy — one extra edit on a seven-letter leaf
/// is not a better hint — so `model.nope` suggests its sibling `model.default` instead of the
/// single-edit-closer `model.provider`.
fn nearest_key_error(key: &str, candidates: &[&str]) -> HarnessError {
    let (leaf, parent) = split_leaf(key);
    let best = candidates
        .iter()
        .map(|candidate| {
            let (candidate_leaf, candidate_parent) = split_leaf(candidate);
            let other_section = usize::from(parent != candidate_parent);
            (
                other_section,
                levenshtein(leaf, candidate_leaf) / DISTANCE_BUCKET,
                *candidate,
            )
        })
        .min();

    match best {
        Some((_, _, nearest)) => HarnessError::ConfigurationError(format!(
            "unknown config key '{key}': the nearest known key is '{nearest}'"
        )),
        None => HarnessError::ConfigurationError(format!("unknown config key '{key}'")),
    }
}

fn split_leaf(key: &str) -> (&str, &str) {
    match key.rfind('.') {
        Some(index) => (&key[index + 1..], &key[..index]),
        None => (key, ""),
    }
}

fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0usize; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(ca != cb);
            current[j + 1] = (previous[j + 1] + 1).min(current[j] + 1).min(substitution);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// Turn a raw string (env var, CLI flag, `config set` argument) into the YAML scalar the key
/// expects. The expected type comes from the key's default, so
/// `LUNA_AGENT_MAX_CONSECUTIVE_FAILURES=3` is the integer 3 and not the string "3".
fn coerce(key: &str, raw: &str, default: Option<&Value>) -> Result<Value, HarnessError> {
    if let Some(field) = provider_field(key) {
        return match field {
            "timeout_secs" => parse_u64(key, raw).map(|value| Value::Number(value.into())),
            _ => Ok(Value::String(raw.to_string())),
        };
    }
    match default {
        Some(Value::Bool(_)) => parse_bool(key, raw).map(Value::Bool),
        Some(Value::Number(number)) if number.is_u64() => {
            parse_u64(key, raw).map(|value| Value::Number(value.into()))
        }
        Some(Value::Number(number)) if number.is_i64() => {
            parse_i64(key, raw).map(|value| Value::Number(value.into()))
        }
        Some(Value::Number(_)) => parse_f64(key, raw).and_then(|value| to_value(&value)),
        Some(Value::String(_)) => Ok(Value::String(if key.starts_with("paths.") {
            expand_tilde(raw)
        } else {
            raw.to_string()
        })),
        _ => Ok(infer_scalar(raw)),
    }
}

fn infer_scalar(raw: &str) -> Value {
    match raw {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        other => match other.parse::<u64>() {
            Ok(number) => Value::Number(number.into()),
            Err(_) => Value::String(other.to_string()),
        },
    }
}

fn parse_bool(key: &str, raw: &str) -> Result<bool, HarnessError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        other => Err(invalid_value(key, other, "a boolean (true/false)")),
    }
}

fn parse_u64(key: &str, raw: &str) -> Result<u64, HarnessError> {
    raw.trim()
        .parse::<u64>()
        .map_err(|_| invalid_value(key, raw, "a non-negative integer"))
}

fn parse_i64(key: &str, raw: &str) -> Result<i64, HarnessError> {
    raw.trim()
        .parse::<i64>()
        .map_err(|_| invalid_value(key, raw, "an integer"))
}

fn parse_f64(key: &str, raw: &str) -> Result<f64, HarnessError> {
    raw.trim()
        .parse::<f64>()
        .map_err(|_| invalid_value(key, raw, "a number"))
}

fn invalid_value(key: &str, raw: &str, expected: &str) -> HarnessError {
    HarnessError::ConfigurationError(format!(
        "invalid value for '{key}': expected {expected}, got '{raw}'"
    ))
}

fn overlay_value(
    entries: &BTreeMap<String, String>,
    known: &BTreeMap<String, Value>,
) -> Result<Value, HarnessError> {
    let mut root = Value::Mapping(Mapping::new());
    for (key, raw) in entries {
        let value = coerce(key, raw, known.get(key))?;
        set_path(&mut root, key, value)?;
    }
    Ok(root)
}

fn set_path(root: &mut Value, key: &str, value: Value) -> Result<(), HarnessError> {
    let mut segments = key.split('.');
    let first = segments.next().unwrap_or_default();
    if first.is_empty() {
        return Err(HarnessError::ConfigurationError(
            "empty config key".to_string(),
        ));
    }
    let rest: Vec<&str> = segments.collect();

    if !root.is_mapping() {
        *root = Value::Mapping(Mapping::new());
    }
    let mapping = root.as_mapping_mut().ok_or_else(|| {
        HarnessError::ConfigurationError(format!("cannot address '{key}' in the config file"))
    })?;
    let entry = mapping
        .entry(Value::String(first.to_string()))
        .or_insert_with(|| Value::Mapping(Mapping::new()));

    if rest.is_empty() {
        *entry = value;
        Ok(())
    } else {
        set_path(entry, &rest.join("."), value)
    }
}

fn deep_merge(base: &mut Value, overlay: Value) {
    match overlay {
        Value::Mapping(overlay_map) => {
            if !base.is_mapping() {
                *base = Value::Mapping(Mapping::new());
            }
            if let Some(base_map) = base.as_mapping_mut() {
                for (key, value) in overlay_map {
                    match base_map.get_mut(&key) {
                        Some(existing) => deep_merge(existing, value),
                        None => {
                            base_map.insert(key, value);
                        }
                    }
                }
            }
        }
        other => *base = other,
    }
}

/// Every leaf of a YAML tree as `dotted.key -> scalar`. `null` means "unset" and is dropped, so an
/// unset `Option` never shows up as a key.
fn flatten(value: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    flatten_into(value, String::new(), &mut out, false);
    out
}

/// Flatten a schema tree into `dotted.key -> default_value`. Includes `null` leaves so that
/// valid `Option<T>` fields with `None` default are recognized as known schema keys.
fn flatten_schema(value: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    flatten_into(value, String::new(), &mut out, true);
    out
}

fn flatten_into(
    value: &Value,
    prefix: String,
    out: &mut BTreeMap<String, Value>,
    include_nulls: bool,
) {
    match value {
        Value::Mapping(mapping) => {
            for (key, child) in mapping {
                let Some(key) = key.as_str() else {
                    continue;
                };
                let dotted = if prefix.is_empty() {
                    key.to_string()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_into(child, dotted, out, include_nulls);
            }
        }
        Value::Null => {
            if include_nulls && !prefix.is_empty() {
                out.insert(prefix, Value::Null);
            }
        }
        scalar => {
            if !prefix.is_empty() {
                out.insert(prefix, scalar.clone());
            }
        }
    }
}

fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        other => format!("{other:?}"),
    }
}

fn expand_paths(paths: &mut PathsSettings) {
    let fields = [
        &mut paths.memory_db,
        &mut paths.notes_db,
        &mut paths.notes_dir,
        &mut paths.sessions_db,
        &mut paths.credentials,
        &mut paths.models_cache,
        &mut paths.workspace_root,
        &mut paths.jobs_dir,
        &mut paths.skills_dir,
    ];
    for field in fields {
        let expanded = expand_tilde(&field.to_string_lossy());
        *field = PathBuf::from(expanded);
    }
}

/// The file layer, or `None` when there is no such file. A file that cannot be parsed is an
/// error: the parse fails with its line number instead of being ignored.
fn read_file_value(path: &Path) -> Result<Option<Value>, HarnessError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(HarnessError::Io(format!(
                "failed to read {}: {}",
                path.display(),
                error
            )))
        }
    };

    // Syntax errors (line/column included) first…
    let value: Value = serde_yaml::from_str(&text).map_err(|error| file_error(path, &error))?;
    // …then the typed view, which is the only pass that can report a wrong type or an unknown key
    // with a line number attached.
    serde_yaml::from_str::<Config>(&text).map_err(|error| file_error(path, &error))?;
    Ok(Some(value))
}

fn file_error(path: &Path, error: &serde_yaml::Error) -> HarnessError {
    HarnessError::ConfigurationError(format!("invalid config file {}: {}", path.display(), error))
}

fn write_file_value(path: &Path, value: &Value) -> Result<(), HarnessError> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).map_err(|error| {
                HarnessError::Io(format!("failed to create {}: {}", parent.display(), error))
            })?;
        }
    }
    let text = serde_yaml::to_string(value)
        .map_err(|error| HarnessError::SerializationError(error.to_string()))?;
    std::fs::write(path, text)
        .map_err(|error| HarnessError::Io(format!("failed to write {}: {}", path.display(), error)))
}

fn to_value<T: Serialize>(value: &T) -> Result<Value, HarnessError> {
    serde_yaml::to_value(value).map_err(|error| HarnessError::SerializationError(error.to_string()))
}

fn from_value<T: DeserializeOwned>(value: &Value) -> Result<T, HarnessError> {
    serde_yaml::from_value(value.clone())
        .map_err(|error| HarnessError::SerializationError(error.to_string()))
}
