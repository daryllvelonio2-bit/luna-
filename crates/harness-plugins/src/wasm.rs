use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use harness_core::{HarnessError, Tool, ToolResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WasmPluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    pub entrypoint: String,
    pub max_memory_mb: u32,
}

pub struct SandboxedPlugin {
    manifest: WasmPluginManifest,
    #[allow(dead_code)]
    wasm_bytes: Vec<u8>,
}

impl SandboxedPlugin {
    pub fn new(manifest: WasmPluginManifest, wasm_bytes: Vec<u8>) -> Self {
        Self {
            manifest,
            wasm_bytes,
        }
    }
}

#[async_trait]
impl Tool for SandboxedPlugin {
    fn name(&self) -> &str {
        &self.manifest.name
    }

    fn description(&self) -> &str {
        &self.manifest.description
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "payload": { "type": "string", "description": "JSON payload to pass to Wasm plugin" }
            },
            "required": ["payload"]
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        _args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        Ok(ToolResult::error(
            call_id,
            format!(
                "Wasm plugin '{}' execution failed: WebAssembly runtime is unsupported and not configured",
                self.manifest.name
            ),
        ))
    }
}
