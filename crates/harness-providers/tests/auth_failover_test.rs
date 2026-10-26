//! Key failover: the credential that a provider actually rejected must be the one retired, and
//! the request must be served by another key.
//!
//! `mark_credential_failed` used to ignore its `api_key` argument (retiring whatever came first)
//! and early-return unless the strategy was `FillFirst`, and nothing in the client ever called
//! it, so a revoked or rate-limited key stayed selected forever. These tests cover both halves:
//! the manager primitive, and the client reporting a 401/429 back to it.
//!
//! The failover happens INSIDE the request: a rejected key is retired and the request is re-sent
//! with the next key at once. Handing the error back instead made the caller wait out the
//! provider's `Retry-After` (up to ten minutes) before it discovered the other key.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use harness_core::config::ProviderOverride;
use harness_core::{CompletionOptions, Message, ModelProvider};
use harness_providers::{
    CredentialManager, GenericProviderClient, ModelDiscovery, RotationStrategy,
};
use secrecy::ExposeSecret;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinHandle;

fn manager(dir: &tempfile::TempDir) -> Arc<CredentialManager> {
    CredentialManager::new(Some(dir.path().join("credentials.json")))
        .expect("a fresh temp store path always loads")
}

async fn seed_two_keys(mgr: &Arc<CredentialManager>) {
    mgr.add_credential("openai", "first", "sk-11111", false)
        .await
        .unwrap();
    mgr.add_credential("openai", "priority", "sk-22222", true)
        .await
        .unwrap();
    mgr.set_rotation_strategy("openai", RotationStrategy::FillFirst)
        .await
        .unwrap();
}

fn disabled_labels(creds: &[harness_providers::CredentialEntry]) -> Vec<String> {
    creds
        .iter()
        .filter(|c| c.is_temporarily_disabled)
        .map(|c| c.label.clone())
        .collect()
}

/// Stub provider: answers each connection with the next status in `statuses` and records the
/// request it received. It serves exactly `statuses.len()` connections and then stops, so a
/// client that asks for more is told so by a refused connection.
async fn stub_provider(statuses: Vec<&'static str>) -> (String, JoinHandle<Vec<String>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let handle = tokio::spawn(async move {
        let mut requests = Vec::new();
        for status in statuses {
            let Ok((mut sock, _)) = listener.accept().await else {
                break;
            };
            let mut buf = vec![0u8; 8192];
            let n = match sock.read(&mut buf).await {
                Ok(n) => n,
                Err(_) => break,
            };
            requests.push(String::from_utf8_lossy(&buf[..n]).to_string());

            let body = if status.starts_with("200") {
                r#"{"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#
            } else {
                ""
            };
            let response = format!(
                "HTTP/1.1 {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                status,
                body.len(),
                body
            );
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.flush().await;
            // Give the client a moment to read the response before the socket drops.
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        requests
    });

    (format!("http://{}", addr), handle)
}

fn client_for(
    base_url: String,
    mgr: &Arc<CredentialManager>,
    dir: &tempfile::TempDir,
) -> GenericProviderClient {
    // The base URL is a `providers.<id>.*` config override now, not a constructor argument.
    let overrides = ProviderOverride {
        base_url: Some(base_url),
        ..Default::default()
    };
    GenericProviderClient::new(
        "openai",
        "gpt-4o",
        mgr.clone(),
        Arc::new(ModelDiscovery::new(
            Some(dir.path().join("models_cache.json")),
            1,
            BTreeMap::new(),
        )),
        Some(&overrides),
    )
    .expect("a known provider with a valid base URL builds a client")
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
        mgr.acquire_credential("openai")
            .await
            .unwrap()
            .expose_secret(),
        "sk-11111"
    );
}

#[tokio::test]
async fn a_rate_limited_key_is_replaced_within_the_request() {
    let (base_url, server) = stub_provider(vec!["429 Too Many Requests", "200 OK"]).await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = manager(&dir);
    seed_two_keys(&mgr).await;

    let client = client_for(base_url, &mgr, &dir);
    let reply = client
        .complete(&[Message::user("ping")], &[], &CompletionOptions::default())
        .await
        .expect("the second key must serve the request");
    let requests = server.await.unwrap();

    assert_eq!(requests.len(), 2, "the request must be re-sent once");
    assert!(
        requests[0].contains("Bearer sk-22222"),
        "the first attempt must use the priority key: {}",
        requests[0].lines().next().unwrap_or_default()
    );
    assert!(
        requests[1].contains("Bearer sk-11111"),
        "the retry must use the other key: {}",
        requests[1].lines().next().unwrap_or_default()
    );
    assert_eq!(
        reply.content.as_deref(),
        Some("ok"),
        "the successful attempt's answer is the one returned"
    );
    assert_eq!(
        disabled_labels(&mgr.get_credentials("openai").await),
        vec!["priority".to_string()],
        "the rate-limited key is retired"
    );
}

