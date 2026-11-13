//! Precedence, defaults and the `luna config` file writer.
//!
//! Unit tests for `harness_core::config`: they use only the module's public API plus a temp
//! directory, which keeps them in the same crate without growing the module file past 800 lines.

use std::collections::BTreeMap;
use std::path::PathBuf;

use harness_core::config::*;
use harness_core::AgentConfig;
use tempfile::TempDir;

fn config_path(dir: &TempDir) -> PathBuf {
    dir.path().join("luna").join("config.yaml")
}

fn layers(file: Option<PathBuf>) -> Layers {
    Layers {
        defaults: Config::default(),
        file,
        profile: None,
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
    assert_eq!(config.paths.notes_dir, luna.join("notes"));
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

    // Unset removes the key
    let unset_res = unset_in_file(&layers, "agent.max_consecutive_failures").unwrap();
    assert_eq!(unset_res, Some(path));
    let resolved_after = resolve(&layers).unwrap();
    assert_eq!(
        resolved_after
            .source("agent.max_consecutive_failures")
            .unwrap(),
        Source::Default
    );
}

#[test]
fn memory_settings_default_to_the_session_writer() {
    let settings = MemorySettings::default();
    assert_eq!(settings.mode, MemoryMode::Session);
    assert_eq!(settings.mode.to_string(), "session");
    assert!(settings.background_notes);
    assert_eq!(settings.background_every_turns, 3);
    assert_eq!(settings.background_tail_messages, 24);
    assert!(settings.background_model.is_none());
    assert!(settings.background_provider.is_none());
    assert!(settings.background_two_pass);
    assert!((settings.background_min_confidence - 0.65).abs() < f32::EPSILON);
    assert_eq!(settings.background_max_memory_writes, 3);
    assert_eq!(settings.background_max_note_writes, 2);
    assert_eq!(settings.background_max_operations, 6);
    assert_eq!(settings.background_idle_secs, 15);
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
fn the_notes_dir_is_a_settable_path() {
    let dir = TempDir::new().unwrap();
    let layers = layers(Some(config_path(&dir)));

    set_in_file(&layers, "paths.notes_dir", "~/notes/markdown").unwrap();

    let resolved = resolve(&layers).unwrap();
    let expected = home_dir().join("notes/markdown");
    assert_eq!(
        resolved.get("paths.notes_dir").unwrap(),
        expected.to_string_lossy()
    );
    assert_eq!(resolved.config().paths.notes_dir, expected);
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
        profile: None,
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

#[test]
fn auto_compact_config_defaults_and_validation() {
    let cfg = Config::default();
    assert!(cfg.auto_compact_enabled);
    assert_eq!(cfg.auto_compact_threshold, 150_000);
    assert_eq!(cfg.auto_compact_percent, Some(50));
    assert_eq!(cfg.auto_compact_turns, 20);
    assert!(cfg.validate().is_ok());

    let mut bad_threshold_low = cfg.clone();
    bad_threshold_low.auto_compact_threshold = 99_999;
    assert!(bad_threshold_low.validate().is_err());

    let mut bad_threshold_high = cfg.clone();
    bad_threshold_high.auto_compact_threshold = 700_001;
    assert!(bad_threshold_high.validate().is_err());

    let mut bad_turns = cfg.clone();
    bad_turns.auto_compact_turns = 0;
    assert!(bad_turns.validate().is_err());

    let mut bad_percent_low = cfg.clone();
    bad_percent_low.auto_compact_percent = Some(0);
    assert!(bad_percent_low.validate().is_err());

    let mut bad_percent_high = cfg.clone();
    bad_percent_high.auto_compact_percent = Some(91);
    assert!(bad_percent_high.validate().is_err());

    let mut good = cfg.clone();
    good.auto_compact_threshold = 200_000;
    good.auto_compact_percent = Some(40);
    good.auto_compact_turns = 10;
    assert!(good.validate().is_ok());
}

#[test]
fn set_in_file_validates_auto_compact_bounds() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("config.yaml");
    let layers = layers(Some(file));

    set_in_file(&layers, "auto_compact_enabled", "false").unwrap();
    set_in_file(&layers, "auto_compact_threshold", "300000").unwrap();
    set_in_file(&layers, "auto_compact_percent", "35").unwrap();
    set_in_file(&layers, "auto_compact_turns", "15").unwrap();

    let resolved = resolve(&layers).unwrap().into_config();
    assert!(!resolved.auto_compact_enabled);
    assert_eq!(resolved.auto_compact_threshold, 300_000);
    assert_eq!(resolved.auto_compact_percent, Some(35));
    assert_eq!(resolved.auto_compact_turns, 15);

    // Invalid threshold rejected
    assert!(set_in_file(&layers, "auto_compact_threshold", "50000").is_err());
    assert!(set_in_file(&layers, "auto_compact_threshold", "900000").is_err());
    // Invalid percent rejected
    assert!(set_in_file(&layers, "auto_compact_percent", "0").is_err());
    assert!(set_in_file(&layers, "auto_compact_percent", "95").is_err());
    // Invalid turns rejected
    assert!(set_in_file(&layers, "auto_compact_turns", "0").is_err());
}

/// The documented top-level keys are the ones `/auto-compact` and onboarding write. `resolve` has
/// to carry them into the `agent.*` copy the loop actually reads, or the setting is lost at restart.
#[test]
fn top_level_auto_compact_keys_reach_the_agent_config() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("config.yaml");
    let layers = layers(Some(file));

    set_in_file(&layers, "auto_compact_enabled", "false").unwrap();
    set_in_file(&layers, "auto_compact_threshold", "300000").unwrap();
    set_in_file(&layers, "auto_compact_percent", "35").unwrap();
    set_in_file(&layers, "auto_compact_turns", "15").unwrap();

    let resolved = resolve(&layers).unwrap().into_config();
    assert!(!resolved.agent.auto_compact_enabled);
    assert_eq!(resolved.agent.auto_compact_threshold, 300_000);
    assert_eq!(resolved.agent.auto_compact_percent, Some(35));
    assert_eq!(resolved.agent.auto_compact_turns, 15);
}

/// A hand-written `agent.auto_compact_*` block is the other accepted spelling; it must win over the
/// top-level defaults and be reflected back so both copies agree.
#[test]
fn agent_auto_compact_keys_are_honoured() {
    let tmp = tempfile::tempdir().unwrap();
    let file = write(
        &tmp,
        "agent:\n  auto_compact_enabled: false\n  auto_compact_threshold: 200000\n  auto_compact_percent: 40\n  auto_compact_turns: 12\n",
    );

    let resolved = resolve(&layers(Some(file))).unwrap().into_config();
    assert!(!resolved.agent.auto_compact_enabled);
    assert!(!resolved.auto_compact_enabled);
    assert_eq!(resolved.agent.auto_compact_threshold, 200_000);
    assert_eq!(resolved.auto_compact_threshold, 200_000);
    assert_eq!(resolved.agent.auto_compact_percent, Some(40));
    assert_eq!(resolved.auto_compact_turns, 12);
}

#[test]
fn set_in_file_persists_compaction_model_and_provider() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("config.yaml");
    let layers = layers(Some(file));

    set_in_file(&layers, "agent.compaction_provider", "gemini").unwrap();
    set_in_file(&layers, "agent.compaction_model", "gemini-3.5-flash-lite").unwrap();

    let resolved = resolve(&layers).unwrap();
    assert_eq!(resolved.get("agent.compaction_provider").unwrap(), "gemini");
    assert_eq!(
        resolved.get("agent.compaction_model").unwrap(),
        "gemini-3.5-flash-lite"
    );

    let cfg = resolved.into_config();
    assert_eq!(cfg.agent.compaction_provider.as_deref(), Some("gemini"));
    assert_eq!(
        cfg.agent.compaction_model.as_deref(),
        Some("gemini-3.5-flash-lite")
    );
}

