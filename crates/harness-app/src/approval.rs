//! UI-agnostic approval channel bridge and settings mapping.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot, Notify};

use harness_core::{
    AllowAll, ApprovalDecision, ApprovalMode, ApprovalRequest, ApprovalSettings, Approver, Config,
    DenyAll, HarnessError, NonInteractive,
};

/// Approver for interactive sessions (TUI or GUI): routes approval requests through a channel.
pub struct ChannelApprover {
    request_tx: mpsc::UnboundedSender<(ApprovalRequest, oneshot::Sender<ApprovalDecision>)>,
    allow_all: Arc<AtomicBool>,
    plan_mode: Arc<AtomicBool>,
    activity: Arc<Notify>,
}

impl ChannelApprover {
    pub fn new(
        request_tx: mpsc::UnboundedSender<(ApprovalRequest, oneshot::Sender<ApprovalDecision>)>,
        allow_all: Arc<AtomicBool>,
        plan_mode: Arc<AtomicBool>,
    ) -> Self {
        Self {
            request_tx,
            allow_all,
            plan_mode,
            activity: Arc::new(Notify::new()),
        }
    }

    pub fn activity_handle(&self) -> Arc<Notify> {
        self.activity.clone()
    }
}

#[async_trait]
impl Approver for ChannelApprover {
    fn activity_notifier(&self) -> Option<Arc<Notify>> {
        Some(self.activity.clone())
    }

    async fn approve(&self, request: &ApprovalRequest) -> Result<ApprovalDecision, HarnessError> {
        if self.plan_mode.load(Ordering::SeqCst) {
            return Err(HarnessError::ApprovalRejected(
                "plan mode active: mutating tool calls are disabled (press Tab to toggle plan mode off)"
                    .to_string(),
            ));
        }
        if self.allow_all.load(Ordering::SeqCst) {
            return Ok(ApprovalDecision::Run);
        }
        let (decision_tx, decision_rx) = oneshot::channel();
        if self
            .request_tx
            .send((request.clone(), decision_tx))
            .is_err()
        {
            return Ok(ApprovalDecision::Deny);
        }
        match decision_rx.await {
            Ok(decision) => Ok(decision),
            Err(_) => Ok(ApprovalDecision::Deny),
        }
    }
}

pub fn build_headless_approver(config: &Config) -> Arc<dyn Approver> {
    match config.tools.approval {
        ApprovalMode::Allow => Arc::new(AllowAll),
        ApprovalMode::Deny => Arc::new(DenyAll),
        ApprovalMode::Ask => Arc::new(NonInteractive::new(
            "headless session: stdin is not a terminal (use --yes or tools.approval=allow to override)",
        )),
    }
}

pub fn approval_settings(config: &Config) -> ApprovalSettings {
    ApprovalSettings {
        timeout: Duration::from_secs(config.tools.approval_timeout_secs),
        mutating_overrides: config.tools.mutating_overrides.clone(),
    }
}

#[cfg(test)]
#[path = "approval_tests.rs"]
mod tests;
