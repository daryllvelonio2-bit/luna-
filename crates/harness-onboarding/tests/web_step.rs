//! End-to-end tests of the web-access step: a scripted answer against a real temp config file.
//!
//! These drive the whole flow through [`harness_onboarding::run`] rather than calling the step
//! directly, because the thing worth testing is the join: that the answer reaches
//! `tools.web.enabled`, `tools.web.search_backend` and `tools.web.search_api_key_env`, that the
//! confirmation reports it, and that the harness which loads the written file agrees with the screen
//! the user just answered.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

use harness_core::config::{Config, Layers, SearchBackend};
use harness_onboarding::{flow, run, InstantPacer, Size};

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

/// The terminal geometry every test renders against, so centering is assertable.
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

/// A blank answer, which is the keyless backend: no account, nothing to export.
#[test]
fn a_blank_web_answer_records_the_keyless_search() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    // memory, auto-compact off, gateway, notes (blank), web (blank)
    let (outcome, rendered) = drive(&file, "1\n2\n1\n\n\n");

    assert!(outcome.web.enabled);
    assert_eq!(outcome.web.backend, SearchBackend::DuckDuckGo);
    assert_eq!(outcome.web.api_key_env, None);

    let config = read_config(&file);
    assert_eq!(config["tools"]["web"]["enabled"].as_bool(), Some(true));
    assert_eq!(
        config["tools"]["web"]["search_backend"].as_str(),
        Some("duckduckgo")
    );
    assert!(
        rendered.contains("WEB ACCESS"),
        "the step has its own panel"
    );
    assert!(rendered.contains("Web access"));
    assert!(rendered.contains("DuckDuckGo"));

    // The written file resolves to the same answer the screen reported.
    let resolved = harness_core::config::resolve(&layers(&file))
        .expect("the written file loads")
        .into_config();
    assert_eq!(resolved.tools.web.search_backend, SearchBackend::DuckDuckGo);
    assert!(resolved.tools.web.enabled);
}

#[test]
fn choosing_a_keyed_backend_records_the_variable_that_holds_the_key() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    // memory, auto-compact off, gateway, notes (blank), web (2 = Brave), variable name
    let (outcome, rendered) = drive(&file, "1\n2\n1\n\n2\nMY_BRAVE_KEY\n");

    assert!(outcome.web.enabled);
    assert_eq!(outcome.web.backend, SearchBackend::Brave);
    assert_eq!(outcome.web.api_key_env.as_deref(), Some("MY_BRAVE_KEY"));

    let config = read_config(&file);
    assert_eq!(
        config["tools"]["web"]["search_backend"].as_str(),
        Some("brave")
    );
    assert_eq!(
        config["tools"]["web"]["search_api_key_env"].as_str(),
        Some("MY_BRAVE_KEY"),
        "the variable is recorded, never the key itself"
    );

    assert!(
        rendered.contains("Brave Search API — key from MY_BRAVE_KEY"),
        "{rendered}"
    );
    assert!(
        rendered.contains("export MY_BRAVE_KEY=your-key"),
        "a key that is not in the environment is flagged on the spot: {rendered}"
    );
    assert!(
        !rendered.contains("MY_BRAVE_KEY=your-key before") || rendered.contains("export"),
        "the hint names the command to run"
    );
}

#[test]
fn the_environment_variable_can_be_left_to_the_backend_default() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    // ... web (3 = Tavily), then a blank variable name.
    let (outcome, rendered) = drive(&file, "1\n2\n1\n\n3\n\n");

    assert_eq!(outcome.web.backend, SearchBackend::Tavily);
    assert_eq!(outcome.web.api_key_env.as_deref(), Some("TAVILY_API_KEY"));
    assert!(
        rendered.contains("Tavily Search API — key from TAVILY_API_KEY"),
        "{rendered}"
    );
    assert!(read_config(&file)["tools"]["web"]["search_backend"]
        .as_str()
        .map(|value| value == "tavily")
        .unwrap_or(false));
}

#[test]
fn web_access_can_be_turned_off_entirely() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    let (outcome, rendered) = drive(&file, "1\n2\n1\n\n4\n");

    assert!(!outcome.web.enabled);
    assert_eq!(outcome.web.backend, SearchBackend::Disabled);

    let config = read_config(&file);
    assert_eq!(config["tools"]["web"]["enabled"].as_bool(), Some(false));
    assert_eq!(
        config["tools"]["web"]["search_backend"].as_str(),
        Some("none")
    );
    assert!(rendered.contains("Disabled (no web tools)"), "{rendered}");
}

#[test]
fn a_rejected_web_answer_is_reported_and_the_next_one_is_honoured() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    let (outcome, rendered) = drive(&file, "1\n2\n1\n\n7\nbanana\nddg\n");

    assert_eq!(outcome.web.backend, SearchBackend::DuckDuckGo);
    assert_eq!(
        rendered.matches('✗').count(),
        2,
        "each rejection is reported: {rendered}"
    );
    assert!(rendered.contains("there is no option 7"), "{rendered}");
    assert!(rendered.contains("is not a number"), "{rendered}");
    assert_eq!(
        read_config(&file)["tools"]["web"]["search_backend"].as_str(),
        Some("duckduckgo")
    );
}

/// The choice is the last question, so a script that ends before it must still leave a working
/// install rather than an unset key.
#[test]
fn a_script_that_ends_before_the_web_question_still_leaves_a_usable_default() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");

    // memory, auto-compact off, gateway, notes (blank) — and nothing more.
    let (outcome, _) = drive(&file, "1\n2\n1\n\n");

    assert!(outcome.web.enabled);
    assert_eq!(outcome.web.backend, SearchBackend::DuckDuckGo);
    let config = read_config(&file);
    assert_eq!(config["tools"]["web"]["enabled"].as_bool(), Some(true));
    assert_eq!(
        config["tools"]["web"]["search_backend"].as_str(),
        Some("duckduckgo")
    );
}
