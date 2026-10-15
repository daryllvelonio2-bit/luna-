//! Key failover: the credential that a provider actually rejected must be the one retired, and the
//! next call must be served by another key.
//!
//! `mark_credential_failed` used to ignore its `api_key` argument (retiring whatever came first) and
//! early-return unless the strategy was `FillFirst`, and nothing in the client ever called it — so a
//! revoked or rate-limited key stayed selected forever. These tests cover both halves: the manager
//! primitive, and the client reporting a 401/429 back to it.

use std::sync::Arc;
use std::time::Duration;

use harness_core::{CompletionOptions, Message, ModelProvider};
use harness_providers::{
    CredentialManager, GenericProviderClient, ModelDiscovery, RotationStrategy,
};
use secrecy::ExposeSecret;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinHandle;

fn manager(dir: &tempfile::TempDir) -> Arc<CredentialManager> {
    CredentialManager::new(Some(dir.path().join("credentials.json")))
}

async fn seed_two_keys(mgr: &Arc<CredentialManager>) {
    mgr.add_credential("openai", "first", "sk-11111", false).await.unwrap();
    mgr.add_credential("openai", "priority", "sk-22222", true).await.unwrap();
    mgr.set_rotation_strategy("openai", RotationStrategy::FillFirst).await.unwrap();
}

fn disabled_labels(creds: &[harness_providers::CredentialEntry]) -> Vec<String> {
    creds
        .iter()
        .filter(|c| c.is_temporarily_disabled)
        .map(|c| c.label.clone())
        .collect()
}

/// One-shot stub provider: captures the request it received and answers `status_line`.
async fn stub_provider(status_line: &'static str) -> (String, JoinHandle<String>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 8192];
        let n = sock.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        let response = format!(
            "HTTP/1.1 {}\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            status_line
        );
        let _ = sock.write_all(response.as_bytes()).await;
        let _ = sock.flush().await;
        // Give the client a moment to read the response before the socket drops.
        tokio::time::sleep(Duration::from_millis(50)).await;
        request
    });

    (format!("http://{}", addr), handle)
}

fn client_for(base_url: String, mgr: &Arc<CredentialManager>, dir: &tempfile::TempDir) -> GenericProviderClient {
    GenericProviderClient::new(
        "openai",
        Some(base_url),
        "gpt-4o",
        mgr.clone(),
        Arc::new(ModelDiscovery::new(Some(dir.path().join("models_cache.json")), 1)),
    )
}

#[tokio::test]
async fn failed_priority_key_fails_over_to_the_other_key() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = manager(&dir);
    seed_two_keys(&mgr).await;

    let selected = mgr.acquire_credential("openai").await.unwrap();
    assert_eq!(
        selected.expose_secret(),
        "sk-22222",
        "fill_first must hand out the priority key first"
    );

    assert!(
        mgr.mark_credential_failed("openai", "sk-22222").await,
        "reporting the failing key must retire it"
    );

    let next = mgr.acquire_credential("openai").await.unwrap();
    assert_eq!(
        next.expose_secret(),
        "sk-11111",
        "the next acquire must fail over to the other key"
    );
}

#[tokio::test]
async fn only_the_reported_key_is_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let mgr = manager(&dir);
    seed_two_keys(&mgr).await;

    mgr.mark_credential_failed("openai", "sk-22222").await;

    assert_eq!(
        disabled_labels(&mgr.get_credentials("openai").await),
        vec!["priority".to_string()],
        "only the key that was reported failed may end up disabled"
    );

    // The last active key is never retired, or the provider would have nothing left to hand out.
    assert!(!mgr.mark_credential_failed("openai", "sk-11111").await);
    assert_eq!(
        disabled_labels(&mgr.get_credentials("openai").await),
        vec!["priority".to_string()]
    );
    assert_eq!(
        mgr.acquire_credential("openai").await.unwrap().expose_secret(),
        "sk-11111"
    );
}

#[tokio::test]
async fn client_retires_the_rate_limited_key_on_429() {
    let (base_url, server) = stub_provider("429 Too Many Requests").await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = manager(&dir);
    seed_two_keys(&mgr).await;

    let client = client_for(base_url, &mgr, &dir);
    let err = client
        .complete(&[Message::user("ping")], &[], &CompletionOptions::default())
        .await
        .expect_err("a 429 must surface as an error");
    let _ = server.await;

    assert!(
        matches!(err, harness_core::HarnessError::RateLimited { .. }),
        "unexpected error: {:?}",
        err
    );
    assert_eq!(
        disabled_labels(&mgr.get_credentials("openai").await),
        vec!["priority".to_string()],
        "a rate-limited key must be retired so the next turn fails over"
    );
    assert_eq!(
        mgr.acquire_credential("openai").await.unwrap().expose_secret(),
        "sk-11111"
    );
}

#[tokio::test]
async fn client_retires_the_rejected_key_on_401() {
    let (base_url, server) = stub_provider("401 Unauthorized").await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = manager(&dir);
    seed_two_keys(&mgr).await;

    let client = client_for(base_url, &mgr, &dir);
    let err = client
        .complete(&[Message::user("ping")], &[], &CompletionOptions::default())
        .await
        .expect_err("a 401 must surface as an error");

    let request = server.await.unwrap();
    assert!(
        request.contains("Bearer sk-22222"),
        "the client must have sent the priority key first: {}",
        request.lines().next().unwrap_or_default()
    );
    assert!(
        format!("{:?}", err).contains("401"),
        "unexpected error: {:?}",
        err
    );

    assert_eq!(
        disabled_labels(&mgr.get_credentials("openai").await),
        vec!["priority".to_string()],
        "the key the provider rejected is the one retired by the client path"
    );
    assert_eq!(
        mgr.acquire_credential("openai").await.unwrap().expose_secret(),
        "sk-11111"
    );
}