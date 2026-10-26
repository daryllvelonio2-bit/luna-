//! Gateway platform adapter definitions and inbound message structures.

use async_trait::async_trait;
use harness_core::error::HarnessError;

/// An inbound message received from an external platform.
#[derive(Debug, Clone)]
pub struct InboundMessage {
    /// Platform name ("telegram", "discord", "whatsapp").
    pub platform: String,
    /// Destination chat / channel ID.
    pub chat_id: String,
    /// Unique sender ID on the platform.
    pub sender_id: String,
    /// Optional sender username or display name.
    pub sender_name: Option<String>,
    /// Message body text.
    pub text: String,
    /// Platform-specific message ID (used for quotes / replies).
    #[allow(dead_code)]
    pub message_id: Option<String>,
}

/// Common trait for all messaging platform adapters.
#[async_trait]
pub trait PlatformAdapter: Send + Sync {
    /// Canonical platform name (e.g. "telegram", "discord", "whatsapp").
    fn name(&self) -> &'static str;

    /// Send a text message back to the target chat / channel.
    async fn send_message(&self, chat_id: &str, text: &str) -> Result<(), HarnessError>;

    /// Poll or fetch any newly arrived inbound messages.
    async fn poll_messages(&mut self) -> Result<Vec<InboundMessage>, HarnessError>;
}