#[tokio::test]
async fn a_rejected_key_is_replaced_within_the_request() {
    let (base_url, server) = stub_provider(vec!["401 Unauthorized", "200 OK"]).await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = manager(&dir);
    seed_two_keys(&mgr).await;

    let client = client_for(base_url, &mgr, &dir);
    let reply = client
        .complete(&[Message::user("ping")], &[], &CompletionOptions::default())
        .await
        .expect("the second key must serve the request");
    let requests = server.await.unwrap();

    assert_eq!(requests.len(), 2, "the request must be re-sent once");
    assert!(
        requests[0].contains("Bearer sk-22222"),
        "the first attempt must use the priority key"
    );
    assert!(
        requests[1].contains("Bearer sk-11111"),
        "the retry must use the other key"
    );
    assert_eq!(reply.content.as_deref(), Some("ok"));
    assert_eq!(
        disabled_labels(&mgr.get_credentials("openai").await),
        vec!["priority".to_string()],
        "the rejected key is retired"
    );
}

#[tokio::test]
async fn client_retires_the_rate_limited_key_on_429() {
    let (base_url, server) =
        stub_provider(vec!["429 Too Many Requests", "429 Too Many Requests"]).await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = manager(&dir);
    seed_two_keys(&mgr).await;

    let client = client_for(base_url, &mgr, &dir);
    let err = client
        .complete(&[Message::user("ping")], &[], &CompletionOptions::default())
        .await
        .expect_err("both keys are rate limited, so the error must surface");
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
        mgr.acquire_credential("openai")
            .await
            .unwrap()
            .expose_secret(),
        "sk-11111"
    );
}

#[tokio::test]
async fn client_retires_the_rejected_key_on_401() {
    let (base_url, server) = stub_provider(vec!["401 Unauthorized", "401 Unauthorized"]).await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = manager(&dir);
    seed_two_keys(&mgr).await;

    let client = client_for(base_url, &mgr, &dir);
    let err = client
        .complete(&[Message::user("ping")], &[], &CompletionOptions::default())
        .await
        .expect_err("both keys are rejected, so the error must surface");

    let requests = server.await.unwrap();
    assert!(
        requests[0].contains("Bearer sk-22222"),
        "the client must have sent the priority key first: {}",
        requests[0].lines().next().unwrap_or_default()
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
        mgr.acquire_credential("openai")
            .await
            .unwrap()
            .expose_secret(),
        "sk-11111"
    );
}

#[tokio::test]
async fn client_retires_the_rejected_key_on_stream_401() {
    let (base_url, server) = stub_provider(vec!["401 Unauthorized", "401 Unauthorized"]).await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = manager(&dir);
    seed_two_keys(&mgr).await;

    let client = client_for(base_url, &mgr, &dir);
    let err = match client
        .stream(&[Message::user("ping")], &[], &CompletionOptions::default())
        .await
    {
        Ok(_) => panic!("a 401 stream must surface as an error"),
        Err(e) => e,
    };

    let requests = server.await.unwrap();
    assert!(
        requests[0].contains("Bearer sk-22222"),
        "the client must have sent the priority key first"
    );
    assert!(
        format!("{:?}", err).contains("401"),
        "unexpected error: {:?}",
        err
    );

    assert_eq!(
        disabled_labels(&mgr.get_credentials("openai").await),
        vec!["priority".to_string()],
        "stream path must retire the rejected key so the next call fails over"
    );
    assert_eq!(
        mgr.acquire_credential("openai")
            .await
            .unwrap()
            .expose_secret(),
        "sk-11111"
    );
}

#[tokio::test]
async fn a_rate_limited_key_is_replaced_within_the_stream_request() {
    let (base_url, server) = stub_provider(vec!["429 Too Many Requests", "200 OK"]).await;
    let dir = tempfile::tempdir().unwrap();
    let mgr = manager(&dir);
    seed_two_keys(&mgr).await;

    let client = client_for(base_url, &mgr, &dir);
    let _stream = client
        .stream(&[Message::user("ping")], &[], &CompletionOptions::default())
        .await
        .expect("the second key must serve the stream");
    let requests = server.await.unwrap();

    assert_eq!(requests.len(), 2, "the stream request must be re-sent once");
    assert!(
        requests[0].contains("Bearer sk-22222"),
        "the first attempt must use the priority key"
    );
    assert!(
        requests[1].contains("Bearer sk-11111"),
        "the retry must use the other key"
    );
    assert_eq!(
        disabled_labels(&mgr.get_credentials("openai").await),
        vec!["priority".to_string()],
        "the rate-limited key is retired"
    );
}
