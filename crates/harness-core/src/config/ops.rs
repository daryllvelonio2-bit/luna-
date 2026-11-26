//! Configuration file and YAML tree manipulation operations.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_yaml::{Mapping, Value};

use super::layers::Layers;
use super::types::{
    expand_tilde, validate_orchestrator_name, validate_worker_name, OrchestratorConfig,
    WorkerProfile,
};
use super::Config;
use crate::error::HarnessError;

/// Fields that may appear under `providers.<id>`.
pub const PROVIDER_FIELDS: [&str; 3] = ["base_url", "models_endpoint", "timeout_secs"];
/// Width of the edit-distance bucket used when suggesting a key for a typo.
pub const DISTANCE_BUCKET: usize = 2;

/// Write `key = raw` into the config file, creating parent directories and the file as needed.
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
    let check_cfg: Config = from_value(&check)?;
    check_cfg.validate()?;

    write_file_value(&path, &root)?;
    Ok(path)
}

/// Write one named worker profile into the config file.
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
pub fn remove_worker_from_file(
    layers: &Layers,
    name: &str,
) -> Result<Option<PathBuf>, HarnessError> {
    validate_worker_name(name)?;
    unset_in_file(layers, &format!("workers.{name}"))
}

/// Write or update one named orchestrator preset in the config file.
pub fn set_orchestrator_in_file(
    layers: &Layers,
    name: &str,
    orch: &OrchestratorConfig,
) -> Result<PathBuf, HarnessError> {
    validate_orchestrator_name(name)?;
    let path = layers.file.clone().ok_or_else(|| {
        HarnessError::ConfigurationError(
            "no config file location: set LUNA_CONFIG or HOME".to_string(),
        )
    })?;

    let mut root = read_file_value(&path)?.unwrap_or_else(|| Value::Mapping(Mapping::new()));
    let value = to_value(orch)?;
    set_path(&mut root, &format!("orchestrators.{name}"), value)?;

    let mut check = to_value(&layers.defaults)?;
    deep_merge(&mut check, root.clone());
    let check_cfg: Config = from_value(&check)?;
    check_cfg.validate()?;

    write_file_value(&path, &root)?;
    Ok(path)
}

/// Remove one named orchestrator preset from the config file.
pub fn remove_orchestrator_from_file(
    layers: &Layers,
    name: &str,
) -> Result<Option<PathBuf>, HarnessError> {
    validate_orchestrator_name(name)?;
    unset_in_file(layers, &format!("orchestrators.{name}"))
}

/// Remove `key` from the config file if present.
pub fn unset_in_file(layers: &Layers, key: &str) -> Result<Option<PathBuf>, HarnessError> {
    let path = match layers.file.as_ref() {
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

pub fn remove_path(root: &mut Value, key: &str) -> bool {
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

pub fn provider_field(key: &str) -> Option<&str> {
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

pub fn is_known_key(key: &str, known: &BTreeMap<String, Value>) -> bool {
    known.contains_key(key) || provider_field(key).is_some()
}

pub fn nearest_key_error(key: &str, candidates: &[&str]) -> HarnessError {
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

pub fn split_leaf(key: &str) -> (&str, &str) {
    match key.rfind('.') {
        Some(index) => (&key[index + 1..], &key[..index]),
        None => (key, ""),
    }
}

pub fn levenshtein(a: &str, b: &str) -> usize {
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

pub fn coerce(key: &str, raw: &str, default: Option<&Value>) -> Result<Value, HarnessError> {
    if let Some(field) = provider_field(key) {
        return match field {
            "timeout_secs" => parse_u64(key, raw).map(|value| Value::Number(value.into())),
            _ => Ok(Value::String(raw.to_string())),
        };
    }
    if matches!(
        key,
        "tools.approval_provider" | "tools.approval_model" | "tools.approval_reasoning_effort"
    ) {
        return Ok(Value::String(raw.to_string()));
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

pub fn infer_scalar(raw: &str) -> Value {
    match raw {
        "true" => Value::Bool(true),
        "false" => Value::Bool(false),
        other => match other.parse::<u64>() {
            Ok(number) => Value::Number(number.into()),
            Err(_) => Value::String(other.to_string()),
        },
    }
}

pub fn parse_bool(key: &str, raw: &str) -> Result<bool, HarnessError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "yes" | "on" | "1" => Ok(true),
        "false" | "no" | "off" | "0" => Ok(false),
        other => Err(invalid_value(key, other, "a boolean (true/false)")),
    }
}

pub fn parse_u64(key: &str, raw: &str) -> Result<u64, HarnessError> {
    raw.trim()
        .parse::<u64>()
        .map_err(|_| invalid_value(key, raw, "a non-negative integer"))
}

pub fn parse_i64(key: &str, raw: &str) -> Result<i64, HarnessError> {
    raw.trim()
        .parse::<i64>()
        .map_err(|_| invalid_value(key, raw, "an integer"))
}

pub fn parse_f64(key: &str, raw: &str) -> Result<f64, HarnessError> {
    raw.trim()
        .parse::<f64>()
        .map_err(|_| invalid_value(key, raw, "a number"))
}

pub fn invalid_value(key: &str, raw: &str, expected: &str) -> HarnessError {
    HarnessError::ConfigurationError(format!(
        "invalid value for '{key}': expected {expected}, got '{raw}'"
    ))
}

pub fn overlay_value(
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

pub fn set_path(root: &mut Value, key: &str, value: Value) -> Result<(), HarnessError> {
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

pub fn deep_merge(base: &mut Value, overlay: Value) {
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

pub fn flatten(value: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    flatten_into(value, String::new(), &mut out, false);
    out
}

pub fn flatten_schema(value: &Value) -> BTreeMap<String, Value> {
    let mut out = BTreeMap::new();
    flatten_into(value, String::new(), &mut out, true);
    out
}

pub fn flatten_into(
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

pub fn scalar_to_string(value: &Value) -> String {
    match value {
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        other => format!("{other:?}"),
    }
}

pub fn read_file_value(path: &Path) -> Result<Option<Value>, HarnessError> {
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

    let value: Value = serde_yaml::from_str(&text).map_err(|error| file_error(path, &error))?;
    serde_yaml::from_str::<Config>(&text).map_err(|error| file_error(path, &error))?;
    Ok(Some(value))
}

pub fn file_error(path: &Path, error: &serde_yaml::Error) -> HarnessError {
    HarnessError::ConfigurationError(format!("invalid config file {}: {}", path.display(), error))
}

pub fn write_file_value(path: &Path, value: &Value) -> Result<(), HarnessError> {
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

pub fn to_value<T: Serialize>(value: &T) -> Result<Value, HarnessError> {
    serde_yaml::to_value(value).map_err(|error| HarnessError::SerializationError(error.to_string()))
}

pub fn from_value<T: DeserializeOwned>(value: &Value) -> Result<T, HarnessError> {
    serde_yaml::from_value(value.clone())
        .map_err(|error| HarnessError::SerializationError(error.to_string()))
}
