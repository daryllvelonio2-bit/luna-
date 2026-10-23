//! Gemini regression: its OpenAI-compatible route must authenticate with a Bearer header.
//!
//! The old native base URL produced `POST /v1beta/chat/completions?key=…`. Google answered
//! that with HTTP 400, "Missing or invalid Authorization header", because the generic
//! chat-completions client needs the `/openai` route and an `Authorization: Bearer …` header.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use harness_core::config::ProviderOverride;
use harness_core::{CompletionOptions, Message, ModelProvider};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

async fn stub_openai_provider() -> (String, JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = vec![0_u8; 8192];

        loop {
            let read = socket.read(&mut buffer).await.unwrap();
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);

            let text = String::from_utf8_lossy(&request);
            let Some(header_end) = text.find("\r\n\r\n") else {
                continue;
            };
            let content_length = text[..header_end]
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if request.len() >= header_end + 4 + content_length {
                break;
            }
        }

        let body = r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#;
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.flush().await;
        tokio::time::sleep(Duration::from_millis(50)).await;

        String::from_utf8_lossy(&request).to_string()
    });

    (format!("http://{}", addr), handle)
}

#[tokio::test]
async fn gemini_request_uses_bearer_authorization() {
    let (base_url, server) = stub_openai_provider().await;
    let dir = tempfile::tempdir().unwrap();
    let manager = CredentialManager::new(Some(dir.path().join("credentials.json"))).unwrap();
    manager
        .add_credential("gemini", "test", "gemini-secret", false)
        .await
        .unwrap();

    let overrides = ProviderOverride {
        base_url: Some(base_url),
        ..Default::default()
    };
    let client = GenericProviderClient::new(
        "gemini",
        "gemini-3.5-flash-lite",
        manager,
        Arc::new(ModelDiscovery::new(
            Some(dir.path().join("models_cache.json")),
            1,
            BTreeMap::new(),
        )),
        Some(&overrides),
    )
    .unwrap();

    let reply = client
        .complete(&[Message::user("ping")], &[], &CompletionOptions::default())
        .await
        .expect("the stub accepts the Gemini request");
    assert_eq!(reply.content.as_deref(), Some("ok"));

    let request = server.await.unwrap();
    assert!(
        request.contains("Bearer gemini-secret"),
        "Gemini's OpenAI-compatible route must receive a Bearer header: {}",
        request.lines().next().unwrap_or_default()
    );
    assert!(
        !request.contains("key=gemini-secret"),
        "the API key must not be placed in the query string for inference"
    );
}
