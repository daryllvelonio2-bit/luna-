use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::engine::CodebaseEvolver;
use harness_core::{EvolverEngine, HarnessError, Tool, ToolResult};

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

    fn is_mutating(&self) -> bool {
        true
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let task = args.get("task").and_then(|t| t.as_str()).unwrap_or("");
        if task.trim().is_empty() {
            return Ok(ToolResult::error(
                call_id,
                "task description cannot be empty",
            ));
        }
        let evidence = args.get("evidence").and_then(|e| e.as_str());

        match self.evolver.delegate_improvement(task, evidence).await {
            Ok(job_id) => {
                let out = format!(
                    "Specialized engineer agent spawned in isolated background worktree.\nJob ID: {}\nBranch: eng/{}\n\nCall `check_improvement_job(job_id: \"{}\")` (or `check_improvement_job()` for latest) at any time to inspect live tool calls, modified files, and test outcomes.",
                    job_id, job_id, job_id
                );
                Ok(ToolResult::success(call_id, out))
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
        "Check the live status, current tool activity, modified files, test outcomes, and logs of a background harness improvement job."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "job_id": {
                    "type": "string",
                    "description": "Optional job ID (or 'latest' for most recent background job). Defaults to latest if omitted."
                }
            }
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
                let status_str = match &job.status {
                    crate::JobStatus::Queued => "queued (waiting to start)".to_string(),
                    crate::JobStatus::Running => {
                        "running (actively executing in background worktree)".to_string()
                    }
                    crate::JobStatus::Verified => {
                        "verified (fix verified and tests clean)".to_string()
                    }
                    crate::JobStatus::ReadyForReenactment { .. } => {
                        "ready for reenactment (update staged)".to_string()
                    }
                    crate::JobStatus::ScheduledUpdate { .. } => {
                        "scheduled update (will apply on restart)".to_string()
                    }
                    crate::JobStatus::FailedTest { error } => format!("failed test: {}", error),
                    crate::JobStatus::FailedVerification { reason } => {
                        format!("failed verification: {}", reason)
                    }
                };

                let recent_logs = if job.logs.len() > 15 {
                    job.logs[job.logs.len() - 15..].to_vec()
                } else {
                    job.logs.clone()
                };

                let mut out_text = format!(
                    "Job ID: {}\nStatus: {}\nTask: {}\nBranch: {}\nWorktree: {}\n",
                    job.id, status_str, job.task, job.branch, job.worktree_dir
                );
                if let Some(ref current) = job.current_tool {
                    out_text.push_str(&format!("Current Activity: {}\n", current));
                }
                if !job.modified_files.is_empty() {
                    out_text.push_str(&format!(
                        "Modified Files: {}\n",
                        job.modified_files.join(", ")
                    ));
                }
                if let Some(ref sum) = job.summary {
                    out_text.push_str(&format!("Engineer Summary: {}\n", sum));
                }
                out_text.push_str("\nProgress Timeline:\n");
                for log in &recent_logs {
                    out_text.push_str(&format!("  {}\n", log));
                }

                Ok(ToolResult::success(call_id, out_text))
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
        "List all background harness improvement jobs with their current status and active tool activity."
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
                if jobs.is_empty() {
                    return Ok(ToolResult::success(
                        call_id,
                        "No harness improvement jobs found.",
                    ));
                }
                let mut out = format!("Found {} improvement jobs:\n", jobs.len());
                for job in &jobs {
                    let status_str = match &job.status {
                        crate::JobStatus::Queued => "queued",
                        crate::JobStatus::Running => "running",
                        crate::JobStatus::Verified => "verified",
                        crate::JobStatus::ReadyForReenactment { .. } => "ready_for_reenactment",
                        crate::JobStatus::ScheduledUpdate { .. } => "scheduled_update",
                        crate::JobStatus::FailedTest { .. } => "failed_test",
                        crate::JobStatus::FailedVerification { .. } => "failed_verification",
                    };
                    out.push_str(&format!("• [{}] {} - {}\n", job.id, status_str, job.task));
                    if let Some(ref current) = job.current_tool {
                        out.push_str(&format!("    Current activity: {}\n", current));
                    }
                    if !job.modified_files.is_empty() {
                        out.push_str(&format!(
                            "    Modified: {}\n",
                            job.modified_files.join(", ")
                        ));
                    }
                }
                Ok(ToolResult::success(call_id, out))
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

    fn is_mutating(&self) -> bool {
        true
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

        match self
            .evolver
            .verify_and_stage(job_id, scenario_input, absent_pattern)
            .await
        {
            Ok(msg) => Ok(ToolResult::success(call_id, msg)),
            Err(e) => Ok(ToolResult::error(call_id, e.to_string())),
        }
    }
}

pub struct PtyScreenCaptureTool {
    workspace_root: std::path::PathBuf,
}

impl PtyScreenCaptureTool {
    pub fn new(workspace_root: std::path::PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for PtyScreenCaptureTool {
    fn name(&self) -> &str {
        "pty_screen_capture"
    }

    fn description(&self) -> &str {
        "Spawn a binary or command in an isolated pseudo-terminal (PTY) with timed inputs and return a 2D virtual screen capture snapshot as seen by a human terminal user."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "binary": { "type": "string", "description": "Path to binary or executable (default: 'target/release/luna')" },
                "args": { "type": "array", "items": { "type": "string" }, "description": "Command-line arguments" },
                "inputs": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "delay_ms": { "type": "integer", "description": "Delay in milliseconds before sending this keystroke sequence" },
                            "input": { "type": "string", "description": "Keystroke string (supports escape sequences like '\\r' for enter, '\\x03' for Ctrl-C, '\\x1b[B' for down)" }
                        },
                        "required": ["delay_ms", "input"]
                    },
                    "description": "Timed keystroke sequence to feed into the PTY"
                },
                "rows": { "type": "integer", "description": "Terminal rows (default: 24)" },
                "cols": { "type": "integer", "description": "Terminal columns (default: 80)" },
                "timeout_secs": { "type": "integer", "description": "Maximum execution time in seconds (default: 15)" }
            }
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let binary_str = args
            .get("binary")
            .and_then(|b| b.as_str())
            .unwrap_or("target/release/luna");

        let bin_path = if std::path::Path::new(binary_str).is_absolute() {
            std::path::PathBuf::from(binary_str)
        } else {
            self.workspace_root.join(binary_str)
        };

        if !bin_path.exists() {
            return Ok(ToolResult::error(
                call_id,
                format!("Binary not found at {}", bin_path.display()),
            ));
        }

        let cmd_args: Vec<String> = args
            .get("args")
            .and_then(|a| a.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let cmd_args_refs: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();

        let mut timed_inputs: Vec<crate::pty::TimedInput> = Vec::new();
        if let Some(arr) = args.get("inputs").and_then(|i| i.as_array()) {
            for item in arr {
                let delay = item.get("delay_ms").and_then(|d| d.as_u64()).unwrap_or(0);
                if let Some(txt) = item.get("input").and_then(|t| t.as_str()) {
                    let unescaped = unescape_keystrokes(txt);
                    timed_inputs.push(crate::pty::TimedInput {
                        delay_ms: delay,
                        input: unescaped,
                    });
                }
            }
        }

        let rows = args.get("rows").and_then(|r| r.as_u64()).unwrap_or(24) as usize;
        let cols = args.get("cols").and_then(|c| c.as_u64()).unwrap_or(80) as usize;
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|t| t.as_u64())
            .unwrap_or(15);

        match crate::pty::run_in_pty_with_capture(
            &bin_path,
            &cmd_args_refs,
            &self.workspace_root,
            &timed_inputs,
            rows,
            cols,
            std::time::Duration::from_secs(timeout_secs),
        ) {
            Ok((code, _raw, capture)) => {
                let out = json!({
                    "exit_code": code,
                    "rows": capture.rows,
                    "cols": capture.cols,
                    "cursor": {
                        "row": capture.cursor_row,
                        "col": capture.cursor_col
                    },
                    "screen_display": capture.rendered_display,
                    "visible_lines": capture.lines
                });
                Ok(ToolResult::success(call_id, out.to_string()))
            }
            Err(e) => Ok(ToolResult::error(call_id, e.to_string())),
        }
    }
}

fn unescape_keystrokes(s: &str) -> String {
    let mut res = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('r') => res.push('\r'),
                Some('n') => res.push('\n'),
                Some('t') => res.push('\t'),
                Some('\\') => res.push('\\'),
                Some('x') => {
                    let mut hex = String::new();
                    if let Some(h1) = chars.next() {
                        hex.push(h1);
                    }
                    if let Some(h2) = chars.next() {
                        hex.push(h2);
                    }
                    if let Ok(b) = u8::from_str_radix(&hex, 16) {
                        res.push(b as char);
                    }
                }
                Some(other) => {
                    res.push('\\');
                    res.push(other);
                }
                None => res.push('\\'),
            }
        } else {
            res.push(c);
        }
    }
    res
}
