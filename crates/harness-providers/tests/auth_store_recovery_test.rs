//! A corrupt credential store must never be silently treated as "no credentials".
//!
//! `CredentialsManager::new` used to `unwrap_or_default()` every load failure, so a truncated
//! `~/.luna/credentials.json` (a `kill -9` mid-write, a half-restored backup, a hand-edit) loaded
//! as an empty store and the next write persisted that emptiness over every registered key.

use std::fs;
use std::path::{Path, PathBuf};

use harness_providers::CredentialManager;

/// A valid store holding two keys for `openai`.
const VALID_STORE: &str = r#"{
  "providers": {
    "openai": {
      "strategy": "round_robin",
      "credentials": [
        {"id": "id-work", "label": "work", "api_key": "sk-fake-work", "is_priority": true, "usage_count": 0, "last_used": null, "is_temporarily_disabled": false},
        {"id": "id-backup", "label": "backup", "api_key": "sk-fake-backup", "is_priority": false, "usage_count": 0, "last_used": null, "is_temporarily_disabled": false}
      ],
      "current_round_robin_index": 0
    }
  }
}"#;

/// The prefix of a valid store that a killed process leaves behind (42 bytes).
fn truncated_store() -> &'static str {
    &VALID_STORE[..42]
}

fn temp_store(contents: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("credentials.json");
    fs::write(&path, contents).expect("write store");
    (dir, path)
}

/// The store's load error as text. A `Result::unwrap_err` is not usable here: it needs
/// `CredentialManager: Debug`, which must not exist — its state holds the plaintext keys.
fn load_err(path: &Path) -> String {
    match CredentialManager::new(Some(path.to_path_buf())) {
        Ok(_) => panic!("expected {} to fail to load", path.display()),
        Err(e) => e.to_string(),
    }
}

#[test]
fn a_truncated_store_is_a_hard_error_naming_the_path() {
    let (_dir, path) = temp_store(truncated_store());

    // No manager exists after the failure, so `get_credentials` — and every write that would
    // follow it — is unreachable: the caller gets the error, not an empty store.
    let rendered = load_err(&path);

    assert!(
        rendered.contains(&path.display().to_string()),
        "error must name the store path, got: {rendered}"
    );
    assert!(
        rendered.contains("not valid JSON"),
        "error must name the parse problem, got: {rendered}"
    );
    assert!(
        rendered.contains("refusing to start"),
        "error must say what was refused, got: {rendered}"
    );

    // The surviving bytes — and therefore the keys still inside them — were not discarded.
    assert_eq!(fs::read_to_string(&path).unwrap(), truncated_store());
}

#[test]
fn garbage_and_empty_stores_are_errors_too() {
    for contents in ["not json at all", "", r#"{"providers": "wrong shape"}"#] {
        let (_dir, path) = temp_store(contents);
        let rendered = load_err(&path);
        assert!(
            rendered.contains(&path.display().to_string()),
            "error must name the store path, got: {rendered}"
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), contents);
    }
}

#[tokio::test]
async fn a_write_never_clobbers_a_store_that_does_not_read_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.json");
    // No file yet: an empty store, loaded without error.
    let manager = CredentialManager::new(Some(path.clone())).unwrap();
    assert!(manager.get_credentials("openai").await.is_empty());

    // Something truncates the store between the load and the write.
    fs::write(&path, truncated_store()).unwrap();

    let err = manager
        .add_credential("openai", "new", "sk-new", false)
        .await
        .expect_err("the add path must refuse to overwrite an unreadable store");
    assert!(
        err.to_string().contains(&path.display().to_string()),
        "error must name the store path, got: {err}"
    );
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        truncated_store(),
        "the add path overwrote the store instead of refusing"
    );
}

#[tokio::test]
async fn repairing_the_file_recovers_every_key() {
    let (_dir, path) = temp_store(VALID_STORE);

    let healthy = CredentialManager::new(Some(path.clone())).unwrap();
    assert_eq!(healthy.get_credentials("openai").await.len(), 2);
    drop(healthy);

    fs::write(&path, truncated_store()).unwrap();
    assert!(CredentialManager::new(Some(path.clone())).is_err());

    // Nothing wrote `{}` over the file in the meantime; a repaired store still has both keys.
    fs::write(&path, VALID_STORE).unwrap();
    let repaired = CredentialManager::new(Some(path)).unwrap();
    let labels: Vec<String> = repaired
        .get_credentials("openai")
        .await
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert_eq!(labels, vec!["work".to_string(), "backup".to_string()]);
}
