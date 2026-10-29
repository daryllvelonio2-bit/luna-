use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::error::HarnessError;
use crate::tasks::BackgroundTaskQueue;
use crate::approval::ToolPreview;
use crate::traits::Tool;
use crate::types::ToolResult;

pub struct ProcessTool {
    queue: Arc<BackgroundTaskQueue>,
}

impl ProcessTool {
    pub fn new(queue: Arc<BackgroundTaskQueue>) -> Self {
        Self { queue }
    }
}

#[async_trait]
impl Tool for ProcessTool {
    fn name(&self) -> &str {
        "process"
    }

    fn description(&self) -> &str {
        "Manage background tasks: 'list' running and recent jobs, 'poll' status and output logs, or 'kill' a running process."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "poll", "kill"],
                    "description": "The action to perform: 'list', 'poll', or 'kill'"
                },
                "job_id": {
                    "type": "string",
                    "description": "Target job ID (required for 'poll' and 'kill')"
                },
                "tail": {
                    "type": "integer",
                    "description": "Number of log lines to read from the tail when polling (default 40)"
                }
            },
            "required": ["action"]
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    fn is_mutating_call(&self, args: &serde_json::Value) -> bool {
        let action = args.get("action").and_then(|a| a.as_str()).unwrap_or("");
        action == "kill"
    }

    async fn preview(&self, args: &serde_json::Value) -> Option<ToolPreview> {
        let action = args.get("action").and_then(|a| a.as_str())?;
        if action == "kill" {
            let job_id = args.get("job_id").and_then(|j| j.as_str())?;
            Some(ToolPreview {
                target: Some(format!("kill background task {job_id}")),
                diff: None,
            })
        } else {
            None
        }
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let action = match args.get("action").and_then(|a| a.as_str()) {
            Some(a) => a,
            None => {
                return Ok(ToolResult::error(
                    call_id,
                    "Missing required argument 'action'",
                ))
            }
        };

        match action {
            "list" => {
                let jobs = self.queue.list();
                if jobs.is_empty() {
                    return Ok(ToolResult::success(call_id, "No background tasks."));
                }
                let out = serde_json::to_string_pretty(&jobs).unwrap_or_default();
                Ok(ToolResult::success(call_id, out))
            }
            "poll" => {
                let job_id = match args.get("job_id").and_then(|j| j.as_str()) {
                    Some(j) => j,
                    None => {
                        return Ok(ToolResult::error(
                            call_id,
                            "Missing argument 'job_id' for poll action",
                        ))
                    }
                };
                let tail = args.get("tail").and_then(|t| t.as_u64()).unwrap_or(40) as usize;
                match self.queue.poll(job_id, tail) {
                    Ok(res) => {
                        let out = serde_json::to_string_pretty(&res).unwrap_or_default();
                        Ok(ToolResult::success(call_id, out))
                    }
                    Err(e) => Ok(ToolResult::error(call_id, e.to_string())),
                }
            }
            "kill" => {
                let job_id = match args.get("job_id").and_then(|j| j.as_str()) {
                    Some(j) => j,
                    None => {
                        return Ok(ToolResult::error(
                            call_id,
                            "Missing argument 'job_id' for kill action",
                        ))
                    }
                };
                match self.queue.kill(job_id) {
                    Ok(true) => Ok(ToolResult::success(
                        call_id,
                        format!("Process '{}' killed.", job_id),
                    )),
                    Ok(false) => Ok(ToolResult::error(
                        call_id,
                        format!("Process '{}' not found or already stopped.", job_id),
                    )),
                    Err(e) => Ok(ToolResult::error(call_id, e.to_string())),
                }
            }
            other => Ok(ToolResult::error(
                call_id,
                format!(
                    "Unknown action '{}'. Valid actions: list, poll, kill",
                    other
                ),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_process_tool_mutation_and_preview() {
        let temp = tempfile::tempdir().unwrap();
        let queue = Arc::new(BackgroundTaskQueue::new(temp.path().to_path_buf()));
        let tool = ProcessTool::new(queue);

        assert!(tool.is_mutating());
        assert!(!tool.is_mutating_call(&json!({"action": "list"})));
        assert!(!tool.is_mutating_call(&json!({"action": "poll", "job_id": "job_123"})));
        assert!(tool.is_mutating_call(&json!({"action": "kill", "job_id": "job_123"})));

        let preview = tool
            .preview(&json!({"action": "kill", "job_id": "job_123"}))
            .await;
        assert!(preview.is_some());
        assert_eq!(
            preview.unwrap().target,
            Some("kill background task job_123".to_string())
        );

        let preview_list = tool.preview(&json!({"action": "list"})).await;
        assert!(preview_list.is_none());
    }
}
