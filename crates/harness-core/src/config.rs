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

use crate::agent::AgentConfig;
use crate::approval::{ApprovalMode, DEFAULT_APPROVAL_TIMEOUT_SECS};
use crate::error::HarnessError;

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
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub model: ModelSettings,
    pub paths: PathsSettings,
    pub providers: BTreeMap<String, ProviderOverride>,
    pub agent: AgentConfig,
    pub tools: ToolsSettings,
    pub session: SessionSettings,
    pub telemetry: TelemetrySettings,
}

/// Which provider and model `luna` talks to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModelSettings {
    pub provider: String,
    pub default: String,
}

impl Default for ModelSettings {
    fn default() -> Self {
        Self {
            provider: "openai".to_string(),
            default: "gpt-4o".to_string(),
        }
    }
}

/// Runtime state locations. These are the same paths the engines used to compute for themselves,
/// so a missing config file resolves to exactly the previous behaviour.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PathsSettings {
    pub memory_db: PathBuf,
    pub notes_db: PathBuf,
    pub sessions_db: PathBuf,
    pub credentials: PathBuf,
    pub models_cache: PathBuf,
    /// Scope every filesystem/shell tool is confined to. Defaults to the process working
    /// directory, which is what the evolver used before this key existed.
    pub workspace_root: PathBuf,
}

impl Default for PathsSettings {
    fn default() -> Self {
        let luna = home_dir().join(".luna");
        Self {
            memory_db: luna.join("memory.db"),
            notes_db: luna.join("notes.db"),
            sessions_db: luna.join("sessions.db"),
            credentials: luna.join("credentials.json"),
            models_cache: luna.join("models_cache.json"),
            workspace_root: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
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
}

impl Default for ToolsSettings {
    fn default() -> Self {
        Self {
            approval: ApprovalMode::Ask,
            approval_timeout_secs: DEFAULT_APPROVAL_TIMEOUT_SECS,
            mutating_overrides: Vec::new(),
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
    let known = flatten(&to_value(defaults)?);
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
    let known = flatten(&defaults);
    let empty = || Value::Mapping(Mapping::new());

    let file = match layers.file.as_deref() {
        Some(path) => read_file_value(path)?.unwrap_or_else(empty),
        None => empty(),
    };
    let env = overlay_value(&layers.env, &known)?;
    let flag = overlay_value(&layers.flags, &known)?;

    let mut merged = defaults;
    deep_merge(&mut merged, file.clone());
    deep_merge(&mut merged, env.clone());
    deep_merge(&mut merged, flag.clone());

    let mut config: Config = from_value(&merged)?;
    expand_paths(&mut config.paths);

    let values: BTreeMap<String, String> = flatten(&to_value(&config)?)
        .into_iter()
        .map(|(key, value)| (key, scalar_to_string(&value)))
        .collect();
    let from_file = flatten(&file);
    let from_env = flatten(&env);
    let from_flag = flatten(&flag);

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
            None => Err(self.unknown_key(key)),
        }
    }

    /// The layer that produced `key`.
    pub fn source(&self, key: &str) -> Result<Source, HarnessError> {
        match self.sources.get(key) {
            Some(source) => Ok(*source),
            None => Err(self.unknown_key(key)),
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
    let known = flatten(&to_value(&layers.defaults)?);
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
    from_value::<Config>(&check)?;

    write_file_value(&path, &root)?;
    Ok(path)
}

/// `~` and `~/…` against `$HOME`.
pub fn expand_tilde(value: &str) -> String {
    let home = home_dir();
    if value == "~" {
        return home.to_string_lossy().into_owned();
    }
    match value.strip_prefix("~/") {
        Some(rest) => home.join(rest).to_string_lossy().into_owned(),
        None => value.to_string(),
    }
}

fn home_dir() -> PathBuf {
    std::env::var("HOME")
        .ok()
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
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
    flatten_into(value, String::new(), &mut out);
    out
}

fn flatten_into(value: &Value, prefix: String, out: &mut BTreeMap<String, Value>) {
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
                flatten_into(child, dotted, out);
            }
        }
        Value::Null => {}
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
        &mut paths.sessions_db,
        &mut paths.credentials,
        &mut paths.models_cache,
        &mut paths.workspace_root,
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
