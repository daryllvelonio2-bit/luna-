use std::sync::Arc;
use async_trait::async_trait;
use serde_json::json;
use tokio_util::sync::CancellationToken;

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

pub struct ProposePatchTool {
    engine: Arc<dyn EvolverEngine>,
}

impl ProposePatchTool {
    pub fn new(engine: Arc<dyn EvolverEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for ProposePatchTool {
    fn name(&self) -> &str {
        "propose_harness_patch"
    }

    fn description(&self) -> &str {
        "Propose a code patch to the harness codebase. Stages the patch and returns a patch ID."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "description": { "type": "string", "description": "Clear explanation of the fix or feature being introduced" },
                "target_file": { "type": "string", "description": "Relative file path being modified (e.g. 'crates/harness-core/src/agent.rs')" },
                "content": { "type": "string", "description": "New complete file content" }
            },
            "required": ["description", "target_file", "content"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let desc = args.get("description").and_then(|d| d.as_str()).unwrap_or("");
        let file = args.get("target_file").and_then(|f| f.as_str()).unwrap_or("");
        let content = args.get("content").and_then(|c| c.as_str()).unwrap_or("");

        let payload = json!({
            "file": file,
            "content": content,
        })
        .to_string();

        match self.engine.propose_patch(desc, &payload).await {
            Ok(patch_id) => Ok(ToolResult::success(
                call_id,
                json!({
                    "status": "staged",
                    "patch_id": patch_id,
                    "target_file": file,
                    "next_step": "Call apply_harness_update with this patch_id to verify in build sandbox and apply."
                })
                .to_string(),
            )),
            Err(e) => Ok(ToolResult::error(call_id, e.to_string())),
        }
    }
}

pub struct ApplyUpdateTool {
    engine: Arc<dyn EvolverEngine>,
}

impl ApplyUpdateTool {
    pub fn new(engine: Arc<dyn EvolverEngine>) -> Self {
        Self { engine }
    }
}

#[async_trait]
impl Tool for ApplyUpdateTool {
    fn name(&self) -> &str {
        "apply_harness_update"
    }

    fn description(&self) -> &str {
        "Verify a proposed patch in an isolated build sandbox (`cargo test`) and apply it to the working repository."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "patch_id": { "type": "string", "description": "ID of staged patch proposal" }
            },
            "required": ["patch_id"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let patch_id = args.get("patch_id").and_then(|p| p.as_str()).unwrap_or("");

        // 1. Run sandbox verification first
        match self.engine.test_patch_sandbox(patch_id).await {
            Ok(_) => {
                // 2. Apply patch
                match self.engine.apply_patch(patch_id).await {
                    Ok(_) => Ok(ToolResult::success(
                        call_id,
                        json!({
                            "status": "applied",
                            "patch_id": patch_id,
                            "message": "Patch verified in sandbox and applied successfully. Ready for supervisor restart."
                        })
                        .to_string(),
                    )),
                    Err(e) => Ok(ToolResult::error(call_id, format!("Failed to apply patch: {}", e))),
                }
            }
            Err(e) => Ok(ToolResult::error(
                call_id,
                format!("Sandbox verification failed. Patch not applied: {}", e),
            )),
        }
    }
}