#[test]
fn worker_profiles_round_trip_through_the_config_file() {
    let tmp = tempfile::tempdir().unwrap();
    let layers = layers(Some(config_path(&tmp)));

    set_worker_in_file(
        &layers,
        "carbon",
        &WorkerProfile {
            provider: "nvidia".to_string(),
            model: "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning".to_string(),
            reasoning_effort: Some(harness_core::ReasoningEffort::Low),
        },
    )
    .unwrap();

    let cfg = resolve(&layers).unwrap().into_config();
    let carbon = cfg.workers.get("carbon").expect("worker persisted");
    assert_eq!(carbon.provider, "nvidia");
    assert_eq!(
        carbon.model,
        "nvidia/nemotron-3-nano-omni-30b-a3b-reasoning"
    );
    assert_eq!(
        carbon.reasoning_effort,
        Some(harness_core::ReasoningEffort::Low)
    );

    let removed = remove_worker_from_file(&layers, "carbon").unwrap();
    assert!(removed.is_some());
    let cfg = resolve(&layers).unwrap().into_config();
    assert!(cfg.workers.is_empty(), "the entry is gone from the file");
}

#[test]
fn worker_names_are_validated() {
    for valid in ["carbon", "c2", "fast-worker", "tiny_worker"] {
        assert!(
            validate_worker_name(valid).is_ok(),
            "{valid} should be accepted"
        );
    }
    for invalid in ["", "Carbon", "2fast", "has space", "dotted.name"] {
        assert!(
            validate_worker_name(invalid).is_err(),
            "{invalid} should be rejected"
        );
    }
    assert!(validate_worker_name(&"a".repeat(WORKER_NAME_MAX_CHARS + 1)).is_err());
}

