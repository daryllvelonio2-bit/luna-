//! The offline tool surface for eval cases: a per-case sandbox plus deterministic tools, and the
//! registry wrapper that records every dispatch and applies the case's approval decision.
//!
//! The tools are deliberately dumb and local: a case asserts on the sandbox's bytes, so nothing here
//! may reach the network, `~/.luna` or the repository.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Serialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use harness_core::{
    DefaultToolRegistry, HarnessError, Tool, ToolCall, ToolDefinition, ToolRegistry, ToolResult,
};

use super::scenario::is_sandbox_relative;

/// Wording matches `HarnessError::ApprovalRejected`, so a denied call reads the same here as it will
/// when the agent loop grows a real gate (card `t_c4f9c07d`).
fn denied(tool: &str) -> String {
    format!(
        "Human approval rejected: '{}' was refused by the reviewer",
        tool
    )
}

pub struct EvalSandbox {
    root: PathBuf,
}

impl EvalSandbox {
    /// A fresh directory per case, wiped first so a rerun cannot inherit a previous case's files.
    pub fn new(case: &str) -> Result<Self, HarnessError> {
        let root = std::env::temp_dir()
            .join(format!("luna-eval-{}", std::process::id()))
            .join(case);
        if root.exists() {
            std::fs::remove_dir_all(&root)
                .map_err(|e| HarnessError::Io(format!("{}: {}", root.display(), e)))?;
        }
        std::fs::create_dir_all(&root)
            .map_err(|e| HarnessError::Io(format!("{}: {}", root.display(), e)))?;
        Ok(Self { root })
    }

    pub fn resolve(&self, relative: &str) -> Result<PathBuf, HarnessError> {
        if !is_sandbox_relative(relative) {
            return Err(HarnessError::Io(format!(
                "'{}' is not a sandbox-relative path",
                relative
            )));
        }
        Ok(self.root.join(relative))
    }

    pub fn exists(&self, relative: &str) -> bool {
        self.resolve(relative).map(|p| p.exists()).unwrap_or(false)
    }

    pub fn read_to_string(&self, relative: &str) -> Result<String, HarnessError> {
        let path = self.resolve(relative)?;
        std::fs::read_to_string(&path)
            .map_err(|e| HarnessError::Io(format!("{}: {}", path.display(), e)))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ObservedCall {
    pub name: String,
    pub arguments: serde_json::Value,
    pub error: bool,
    /// True when the call never reached the tool because approval was denied.
    pub denied: bool,
}

/// Records every dispatched call (including denied and unknown-tool calls, which never reach a tool)
/// and refuses the tools the case's `approval.deny` list names.
pub struct RecordingRegistry {
    inner: Arc<dyn ToolRegistry>,
    log: Mutex<Vec<ObservedCall>>,
    deny: Vec<String>,
}

impl RecordingRegistry {
    pub fn new(deny: Vec<String>) -> Self {
        Self {
            inner: Arc::new(DefaultToolRegistry::new()),
            log: Mutex::new(Vec::new()),
            deny,
        }
    }

    pub fn calls(&self) -> Vec<ObservedCall> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn record(&self, call: &ToolCall, error: bool, denied: bool) {
        self.log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(ObservedCall {
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                error,
                denied,
            });
    }
}

#[async_trait]
impl ToolRegistry for RecordingRegistry {
    async fn register(&self, tool: Arc<dyn Tool>) -> Result<(), HarnessError> {
        self.inner.register(tool).await
    }

    async fn list_tools(&self) -> Result<Vec<ToolDefinition>, HarnessError> {
        self.inner.list_tools().await
    }

    async fn is_mutating(&self, name: &str) -> Result<bool, HarnessError> {
        self.inner.is_mutating(name).await
    }

    async fn execute(
        &self,
        call: &ToolCall,
        cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        if self.deny.iter().any(|d| d == &call.name) {
            self.record(call, true, true);
            return Ok(ToolResult::error(&call.id, denied(&call.name)));
        }

        let result = self.inner.execute(call, cancel_token).await;
        self.record(
            call,
            result.as_ref().map(|r| r.is_error).unwrap_or(true),
            false,
        );
        result
    }
}

/// Installs one of [`super::scenario::BUILTIN_TOOLS`] into `registry`.
pub async fn register_builtin(
    registry: &Arc<RecordingRegistry>,
    name: &str,
    sandbox: Arc<EvalSandbox>,
) -> Result<(), HarnessError> {
    let tool: Arc<dyn Tool> = match name {
        "eval_write_file" => Arc::new(EvalWriteFileTool { sandbox }),
        "eval_read_file" => Arc::new(EvalReadFileTool { sandbox }),
        "eval_echo" => Arc::new(EvalEchoTool),
        "eval_fail" => Arc::new(EvalFailTool),
        other => {
            return Err(HarnessError::ConfigurationError(format!(
                "unknown eval tool '{}'",
                other
            )))
        }
    };
    registry.register(tool).await
}

/// Argument reading shared by the tools: a non-object payload or a missing/mistyped key is a tool
/// error the model must see, not a panic.
fn string_arg(arguments: &serde_json::Value, key: &str) -> Result<String, String> {
    arguments
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| {
            format!(
                "invalid arguments: '{}' must be a string (got {})",
                key,
                serde_json::to_string(arguments).unwrap_or_else(|_| "unserializable".to_string())
            )
        })
}

pub struct EvalWriteFileTool {
    sandbox: Arc<EvalSandbox>,
}

#[async_trait]
impl Tool for EvalWriteFileTool {
    fn name(&self) -> &str {
        "eval_write_file"
    }

