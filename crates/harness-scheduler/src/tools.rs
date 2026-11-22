//! Model-facing scheduling tools matching DeepSeek Harness:
//! `schedule_create`, `schedule_list`, `schedule_view`, and `schedule_edit`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use harness_core::tools::SleepTool;
use harness_core::{HarnessError, Tool, ToolRegistry, ToolResult};

use crate::model::{DeliveryTarget, ExecutionMode};
use crate::service::SchedulerService;
use crate::store::{ScheduleCreateOptions, ScheduleStore, ScheduleUpdateOptions};

/// Register the four schedule tools onto `registry` (plus `sleep` if absent).
pub async fn register_schedule_tools(
    registry: &Arc<dyn ToolRegistry>,
    store: Arc<ScheduleStore>,
    current_session_id: Option<String>,
    allow_mutation: bool,
) -> Result<(), HarnessError> {
    register_schedule_tools_inner(registry, store, None, current_session_id, allow_mutation).await
}

/// Register schedule tools wired to a live [`SchedulerService`] so pausing/deleting
/// immediately cancels in-flight/sleeping tasks.
pub async fn register_schedule_tools_with_service(
    registry: &Arc<dyn ToolRegistry>,
    service: Arc<SchedulerService>,
    current_session_id: Option<String>,
    allow_mutation: bool,
) -> Result<(), HarnessError> {
    let store = service.store().clone();
    register_schedule_tools_inner(
        registry,
        store,
        Some(service),
        current_session_id,
        allow_mutation,
    )
    .await
}

async fn register_schedule_tools_inner(
    registry: &Arc<dyn ToolRegistry>,
    store: Arc<ScheduleStore>,
    service: Option<Arc<SchedulerService>>,
    current_session_id: Option<String>,
    allow_mutation: bool,
) -> Result<(), HarnessError> {
    if allow_mutation {
        let has_sleep = registry
            .list_tools()
            .await
            .map(|list| list.iter().any(|t| t.name == "sleep"))
            .unwrap_or(false);
        if !has_sleep {
            registry.register(Arc::new(SleepTool)).await?;
        }
    }
    registry
        .register(Arc::new(ScheduleCreateTool {
            store: store.clone(),
            current_session_id: current_session_id.clone(),
            allow_mutation,
        }))
        .await?;
    registry
        .register(Arc::new(ScheduleListTool {
            store: store.clone(),
            allow_mutation,
        }))
        .await?;
    registry
        .register(Arc::new(ScheduleViewTool {
            store: store.clone(),
            allow_mutation,
        }))
        .await?;
    registry
        .register(Arc::new(ScheduleEditTool {
            store,
            service,
            current_session_id,
            allow_mutation,
        }))
        .await?;
    Ok(())
}

fn parse_target(
    mode: Option<&str>,
    session_id: Option<&str>,
    current_session_id: Option<&str>,
) -> DeliveryTarget {
    match mode.unwrap_or("new_session_each_run") {
        "existing_session" => DeliveryTarget::ExistingSession {
            session_id: session_id
                .map(str::to_string)
                .or_else(|| current_session_id.map(str::to_string))
                .unwrap_or_else(|| "default".to_string()),
        },
        "single_new_session" => DeliveryTarget::SingleNewSession {
            session_id: session_id.map(str::to_string),
        },
        _ => DeliveryTarget::NewSessionEachRun,
    }
}

pub struct ScheduleCreateTool {
    store: Arc<ScheduleStore>,
    current_session_id: Option<String>,
    allow_mutation: bool,
}

#[async_trait]
impl Tool for ScheduleCreateTool {
    fn name(&self) -> &str {
        "schedule_create"
    }

