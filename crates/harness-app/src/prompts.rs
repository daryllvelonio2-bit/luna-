//! UI-agnostic sudo prompt channel bridge.

use tokio::sync::{mpsc, oneshot};

use harness_core::HarnessError;

pub struct ChannelSudoPrompter {
    request_tx: mpsc::UnboundedSender<(String, oneshot::Sender<Option<String>>)>,
}

impl ChannelSudoPrompter {
    pub fn new(
        request_tx: mpsc::UnboundedSender<(String, oneshot::Sender<Option<String>>)>,
    ) -> Self {
        Self { request_tx }
    }
}

#[async_trait::async_trait]
impl harness_core::tools::SudoPrompter for ChannelSudoPrompter {
    async fn prompt_sudo_password(&self, command: &str) -> Result<Option<String>, HarnessError> {
        let (tx, rx) = oneshot::channel();
        if self.request_tx.send((command.to_string(), tx)).is_err() {
            return Ok(None);
        }
        match rx.await {
            Ok(pwd) => Ok(pwd),
            Err(_) => Ok(None),
        }
    }
}