    fn description(&self) -> &str {
        "Write text to a file inside the eval sandbox. Arguments: path (sandbox-relative), content."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Sandbox-relative file path" },
                "content": { "type": "string", "description": "Exact bytes to write" }
            },
            "required": ["path", "content"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let (path, content) = match (
            string_arg(arguments, "path"),
            string_arg(arguments, "content"),
        ) {
            (Ok(p), Ok(c)) => (p, c),
            (Err(e), _) | (_, Err(e)) => return Ok(ToolResult::error(call_id, e)),
        };

        let target = match self.sandbox.resolve(&path) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(call_id, e.to_string())),
        };
        if let Some(parent) = target.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return Ok(ToolResult::error(
                    call_id,
                    format!("create {}: {}", parent.display(), e),
                ));
            }
        }
        if let Err(e) = std::fs::write(&target, &content) {
            return Ok(ToolResult::error(
                call_id,
                format!("write {}: {}", target.display(), e),
            ));
        }

        Ok(ToolResult::success(
            call_id,
            json!({ "status": "written", "path": path, "bytes": content.len() }).to_string(),
        ))
    }
}

pub struct EvalReadFileTool {
    sandbox: Arc<EvalSandbox>,
}

#[async_trait]
impl Tool for EvalReadFileTool {
    fn name(&self) -> &str {
        "eval_read_file"
    }

    fn description(&self) -> &str {
        "Read a file from the eval sandbox verbatim. Arguments: path (sandbox-relative)."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Sandbox-relative file path" }
            },
            "required": ["path"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let path = match string_arg(arguments, "path") {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(call_id, e)),
        };

        match self.sandbox.read_to_string(&path) {
            Ok(content) => Ok(ToolResult::success(
                call_id,
                json!({ "path": path, "content": content }).to_string(),
            )),
            Err(e) => Ok(ToolResult::error(call_id, e.to_string())),
        }
    }
}

pub struct EvalEchoTool;

#[async_trait]
impl Tool for EvalEchoTool {
    fn name(&self) -> &str {
        "eval_echo"
    }

    fn description(&self) -> &str {
        "Echo text back. Always succeeds. Arguments: text."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "text": { "type": "string", "description": "Text to echo" }
            },
            "required": ["text"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        match string_arg(arguments, "text") {
            Ok(text) => Ok(ToolResult::success(
                call_id,
                json!({ "echo": text }).to_string(),
            )),
            Err(e) => Ok(ToolResult::error(call_id, e)),
        }
    }
}

pub struct EvalFailTool;

#[async_trait]
impl Tool for EvalFailTool {
    fn name(&self) -> &str {
        "eval_fail"
    }

    fn description(&self) -> &str {
        "Always fails with the given message, to exercise failure recovery. Arguments: message."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "message": { "type": "string", "description": "Failure message" }
            },
            "required": ["message"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let message = arguments
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("eval_fail was called without a message")
            .to_string();
        Ok(ToolResult::error(call_id, message))
    }
}
