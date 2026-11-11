//! End-to-end tests of the onboarding flow: a scripted answer against a real temp config file.
//!
//! The flow is driven through [`harness_onboarding::run`] with an `InstantPacer` and a fixed column
//! count, so these tests exercise the production code path — briefing, settings screens, validation,
//! persistence, confirmation — without a terminal and without waiting for the animation.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

use harness_core::config::{Config, Layers};
use harness_onboarding::{flow, needs_onboarding, run, InstantPacer, Size};

/// Layers pointing at a temp config file: no env, no flags, catalog-free defaults.
fn layers(file: &Path) -> Layers {
    Layers {
        defaults: Config::default(),
        file: Some(file.to_path_buf()),
        profile: None,
        env: BTreeMap::new(),
        flags: BTreeMap::new(),
    }
}

/// The terminal width and height every test renders against, so centering is assertable.
fn columns() -> Size {
    Size::new(100, 40)
}

/// Drive the flow with a scripted answer, returning the outcome and what was rendered.
fn drive(file: &Path, script: &str) -> (flow::Outcome, String) {
    let layers = layers(file);
    let mut reader = Cursor::new(script.as_bytes().to_vec());
    let mut out: Vec<u8> = Vec::new();
    let mut pacer = InstantPacer;
    let outcome = run(&layers, &mut reader, &mut out, &mut pacer, &columns)
        .expect("the scripted answer is accepted");
    (outcome, strip_escapes(&out))
}

/// Drive a script that is expected to fail, returning the error text.
fn drive_expecting_error(file: &Path, script: &str) -> String {
    let layers = layers(file);
    let mut reader = Cursor::new(script.as_bytes().to_vec());
    let mut out: Vec<u8> = Vec::new();
    let mut pacer = InstantPacer;
    let error = run(&layers, &mut reader, &mut out, &mut pacer, &columns)
        .expect_err("the script does not complete the flow");
    error.to_string()
}

/// What the user actually sees, with the ANSI sequences removed.
fn strip_escapes(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for escape in chars.by_ref() {
                if escape.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

fn read_config(file: &Path) -> serde_yaml::Value {
    let raw = std::fs::read_to_string(file).expect("the flow wrote a config file");
    serde_yaml::from_str(&raw).expect("the flow wrote valid YAML")
}

#[test]
fn a_fresh_install_is_onboarded_with_the_default_profile() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");
    let layers = layers(&file);

    assert!(
        needs_onboarding(&layers).expect("resolves"),
        "a missing file is a first run"
    );
    assert!(!file.exists());

    // The first answer is the memory writer. There is no persona prompt consuming input.
    let (outcome, rendered) = drive(&file, "2\n");

    assert_eq!(outcome.profile, harness_core::profiles::DEFAULT_PROFILE);
    assert_eq!(outcome.memory, harness_core::MemoryMode::Background);
    assert_eq!(outcome.config_path, file);

    let config = read_config(&file);
    assert_eq!(config["memory"]["mode"].as_str(), Some("background"));
    assert_eq!(config["first_run"].as_bool(), Some(false));
    assert!(
        config.get("persona").is_none(),
        "onboarding no longer writes a persona"
    );

    assert!(rendered.contains("Profile configured"));
    assert!(rendered.contains("default (built-in)"));
    assert!(rendered.contains("Background saver"));
    assert!(rendered.contains("Config written to"));
}

#[test]
fn the_flow_writes_the_config_and_the_gate_then_stays_closed() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("nested").join("luna").join("config.yaml");
    let layers = layers(&file);

    drive(&file, "1\n");

    assert!(file.exists(), "the flow creates parent directories");
    assert!(
        !needs_onboarding(&layers).expect("resolves"),
        "an onboarded install must not be asked again"
    );

    let resolved = harness_core::config::resolve(&layers)
        .expect("the written file loads")
        .into_config();
    assert_eq!(resolved.memory.mode, harness_core::MemoryMode::Session);
    assert!(!resolved.first_run);
}

#[test]
fn a_blank_line_takes_the_default_memory_writer() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    let (outcome, _) = drive(&file, "\n");

    assert_eq!(outcome.memory, harness_core::MemoryMode::Session);
    assert_eq!(
        read_config(&file)["memory"]["mode"].as_str(),
        Some("session")
    );
}

