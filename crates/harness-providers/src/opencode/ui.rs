use harness_core::approval::{ApprovalDecision, ApprovalRequest};
use harness_core::{ClarifyRequest, ClarifyResponse};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock, RwLock};

#[derive(Clone)]
pub struct OpencodeUiBridge {
    pub clarify_tx: tokio::sync::mpsc::UnboundedSender<(
        ClarifyRequest,
        tokio::sync::oneshot::Sender<ClarifyResponse>,
    )>,
    pub approval_tx: tokio::sync::mpsc::UnboundedSender<(
        ApprovalRequest,
        tokio::sync::oneshot::Sender<ApprovalDecision>,
    )>,
    pub allow_all: Arc<AtomicBool>,
    pub update_context_used_fn: Option<Arc<dyn Fn(u32) + Send + Sync>>,
}

static UI_BRIDGE: OnceLock<RwLock<Option<OpencodeUiBridge>>> = OnceLock::new();

pub fn set_ui_bridge(bridge: OpencodeUiBridge) {
    let cell = UI_BRIDGE.get_or_init(|| RwLock::new(None));
    if let Ok(mut lock) = cell.write() {
        *lock = Some(bridge);
    }
}

pub fn get_ui_bridge() -> Option<OpencodeUiBridge> {
    UI_BRIDGE
        .get()
        .and_then(|lock| lock.read().ok())
        .and_then(|guard| guard.clone())
}