    fn description(&self) -> &str {
        "Create a scheduled reminder or flexible recurring/continuous automation task. Supports profiles, targeted working-time windows, trigger commands/logic, continuous never-stop mode (with sleep), and local/UTC rules."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Short task name" },
                "instruction": { "type": "string", "description": "Self-contained prompt/instruction delivered to the agent when triggered" },
                "rule": {
                    "type": "string",
                    "description": "Recurrence rule: 'daily-local:<HH:MM>', 'local-cron:<5-field>', 'daily:<HH:MM>' (UTC), 'every:<seconds>', 'once:<RFC3339>', 'continuous[:<cooldown_secs>]', or 5-field UTC cron ('min hour dom mon dow')."
                },
                "target": {
                    "type": "string",
                    "enum": ["existing_session", "single_new_session", "new_session_each_run"],
                    "description": "Where each run delivers its instruction"
                },
                "session_id": { "type": "string", "description": "Optional explicit session id for existing_session or single_new_session" },
                "require_approval": {
                    "type": "boolean",
                    "description": "Whether tool calls during scheduled runs ask for permission (default: false — automations run autonomously)"
                },
                "profile": {
                    "type": "string",
                    "description": "Optional Luna profile name (created via profile_create) whose persona, mindset/behavior, and model overrides govern each run."
                },
                "execution_mode": {
                    "type": "string",
                    "enum": ["scheduled", "continuous"],
                    "description": "'scheduled' (default) runs on each cadence tick; 'continuous' runs persistently as a never-stop worker loop using the sleep tool."
                },
                "active_window": {
                    "type": "string",
                    "description": "Optional targeted working-time window (local time by default, or add 'UTC'), e.g. 'Mon-Fri 09:00-17:00', '22:00-06:00', 'weekdays 08:30-18:00'."
                },
                "trigger_command": {
                    "type": "string",
                    "description": "Optional lightweight shell gate ('sh -c <cmd>'). Exit 0 triggers the full agent run; non-zero skips this tick cheaply without burning tokens."
                },
                "trigger_logic": {
                    "type": "string",
                    "description": "Optional description of conditional trigger/escalation/stop criteria injected into the agent's context."
                }
            },
            "required": ["name", "instruction", "rule"]
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        if !self.allow_mutation {
            return Ok(ToolResult::error(
                call_id,
                "delegated subagents cannot create scheduled tasks",
            ));
        }
        let name = args["name"].as_str().unwrap_or_default().to_string();
        let instruction = args["instruction"].as_str().unwrap_or_default().to_string();
        let rule = args["rule"].as_str().unwrap_or_default().to_string();
        let require_approval = args["require_approval"].as_bool().unwrap_or(false);
        let profile = args["profile"].as_str().map(str::to_string);
        let execution_mode = match args["execution_mode"].as_str() {
            Some(raw) => match ExecutionMode::from_str_lossy(raw) {
                Ok(m) => m,
                Err(e) => return Ok(ToolResult::error(call_id, e)),
            },
            None => ExecutionMode::Scheduled,
        };
        let active_window = args["active_window"].as_str().map(str::to_string);
        let trigger_command = args["trigger_command"].as_str().map(str::to_string);
        let trigger_logic = args["trigger_logic"].as_str().map(str::to_string);
        let target = parse_target(
            args["target"].as_str(),
            args["session_id"].as_str(),
            self.current_session_id.as_deref(),
        );
        match self.store.create_with_options(ScheduleCreateOptions {
            id: None,
            name,
            instruction,
            rule,
            delivery_target: target,
            plugin_id: None,
            require_approval,
            profile,
            execution_mode,
            active_window,
            trigger_command,
            trigger_logic,
        }) {
            Ok(task) => Ok(ToolResult::success(
                call_id,
                serde_json::to_string_pretty(&task).unwrap_or_default(),
            )),
            Err(e) => Ok(ToolResult::error(call_id, e)),
        }
    }
}

pub struct ScheduleListTool {
    store: Arc<ScheduleStore>,
    allow_mutation: bool,
}

#[async_trait]
impl Tool for ScheduleListTool {
    fn name(&self) -> &str {
        "schedule_list"
    }

    fn description(&self) -> &str {
        "List all scheduled reminders and automation tasks."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({ "type": "object", "properties": {} })
    }

    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        call_id: &str,
        _args: &serde_json::Value,
        _cancel: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        if !self.allow_mutation {
            return Ok(ToolResult::error(
                call_id,
                "delegated subagents cannot read host schedules",
            ));
        }
        match self.store.list() {
            Ok(tasks) => Ok(ToolResult::success(
                call_id,
                serde_json::to_string_pretty(&tasks).unwrap_or_else(|_| "[]".to_string()),
            )),
            Err(e) => Ok(ToolResult::error(call_id, e)),
        }
    }
}

pub struct ScheduleViewTool {
    store: Arc<ScheduleStore>,
    allow_mutation: bool,
}

#[async_trait]
impl Tool for ScheduleViewTool {
    fn name(&self) -> &str {
        "schedule_view"
    }

    fn description(&self) -> &str {
        "Inspect one scheduled task by id, including its profile, active_window, trigger logic, and recent delivery history."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Schedule id" }
            },
            "required": ["id"]
        })
    }

    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        if !self.allow_mutation {
            return Ok(ToolResult::error(
                call_id,
                "delegated subagents cannot view host schedules",
            ));
        }
        let id = args["id"].as_str().unwrap_or_default();
        match self.store.get(id) {
            Ok(task) => Ok(ToolResult::success(
                call_id,
                serde_json::to_string_pretty(&task).unwrap_or_default(),
            )),
            Err(e) => Ok(ToolResult::error(call_id, e)),
        }
    }
}

pub struct ScheduleEditTool {
    store: Arc<ScheduleStore>,
    service: Option<Arc<SchedulerService>>,
    current_session_id: Option<String>,
    allow_mutation: bool,
}

#[async_trait]
impl Tool for ScheduleEditTool {
    fn name(&self) -> &str {
        "schedule_edit"
    }

