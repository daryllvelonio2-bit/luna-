use std::sync::Arc;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use harness_core::{HarnessError, Tool, ToolResult};
use crate::stdio::McpStdioClient;
use crate::types::McpToolInfo;

pub struct McpToolAdapter {
    info: McpToolInfo,
    client: Arc<McpStdioClient>,
}

impl McpToolAdapter {
    pub fn new(info: McpToolInfo, client: Arc<McpStdioClient>) -> Self {
        Self { info, client }
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.info.name
    }

    fn description(&self) -> &str {
        self.info.description.as_deref().unwrap_or("MCP tool")
    }

    fn parameters(&self) -> serde_json::Value {
        self.info.input_schema.clone()
    }

    async fn execute(
        &self,
        call_id: &str,
        arguments: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        match self.client.call_tool(&self.info.name, arguments).await {
            Ok(output) => Ok(ToolResult::success(call_id, output)),
            Err(e) => Ok(ToolResult::error(call_id, e.to_string())),
        }
    }
}
