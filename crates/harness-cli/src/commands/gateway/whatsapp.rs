//! WhatsApp Cloud API platform adapter.
//!
//! Outbound: Meta Graph API `/messages`.
//! Inbound: webhook server on the configured port.
//!
//! The webhook is the only part of the gateway that listens, so it is the only part an unauthenticated
//! caller can reach — and what it receives becomes a prompt to an agent that holds tools. Every
//! delivery is therefore checked against Meta's `X-Hub-Signature-256` (HMAC-SHA256 of the raw body,
//! keyed with the app secret) before it is read as a message, and the server refuses to start without
//! the secret rather than accepting deliveries nobody signed.

use std::sync::Arc;

use async_trait::async_trait;
use hmac::{Hmac, Mac};
use reqwest::Client;
use serde_json::{json, Value};
use sha2::Sha256;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tracing::{debug, error, info, warn};

use harness_core::error::HarnessError;

use super::adapter::{InboundMessage, PlatformAdapter};
use super::outbound::{post_json, send_error};

/// The most header bytes a request may carry before it is refused.
const MAX_HEAD_BYTES: usize = 16 * 1024;

/// The most body bytes a delivery may carry. Meta's payloads are kilobytes; anything larger is a
/// caller this process is not being asked to serve.
const MAX_BODY_BYTES: usize = 1024 * 1024;

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
    /// `app_secret` is the Meta app secret the deliveries are signed with. It is required: the
    /// webhook is a listening socket whose contents are prompts.
    pub async fn new(
        token: String,
        phone_number_id: String,
        verify_token: String,
        app_secret: String,
        port: u16,
    ) -> Result<Self, HarnessError> {
        let (tx, rx) = unbounded_channel();

        let vt = Arc::new(verify_token.clone());
        let secret = Arc::new(app_secret);
        let tx_clone = tx.clone();
        let handle = tokio::spawn(async move {
            run_webhook_server(port, vt, secret, tx_clone).await;
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

/// One HTTP request, as much of it as this server needs.
struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

async fn run_webhook_server(
    port: u16,
    verify_token: Arc<String>,
    app_secret: Arc<String>,
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
        let (mut socket, peer) = match listener.accept().await {
            Ok(s) => s,
            Err(_) => continue,
        };

        let vt = verify_token.clone();
        let secret = app_secret.clone();
        let tx = tx.clone();

        tokio::spawn(async move {
            let request = match read_request(&mut socket).await {
                Ok(request) => request,
                Err(error) => {
                    warn!("WhatsApp webhook: cannot read the request from {peer}: {error}");
                    let _ = respond(&mut socket, 400, "Bad Request", "").await;
                    return;
                }
            };

            if request.method == "GET" {
                // Verification challenge: /webhook?hub.mode=subscribe&hub.challenge=...&hub.verify_token=...
                let challenge = extract_query_param(&request.path, "hub.challenge");
                let token = extract_query_param(&request.path, "hub.verify_token");
                match (challenge, token) {
                    (Some(challenge), Some(token)) if token == *vt => {
                        let _ = respond(&mut socket, 200, "OK", &challenge).await;
                    }
                    _ => {
                        warn!(
                            "WhatsApp webhook: refused a verification challenge with a wrong token"
                        );
                        let _ = respond(&mut socket, 403, "Forbidden", "").await;
                    }
                }
                return;
            }

            if request.method != "POST" {
                let _ = respond(&mut socket, 405, "Method Not Allowed", "").await;
                return;
            }

            // Read, then verify, then acknowledge. Acknowledging first — which is what this did —
            // answers 200 to a delivery it has not looked at, so a rejected one is never retried by
            // Meta and the loss is invisible.
            if !signature_matches(
                &secret,
                &request.body,
                request.header("X-Hub-Signature-256"),
            ) {
                warn!(
                    "WhatsApp webhook: refused an unsigned or mis-signed delivery from {peer} ({} bytes)",
                    request.body.len()
                );
                let _ = respond(&mut socket, 401, "Unauthorized", "").await;
                return;
            }

            match serde_json::from_slice::<Value>(&request.body) {
                Ok(value) => {
                    parse_and_enqueue_whatsapp_messages(&value, &tx);
                    let _ = respond(&mut socket, 200, "OK", "").await;
                }
                Err(error) => {
                    warn!("WhatsApp webhook: signed delivery is not JSON: {error}");
                    let _ = respond(&mut socket, 400, "Bad Request", "").await;
                }
            }
        });
    }
}

/// Read the head, then exactly the body it declares.
///
/// One `read` is not a request: a delivery arrives in as many segments as the network gives it, and
/// a signature over a truncated body never matches. `Content-Length` is what says where the body
/// ends, which is also the only thing the signature is computed over.
async fn read_request(socket: &mut tokio::net::TcpStream) -> std::io::Result<Request> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];

    let head_end = loop {
        if let Some(index) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            break index;
        }
        if buffer.len() > MAX_HEAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request headers are too large",
            ));
        }
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed inside the request headers",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
    };

    let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
    let mut lines = head.lines();
    let mut request_line = lines.next().unwrap_or_default().split_whitespace();
    let method = request_line.next().unwrap_or_default().to_string();
    let path = request_line.next().unwrap_or_default().to_string();

    let mut headers = Vec::new();
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    let content_length = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    if content_length > MAX_BODY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "request body is too large",
        ));
    }

    let mut body = buffer[head_end + 4..].to_vec();
    while body.len() < content_length {
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);

    debug!(
        "WhatsApp webhook: {} {} ({} body bytes)",
        method,
        path,
        body.len()
    );
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

