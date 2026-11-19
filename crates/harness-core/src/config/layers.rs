//! Configuration layer resolution: precedence order, environment variables, and provenance tracking.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_yaml::{Mapping, Value};

use super::ops::{
    deep_merge, flatten, flatten_schema, from_value, is_known_key, nearest_key_error,
    overlay_value, read_file_value, scalar_to_string, to_value,
};
use super::types::{expand_paths, expand_tilde, home_dir, Source};
use super::Config;
use crate::error::HarnessError;

/// Environment variable that relocates the config file.
pub const CONFIG_PATH_ENV: &str = "LUNA_CONFIG";
/// Home-relative location of the config file.
pub const CONFIG_FILE_RELATIVE: &str = ".luna/config.yaml";
/// Prefix of every environment override.
pub const ENV_PREFIX: &str = "LUNA_";

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

/// Every `LUNA_*` variable that maps onto a key of `defaults`.
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

    sync_auto_compact(&mut config, &from_file, &from_env, &from_flag);

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

/// Sync auto-compact config between the top-level and `agent` sections.
fn sync_auto_compact(
    config: &mut Config,
    from_file: &BTreeMap<String, Value>,
    from_env: &BTreeMap<String, Value>,
    from_flag: &BTreeMap<String, Value>,
) {
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

    /// A key that no layer set is still "known" when the schema knows it.
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
