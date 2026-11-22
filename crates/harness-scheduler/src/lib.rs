//! Persistent scheduled reminders and flexible automation tasks for Luna.

pub mod builtin_plugin;
pub mod model;
pub mod service;
pub mod skill;
pub mod store;
pub mod tools;

pub use builtin_plugin::{ensure_scheduler_plugin, SCHEDULER_PLUGIN_ID};
pub use model::{
    compute_effective_next_run, compute_next_run, is_within_active_window,
    next_active_window_start, validate_active_window, validate_rule, DeliveryReceipt,
    DeliveryTarget, ExecutionMode, ScheduleTask,
};
pub use service::{CancellableDeliveryHandler, ScheduleDeliveryHandler, SchedulerService};
pub use skill::{
    automation_skill, build_automation_turn_prompt, AUTOMATION_PREFILL,
    AUTOMATION_SKILL_DESCRIPTION, AUTOMATION_SKILL_INSTRUCTIONS, AUTOMATION_SKILL_NAME,
};
pub use store::{ScheduleCreateOptions, ScheduleStore, ScheduleUpdateOptions};
pub use tools::{register_schedule_tools, register_schedule_tools_with_service};

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
        assert!(compute_next_run("daily-local:14:30", now)
            .unwrap()
            .is_some());
        assert!(compute_next_run("0 9 * * *", now).unwrap().is_some());
        assert!(compute_next_run("local-cron:0 9 * * *", now)
            .unwrap()
            .is_some());
        assert!(compute_next_run("continuous", now).unwrap().is_some());
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
            .create_with_options(ScheduleCreateOptions {
                name: "Morning Brief".to_string(),
                instruction: "Summarize open tasks".to_string(),
                rule: "every:60".to_string(),
                delivery_target: DeliveryTarget::NewSessionEachRun,
                profile: Some("briefing-analyst".to_string()),
                execution_mode: ExecutionMode::Scheduled,
                active_window: Some("00:00-24:00 UTC".to_string()),
                trigger_logic: Some("Skip if no new activity".to_string()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(task.profile.as_deref(), Some("briefing-analyst"));

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
            .update_with_approval(&task.id, None, None, None, None, Some(true), Some(true))
            .unwrap();
        assert!(paused.paused);
        assert!(paused.require_approval);
    }

    #[tokio::test]
    async fn trigger_command_gates_execution() {
        let store = Arc::new(ScheduleStore::open(None).unwrap());
        let service = SchedulerService::new(store.clone());

        let task = store
            .create_with_options(ScheduleCreateOptions {
                name: "Condition Gated".to_string(),
                instruction: "Run only when gate passes".to_string(),
                rule: "continuous:10".to_string(),
                delivery_target: DeliveryTarget::NewSessionEachRun,
                execution_mode: ExecutionMode::Continuous,
                trigger_command: Some("false".to_string()),
                ..Default::default()
            })
            .unwrap();

        let handler: ScheduleDeliveryHandler = Arc::new(|t| {
            Box::pin(async move {
                Ok(DeliveryReceipt {
                    delivered_at: Utc::now(),
                    session_id: "s_never".to_string(),
                    status: "delivered".to_string(),
                    summary: t.name,
                })
            })
        });

        let results = service.tick_once(&handler).await;
        assert!(results.is_empty(), "false trigger_command must skip run");
        let after = store.get(&task.id).unwrap();
        assert!(after.history.is_empty());
        assert!(after.next_run_at.is_some());
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
