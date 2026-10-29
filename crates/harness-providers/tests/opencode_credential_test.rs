//! Which OpenCode credentials Luna finds on the machine, and which of them may be handed out
//! first.
//!
//! The anonymous free-tier token is metered: when it wins the rotation, every request becomes a
//! rate-limit wait (HTTP 429, `Retry-After: 600`) even though the user has a real account key
//! sitting in the same pool. The rules under test: a real account key is detected and is the
//! priority one, the free token is only ever a fallback, and a key that belongs to another
//! provider (Groq) never lands in the OpenCode pool.

use harness_providers::{detect_opencode_credentials_in, DetectedOpencodeCredential};

fn write_json(path: &std::path::Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn find<'a>(
    detected: &'a [DetectedOpencodeCredential],
    provider: &str,
    label: &str,
) -> Option<&'a DetectedOpencodeCredential> {
    detected
        .iter()
        .find(|c| c.provider == provider && c.label == label)
}

#[test]
fn the_logged_in_opencode_account_is_detected_as_the_priority_key() {
    let home = tempfile::tempdir().unwrap();
    write_json(
        &home.path().join(".local/share/opencode/auth.json"),
        r#"{"opencode":{"type":"api","key":"sk-account"},"groq":{"type":"api","key":"gsk-groq"}}"#,
    );

    let detected = detect_opencode_credentials_in(home.path(), None).unwrap();

    let account = find(&detected, "opencode", "opencode-account").expect(
        "the key OpenCode itself stores must be found: it used to be read for Groq only, so a \
         logged-in account was invisible to Luna",
    );
    assert_eq!(account.api_key, "sk-account");
    assert!(
        account.is_priority,
        "a real account key must be the one the pool hands out first"
    );

    let free =
        find(&detected, "opencode", "opencode-free").expect("the free tier stays a fallback");
    assert_eq!(free.api_key, "public");
    assert!(
        !free.is_priority,
        "the metered free token must never outrank a real account"
    );
}

#[test]
fn a_groq_key_is_registered_under_groq_not_under_opencode() {
    let home = tempfile::tempdir().unwrap();
    write_json(
        &home.path().join(".local/share/opencode/auth.json"),
        r#"{"groq":{"type":"api","key":"gsk-groq"}}"#,
    );

    let detected = detect_opencode_credentials_in(home.path(), None).unwrap();

    let groq = find(&detected, "groq", "opencode-groq").expect("the Groq key is found");
    assert_eq!(groq.api_key, "gsk-groq");
    assert!(
        !detected
            .iter()
            .any(|c| c.provider == "opencode" && c.api_key == "gsk-groq"),
        "a Groq key in the OpenCode pool only earns a rejection from opencode.ai"
    );
}

#[test]
fn the_shiina_opencode_zen_token_is_detected_as_the_priority_key() {
    let home = tempfile::tempdir().unwrap();
    write_json(
        &home.path().join(".shiina/auth.json"),
        r#"{"credential_pool":{"opencode-zen":[{"label":"main","access_token":"sk-zen"}]}}"#,
    );

    let detected = detect_opencode_credentials_in(home.path(), None).unwrap();

    let zen = find(&detected, "opencode", "main").expect("the Shiina token is found");
    assert_eq!(zen.api_key, "sk-zen");
    assert!(zen.is_priority, "a real account key is the priority one");
}

#[test]
fn the_free_token_is_the_only_credential_on_a_machine_without_an_account() {
    let home = tempfile::tempdir().unwrap();

    let detected = detect_opencode_credentials_in(home.path(), None).unwrap();

    assert_eq!(detected.len(), 1, "only the free fallback is offered");
    assert_eq!(detected[0].api_key, "public");
    assert!(
        !detected[0].is_priority,
        "the fallback is never the priority key"
    );
}
