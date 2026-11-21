//! Model-facing scheduling tools matching DeepSeek Harness:
//! `schedule_create`, `schedule_list`, `schedule_view`, and `schedule_edit`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use harness_core::{HarnessError, Tool, ToolRegistry, ToolResult};

use crate::model::DeliveryTarget;
use crate::store::ScheduleStore;

/// Register the four schedule tools onto `registry`.
pub async fn register_schedule_tools(
    registry: &Arc<dyn ToolRegistry>,
    store: Arc<ScheduleStore>,
    current_session_id: Option<String>,
    allow_mutation: bool,
) -> Result<(), HarnessError> {
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
        "Create a scheduled reminder or recurring automation task. Rules accept 'once:<RFC3339>', 'every:<seconds>', 'daily:<HH:MM>', or 5-field UTC cron ('min hour dom mon dow')."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "Short task name" },
                "instruction": { "type": "string", "description": "Prompt/instruction delivered to the agent when due" },
                "rule": { "type": "string", "description": "Recurrence rule: once:<RFC3339>, every:<secs>, daily:<HH:MM>, or 5-field cron" },
                "target": {
                    "type": "string",
                    "enum": ["existing_session", "single_new_session", "new_session_each_run"],
                    "description": "Where each run delivers its instruction"
                },
                "session_id": { "type": "string", "description": "Optional explicit session id for existing_session" },
                "require_approval": {
                    "type": "boolean",
                    "description": "Whether tool calls during scheduled runs ask for permission (default: false — automations run without needing permissions unless changed)"
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
        let target = parse_target(
            args["target"].as_str(),
            args["session_id"].as_str(),
            self.current_session_id.as_deref(),
        );
        match self.store.create_with_approval(
            None,
            name,
            instruction,
            rule,
            target,
            None,
            require_approval,
        ) {
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
        "Inspect one scheduled task by id, including its recent delivery history."
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
    current_session_id: Option<String>,
    allow_mutation: bool,
}

#[async_trait]
impl Tool for ScheduleEditTool {
    fn name(&self) -> &str {
        "schedule_edit"
    }

    fn description(&self) -> &str {
        "Edit, pause, resume, or delete a scheduled task by id."
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
                "require_approval": { "type": "boolean", "description": "Toggle whether scheduled runs require interactive approval (default false)" }
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
            "delete" => match self.store.delete(id) {
                Ok(true) => Ok(ToolResult::success(
                    call_id,
                    format!("deleted schedule {id}"),
                )),
                Ok(false) => Ok(ToolResult::error(
                    call_id,
                    format!("schedule '{id}' not found"),
                )),
                Err(e) => Ok(ToolResult::error(call_id, e)),
            },
            "pause" => match self.store.update(id, None, None, None, None, Some(true)) {
                Ok(t) => Ok(ToolResult::success(
                    call_id,
                    serde_json::to_string_pretty(&t).unwrap_or_default(),
                )),
                Err(e) => Ok(ToolResult::error(call_id, e)),
            },
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
                match self.store.update_with_approval(
                    id,
                    name,
                    instruction,
                    rule,
                    target,
                    None,
                    require_approval,
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
