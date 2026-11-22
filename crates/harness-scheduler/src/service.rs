//! Background scheduler execution engine and plugin-schedule synchronization.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::Utc;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use harness_plugins::PluginScheduleDecl;

use crate::model::{
    compute_effective_next_run, is_within_active_window, next_active_window_start, DeliveryReceipt,
    DeliveryTarget, ScheduleTask,
};
use crate::store::ScheduleStore;

pub type ScheduleDeliveryHandler = Arc<
    dyn Fn(ScheduleTask) -> Pin<Box<dyn Future<Output = Result<DeliveryReceipt, String>> + Send>>
        + Send
        + Sync,
>;

pub type CancellableDeliveryHandler = Arc<
    dyn Fn(
            ScheduleTask,
            CancellationToken,
        ) -> Pin<Box<dyn Future<Output = Result<DeliveryReceipt, String>> + Send>>
        + Send
        + Sync,
>;

pub struct SchedulerService {
    store: Arc<ScheduleStore>,
    workspace_root: Mutex<Option<PathBuf>>,
    in_flight: Mutex<HashSet<String>>,
    active_cancels: Mutex<HashMap<String, CancellationToken>>,
}

impl SchedulerService {
    pub fn new(store: Arc<ScheduleStore>) -> Self {
        Self {
            store,
            workspace_root: Mutex::new(None),
            in_flight: Mutex::new(HashSet::new()),
            active_cancels: Mutex::new(HashMap::new()),
        }
    }

    pub fn set_workspace_root(&self, root: PathBuf) {
        if let Ok(mut guard) = self.workspace_root.lock() {
            *guard = Some(root);
        }
    }

    pub fn store(&self) -> &Arc<ScheduleStore> {
        &self.store
    }

    /// Cancel an active/sleeping task run immediately (e.g. on pause or delete).
    pub fn cancel_task(&self, id: &str) -> bool {
        if let Ok(mut cancels) = self.active_cancels.lock() {
            if let Some(tok) = cancels.remove(id) {
                tok.cancel();
                return true;
            }
        }
        false
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.in_flight
            .lock()
            .map(|set| set.contains(id))
            .unwrap_or(false)
    }

    /// Ensure plugin-declared schedules exist in the persistent store.
    pub fn sync_plugin_declarations(&self, decls: &[PluginScheduleDecl]) -> Result<(), String> {
        let existing = self.store.list()?;
        for d in decls {
            if existing.iter().any(|t| t.id == d.id) {
                continue;
            }
            let target = match d.delivery_mode.as_str() {
                "single_new_session" => DeliveryTarget::SingleNewSession { session_id: None },
                _ => DeliveryTarget::NewSessionEachRun,
            };
            self.store.create(
                Some(d.id.clone()),
                d.name.clone(),
                d.instruction.clone(),
                d.rule.clone(),
                target,
                Some(d.plugin_id.clone()),
            )?;
        }
        Ok(())
    }

    fn try_mark_in_flight(&self, id: &str, cancel: CancellationToken) -> bool {
        let Ok(mut set) = self.in_flight.lock() else {
            return false;
        };
        if set.contains(id) {
            return false;
        }
        set.insert(id.to_string());
        drop(set);
        if let Ok(mut cancels) = self.active_cancels.lock() {
            cancels.insert(id.to_string(), cancel);
        }
        true
    }

    fn clear_in_flight(&self, id: &str) {
        if let Ok(mut set) = self.in_flight.lock() {
            set.remove(id);
        }
        if let Ok(mut cancels) = self.active_cancels.lock() {
            cancels.remove(id);
        }
    }

    async fn should_dispatch_due_task(&self, task: &ScheduleTask) -> bool {
        let now = Utc::now();

        // 1. Respect targeted working-time window (`active_window`).
        if let Some(win) = task.active_window.as_deref() {
            match is_within_active_window(win, now) {
                Ok(true) => {}
                Ok(false) => {
                    if let Ok(next_open) = next_active_window_start(win, now) {
                        let _ = self.store.set_next_run_at(&task.id, Some(next_open));
                    }
                    return false;
                }
                Err(err) => {
                    tracing::warn!(
                        task_id = %task.id,
                        "invalid active_window '{}': {}",
                        win,
                        err
                    );
                }
            }
        }

        // 2. Respect optional lightweight shell trigger gate (`trigger_command`).
        if let Some(cmd) = task.trigger_command.as_deref() {
            let ws = self.workspace_root.lock().ok().and_then(|g| g.clone());
            let passed = run_trigger_command(cmd, ws.as_deref()).await;
            if !passed {
                // Advance next_run_at cleanly without firing an LLM turn.
                if let Ok(next) =
                    compute_effective_next_run(&task.rule, task.active_window.as_deref(), now)
                {
                    let _ = self.store.set_next_run_at(&task.id, next);
                }
                return false;
            }
        }

        true
    }

