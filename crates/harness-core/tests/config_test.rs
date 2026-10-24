//! Precedence, defaults and the `luna config` file writer.
//!
//! Unit tests for `harness_core::config`: they use only the module's public API plus a temp
//! directory, which keeps them in the same crate without growing the module file past 800 lines.

use std::collections::BTreeMap;
use std::path::PathBuf;

use harness_core::config::*;
use harness_core::AgentConfig;
use tempfile::TempDir;

/// The same `$HOME` resolution the module uses, written out here so these tests do not depend on a
/// private helper.
fn home_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME is set in this environment"))
}

fn config_path(dir: &TempDir) -> PathBuf {
    dir.path().join("luna").join("config.yaml")
}

fn layers(file: Option<PathBuf>) -> Layers {
    Layers {
        defaults: Config::default(),
        file,
        env: BTreeMap::new(),
        flags: BTreeMap::new(),
    }
}

/// Stands in for the catalog overlay the CLI injects before resolving.
fn with_provider(defaults: &mut Config) {
    defaults.providers.insert(
        "nvidia".to_string(),
        ProviderOverride {
            base_url: None,
            models_endpoint: None,
            timeout_secs: None,
            context_window: None,
        },
    );
}

fn write(dir: &TempDir, body: &str) -> PathBuf {
    let path = config_path(dir);
    std::fs::create_dir_all(path.parent().expect("temp dir has a parent")).unwrap();
    std::fs::write(&path, body).unwrap();
    path
}

#[test]
fn defaults_match_the_previous_literals() {
    let config = Config::default();
    let luna = home_dir().join(".luna");

    assert_eq!(config.paths.memory_db, luna.join("memory.db"));
    assert_eq!(config.paths.notes_db, luna.join("notes.db"));
    assert_eq!(config.paths.credentials, luna.join("credentials.json"));
    assert_eq!(config.paths.models_cache, luna.join("models_cache.json"));
    assert_eq!(
        config.paths.workspace_root,
        std::env::current_dir().unwrap()
    );

    assert_eq!(config.model.provider, "");
    assert_eq!(config.model.default, "");

    assert_eq!(config.agent, AgentConfig::default());
    assert_eq!(config.agent.max_consecutive_failures, 5);
    assert!(config.agent.require_hitl_for_mutation);

    assert!(!config.telemetry.json);
    assert_eq!(config.telemetry.level, "harness=info,harness_cli=info");
}

#[test]
fn missing_file_is_not_an_error() {
    let dir = TempDir::new().unwrap();
    let resolved = resolve(&layers(Some(config_path(&dir)))).unwrap();

    assert_eq!(
        resolved.get("paths.memory_db").unwrap(),
        Config::default().paths.memory_db.to_string_lossy()
    );
    assert_eq!(resolved.source("paths.memory_db").unwrap(), Source::Default);
}

#[test]
fn file_beats_default() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "model:\n  provider: nvidia\n");

    let resolved = resolve(&layers(Some(path))).unwrap();
    assert_eq!(resolved.get("model.provider").unwrap(), "nvidia");
    assert_eq!(resolved.source("model.provider").unwrap(), Source::File);
    assert_eq!(resolved.source("model.default").unwrap(), Source::Default);
    assert_eq!(resolved.config().model.provider, "nvidia");
}

#[test]
fn env_beats_file() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "model:\n  default: from-file\n");

    let mut layers = layers(Some(path));
    layers
        .env
        .insert("model.default".to_string(), "from-env".to_string());

    let resolved = resolve(&layers).unwrap();
    assert_eq!(resolved.get("model.default").unwrap(), "from-env");
    assert_eq!(resolved.source("model.default").unwrap(), Source::Env);
}

#[test]
fn env_values_are_coerced_to_the_key_type() {
    let mut layers = layers(Some(PathBuf::from("/nonexistent/config.yaml")));
    layers.env.insert(
        "agent.max_consecutive_failures".to_string(),
        "3".to_string(),
    );
    layers
        .env
        .insert("telemetry.json".to_string(), "true".to_string());

    let resolved = resolve(&layers).unwrap();
    assert_eq!(resolved.config().agent.max_consecutive_failures, 3);
    assert!(resolved.config().telemetry.json);
    assert_eq!(resolved.get("agent.max_consecutive_failures").unwrap(), "3");
}

