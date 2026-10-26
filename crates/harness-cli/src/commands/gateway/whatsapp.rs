//! WhatsApp Cloud API platform adapter.
//!
//! Outbound: Meta Graph API `/messages`.
//! Inbound: Lightweight HTTP webhook server on configured port.

use async_trait::async_trait;
use harness_core::error::HarnessError;
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tracing::{debug, error, info};

use super::adapter::{InboundMessage, PlatformAdapter};

/// WhatsApp Cloud platform adapter.
pub struct WhatsAppAdapter {
    token: String,
    phone_number_id: String,
    #[allow(dead_code)]
    verify_token: String,
    client: Client,
    inbound_rx: UnboundedReceiver<InboundMessage>,
    _server_task: Option<tokio::task::JoinHandle<()>>,
}

impl WhatsAppAdapter {
    pub async fn new(
        token: String,
        phone_number_id: String,
        verify_token: String,
        port: u16,
    ) -> Result<Self, HarnessError> {
        let (tx, rx) = unbounded_channel();

        // Spawn lightweight webhook server
        let vt = Arc::new(verify_token.clone());
        let tx_clone = tx.clone();
        let handle = tokio::spawn(async move {
            run_webhook_server(port, vt, tx_clone).await;
        });

        Ok(Self {
            token,
            phone_number_id,
            verify_token,
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            inbound_rx: rx,
            _server_task: Some(handle),
        })
    }
}

async fn run_webhook_server(
    port: u16,
    verify_token: Arc<String>,
    tx: UnboundedSender<InboundMessage>,
) {
    let addr = format!("0.0.0.0:{port}");
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => {
            info!("WhatsApp webhook server listening on http://{addr}");
            l
        }
        Err(e) => {
            error!("Failed to bind WhatsApp webhook server to {addr}: {e}");
            return;
        }
    };

    loop {
        let (mut socket, _) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => continue,
        };

        let vt = verify_token.clone();
        let tx = tx.clone();

        tokio::spawn(async move {
            let mut buf = vec![0u8; 8192];
            let n = match socket.read(&mut buf).await {
                Ok(n) if n > 0 => n,
                _ => return,
            };

            let req_str = String::from_utf8_lossy(&buf[..n]);
            let first_line = req_str.lines().next().unwrap_or("");
            let mut parts = first_line.split_whitespace();
            let method = parts.next().unwrap_or("");
            let path = parts.next().unwrap_or("");

            if method == "GET" {
                // Verification challenge: e.g. /webhook?hub.mode=subscribe&hub.challenge=1158201444&hub.verify_token=me
                let challenge = extract_query_param(path, "hub.challenge");
                let token = extract_query_param(path, "hub.verify_token");

                if let (Some(ch), Some(tok)) = (challenge, token) {
                    if tok == *vt {
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
                            ch.len(),
                            ch
                        );
                        let _ = socket.write_all(response.as_bytes()).await;
                        return;
                    }
                }

                let response = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n";
                let _ = socket.write_all(response.as_bytes()).await;
            } else if method == "POST" {
                // Acknowledge immediately to Meta
                let ack = "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n";
                let _ = socket.write_all(ack.as_bytes()).await;

                // Extract body after \r\n\r\n
                if let Some(body_start) = req_str.find("\r\n\r\n") {
                    let body = &req_str[body_start + 4..];
                    if let Ok(val) = serde_json::from_str::<Value>(body) {
                        parse_and_enqueue_whatsapp_messages(&val, &tx);
                    }
                }
            }
        });
    }
}

pub(crate) fn extract_query_param(path: &str, key: &str) -> Option<String> {
    let q_index = path.find('?')?;
    let query = &path[q_index + 1..];
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        if k == key {
            return Some(v.into_owned());
        }
    }
    None
}

fn parse_and_enqueue_whatsapp_messages(payload: &Value, tx: &UnboundedSender<InboundMessage>) {
    let Some(entries) = payload.get("entry").and_then(|e| e.as_array()) else {
        return;
    };

    for entry in entries {
        let Some(changes) = entry.get("changes").and_then(|c| c.as_array()) else {
            continue;
        };
        for change in changes {
            let Some(value) = change.get("value") else {
                continue;
            };
            let Some(messages) = value.get("messages").and_then(|m| m.as_array()) else {
                continue;
            };

            for msg in messages {
                let from = msg.get("from").and_then(|f| f.as_str()).unwrap_or("");
                let msg_id = msg.get("id").and_then(|i| i.as_str()).map(str::to_string);
                let text = msg
                    .get("text")
                    .and_then(|t| t.get("body"))
                    .and_then(|b| b.as_str())
                    .unwrap_or("");

                if text.trim().is_empty() || from.is_empty() {
                    continue;
                }

                info!("Inbound WhatsApp message from {from}: {text}");

                let _ = tx.send(InboundMessage {
                    platform: "whatsapp".to_string(),
                    chat_id: from.to_string(),
                    sender_id: from.to_string(),
                    sender_name: Some(from.to_string()),
                    text: text.to_string(),
                    message_id: msg_id,
                });
            }
        }
    }
}

#[async_trait]
impl PlatformAdapter for WhatsAppAdapter {
    fn name(&self) -> &'static str {
        "whatsapp"
    }

    async fn send_message(&self, chat_id: &str, text: &str) -> Result<(), HarnessError> {
        let url = format!(
            "https://graph.facebook.com/v21.0/{}/messages",
            self.phone_number_id
        );

        let payload = json!({
            "messaging_product": "whatsapp",
            "recipient_type": "individual",
            "to": chat_id,
            "type": "text",
            "text": {
                "preview_url": false,
                "body": text
            }
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                debug!("WhatsApp message sent to {chat_id}");
            }
            Ok(r) => {
                let status = r.status();
                let err_body = r.text().await.unwrap_or_default();
                error!("WhatsApp send failed: HTTP {status} - {err_body}");
            }
            Err(e) => {
                error!("WhatsApp send network error: {e}");
            }
        }

        Ok(())
    }

    async fn poll_messages(&mut self) -> Result<Vec<InboundMessage>, HarnessError> {
        let mut messages = Vec::new();
        while let Ok(msg) = self.inbound_rx.try_recv() {
            messages.push(msg);
        }
        Ok(messages)
    }
}
