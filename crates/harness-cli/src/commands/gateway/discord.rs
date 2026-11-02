//! Discord platform adapter via Discord REST API.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use harness_core::error::HarnessError;
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{info, warn};

use super::adapter::{InboundMessage, PlatformAdapter};
use super::outbound::{chunk_message, post_json, send_error};

/// Discord channel polling bot adapter.
pub struct DiscordAdapter {
    token: String,
    channel_id: String,
    client: Client,
    last_message_id: Option<String>,
    allowed_users: Vec<String>,
    /// Where the last id this adapter saw is kept. Restarting a gateway used to re-seed from the
    /// newest message and return nothing, so everything said while it was down was skipped in
    /// silence — the one failure mode of a chat bot a user cannot work around.
    cursor: Option<PathBuf>,
}

impl DiscordAdapter {
    /// `cursor` is where the last seen message id is remembered between runs; `None` keeps this
    /// adapter's cursor in memory only, which is what the unit tests want.
    pub fn new(
        token: String,
        channel_id: String,
        allowed_users: Vec<String>,
        cursor: Option<PathBuf>,
    ) -> Self {
        let last_message_id = cursor.as_deref().and_then(read_cursor);
        Self {
            token,
            channel_id,
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(30))
                .build()
                .unwrap_or_default(),
            // Seeded from the cursor: a restart resumes where the last run stopped instead of
            // declaring everything it missed to be ancient history.
            last_message_id,
            allowed_users,
            cursor,
        }
    }

    /// Whether this adapter is past its first poll: it has an id to ask `after=` for.
    pub(crate) fn is_seeded(&self) -> bool {
        self.last_message_id.is_some()
    }

    /// Record the newest id seen, so the next run starts after it.
    pub(crate) fn remember(&self, id: &str) {
        if let Some(path) = &self.cursor {
            if let Err(error) = write_cursor(path, id) {
                warn!(
                    "Cannot record the Discord cursor at {}: {error}",
                    path.display()
                );
            }
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

/// The id in a cursor file, if one was written.
fn read_cursor(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let id = text.trim();
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// Write `id` as the cursor, through a temporary file so a killed process cannot leave a half
/// written id behind — the next run would then ask Discord for messages after a nonsense id.
fn write_cursor(path: &Path, id: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, format!("{id}\n"))?;
    std::fs::rename(&temporary, path)
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
        let what = format!("Discord message to channel {channel}");
        let authorization = format!("Bot {}", self.token);

        // Discord caps a message at 2000 characters; 1900 leaves room for the closing fence the
        // splitter adds when a code block runs past a chunk.
        const MAX_LEN: usize = 1900;

        for chunk in chunk_message(text, MAX_LEN) {
            let payload = json!({ "content": chunk });
            post_json(
                &self.client,
                &url,
                &[("Authorization", authorization.clone())],
                &payload,
                &what,
            )
            .await
            .map_err(|failure| send_error("discord", &what, failure))?;
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

        // A first run with no cursor records the newest id and replays nothing: an adapter that
        // delivered the last twenty messages of a channel it has never seen would answer questions
        // asked before it existed. From then on the id comes from the cursor, so a restart
        // continues instead of skipping.
        if !self.is_seeded() {
            if let Some(id) = arr
                .first()
                .and_then(|msg| msg.get("id"))
                .and_then(|i| i.as_str())
            {
                self.last_message_id = Some(id.to_string());
                self.remember(id);
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
            self.remember(id);

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
