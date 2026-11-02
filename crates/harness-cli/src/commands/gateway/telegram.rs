//! Telegram platform adapter via Telegram Bot API long-polling.

use async_trait::async_trait;
use harness_core::error::HarnessError;
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{info, warn};

use super::adapter::{InboundMessage, PlatformAdapter};
use super::outbound::{chunk_message, post_json, send_error};

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
        let what = format!("Telegram message to {chat_id}");

        // Telegram caps a message at 4096 characters; 4000 leaves room for the closing fence the
        // splitter adds when a code block runs past a chunk.
        const MAX_LEN: usize = 4000;

        for chunk in chunk_message(text, MAX_LEN) {
            // Markdown first, because the reply is written in it.
            let markdown = json!({
                "chat_id": chat_id,
                "text": chunk,
                "parse_mode": "Markdown",
            });

            match post_json(&self.client, &url, &[], &markdown, &what).await {
                Ok(()) => {}
                // Telegram refuses a payload whose Markdown it cannot parse, and the text itself is
                // fine — so it goes again unformatted. Reported, because a reply that quietly loses
                // its formatting reads as a formatting bug rather than as the fallback it is.
                Err(failure) if failure.rejected_the_payload() => {
                    warn!(
                        "{what} refused as Markdown ({}); sending it unformatted",
                        failure.detail
                    );
                    let plain = json!({ "chat_id": chat_id, "text": chunk });
                    post_json(&self.client, &url, &[], &plain, &what)
                        .await
                        .map_err(|failure| send_error("telegram", &what, failure))?;
                }
                Err(failure) => return Err(send_error("telegram", &what, failure)),
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
