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

#[async_trait]
pub trait SudoPrompter: Send + Sync {
    async fn prompt_sudo_password(&self, command: &str) -> Result<Option<String>, HarnessError>;
}

pub struct TerminalTool {
    workspace_root: PathBuf,
    /// Additional roots allowed as a `workdir` (`paths.extra_roots`).
    extra_roots: Vec<PathBuf>,
    default_timeout_secs: u64,
    output_max_bytes: usize,
    env_vars: std::collections::HashMap<String, String>,
    sudo_cache: Arc<Mutex<Option<String>>>,
    sudo_prompter: Option<Arc<dyn SudoPrompter>>,
    queue: Option<Arc<crate::tasks::BackgroundTaskQueue>>,
    prefix: Option<String>,
    rtk_rewrite: bool,
}

impl TerminalTool {
    pub fn new(
        workspace_root: PathBuf,
        default_timeout_secs: u64,
        output_max_bytes: usize,
    ) -> Self {
        Self {
            workspace_root,
            extra_roots: Vec::new(),
            default_timeout_secs,
            output_max_bytes,
            env_vars: std::collections::HashMap::new(),
            sudo_cache: Arc::new(Mutex::new(std::env::var("SUDO_PASSWORD").ok())),
            sudo_prompter: None,
            queue: None,
            prefix: None,
            rtk_rewrite: true,
        }
    }

    /// Extra roots a `workdir` may point at, mirroring the filesystem tools.
    pub fn with_extra_roots(mut self, extra_roots: Vec<PathBuf>) -> Self {
        self.extra_roots = extra_roots;
        self
    }

    pub fn with_prefix(mut self, prefix: Option<String>) -> Self {
        self.prefix = prefix;
        self
    }

    pub fn with_rtk_rewrite(mut self, enabled: bool) -> Self {
        self.rtk_rewrite = enabled;
        self
    }

    pub fn with_background_queue(mut self, queue: Arc<crate::tasks::BackgroundTaskQueue>) -> Self {
        self.queue = Some(queue);
        self
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

    pub async fn resolve_command(&self, command: &str) -> String {
        let trimmed = command.trim();
        if trimmed.is_empty() {
            return command.to_string();
        }

        if self.rtk_rewrite {
            if let Some(rtk_path) = find_rtk_executable(self.prefix.as_deref()) {
                if let Some(rewritten) = try_rtk_rewrite(trimmed, &rtk_path).await {
                    return rewritten;
                }
            }
        }

        if let Some(ref pfx) = self.prefix {
            let pfx_trimmed = pfx.trim();
            if !pfx_trimmed.is_empty() && !trimmed.starts_with(pfx_trimmed) {
                return format!("{} {}", pfx_trimmed, trimmed);
            }
        }

        command.to_string()
    }
}

pub fn find_rtk_executable(custom_prefix: Option<&str>) -> Option<PathBuf> {
    if let Some(prefix) = custom_prefix {
        let p_str = prefix.trim();
        let expanded = if p_str.starts_with("~/") {
            if let Ok(home) = std::env::var("HOME") {
                PathBuf::from(home).join(&p_str[2..])
            } else {
                PathBuf::from(p_str)
            }
        } else {
            PathBuf::from(p_str)
        };
        if expanded.is_file() {
            return Some(expanded);
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        let p = PathBuf::from(home).join(".local/bin/rtk");
        if p.is_file() {
            return Some(p);
        }
    }

    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let p = dir.join("rtk");
            if p.is_file() {
                return Some(p);
            }
        }
    }

    None
}

pub async fn try_rtk_rewrite(command: &str, rtk_path: &Path) -> Option<String> {
    let trimmed = command.trim_start();
    if trimmed.starts_with("rtk ") || trimmed == "rtk" {
        return None;
    }

    let mut cmd = tokio::process::Command::new(rtk_path);
    cmd.arg("rewrite")
        .arg(command)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);

    let output = match tokio::time::timeout(std::time::Duration::from_millis(1500), cmd.output()).await {
        Ok(Ok(out)) => out,
        _ => return None,
    };

    if output.status.code() == Some(0) || output.status.code() == Some(3) {
        let rewritten = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !rewritten.is_empty() && rewritten != command {
            return Some(rewritten);
        }
    }

    None
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
                "workdir": { "type": "string", "description": "Working directory relative to workspace root" },
                "background": { "type": "boolean", "description": "Run asynchronously in the background task queue" }
            },
            "required": ["command"]
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn preview(&self, args: &serde_json::Value) -> Option<ToolPreview> {
        let cmd = args.get("command").and_then(|c| c.as_str())?;
        let effective = self.resolve_command(cmd).await;
        Some(ToolPreview {
            target: Some(effective),
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

        let effective_command = self.resolve_command(command).await;

        let workdir_str = args.get("workdir").and_then(|w| w.as_str()).unwrap_or(".");
        let sandbox = crate::tools::sandbox::Sandbox::with_extra_roots(
            self.workspace_root.clone(),
            self.extra_roots.clone(),
        );
        let resolved_workdir = match sandbox.resolve(workdir_str) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(call_id, e)),
        };

        let background = args.get("background").and_then(|b| b.as_bool()).unwrap_or(false);
        if background {
            if let Some(ref q) = self.queue {
                return match q.spawn(&effective_command, &resolved_workdir) {
                    Ok(job_id) => {
                        let msg = json!({
                            "job_id": job_id,
                            "command": effective_command,
                            "status": "running",
                            "message": format!("Task running in background. Use process(action='poll', job_id='{}') to check progress.", job_id),
                        });
                        Ok(ToolResult::success(call_id, serde_json::to_string_pretty(&msg).unwrap_or_default()))
                    }
                    Err(e) => Ok(ToolResult::error(call_id, format!("Failed to queue background task: {}", e))),
                };
            }
        }

        let timeout_secs = args
            .get("timeout_s")
            .and_then(|t| t.as_u64())
            .unwrap_or(self.default_timeout_secs);

        let (exec_command, sudo_stdin) = {
            let (rewritten, sudo_count) = rewrite_sudo_command(&effective_command);
            if sudo_count > 0 {
                if check_sudo_nopasswd(&resolved_workdir).await {
                    (effective_command.clone(), None)
                } else {
                    let cached_opt = self.sudo_cache.lock().unwrap().clone();
                    let password_opt = match cached_opt {
                        Some(p) => Some(p),
                        None => {
                            if let Some(ref prompter) = self.sudo_prompter {
                                match prompter.prompt_sudo_password(&effective_command).await {
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
                        (effective_command.clone(), None)
                    }
                }
            } else {
                (effective_command.clone(), None)
            }
        };

        let is_background = exec_command.trim().ends_with('&');
        let final_exec_command = if is_background && sudo_stdin.is_none() {
            let stripped = exec_command.trim().trim_end_matches('&').trim();
            format!("( {} ) </dev/null >/dev/null 2>&1 & echo $!", stripped)
        } else {
            exec_command.clone()
        };

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c")
            .arg(&final_exec_command)
            .current_dir(&resolved_workdir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        if sudo_stdin.is_some() {
            cmd.stdin(std::process::Stdio::piped());
        } else {
            cmd.stdin(std::process::Stdio::null());
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