    fn description(&self) -> &str {
        "Edit, pause, resume, or delete a scheduled task by id (including profile, execution_mode, active_window, trigger_command, and trigger_logic)."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Schedule id" },
                "action": { "type": "string", "enum": ["update", "pause", "resume", "delete"], "description": "Operation to perform" },
                "name": { "type": "string" },
                "instruction": { "type": "string" },
                "rule": { "type": "string" },
                "target": { "type": "string", "enum": ["existing_session", "single_new_session", "new_session_each_run"] },
                "session_id": { "type": "string" },
                "require_approval": { "type": "boolean", "description": "Toggle whether scheduled runs require interactive approval (default false)" },
                "profile": { "type": "string", "description": "Assign/change Luna profile name (empty string clears to default)." },
                "execution_mode": { "type": "string", "enum": ["scheduled", "continuous"] },
                "active_window": { "type": "string", "description": "Set/change targeted working-time window (empty string clears to 24/7)." },
                "trigger_command": { "type": "string", "description": "Set/change lightweight shell gate command (empty string clears)." },
                "trigger_logic": { "type": "string", "description": "Set/change conditional trigger/stop logic description (empty string clears)." }
            },
            "required": ["id"]
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        if !self.allow_mutation {
            return Ok(ToolResult::error(
                call_id,
                "delegated subagents cannot edit host schedules",
            ));
        }
        let id = args["id"].as_str().unwrap_or_default();
        let action = args["action"].as_str().unwrap_or("update");
        match action {
            "delete" => {
                if let Some(svc) = &self.service {
                    svc.cancel_task(id);
                }
                match self.store.delete(id) {
                    Ok(true) => Ok(ToolResult::success(call_id, format!("Deleted schedule {id}"))),
                    Ok(false) => Ok(ToolResult::error(call_id, format!("Schedule {id} not found"))),
                    Err(e) => Ok(ToolResult::error(call_id, e)),
                }
            }
            "pause" => {
                if let Some(svc) = &self.service {
                    svc.cancel_task(id);
                }
                match self.store.update(id, None, None, None, None, Some(true)) {
                    Ok(t) => Ok(ToolResult::success(
                        call_id,
                        serde_json::to_string_pretty(&t).unwrap_or_default(),
                    )),
                    Err(e) => Ok(ToolResult::error(call_id, e)),
                }
            }
            "resume" => match self.store.update(id, None, None, None, None, Some(false)) {
                Ok(t) => Ok(ToolResult::success(
                    call_id,
                    serde_json::to_string_pretty(&t).unwrap_or_default(),
                )),
                Err(e) => Ok(ToolResult::error(call_id, e)),
            },
            _ => {
                let name = args["name"].as_str().map(str::to_string);
                let instruction = args["instruction"].as_str().map(str::to_string);
                let rule = args["rule"].as_str().map(str::to_string);
                let require_approval = args["require_approval"].as_bool();
                let target = args["target"].as_str().map(|mode| {
                    parse_target(
                        Some(mode),
                        args["session_id"].as_str(),
                        self.current_session_id.as_deref(),
                    )
                });
                let profile = args.get("profile").and_then(|v| {
                    if v.is_null() {
                        Some(None)
                    } else {
                        v.as_str().map(|s| Some(s.to_string()))
                    }
                });
                let execution_mode = match args["execution_mode"].as_str() {
                    Some(raw) => match ExecutionMode::from_str_lossy(raw) {
                        Ok(m) => Some(m),
                        Err(e) => return Ok(ToolResult::error(call_id, e)),
                    },
                    None => None,
                };
                let active_window = args.get("active_window").and_then(|v| {
                    if v.is_null() {
                        Some(None)
                    } else {
                        v.as_str().map(|s| Some(s.to_string()))
                    }
                });
                let trigger_command = args.get("trigger_command").and_then(|v| {
                    if v.is_null() {
                        Some(None)
                    } else {
                        v.as_str().map(|s| Some(s.to_string()))
                    }
                });
                let trigger_logic = args.get("trigger_logic").and_then(|v| {
                    if v.is_null() {
                        Some(None)
                    } else {
                        v.as_str().map(|s| Some(s.to_string()))
                    }
                });

                match self.store.update_with_options(
                    id,
                    ScheduleUpdateOptions {
                        name,
                        instruction,
                        rule,
                        delivery_target: target,
                        paused: None,
                        require_approval,
                        profile,
                        execution_mode,
                        active_window,
                        trigger_command,
                        trigger_logic,
                    },
                ) {
                    Ok(t) => Ok(ToolResult::success(
                        call_id,
                        serde_json::to_string_pretty(&t).unwrap_or_default(),
                    )),
                    Err(e) => Ok(ToolResult::error(call_id, e)),
                }
            }
        }
    }
}
