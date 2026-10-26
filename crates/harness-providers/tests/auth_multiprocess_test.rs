//! The credential store is shared by every Luna process on the machine, so a write made in one
//! process must not undo a change made in another.
//!
//! Every write used to start from the writing process's own in-memory view of the store. A chat
//! session that started before `luna auth remove` or `luna auth priority` ran therefore wrote
//! its stale view back on the next request: the credential the user had just deleted came back,
//! and the key they had just promoted lost its place. The user could not make a credential
//! change stick while a session was open, which is exactly what "my credentials are not being
//! used" looks like.

use std::path::Path;
use std::sync::Arc;

use harness_providers::{CredentialManager, RotationStrategy};
use secrecy::ExposeSecret;

fn manager(path: &Path) -> Arc<CredentialManager> {
    CredentialManager::new(Some(path.to_path_buf())).expect("a fresh temp store path always loads")
}

/// A session that was already running must not resurrect a credential the user deleted in
/// another process.
#[tokio::test]
async fn a_running_session_does_not_resurrect_a_deleted_credential() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.json");

    // The "session" process: it loads the store and keeps it in memory.
    let session = manager(&path);
    session
        .add_credential("openai", "keep", "sk-keep", false)
        .await
        .unwrap();
    session
        .add_credential("openai", "gone", "sk-gone", false)
        .await
        .unwrap();

    // Another process deletes one of them.
    let editor = manager(&path);
    assert!(editor.remove_credential("openai", "gone").await.unwrap());

    // The session keeps working: its next write must leave the deletion alone.
    let _ = session.acquire_credential("openai").await.unwrap();

    let after = manager(&path);
    let labels: Vec<String> = after
        .get_credentials("openai")
        .await
        .into_iter()
        .map(|c| c.label)
        .collect();
    assert_eq!(
        labels,
        vec!["keep".to_string()],
        "the deleted credential must stay deleted"
    );
}

/// A session that was already running must not demote the key the user promoted in another
/// process: the metered free token kept winning the rotation this way, and every request went
/// out on it.
#[tokio::test]
async fn a_running_session_does_not_demote_a_promoted_credential() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.json");

    let session = manager(&path);
    session
        .add_credential("openai", "free", "public", true)
        .await
        .unwrap();
    session
        .add_credential("openai", "account", "sk-account", false)
        .await
        .unwrap();
    session
        .set_rotation_strategy("openai", RotationStrategy::FillFirst)
        .await
        .unwrap();

    // Another process promotes the account key.
    let editor = manager(&path);
    assert!(editor
        .set_priority_credential("openai", "account")
        .await
        .unwrap());

    // The session's next request must use the key the user promoted.
    let key = session.acquire_credential("openai").await.unwrap();
    assert_eq!(
        key.expose_secret(),
        "sk-account",
        "the promoted key must stay the one the pool hands out"
    );

    let after = manager(&path);
    let priority: Vec<String> = after
        .get_credentials("openai")
        .await
        .into_iter()
        .filter(|c| c.is_priority)
        .map(|c| c.label)
        .collect();
    assert_eq!(
        priority,
        vec!["account".to_string()],
        "the priority flag must survive a write from another process"
    );
}

/// A credential added in another process must become usable without restarting this one.
#[tokio::test]
async fn a_credential_added_elsewhere_becomes_usable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.json");

    let session = manager(&path);

    let editor = manager(&path);
    editor
        .add_credential("openai", "account", "sk-account", true)
        .await
        .unwrap();

    let key = session.acquire_credential("openai").await.unwrap();
    assert_eq!(
        key.expose_secret(),
        "sk-account",
        "a key added in another process must be picked up"
    );
}