#[test]
fn invalid_answers_are_rejected_and_then_recovered_from() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    let (outcome, rendered) = drive(&file, "9\nbanana\n2\n");

    assert_eq!(outcome.memory, harness_core::MemoryMode::Background);
    assert_eq!(
        rendered.matches('✗').count(),
        2,
        "each rejection is reported"
    );
    assert!(rendered.contains("there is no option 9"));
    assert!(rendered.contains("'banana' is not a number"));
}

#[test]
fn a_closed_input_is_an_error_rather_than_a_silent_default() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    let error = drive_expecting_error(&file, "");

    assert!(
        error.contains("input closed before a memory mode was chosen"),
        "{error}"
    );
    assert!(!file.exists());
}

#[test]
fn the_greeting_is_centered_and_the_briefing_keeps_its_shape() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    let (_, rendered) = drive(&file, "1\n1\n");

    let greeting = rendered
        .lines()
        .find(|line| line.contains("Hi! I am Luna"))
        .expect("the greeting was streamed");
    let text = "Hi! I am Luna, your personal AI agent harness.";
    assert_eq!(
        greeting.len() - text.len(),
        (columns().columns as usize - text.len()) / 2,
        "greeting is not centered: {greeting:?}"
    );
    assert!(rendered.contains("I inspect, execute, and reflect"));
    assert!(rendered.contains("tune my neural core to your style"));
    assert!(rendered.contains("Profile configured"));
    assert!(
        !rendered.contains("NEURAL CORE CALIBRATION"),
        "the persona menu is gone"
    );
    assert!(!rendered.contains("Classic Tsundere"));
}

#[test]
fn the_memory_screen_offers_both_writers_and_a_blank_keeps_the_session_writer() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");
    let layers = layers(&file);

    let (outcome, rendered) = drive(&file, "\n");

    assert_eq!(outcome.memory, harness_core::MemoryMode::Session);
    assert_eq!(
        read_config(&file)["memory"]["mode"].as_str(),
        Some("session"),
        "the default is written explicitly, so the file reads as intent"
    );

    assert!(
        rendered.contains("MEMORY CORE"),
        "the step has its own panel"
    );
    assert!(rendered.contains("Session writer"));
    assert!(rendered.contains("Background saver"));
    assert!(
        rendered.contains("luna config set memory.mode background"),
        "the escape hatch is on screen"
    );
    assert!(rendered.contains("Session writer — I save memories as we talk"));

    let resolved = harness_core::config::resolve(&layers)
        .expect("the written file loads")
        .into_config();
    assert_eq!(resolved.memory.mode, harness_core::MemoryMode::Session);
}

#[test]
fn a_rejected_memory_answer_is_reported_and_the_next_one_is_honoured() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    let (outcome, rendered) = drive(&file, "4\n9\nbanana\n2\n");

    assert_eq!(outcome.memory, harness_core::MemoryMode::Background);
    assert_eq!(
        read_config(&file)["memory"]["mode"].as_str(),
        Some("background")
    );
    assert_eq!(
        rendered.matches('✗').count(),
        3,
        "each rejection is reported"
    );
    assert!(rendered.contains("there is no option 9"));
    assert!(rendered.contains("'banana' is not a number"));
}

#[test]
fn a_closed_input_before_the_memory_mode_writes_nothing() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    let error = drive_expecting_error(&file, "");

    assert!(
        error.contains("input closed before a memory mode was chosen"),
        "{error}"
    );
    assert!(
        !file.exists(),
        "nothing is written when the first answer is missing"
    );
}

#[test]
fn re_running_the_flow_overwrites_the_memory_mode() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    drive(&file, "2\n");
    assert_eq!(
        read_config(&file)["memory"]["mode"].as_str(),
        Some("background")
    );

    // `luna setup` bypasses the first_run gate, so a second run must be able to switch back.
    drive(&file, "1\n");
    assert_eq!(
        read_config(&file)["memory"]["mode"].as_str(),
        Some("session")
    );
}

#[test]
fn the_terminal_is_prepared_and_handed_back_clean() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");
    let layers = layers(&file);
    let mut reader = Cursor::new(b"1\n".to_vec());
    let mut out: Vec<u8> = Vec::new();
    let mut pacer = InstantPacer;

    run(&layers, &mut reader, &mut out, &mut pacer, &columns).expect("flow completes");
    let raw = String::from_utf8_lossy(&out);

    assert!(
        raw.contains("\u{1b}[?25l"),
        "cursor is not hidden for the animation"
    );
    assert!(
        raw.contains("\u{1b}[2J"),
        "the screen is not cleared between phases"
    );
    assert!(
        raw.matches("\u{1b}[?25h").count() >= 2,
        "the cursor must be restored before input and again at the end"
    );
}