#[test]
fn flag_beats_env() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "model:\n  default: from-file\n");

    let mut layers = layers(Some(path));
    layers
        .env
        .insert("model.default".to_string(), "from-env".to_string());
    layers
        .flags
        .insert("model.default".to_string(), "from-flag".to_string());

    let resolved = resolve(&layers).unwrap();
    assert_eq!(resolved.get("model.default").unwrap(), "from-flag");
    assert_eq!(resolved.source("model.default").unwrap(), Source::Flag);
}

#[test]
fn malformed_file_fails_with_the_parse_error_and_line() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "model:\n  provider: [unclosed\n");

    let error = resolve(&layers(Some(path))).unwrap_err().to_string();
    assert!(error.contains("invalid config file"), "{error}");
    assert!(error.contains("line"), "{error}");
}

#[test]
fn unknown_key_in_the_file_fails_with_the_line() {
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "model:\n  nope: 1\n");

    let error = resolve(&layers(Some(path))).unwrap_err().to_string();
    assert!(error.contains("invalid config file"), "{error}");
    assert!(error.contains("line 2"), "{error}");
}

#[test]
fn unknown_key_reports_the_nearest_known_key() {
    let resolved = resolve(&layers(None)).unwrap();

    let error = resolved.get("model.nope").unwrap_err().to_string();
    assert!(error.contains("model.default"), "{error}");
    assert!(error.contains("unknown config key 'model.nope'"), "{error}");

    let error = resolved.get("telemetry.jsn").unwrap_err().to_string();
    assert!(error.contains("telemetry.json"), "{error}");
}

#[test]
fn set_then_load_round_trips() {
    let dir = TempDir::new().unwrap();
    let path = config_path(&dir);
    let layers = layers(Some(path.clone()));

    let written = set_in_file(&layers, "agent.max_consecutive_failures", "3").unwrap();
    assert_eq!(written, path);
    set_in_file(&layers, "model.provider", "nvidia").unwrap();

    let resolved = resolve(&layers).unwrap();
    assert_eq!(resolved.get("agent.max_consecutive_failures").unwrap(), "3");
    assert_eq!(
        resolved.source("agent.max_consecutive_failures").unwrap(),
        Source::File
    );
    assert_eq!(resolved.config().agent.max_consecutive_failures, 3);
    // The second write kept the first key.
    assert_eq!(resolved.get("model.provider").unwrap(), "nvidia");
}

#[test]
fn memory_settings_default_to_the_session_writer() {
    let settings = MemorySettings::default();
    assert_eq!(settings.mode, MemoryMode::Session);
    assert_eq!(settings.mode.to_string(), "session");
    assert!(settings.background_notes);
    assert_eq!(settings.background_every_turns, 1);
    assert_eq!(settings.background_tail_messages, 24);
    assert!(settings.background_model.is_none());
    assert!(settings.background_provider.is_none());
    assert!(!settings.mode.is_background());
    // A config file written before these keys existed keeps the old behaviour.
    let dir = TempDir::new().unwrap();
    let path = write(&dir, "persona:\n  id: 3\n");
    let resolved = resolve(&layers(Some(path))).unwrap();
    assert_eq!(resolved.config().memory, MemorySettings::default());
    assert_eq!(resolved.get("memory.mode").unwrap(), "session");
}

#[test]
fn memory_mode_accepts_its_aliases_and_rejects_everything_else() {
    use std::str::FromStr;

    assert_eq!(
        MemoryMode::from_str("background").unwrap(),
        MemoryMode::Background
    );
    assert_eq!(
        MemoryMode::from_str("  BACKGROUND ").unwrap(),
        MemoryMode::Background
    );
    assert_eq!(
        MemoryMode::from_str("saver").unwrap(),
        MemoryMode::Background
    );
    assert_eq!(MemoryMode::from_str("agent").unwrap(), MemoryMode::Session);
    assert_eq!(MemoryMode::from_str("main").unwrap(), MemoryMode::Session);

    let error = MemoryMode::from_str("telepathy").unwrap_err();
    assert!(
        error.contains("session") && error.contains("background"),
        "{error}"
    );
}

#[test]
fn the_memory_mode_is_settable_and_survives_a_reload() {
    let dir = TempDir::new().unwrap();
    let layers = layers(Some(config_path(&dir)));

    set_in_file(&layers, "memory.mode", "background").unwrap();
    set_in_file(&layers, "memory.background_notes", "false").unwrap();
    set_in_file(&layers, "memory.background_every_turns", "3").unwrap();

    let resolved = resolve(&layers).unwrap();
    assert_eq!(resolved.get("memory.mode").unwrap(), "background");
    assert_eq!(resolved.config().memory.mode, MemoryMode::Background);
    assert!(resolved.config().memory.mode.is_background());
    assert!(!resolved.config().memory.background_notes);
    assert_eq!(resolved.config().memory.background_every_turns, 3);
    assert_eq!(resolved.source("memory.mode").unwrap(), Source::File);

    // A typo is refused before the file is touched, exactly like any other key.
    let error = set_in_file(&layers, "memory.mode", "telepathy")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("unknown variant") || error.contains("telepathy"),
        "{error}"
    );
    assert_eq!(
        resolve(&layers).unwrap().config().memory.mode,
        MemoryMode::Background,
        "the failed write clobbered the stored value"
    );
}