/// Write a minimal HTTP response and close.
async fn respond(
    socket: &mut tokio::net::TcpStream,
    status: u16,
    reason: &str,
    body: &str,
) -> std::io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await
}

/// Whether `header` is Meta's HMAC-SHA256 of `body` under `secret`.
///
/// The comparison is the HMAC crate's own `verify_slice`, which is constant time: a byte-by-byte
/// comparison would leak how much of a guessed signature was right.
fn signature_matches(secret: &str, body: &[u8], header: Option<&str>) -> bool {
    let Some(digest) = header.and_then(|value| value.trim().strip_prefix("sha256=")) else {
        return false;
    };
    let Some(expected) = decode_hex(digest) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

/// Hex as bytes; `None` when the text is not an even run of hex digits.
fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let digits = text.as_bytes();
    let mut out = Vec::with_capacity(digits.len() / 2);
    for pair in digits.chunks(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        out.push((high * 16 + low) as u8);
    }
    Some(out)
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
        let what = format!("WhatsApp message to {chat_id}");
        let authorization = format!("Bearer {}", self.token);

        // Meta caps a text body at 4096 characters; 4000 leaves room for the closing fence the
        // splitter adds when a code block runs past a chunk.
        const MAX_LEN: usize = 4000;

        for chunk in super::outbound::chunk_message(text, MAX_LEN) {
            let payload = json!({
                "messaging_product": "whatsapp",
                "recipient_type": "individual",
                "to": chat_id,
                "type": "text",
                "text": {
                    "preview_url": false,
                    "body": chunk
                }
            });

            post_json(
                &self.client,
                &url,
                &[
                    ("Authorization", authorization.clone()),
                    ("Content-Type", "application/json".to_string()),
                ],
                &payload,
                &what,
            )
            .await
            .map_err(|failure| send_error("whatsapp", &what, failure))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A delivery Meta signed is accepted; one signed with anything else is not. This is the check
    /// that stands between the open port and an agent holding tools.
    #[test]
    fn only_metas_own_signature_is_accepted() {
        let body = br#"{"object":"whatsapp_business_account"}"#;
        let signature = "sha256=f6491327cb5b041cdf4fe69b94711c7b68bb0aef719854bae676a59788a2b6b6";
        assert!(signature_matches("appsecret", body, Some(signature)));

        assert!(!signature_matches("another-secret", body, Some(signature)));
        assert!(!signature_matches("appsecret", b"{}", Some(signature)));
        // No header at all, and a header in some other scheme, are both refusals.
        assert!(!signature_matches("appsecret", body, None));
        assert!(!signature_matches("appsecret", body, Some("sha1=abcdef")));
        // A digest that is not hex, or is an odd number of digits, is a refusal rather than a panic.
        assert!(!signature_matches("appsecret", body, Some("sha256=zzzz")));
        assert!(!signature_matches("appsecret", body, Some("sha256=abc")));
    }

    #[test]
    fn hex_decodes_and_refuses_what_is_not_hex() {
        assert_eq!(decode_hex("00ff10"), Some(vec![0, 255, 16]));
        assert_eq!(decode_hex("0"), None);
        assert_eq!(decode_hex("gg"), None);
    }

    /// A request arrives in whatever segments the network gives it; the body is read to the length
    /// the head declares, which is what the signature covers.
    #[tokio::test]
    async fn a_request_body_is_read_to_the_declared_length() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let body = "{\"entry\":[]}";

        let client = tokio::spawn(async move {
            let mut socket = tokio::net::TcpStream::connect(addr).await.expect("connect");
            let head = format!(
                "POST /webhook HTTP/1.1\r\nHost: x\r\nX-Hub-Signature-256: sha256=00\r\nContent-Length: {}\r\n\r\n",
                body.len()
            );
            // Written in two pieces, so one `read` on the server cannot be enough.
            socket.write_all(head.as_bytes()).await.expect("head");
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            socket.write_all(body.as_bytes()).await.expect("body");
            socket.flush().await.expect("flush");
        });

        let (mut accepted, _) = listener.accept().await.expect("accept");
        let request = read_request(&mut accepted).await.expect("read");
        client.await.expect("client task");

        assert_eq!(request.method, "POST");
        assert_eq!(request.path, "/webhook");
        assert_eq!(request.header("X-Hub-Signature-256"), Some("sha256=00"));
        assert_eq!(String::from_utf8_lossy(&request.body), body);
    }
}