#[test]
fn onboarding_configures_custom_auto_compact() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    // memory 1, enable auto-compact, threshold 250k, percent/turns 15
    let (outcome, rendered) = drive(&file, "1\n1\n250k\n15\n");

    assert!(outcome.auto_compact.enabled);
    assert_eq!(outcome.auto_compact.threshold, 250_000);
    assert_eq!(outcome.auto_compact.turns, 15);

    let config = read_config(&file);
    assert_eq!(config["auto_compact_enabled"].as_bool(), Some(true));
    assert_eq!(config["auto_compact_threshold"].as_u64(), Some(250_000));
    assert_eq!(config["auto_compact_turns"].as_u64(), Some(15));
    assert!(rendered.contains("Auto-compact"));
}

#[test]
fn onboarding_configures_telegram_gateway() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    // memory 1, auto-compact 1, default threshold, default percent, gateway 2, bot token
    let (outcome, rendered) = drive(
        &file,
        "1\n1\n\n\n2\n123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11\n",
    );

    assert!(outcome.gateway.enabled);
    assert_eq!(outcome.gateway.platform, "telegram");
    assert_eq!(
        outcome.gateway.token.as_deref(),
        Some("123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11")
    );

    let config = read_config(&file);
    assert_eq!(config["gateway"]["enabled"].as_bool(), Some(true));
    assert_eq!(config["gateway"]["platform"].as_str(), Some("telegram"));
    assert_eq!(
        config["gateway"]["telegram_bot_token"].as_str(),
        Some("123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11")
    );
    assert!(rendered.contains("Messaging gateway"));
    assert!(rendered.contains("Telegram Bot (enabled)"));
}

#[test]
fn onboarding_defaults_to_terminal_only_gateway() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    // memory 1, auto-compact 1, default threshold, default percent, gateway 1
    let (outcome, rendered) = drive(&file, "1\n1\n\n\n1\n");

    assert!(!outcome.gateway.enabled);
    assert_eq!(outcome.gateway.platform, "none");

    let config = read_config(&file);
    assert_eq!(config["gateway"]["enabled"].as_bool(), Some(false));
    assert_eq!(config["gateway"]["platform"].as_str(), Some("none"));
    assert!(rendered.contains("Terminal only (disabled)"));
}

#[test]
fn onboarding_disables_auto_compact() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    // memory 1, disable auto-compact (choice 2)
    let (outcome, rendered) = drive(&file, "1\n2\n");

    assert!(!outcome.auto_compact.enabled);

    let config = read_config(&file);
    assert_eq!(config["auto_compact_enabled"].as_bool(), Some(false));
    assert!(rendered.contains("Disabled"));
}

#[test]
fn compact_step_parsers() {
    use harness_onboarding::compact_step::{
        parse_percent, parse_selection, parse_threshold, parse_turns,
    };

    assert!(parse_selection("1").unwrap().enabled);
    assert!(!parse_selection("2").unwrap().enabled);
    assert!(parse_selection("").unwrap().enabled);

    assert_eq!(parse_threshold("150000").unwrap(), 150_000);
    assert_eq!(parse_threshold("200k").unwrap(), 200_000);
    assert_eq!(parse_threshold("").unwrap(), 150_000);
    assert!(parse_threshold("50k").is_err()); // below 100k
    assert!(parse_threshold("800k").is_err()); // above 700k

    assert_eq!(parse_percent("50").unwrap(), 50);
    assert_eq!(parse_percent("50%").unwrap(), 50);
    assert_eq!(parse_percent("").unwrap(), 50);
    assert!(parse_percent("0").is_err());
    assert!(parse_percent("95").is_err());

    assert_eq!(parse_turns("20").unwrap(), 20);
    assert_eq!(parse_turns("5").unwrap(), 5);
    assert_eq!(parse_turns("").unwrap(), 20);
    assert!(parse_turns("0").is_err());
}

