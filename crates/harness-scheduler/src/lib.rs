//! Persistent scheduled reminders and automation tasks for Luna.

pub mod builtin_plugin;
pub mod model;
pub mod service;
pub mod store;
pub mod tools;

pub use builtin_plugin::{ensure_scheduler_plugin, SCHEDULER_PLUGIN_ID};
pub use model::{compute_next_run, validate_rule, DeliveryReceipt, DeliveryTarget, ScheduleTask};
pub use service::{ScheduleDeliveryHandler, SchedulerService};
pub use store::ScheduleStore;
pub use tools::register_schedule_tools;

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use harness_core::{DefaultToolRegistry, ToolRegistry};
    use harness_plugins::PluginManager;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn parses_every_daily_once_and_cron_rules() {
        let now = Utc::now();
        assert!(compute_next_run("every:60", now).unwrap().is_some());
        assert!(compute_next_run("daily:14:30", now).unwrap().is_some());
        assert!(compute_next_run("0 9 * * *", now).unwrap().is_some());
        let future = (now + chrono::Duration::hours(2)).to_rfc3339();
        assert!(compute_next_run(&format!("once:{future}"), now)
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn creates_executes_and_pauses_scheduled_task() {
        let store = Arc::new(ScheduleStore::open(None).unwrap());
        let service = SchedulerService::new(store.clone());

        let task = store
            .create(
                None,
                "Morning Brief".to_string(),
                "Summarize open tasks".to_string(),
                "every:60".to_string(),
                DeliveryTarget::NewSessionEachRun,
                None,
            )
            .unwrap();
        assert!(!task.paused);
        assert!(task.next_run_at.is_some());

        let handler: ScheduleDeliveryHandler = Arc::new(|t| {
            Box::pin(async move {
                Ok(DeliveryReceipt {
                    delivered_at: Utc::now(),
                    session_id: "s_sched_1".to_string(),
                    status: "delivered".to_string(),
                    summary: format!("ran {}", t.name),
                })
            })
        });

        let updated = service.run_now(&task.id, &handler).await.unwrap();
        assert_eq!(updated.history.len(), 1);
        assert_eq!(updated.history[0].session_id, "s_sched_1");

        let paused = store
            .update(&task.id, None, None, None, None, Some(true))
            .unwrap();
        assert!(paused.paused);
    }

    #[tokio::test]
    async fn schedule_tools_respect_subagent_restriction() {
        let store = Arc::new(ScheduleStore::open(None).unwrap());
        let reg: Arc<dyn ToolRegistry> = Arc::new(DefaultToolRegistry::new());
        register_schedule_tools(&reg, store, Some("s1".into()), false)
            .await
            .unwrap();
        let cancel = CancellationToken::new();
        let call = harness_core::ToolCall {
            id: "c1".into(),
            name: "schedule_list".into(),
            arguments: serde_json::json!({}),
            signature: None,
        };
        let res = reg.execute(&call, &cancel).await.unwrap();
        assert!(res.is_error);
        assert!(res.content.contains("subagent"));
    }

    #[test]
    fn builtin_scheduler_plugin_registers_sidebar_agent_and_panel() {
        let tmp = tempfile::tempdir().unwrap();
        let mgr = PluginManager::new(tmp.path().to_path_buf(), None);
        ensure_scheduler_plugin(&mgr).unwrap();
        mgr.discover_and_load().unwrap();
        let agents = mgr.sidebar_agents();
        assert!(agents.iter().any(|a| a.plugin_id == SCHEDULER_PLUGIN_ID));
        assert!(!mgr.ui_panels().is_empty());
    }
}
