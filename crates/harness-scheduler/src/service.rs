//! Background scheduler execution engine and plugin-schedule synchronization.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tokio_util::sync::CancellationToken;

use harness_plugins::PluginScheduleDecl;

use crate::model::{DeliveryReceipt, DeliveryTarget, ScheduleTask};
use crate::store::ScheduleStore;

pub type ScheduleDeliveryHandler = Arc<
    dyn Fn(ScheduleTask) -> Pin<Box<dyn Future<Output = Result<DeliveryReceipt, String>> + Send>>
        + Send
        + Sync,
>;

pub struct SchedulerService {
    store: Arc<ScheduleStore>,
}

impl SchedulerService {
    pub fn new(store: Arc<ScheduleStore>) -> Self {
        Self { store }
    }

    pub fn store(&self) -> &Arc<ScheduleStore> {
        &self.store
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

    /// Check for due tasks once and deliver them sequentially.
    pub async fn tick_once(&self, handler: &ScheduleDeliveryHandler) -> Vec<Result<ScheduleTask, String>> {
        let due = match self.store.due_tasks(Utc::now()) {
            Ok(d) => d,
            Err(e) => return vec![Err(e)],
        };
        let mut results = Vec::with_capacity(due.len());
        for task in due {
            results.push(self.run_task(task, handler).await);
        }
        results
    }

    pub async fn run_now(&self, id: &str, handler: &ScheduleDeliveryHandler) -> Result<ScheduleTask, String> {
        let task = self.store.get(id)?;
        self.run_task(task, handler).await
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

    /// Spawn the background scheduler ticker (checks every `interval_secs`, default 2s).
    pub fn spawn_loop(
        self: Arc<Self>,
        handler: ScheduleDeliveryHandler,
        interval: Duration,
        cancel: CancellationToken,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => break,
                    _ = ticker.tick() => {
                        let _ = self.tick_once(&handler).await;
                    }
                }
            }
        })
    }
}
