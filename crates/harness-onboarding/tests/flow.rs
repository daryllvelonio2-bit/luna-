//! End-to-end tests of the onboarding flow: a scripted answer against a real temp config file.
//!
//! The flow is driven through [`harness_onboarding::run`] with an `InstantPacer` and a fixed column
//! count, so these tests exercise the production code path — briefing, menu, validation, persistence,
//! confirmation — without a terminal and without waiting three seconds for the animation.

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
        .expect_err("the script never picks a valid persona");
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
fn a_fresh_install_is_onboarded_and_the_choice_is_persisted() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");
    let layers = layers(&file);

    assert!(
        needs_onboarding(&layers).expect("resolves"),
        "a missing file is a first run"
    );
    assert!(!file.exists());

    let (outcome, rendered) = drive(&file, "3\n2\n");

    assert_eq!(outcome.persona.id, 3);
    assert_eq!(outcome.persona.name, "Celestial Oracle");
    assert_eq!(outcome.memory, harness_core::MemoryMode::Background);
    assert_eq!(outcome.config_path, file);

    let config = read_config(&file);
    assert_eq!(config["persona"]["id"].as_u64(), Some(3));
    assert_eq!(config["persona"]["name"].as_str(), Some("Celestial Oracle"));
    assert_eq!(config["memory"]["mode"].as_str(), Some("background"));
    assert_eq!(config["first_run"].as_bool(), Some(false));

    assert!(rendered.contains("Persona configured"));
    assert!(rendered.contains("Celestial Oracle (id 3)"));
    assert!(rendered.contains("Background saver"));
    assert!(rendered.contains("Config written to"));
}

#[test]
fn the_flow_writes_the_config_and_the_gate_then_stays_closed() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("nested").join("luna").join("config.yaml");
    let layers = layers(&file);

    drive(&file, "6\n1\n");

    assert!(file.exists(), "the flow creates parent directories");
    assert!(
        !needs_onboarding(&layers).expect("resolves"),
        "an onboarded install must not be asked again"
    );

    // Re-resolving the same file is what the harness does on the next launch: the persona is there.
    let resolved = harness_core::config::resolve(&layers)
        .expect("the written file loads")
        .into_config();
    assert_eq!(resolved.persona.id, 6);
    assert_eq!(resolved.persona.name, "Supportive Android Maid");
    assert!(!resolved.first_run);
}

#[test]
fn a_blank_line_takes_the_default_persona() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    let (outcome, _) = drive(&file, "\n\n");

    assert_eq!(outcome.persona.id, 1);
    assert_eq!(read_config(&file)["persona"]["id"].as_u64(), Some(1));
}

#[test]
fn invalid_answers_are_rejected_and_then_recovered_from() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    let (outcome, rendered) = drive(&file, "0\nbanana\n11\n7\n2\n");

    assert_eq!(outcome.persona.id, 7, "the fourth answer is honoured");
    assert_eq!(
        rendered.matches('✗').count(),
        3,
        "each rejection is reported"
    );
    assert!(rendered.contains("is outside the catalog"));
    assert!(rendered.contains("is not a number"));
    assert_eq!(read_config(&file)["persona"]["id"].as_u64(), Some(7));
}

#[test]
fn a_script_that_never_answers_validly_persists_nothing() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    let error = drive_expecting_error(&file, "99\n99\n99\n99\n99\n99\n99\n99\n99\n99\n99\n");

    assert!(
        error.contains("no personality core was selected"),
        "{error}"
    );
    assert!(
        !file.exists(),
        "a rejected flow must not write a half-config"
    );
}

#[test]
fn a_closed_input_is_an_error_rather_than_a_silent_default() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    let error = drive_expecting_error(&file, "");

    assert!(error.contains("input closed"), "{error}");
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
    assert!(rendered.contains("I inspect, execute, reflect, and self-improve"));
    assert!(rendered.contains("tune my neural core to your style"));
    assert!(rendered.contains("NEURAL CORE CALIBRATION"));
    for persona in harness_onboarding::CATALOG {
        assert!(
            rendered.contains(persona.name),
            "{} is missing from the menu",
            persona.name
        );
    }
}

#[test]
fn the_memory_screen_offers_both_writers_and_a_blank_keeps_the_session_writer() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");
    let layers = layers(&file);

    let (outcome, rendered) = drive(&file, "5\n\n");

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
    // Both answers are echoed, so the user saw what they chose before the flow ended.
    assert!(rendered.contains("Session writer — I save memories as we talk"));

    let resolved = harness_core::config::resolve(&layers)
        .expect("the written file loads")
        .into_config();
    assert_eq!(resolved.memory.mode, harness_core::MemoryMode::Session);
    assert_eq!(resolved.persona.id, 5);
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
        2,
        "each rejection is reported"
    );
    assert!(rendered.contains("there is no option 9"));
    assert!(rendered.contains("'banana' is not a number"));
}

#[test]
fn a_closed_input_before_the_memory_mode_writes_nothing() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    let error = drive_expecting_error(&file, "4\n");

    assert!(
        error.contains("input closed before a memory mode was chosen"),
        "{error}"
    );
    assert!(
        !file.exists(),
        "neither answer is written when the second one is missing"
    );
}

#[test]
fn re_running_the_flow_overwrites_the_memory_mode() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    drive(&file, "4\n2\n");
    assert_eq!(
        read_config(&file)["memory"]["mode"].as_str(),
        Some("background")
    );

    // `luna setup` bypasses the first_run gate, so a second run must be able to switch back.
    drive(&file, "4\n1\n");
    assert_eq!(
        read_config(&file)["memory"]["mode"].as_str(),
        Some("session")
    );
    assert_eq!(read_config(&file)["persona"]["id"].as_u64(), Some(4));
}

#[test]
fn the_terminal_is_prepared_and_handed_back_clean() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");
    let layers = layers(&file);
    let mut reader = Cursor::new(b"2\n1\n".to_vec());
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