#[test]
fn onboarding_configures_custom_notes_dir() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");
    let notes_dir = tmp.path().join("custom-notes");

    // memory 1, auto-compact default, gateway default, custom notes_dir
    let (outcome, rendered) = drive(&file, &format!("1\n1\n\n\n1\n{}\n", notes_dir.display()));

    assert_eq!(outcome.notes_dir, notes_dir);
    assert!(notes_dir.is_dir(), "the chosen folder is created");

    let config = read_config(&file);
    assert_eq!(
        config["paths"]["notes_dir"].as_str(),
        Some(notes_dir.to_str().unwrap())
    );
    assert!(rendered.contains("Notes directory"));
    assert!(rendered.contains(notes_dir.to_str().unwrap()));
}

#[test]
fn targeted_memory_step_only_runs_memory_and_updates_config() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");
    let layers = layers(&file);
    let mut reader = Cursor::new(b"2\n".to_vec());
    let mut out = Vec::new();
    let mut pacer = InstantPacer;

    let (mode, path) =
        harness_onboarding::run_memory(&layers, &mut reader, &mut out, &mut pacer, &columns)
            .expect("memory step succeeds");

    assert_eq!(mode, harness_core::MemoryMode::Background);
    assert_eq!(path, file);

    let config = read_config(&file);
    assert_eq!(config["memory"]["mode"].as_str(), Some("background"));
    assert_eq!(config["first_run"].as_bool(), Some(false));
}

#[test]
fn targeted_notes_step_only_runs_notes_and_updates_config() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");
    let custom_notes = tmp.path().join("targeted-notes");
    let layers = layers(&file);
    let mut reader = Cursor::new(format!("{}\n", custom_notes.display()).into_bytes());
    let mut out = Vec::new();
    let mut pacer = InstantPacer;

    let (notes_dir, path) =
        harness_onboarding::run_notes(&layers, &mut reader, &mut out, &mut pacer, &columns)
            .expect("notes step succeeds");

    assert_eq!(notes_dir, custom_notes);
    assert_eq!(path, file);

    let config = read_config(&file);
    assert_eq!(
        config["paths"]["notes_dir"].as_str(),
        Some(custom_notes.to_str().unwrap())
    );
    assert_eq!(config["first_run"].as_bool(), Some(false));
}

#[test]
fn targeted_compact_step_only_runs_compact_and_updates_config() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");
    let layers = layers(&file);
    // 2 disables auto compact
    let mut reader = Cursor::new(b"2\n".to_vec());
    let mut out = Vec::new();
    let mut pacer = InstantPacer;

    let (compact, path) =
        harness_onboarding::run_compact(&layers, &mut reader, &mut out, &mut pacer, &columns)
            .expect("compact step succeeds");

    assert!(!compact.enabled);
    assert_eq!(path, file);

    let config = read_config(&file);
    assert_eq!(config["auto_compact_enabled"].as_bool(), Some(false));
    assert_eq!(config["first_run"].as_bool(), Some(false));
}

#[test]
fn targeted_gateway_step_only_runs_gateway_and_updates_config() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");
    let layers = layers(&file);
    // 1 selects none/disabled
    let mut reader = Cursor::new(b"1\n".to_vec());
    let mut out = Vec::new();
    let mut pacer = InstantPacer;

    let (gateway, path) =
        harness_onboarding::run_gateway(&layers, &mut reader, &mut out, &mut pacer, &columns)
            .expect("gateway step succeeds");

    assert!(!gateway.enabled);
    assert_eq!(gateway.platform, "none");
    assert_eq!(path, file);

    let config = read_config(&file);
    assert_eq!(config["gateway"]["enabled"].as_bool(), Some(false));
    assert_eq!(config["gateway"]["platform"].as_str(), Some("none"));
    assert_eq!(config["first_run"].as_bool(), Some(false));
}

#[test]
fn targeted_web_step_only_runs_web_and_updates_config() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");
    let layers = layers(&file);
    // 1 selects duckduckgo
    let mut reader = Cursor::new(b"1\n".to_vec());
    let mut out = Vec::new();
    let mut pacer = InstantPacer;

    let (web, path) =
        harness_onboarding::run_web(&layers, &mut reader, &mut out, &mut pacer, &columns)
            .expect("web step succeeds");

    assert!(web.enabled);
    assert_eq!(web.backend, harness_core::config::SearchBackend::DuckDuckGo);
    assert_eq!(path, file);

    let config = read_config(&file);
    assert_eq!(config["tools"]["web"]["enabled"].as_bool(), Some(true));
    assert_eq!(
        config["tools"]["web"]["search_backend"].as_str(),
        Some("duckduckgo")
    );
    assert_eq!(config["first_run"].as_bool(), Some(false));
}
