use std::sync::Arc;
use async_trait::async_trait;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use harness_core::{EvolverEngine, HarnessError, Tool, ToolResult};
use crate::engine::CodebaseEvolver;

pub struct InspectSourceTool {
    engine: Arc<dyn EvolverEngine>,
}

impl InspectSourceTool {
    pub fn new(engine: Arc<dyn EvolverEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for InspectSourceTool {
    fn name(&self) -> &str {
        "inspect_harness_source"
    }

    fn description(&self) -> &str {
        "Inspect the agent harness codebase source tree or read a specific source file to diagnose issues, review implementations, or plan self-improvements."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Relative path within workspace (e.g. 'crates/harness-core/src/agent.rs' or 'crates')" }
            },
            "required": ["path"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or("");
        match self.engine.inspect_source(path).await {
            Ok(content) => Ok(ToolResult::success(call_id, content)),
            Err(e) => Ok(ToolResult::error(call_id, e.to_string())),
        }
    }
}

pub struct DelegateImprovementTool {
    evolver: Arc<CodebaseEvolver>,
}

impl DelegateImprovementTool {
    pub fn new(evolver: Arc<CodebaseEvolver>) -> Self {
        Self { evolver }
    }
}

#[async_trait]
impl Tool for DelegateImprovementTool {
    fn name(&self) -> &str {
        "delegate_improvement"
    }

    fn description(&self) -> &str {
        "Spawn an autonomous background engineer agent in an isolated git worktree to fix a bug, optimize performance, or improve the harness codebase."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "task": {
                    "type": "string",
                    "description": "Clear explanation of the bug, bottleneck, or feature to implement in the harness codebase."
                },
                "evidence": {
                    "type": "string",
                    "description": "Optional reproduction context, error logs, command, or observed bottleneck."
                }
            },
            "required": ["task"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let task = args.get("task").and_then(|t| t.as_str()).unwrap_or("");
        if task.trim().is_empty() {
            return Ok(ToolResult::error(call_id, "task description cannot be empty"));
        }
        let evidence = args.get("evidence").and_then(|e| e.as_str());

        match self.evolver.delegate_improvement(task, evidence).await {
            Ok(job_id) => {
                let out = json!({
                    "status": "queued",
                    "job_id": job_id,
                    "branch": format!("eng/{}", job_id),
                    "message": "Specialized engineer agent spawned in isolated background worktree. Use `check_improvement_job` with this job_id to monitor progress, and `verify_and_stage_update` to verify with scenario PTY re-enactment."
                });
                Ok(ToolResult::success(call_id, out.to_string()))
            }
            Err(e) => Ok(ToolResult::error(call_id, e.to_string())),
        }
    }
}

pub struct CheckImprovementTool {
    evolver: Arc<CodebaseEvolver>,
}

impl CheckImprovementTool {
    pub fn new(evolver: Arc<CodebaseEvolver>) -> Self {
        Self { evolver }
    }
}

#[async_trait]
impl Tool for CheckImprovementTool {
    fn name(&self) -> &str {
        "check_improvement_job"
    }

    fn description(&self) -> &str {
        "Check the status, test outcomes, and logs of a background harness improvement job."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "job_id": { "type": "string", "description": "The job ID returned by delegate_improvement." }
            },
            "required": ["job_id"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let job_id = args.get("job_id").and_then(|j| j.as_str()).unwrap_or("");
        match self.evolver.check_job(job_id).await {
            Ok(job) => {
                let out = json!({
                    "job_id": job.id,
                    "task": job.task,
                    "branch": job.branch,
                    "status": job.status,
                    "logs": job.logs,
                    "created_at": job.created_at.to_rfc3339(),
                    "updated_at": job.updated_at.to_rfc3339(),
                });
                Ok(ToolResult::success(call_id, out.to_string()))
            }
            Err(e) => Ok(ToolResult::error(call_id, e.to_string())),
        }
    }
}

pub struct ListImprovementJobsTool {
    evolver: Arc<CodebaseEvolver>,
}

impl ListImprovementJobsTool {
    pub fn new(evolver: Arc<CodebaseEvolver>) -> Self {
        Self { evolver }
    }
}

#[async_trait]
impl Tool for ListImprovementJobsTool {
    fn name(&self) -> &str {
        "list_improvement_jobs"
    }

    fn description(&self) -> &str {
        "List all background harness improvement jobs and their current status in the queue."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        _args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        match self.evolver.list_jobs().await {
            Ok(jobs) => {
                let items: Vec<serde_json::Value> = jobs
                    .into_iter()
                    .map(|job| {
                        json!({
                            "job_id": job.id,
                            "task": job.task,
                            "status": job.status,
                            "branch": job.branch,
                            "created_at": job.created_at.to_rfc3339(),
                        })
                    })
                    .collect();
                Ok(ToolResult::success(call_id, json!(items).to_string()))
            }
            Err(e) => Ok(ToolResult::error(call_id, e.to_string())),
        }
    }
}

pub struct VerifyAndStageUpdateTool {
    evolver: Arc<CodebaseEvolver>,
}

impl VerifyAndStageUpdateTool {
    pub fn new(evolver: Arc<CodebaseEvolver>) -> Self {
        Self { evolver }
    }
}

#[async_trait]
impl Tool for VerifyAndStageUpdateTool {
    fn name(&self) -> &str {
        "verify_and_stage_update"
    }

    fn description(&self) -> &str {
        "Re-enact a scenario against the compiled worktree binary inside an isolated PTY to verify the fix and schedule the update."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "job_id": { "type": "string", "description": "ID of the completed job to verify." },
                "scenario_input": { "type": "string", "description": "Input sequence to feed into the binary in the PTY." },
                "absent_pattern": { "type": "string", "description": "Forbidden error or bottleneck substring that must be absent." }
            },
            "required": ["job_id"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let job_id = args.get("job_id").and_then(|j| j.as_str()).unwrap_or("");
        let scenario_input = args.get("scenario_input").and_then(|s| s.as_str());
        let absent_pattern = args.get("absent_pattern").and_then(|p| p.as_str());

        match self.evolver.verify_and_stage(job_id, scenario_input, absent_pattern).await {
            Ok(msg) => Ok(ToolResult::success(call_id, msg)),
            Err(e) => Ok(ToolResult::error(call_id, e.to_string())),
        }
    }
}
