//! Telegram platform adapter via Telegram Bot API long-polling.

use async_trait::async_trait;
use harness_core::error::HarnessError;
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{debug, error, info, warn};

use super::adapter::{InboundMessage, PlatformAdapter};

/// Telegram long-polling bot adapter.
pub struct TelegramAdapter {
    token: String,
    client: Client,
    offset: i64,
    allowed_users: Vec<String>,
}

impl TelegramAdapter {
    pub fn new(token: String, allowed_users: Vec<String>) -> Self {
        Self {
            token,
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(45))
                .build()
                .unwrap_or_default(),
            offset: 0,
            allowed_users,
        }
    }

    pub(crate) fn is_user_allowed(&self, user_id: i64, username: Option<&str>) -> bool {
        if self.allowed_users.is_empty() {
            return true;
        }
        let uid_str = user_id.to_string();
        for allowed in &self.allowed_users {
            let clean = allowed.trim().trim_start_matches('@');
            if clean == uid_str {
                return true;
            }
            if let Some(uname) = username {
                if clean.eq_ignore_ascii_case(uname.trim_start_matches('@')) {
                    return true;
                }
            }
        }
        false
    }
}

#[async_trait]
impl PlatformAdapter for TelegramAdapter {
    fn name(&self) -> &'static str {
        "telegram"
    }

    async fn send_message(&self, chat_id: &str, text: &str) -> Result<(), HarnessError> {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.token);

        // Telegram max message length is 4096 characters. Chunk if necessary.
        const MAX_LEN: usize = 4000;
        let mut remaining = text;
        while !remaining.is_empty() {
            let chunk = if remaining.len() > MAX_LEN {
                let split_at = remaining[..MAX_LEN].rfind('\n').unwrap_or(MAX_LEN);
                let (part, rest) = remaining.split_at(split_at);
                remaining = rest.trim_start_matches('\n');
                part
            } else {
                let part = remaining;
                remaining = "";
                part
            };

            // Attempt Markdown first, fall back to plain text if Telegram rejects parse format.
            let payload_md = json!({
                "chat_id": chat_id,
                "text": chunk,
                "parse_mode": "Markdown",
            });

            let res = self.client.post(&url).json(&payload_md).send().await;
            match res {
                Ok(resp) if resp.status().is_success() => {
                    debug!("Telegram message sent to {chat_id}");
                }
                _ => {
                    // Fallback to unformatted plain text
                    let payload_plain = json!({
                        "chat_id": chat_id,
                        "text": chunk,
                    });
                    if let Err(e) = self.client.post(&url).json(&payload_plain).send().await {
                        error!("Failed to send plain Telegram message to {chat_id}: {e}");
                    }
                }
            }
        }
        Ok(())
    }

    async fn poll_messages(&mut self) -> Result<Vec<InboundMessage>, HarnessError> {
        let url = format!(
            "https://api.telegram.org/bot{}/getUpdates?offset={}&timeout=20",
            self.token, self.offset
        );

        let resp = match self.client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                warn!("Telegram getUpdates network error: {e}");
                return Ok(Vec::new());
            }
        };

        if !resp.status().is_success() {
            let code = resp.status();
            warn!("Telegram getUpdates HTTP {code}");
            return Ok(Vec::new());
        }

        let body: Value = match resp.json().await {
            Ok(val) => val,
            Err(e) => {
                warn!("Telegram getUpdates JSON parse error: {e}");
                return Ok(Vec::new());
            }
        };

        let mut messages = Vec::new();
        if let Some(results) = body.get("result").and_then(|r| r.as_array()) {
            for update in results {
                if let Some(up_id) = update.get("update_id").and_then(|id| id.as_i64()) {
                    if up_id >= self.offset {
                        self.offset = up_id + 1;
                    }
                }

                let message_obj = update
                    .get("message")
                    .or_else(|| update.get("edited_message"));
                let Some(msg) = message_obj else {
                    continue;
                };

                let text = msg.get("text").and_then(|t| t.as_str()).unwrap_or("");
                if text.trim().is_empty() {
                    continue;
                }

                let from = msg.get("from");
                let user_id = from
                    .and_then(|f| f.get("id"))
                    .and_then(|id| id.as_i64())
                    .unwrap_or(0);
                let username = from
                    .and_then(|f| f.get("username"))
                    .and_then(|u| u.as_str());
                let first_name = from
                    .and_then(|f| f.get("first_name"))
                    .and_then(|u| u.as_str());

                if !self.is_user_allowed(user_id, username) {
                    warn!(
                        "Telegram message from unauthorized user {user_id} (@{username:?}) dropped"
                    );
                    continue;
                }

                let chat_id = msg
                    .get("chat")
                    .and_then(|c| c.get("id"))
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| user_id.to_string());
                let message_id = msg.get("message_id").map(|id| id.to_string());

                info!(
                    "Inbound Telegram message from @{} ({chat_id}): {}",
                    username.unwrap_or(first_name.unwrap_or("unknown")),
                    text
                );

                messages.push(InboundMessage {
                    platform: "telegram".to_string(),
                    chat_id,
                    sender_id: user_id.to_string(),
                    sender_name: username
                        .map(|u| format!("@{u}"))
                        .or(first_name.map(str::to_string)),
                    text: text.to_string(),
                    message_id,
                });
            }
        }

        Ok(messages)
    }
}
