use std::path::PathBuf;
use std::time::Duration;
use async_trait::async_trait;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::approval::ToolPreview;
use crate::error::HarnessError;
use crate::traits::Tool;
use crate::types::ToolResult;
use super::fs::resolve_path;

pub struct TerminalTool {
    workspace_root: PathBuf,
    default_timeout_secs: u64,
    output_max_bytes: usize,
}

impl TerminalTool {
    pub fn new(
        workspace_root: PathBuf,
        default_timeout_secs: u64,
        output_max_bytes: usize,
    ) -> Self {
        Self {
            workspace_root,
            default_timeout_secs,
            output_max_bytes,
        }
    }
}

#[async_trait]
impl Tool for TerminalTool {
    fn name(&self) -> &str {
        "terminal"
    }

    fn description(&self) -> &str {
        "Execute shell commands in the workspace with bounded timeout and output capture."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The shell command to execute" },
                "timeout_s": { "type": "integer", "description": "Max seconds to wait before timing out" },
                "workdir": { "type": "string", "description": "Working directory relative to workspace root" }
            },
            "required": ["command"]
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn preview(&self, args: &serde_json::Value) -> Option<ToolPreview> {
        let cmd = args.get("command").and_then(|c| c.as_str())?;
        Some(ToolPreview {
            target: Some(cmd.to_string()),
            diff: None,
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let command = match args.get("command").and_then(|c| c.as_str()) {
            Some(c) => c,
            None => return Ok(ToolResult::error(call_id, "Missing required argument 'command'")),
        };

        let workdir_str = args.get("workdir").and_then(|w| w.as_str()).unwrap_or(".");
        let resolved_workdir = match resolve_path(&self.workspace_root, workdir_str) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(call_id, e)),
        };

        let timeout_secs = args
            .get("timeout_s")
            .and_then(|t| t.as_u64())
            .unwrap_or(self.default_timeout_secs);

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(command)
            .current_dir(&resolved_workdir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolResult::error(
                    call_id,
                    format!("Failed to spawn command: {}", e),
                ))
            }
        };

        tokio::select! {
            _ = cancel_token.cancelled() => {
                Ok(ToolResult::error(call_id, "Command cancelled"))
            }
            res = tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait_with_output()) => {
                match res {
                    Ok(Ok(output)) => {
                        let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
                        if !stderr.is_empty() {
                            if !stdout.is_empty() {
                                stdout.push('\n');
                            }
                            stdout.push_str(&stderr);
                        }

                        if stdout.len() > self.output_max_bytes {
                            stdout.truncate(self.output_max_bytes);
                            stdout.push_str("\n\n[Truncated: output limit reached]");
                        }

                        if output.status.success() {
                            Ok(ToolResult::success(call_id, stdout))
                        } else {
                            let code = output.status.code().unwrap_or(-1);
                            Ok(ToolResult {
                                call_id: call_id.to_string(),
                                content: format!("exit code: {}\n{}", code, stdout),
                                is_error: true,
                            })
                        }
                    }
                    Ok(Err(e)) => {
                        Ok(ToolResult::error(call_id, format!("Process error: {}", e)))
                    }
                    Err(_) => {
                        Ok(ToolResult {
                            call_id: call_id.to_string(),
                            content: format!("Command timed out after {}s", timeout_secs),
                            is_error: true,
                        })
                    }
                }
            }
        }
    }
}
