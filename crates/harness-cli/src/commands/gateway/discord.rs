//! Discord platform adapter via Discord REST API.

use async_trait::async_trait;
use harness_core::error::HarnessError;
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{debug, error, info, warn};

use super::adapter::{InboundMessage, PlatformAdapter};

/// Discord channel polling bot adapter.
pub struct DiscordAdapter {
    token: String,
    channel_id: String,
    client: Client,
    last_message_id: Option<String>,
    allowed_users: Vec<String>,
    seeded: bool,
}

impl DiscordAdapter {
    pub fn new(token: String, channel_id: String, allowed_users: Vec<String>) -> Self {
        Self {
            token,
            channel_id,
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            last_message_id: None,
            allowed_users,
            seeded: false,
        }
    }

    pub(crate) fn is_user_allowed(&self, user_id: &str, username: Option<&str>) -> bool {
        if self.allowed_users.is_empty() {
            return true;
        }
        for allowed in &self.allowed_users {
            let clean = allowed.trim().trim_start_matches('@');
            if clean == user_id {
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
impl PlatformAdapter for DiscordAdapter {
    fn name(&self) -> &'static str {
        "discord"
    }

    async fn send_message(&self, chat_id: &str, text: &str) -> Result<(), HarnessError> {
        let channel = if chat_id.is_empty() {
            &self.channel_id
        } else {
            chat_id
        };
        let url = format!("https://discord.com/api/v10/channels/{channel}/messages");

        // Discord limit is 2000 characters per message. Chunk at 1900.
        const MAX_LEN: usize = 1900;
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

            let payload = json!({
                "content": chunk,
            });

            let resp = self
                .client
                .post(&url)
                .header("Authorization", format!("Bot {}", self.token))
                .header("Content-Type", "application/json")
                .json(&payload)
                .send()
                .await;

            match resp {
                Ok(r) if r.status().is_success() => {
                    debug!("Discord message sent to channel {channel}");
                }
                Ok(r) => {
                    let status = r.status();
                    let err_body = r.text().await.unwrap_or_default();
                    error!("Discord send failed: HTTP {status} - {err_body}");
                }
                Err(e) => {
                    error!("Discord send network error: {e}");
                }
            }
        }

        Ok(())
    }

    async fn poll_messages(&mut self) -> Result<Vec<InboundMessage>, HarnessError> {
        if self.channel_id.is_empty() {
            return Ok(Vec::new());
        }

        let mut url = format!(
            "https://discord.com/api/v10/channels/{}/messages?limit=20",
            self.channel_id
        );
        if let Some(after) = &self.last_message_id {
            url.push_str(&format!("&after={after}"));
        }

        let resp = match self
            .client
            .get(&url)
            .header("Authorization", format!("Bot {}", self.token))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!("Discord getMessages network error: {e}");
                return Ok(Vec::new());
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            warn!("Discord getMessages HTTP {status}");
            return Ok(Vec::new());
        }

        let body: Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                warn!("Discord getMessages parse error: {e}");
                return Ok(Vec::new());
            }
        };

        let Some(arr) = body.as_array() else {
            return Ok(Vec::new());
        };

        // If not seeded yet, record latest ID without replaying ancient messages
        if !self.seeded {
            self.seeded = true;
            if let Some(first) = arr.first() {
                if let Some(id) = first.get("id").and_then(|i| i.as_str()) {
                    self.last_message_id = Some(id.to_string());
                }
            }
            return Ok(Vec::new());
        }

        let mut messages = Vec::new();
        // Discord returns messages in reverse chronological order: reverse to process oldest first
        for msg in arr.iter().rev() {
            let Some(id) = msg.get("id").and_then(|i| i.as_str()) else {
                continue;
            };
            self.last_message_id = Some(id.to_string());

            let author = msg.get("author");
            let is_bot = author
                .and_then(|a| a.get("bot"))
                .and_then(|b| b.as_bool())
                .unwrap_or(false);
            if is_bot {
                continue;
            }

            let user_id = author
                .and_then(|a| a.get("id"))
                .and_then(|i| i.as_str())
                .unwrap_or("");
            let username = author
                .and_then(|a| a.get("username"))
                .and_then(|u| u.as_str());

            if !self.is_user_allowed(user_id, username) {
                warn!("Discord message from unauthorized user {user_id} (@{username:?}) dropped");
                continue;
            }

            let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
            if content.trim().is_empty() {
                continue;
            }

            let channel_id = msg
                .get("channel_id")
                .and_then(|c| c.as_str())
                .unwrap_or(&self.channel_id);

            info!(
                "Inbound Discord message from @{} ({user_id}): {}",
                username.unwrap_or("unknown"),
                content
            );

            messages.push(InboundMessage {
                platform: "discord".to_string(),
                chat_id: channel_id.to_string(),
                sender_id: user_id.to_string(),
                sender_name: username.map(|u| format!("@{u}")),
                text: content.to_string(),
                message_id: Some(id.to_string()),
            });
        }

        Ok(messages)
    }
}