#[test]
fn worker_profiles_require_a_provider_and_model() {
    let mut cfg = Config::default();
    cfg.workers.insert(
        "carbon".to_string(),
        WorkerProfile {
            provider: " ".to_string(),
            model: "m".to_string(),
            reasoning_effort: None,
        },
    );
    assert!(cfg.validate().is_err());

    cfg.workers.get_mut("carbon").unwrap().provider = "nvidia".to_string();
    cfg.workers.get_mut("carbon").unwrap().model = String::new();
    assert!(cfg.validate().is_err());

    cfg.workers.get_mut("carbon").unwrap().model = "m".to_string();
    assert!(cfg.validate().is_ok());
}

#[test]
fn display_defaults_are_uncapped_and_auto_collapse() {
    let cfg = Config::default();
    assert_eq!(cfg.display.block_limit, None);
    assert!(cfg.display.auto_collapse);
    assert!(cfg.display.auto_hide_thinking);
    assert!(cfg.validate().is_ok());
}

#[test]
fn display_block_limit_validation() {
    let mut cfg = Config::default();
    cfg.display.block_limit = Some(5);
    assert!(cfg.validate().is_ok());

    cfg.display.block_limit = Some(1);
    assert!(cfg.validate().is_ok());

    cfg.display.block_limit = Some(10);
    assert!(cfg.validate().is_ok());

    cfg.display.block_limit = Some(0);
    assert!(cfg.validate().is_err());

    cfg.display.block_limit = Some(11);
    assert!(cfg.validate().is_err());
}

#[test]
fn display_settings_round_trip_through_file() {
    let tmp = tempfile::tempdir().unwrap();
    let layers = layers(Some(config_path(&tmp)));

    set_in_file(&layers, "display.block_limit", "4").unwrap();
    set_in_file(&layers, "display.auto_collapse", "false").unwrap();
    set_in_file(&layers, "display.auto_hide_thinking", "false").unwrap();

    let cfg = resolve(&layers).unwrap().into_config();
    assert_eq!(cfg.display.block_limit, Some(4));
    assert!(!cfg.display.auto_collapse);
    assert!(!cfg.display.auto_hide_thinking);

    unset_in_file(&layers, "display.block_limit").unwrap();
    let cfg = resolve(&layers).unwrap().into_config();
    assert_eq!(cfg.display.block_limit, None);
}
