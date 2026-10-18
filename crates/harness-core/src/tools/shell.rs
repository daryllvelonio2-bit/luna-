use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use async_trait::async_trait;
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

use crate::approval::ToolPreview;
use crate::error::HarnessError;
use crate::traits::Tool;
use crate::types::ToolResult;
use super::fs::resolve_path;

#[async_trait]
pub trait SudoPrompter: Send + Sync {
    async fn prompt_sudo_password(&self, command: &str) -> Result<Option<String>, HarnessError>;
}

pub struct TerminalTool {
    workspace_root: PathBuf,
    default_timeout_secs: u64,
    output_max_bytes: usize,
    env_vars: std::collections::HashMap<String, String>,
    sudo_cache: Arc<Mutex<Option<String>>>,
    sudo_prompter: Option<Arc<dyn SudoPrompter>>,
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
            env_vars: std::collections::HashMap::new(),
            sudo_cache: Arc::new(Mutex::new(std::env::var("SUDO_PASSWORD").ok())),
            sudo_prompter: None,
        }
    }

    pub fn with_env(mut self, key: impl Into<String>, val: impl Into<String>) -> Self {
        self.env_vars.insert(key.into(), val.into());
        self
    }

    pub fn with_sudo_cache(mut self, cache: Arc<Mutex<Option<String>>>) -> Self {
        self.sudo_cache = cache;
        self
    }

    pub fn with_sudo_prompter(mut self, prompter: Arc<dyn SudoPrompter>) -> Self {
        self.sudo_prompter = Some(prompter);
        self
    }

    pub fn sudo_cache(&self) -> Arc<Mutex<Option<String>>> {
        self.sudo_cache.clone()
    }
}

pub fn rewrite_sudo_command(command: &str) -> (String, usize) {
    let mut out = String::new();
    let mut sudo_count = 0;
    let mut at_command_start = true;

    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let mut word = String::new();

    let flush_word = |word: &str, at_cmd: bool, out: &mut String, count: &mut usize| -> bool {
        if word.is_empty() {
            return at_cmd;
        }
        let executable = word.rsplit('/').next().unwrap_or(word);
        if at_cmd && executable == "sudo" {
            out.push_str(word);
            out.push_str(" -S -p ''");
            *count += 1;
            false
        } else {
            out.push_str(word);
            if at_cmd && word.contains('=') && !word.starts_with('=') {
                true
            } else {
                false
            }
        }
    };

    let chars: Vec<char> = command.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if escaped {
            word.push(c);
            escaped = false;
            i += 1;
            continue;
        }
        if c == '\\' && !in_single {
            escaped = true;
            word.push(c);
            i += 1;
            continue;
        }
        if c == '\'' && !in_double {
            in_single = !in_single;
            word.push(c);
            i += 1;
            continue;
        }
        if c == '"' && !in_single {
            in_double = !in_double;
            word.push(c);
            i += 1;
            continue;
        }
        if in_single || in_double {
            word.push(c);
            i += 1;
            continue;
        }

        if c.is_whitespace() || c == ';' || c == '&' || c == '|' || c == '(' || c == ')' {
            at_command_start = flush_word(&word, at_command_start, &mut out, &mut sudo_count);
            word.clear();
            out.push(c);
            if c == ';' || c == '&' || c == '|' || c == '(' || c == ')' || c == '\n' {
                at_command_start = true;
            }
        } else {
            word.push(c);
        }
        i += 1;
    }
    flush_word(&word, at_command_start, &mut out, &mut sudo_count);

    (out, sudo_count)
}

pub fn contains_sudo(command: &str) -> bool {
    let (_, count) = rewrite_sudo_command(command);
    count > 0
}

pub fn is_sudo_auth_failure(output: &str) -> bool {
    let lower = output.to_lowercase();
    lower.contains("sudo: authentication failed")
        || lower.contains("incorrect password attempt")
        || lower.contains("3 incorrect password attempts")
        || lower.contains("sudo: a password is required")
        || lower.contains("conversation failed")
}

pub async fn check_sudo_nopasswd(workdir: &Path) -> bool {
    let res = tokio::time::timeout(
        Duration::from_millis(500),
        tokio::process::Command::new("sudo")
            .arg("-n")
            .arg("true")
            .current_dir(workdir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status(),
    )
    .await;
    match res {
        Ok(Ok(status)) => status.success(),
        _ => false,
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

        let (exec_command, sudo_stdin) = {
            let (rewritten, sudo_count) = rewrite_sudo_command(command);
            if sudo_count > 0 {
                if check_sudo_nopasswd(&resolved_workdir).await {
                    (command.to_string(), None)
                } else {
                    let cached_opt = self.sudo_cache.lock().unwrap().clone();
                    let password_opt = match cached_opt {
                        Some(p) => Some(p),
                        None => {
                            if let Some(ref prompter) = self.sudo_prompter {
                                match prompter.prompt_sudo_password(command).await {
                                    Ok(Some(pwd)) if !pwd.is_empty() => {
                                        *self.sudo_cache.lock().unwrap() = Some(pwd.clone());
                                        Some(pwd)
                                    }
                                    _ => None,
                                }
                            } else {
                                None
                            }
                        }
                    };

                    if let Some(pwd) = password_opt {
                        let stdin_data = (pwd + "\n").repeat(sudo_count);
                        (rewritten, Some(stdin_data))
                    } else {
                        (command.to_string(), None)
                    }
                }
            } else {
                (command.to_string(), None)
            }
        };

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(&exec_command)
            .current_dir(&resolved_workdir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        if sudo_stdin.is_some() {
            cmd.stdin(std::process::Stdio::piped());
        }

        for (k, v) in &self.env_vars {
            cmd.env(k, v);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolResult::error(
                    call_id,
                    format!("Failed to spawn command: {}", e),
                ))
            }
        };

        if let Some(ref data) = sudo_stdin {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(data.as_bytes()).await;
                let _ = stdin.flush().await;
            }
        }

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
                            if is_sudo_auth_failure(&stdout) {
                                *self.sudo_cache.lock().unwrap() = None;
                            }
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