    /// Check for due tasks once and deliver them (awaits completion of due batch).
    pub async fn tick_once(
        &self,
        handler: &ScheduleDeliveryHandler,
    ) -> Vec<Result<ScheduleTask, String>> {
        let now = Utc::now();
        let due = match self.store.due_tasks(now) {
            Ok(d) => d,
            Err(e) => return vec![Err(e)],
        };
        let mut results = Vec::with_capacity(due.len());
        for task in due {
            if self.is_running(&task.id) {
                continue;
            }
            if !self.should_dispatch_due_task(&task).await {
                continue;
            }
            let token = CancellationToken::new();
            if !self.try_mark_in_flight(&task.id, token) {
                continue;
            }
            let id = task.id.clone();
            let res = self.run_task(task, handler).await;
            self.clear_in_flight(&id);
            results.push(res);
        }
        results
    }

    pub async fn run_now(
        &self,
        id: &str,
        handler: &ScheduleDeliveryHandler,
    ) -> Result<ScheduleTask, String> {
        let task = self.store.get(id)?;
        let token = CancellationToken::new();
        if !self.try_mark_in_flight(&task.id, token) {
            return Err(format!("automation '{}' is already running", task.name));
        }
        let res = self.run_task(task, handler).await;
        self.clear_in_flight(id);
        res
    }

    pub async fn run_now_cancellable(
        &self,
        id: &str,
        handler: &CancellableDeliveryHandler,
    ) -> Result<ScheduleTask, String> {
        let task = self.store.get(id)?;
        let token = CancellationToken::new();
        if !self.try_mark_in_flight(&task.id, token.clone()) {
            return Err(format!("automation '{}' is already running", task.name));
        }
        let res = self.run_task_cancellable(task, token, handler).await;
        self.clear_in_flight(id);
        res
    }

    async fn run_task(
        &self,
        task: ScheduleTask,
        handler: &ScheduleDeliveryHandler,
    ) -> Result<ScheduleTask, String> {
        let id = task.id.clone();
        match handler(task).await {
            Ok(receipt) => self.store.record_delivery(&id, receipt),
            Err(err) => {
                let receipt = DeliveryReceipt {
                    delivered_at: Utc::now(),
                    session_id: String::new(),
                    status: "failed".to_string(),
                    summary: err,
                };
                self.store.record_delivery(&id, receipt)
            }
        }
    }

    async fn run_task_cancellable(
        &self,
        task: ScheduleTask,
        cancel: CancellationToken,
        handler: &CancellableDeliveryHandler,
    ) -> Result<ScheduleTask, String> {
        let id = task.id.clone();
        match handler(task, cancel).await {
            Ok(receipt) => self.store.record_delivery(&id, receipt),
            Err(err) => {
                let receipt = DeliveryReceipt {
                    delivered_at: Utc::now(),
                    session_id: String::new(),
                    status: "failed".to_string(),
                    summary: err,
                };
                self.store.record_delivery(&id, receipt)
            }
        }
    }

    /// Spawn the background scheduler ticker (checks every `interval`, dispatching tasks concurrently
    /// so long-running or sleeping continuous automations never block other schedules).
    pub fn spawn_loop(
        self: Arc<Self>,
        handler: ScheduleDeliveryHandler,
        interval: Duration,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        let wrapped: CancellableDeliveryHandler = Arc::new(move |task, _tok| handler(task));
        self.spawn_loop_cancellable(wrapped, interval, cancel)
    }

    pub fn spawn_loop_cancellable(
        self: Arc<Self>,
        handler: CancellableDeliveryHandler,
        interval: Duration,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        let now = Utc::now();
                        let Ok(due) = self.store.due_tasks(now) else {
                            continue;
                        };
                        for task in due {
                            if self.is_running(&task.id) {
                                continue;
                            }
                            if !self.should_dispatch_due_task(&task).await {
                                continue;
                            }
                            let task_cancel = cancel.child_token();
                            if !self.try_mark_in_flight(&task.id, task_cancel.clone()) {
                                continue;
                            }
                            let svc = Arc::clone(&self);
                            let h = Arc::clone(&handler);
                            tokio::spawn(async move {
                                let id = task.id.clone();
                                let _ = svc.run_task_cancellable(task, task_cancel, &h).await;
                                svc.clear_in_flight(&id);
                            });
                        }
                    }
                }
            }
        })
    }
}

async fn run_trigger_command(cmd: &str, workspace_root: Option<&std::path::Path>) -> bool {
    let mut command = Command::new("sh");
    command.arg("-c").arg(cmd);
    if let Some(ws) = workspace_root {
        command.current_dir(ws);
    }
    command.kill_on_drop(true);

    match tokio::time::timeout(Duration::from_secs(15), command.output()).await {
        Ok(Ok(out)) => out.status.success(),
        Ok(Err(err)) => {
            tracing::warn!("trigger_command '{}' failed to spawn: {}", cmd, err);
            false
        }
        Err(_) => {
            tracing::warn!("trigger_command '{}' timed out after 15s", cmd);
            false
        }
    }
}