#[test]
fn set_rejects_unknown_keys_and_bad_values() {
    let dir = TempDir::new().unwrap();
    let layers = layers(Some(config_path(&dir)));

    let error = set_in_file(&layers, "model.nope", "x")
        .unwrap_err()
        .to_string();
    assert!(error.contains("model.default"), "{error}");

    let error = set_in_file(&layers, "agent.max_consecutive_failures", "lots")
        .unwrap_err()
        .to_string();
    assert!(error.contains("non-negative integer"), "{error}");

    // Nothing was written by the failed attempts.
    assert!(!config_path(&dir).exists());
}

#[test]
fn set_expands_tilde_in_paths() {
    let dir = TempDir::new().unwrap();
    let layers = layers(Some(config_path(&dir)));

    set_in_file(&layers, "paths.notes_db", "~/notes/luna.db").unwrap();

    let resolved = resolve(&layers).unwrap();
    let expected = home_dir().join("notes/luna.db");
    assert_eq!(
        resolved.get("paths.notes_db").unwrap(),
        expected.to_string_lossy()
    );
    assert_eq!(resolved.config().paths.notes_db, expected);
}

#[test]
fn provider_override_keys_are_known_and_reported() {
    let mut defaults = Config::default();
    with_provider(&mut defaults);
    defaults.providers.get_mut("nvidia").unwrap().base_url =
        Some("https://integrate.api.nvidia.com/v1".to_string());

    let dir = TempDir::new().unwrap();
    let path = write(&dir, "providers:\n  nvidia:\n    timeout_secs: 900\n");
    let resolved = resolve(&Layers {
        defaults,
        file: Some(path),
        env: BTreeMap::new(),
        flags: BTreeMap::new(),
    })
    .unwrap();

    assert_eq!(
        resolved.get("providers.nvidia.base_url").unwrap(),
        "https://integrate.api.nvidia.com/v1"
    );
    assert_eq!(
        resolved.source("providers.nvidia.base_url").unwrap(),
        Source::Default
    );
    assert_eq!(
        resolved.get("providers.nvidia.timeout_secs").unwrap(),
        "900"
    );
    assert_eq!(
        resolved.source("providers.nvidia.timeout_secs").unwrap(),
        Source::File
    );
    assert_eq!(
        resolved.config().providers["nvidia"].timeout_secs,
        Some(900)
    );
}

#[test]
fn provider_fields_are_settable_keys() {
    let dir = TempDir::new().unwrap();
    let layers = layers(Some(config_path(&dir)));

    set_in_file(
        &layers,
        "providers.nvidia.base_url",
        "https://example.test/v1",
    )
    .unwrap();
    set_in_file(&layers, "providers.nvidia.models_endpoint", "/models").unwrap();
    set_in_file(&layers, "providers.nvidia.timeout_secs", "900").unwrap();

    let resolved = resolve(&layers).unwrap();
    assert_eq!(
        resolved.get("providers.nvidia.base_url").unwrap(),
        "https://example.test/v1"
    );
    assert_eq!(
        resolved.config().providers["nvidia"].timeout_secs,
        Some(900)
    );

    // A field outside the schema is rejected, not written.
    let error = set_in_file(&layers, "providers.nvidia.nope", "x")
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("unknown config key 'providers.nvidia.nope'"),
        "{error}"
    );
}

#[test]
fn env_var_names_map_to_dotted_keys() {
    assert_eq!(env_var_name("model.default"), "LUNA_MODEL_DEFAULT");
    assert_eq!(env_var_name("model.provider"), "LUNA_MODEL_PROVIDER");
    assert_eq!(
        env_var_name("providers.nvidia.base_url"),
        "LUNA_PROVIDERS_NVIDIA_BASE_URL"
    );
    assert!(env_var_name("paths.memory_db").starts_with(ENV_PREFIX));
}

#[test]
fn config_file_path_follows_luna_config() {
    let resolved = config_file_path();
    assert!(resolved.to_string_lossy().ends_with("config.yaml"));
}
