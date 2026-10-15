use std::collections::HashMap;
use std::sync::Arc;
use async_trait::async_trait;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::approval::ToolPreview;
use crate::error::HarnessError;
use crate::traits::{Tool, ToolRegistry};
use crate::types::{ToolCall, ToolDefinition, ToolResult};

#[derive(Default)]
pub struct DefaultToolRegistry {
    tools: RwLock<HashMap<String, Arc<dyn Tool>>>,
}

impl DefaultToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
        }
    }
}

#[async_trait]
impl ToolRegistry for DefaultToolRegistry {
    async fn register(&self, tool: Arc<dyn Tool>) -> Result<(), HarnessError> {
        let mut map = self.tools.write().await;
        map.insert(tool.name().to_string(), tool);
        Ok(())
    }

    async fn list_tools(&self) -> Result<Vec<ToolDefinition>, HarnessError> {
        let map = self.tools.read().await;
        Ok(map.values().map(|t| t.definition()).collect())
    }

    /// An unknown tool is an error rather than `false`: the gate fails closed on it, and
    /// `execute` will report `ToolNotFound` a moment later anyway.
    async fn is_mutating(&self, name: &str) -> Result<bool, HarnessError> {
        let map = self.tools.read().await;
        match map.get(name) {
            Some(tool) => Ok(tool.is_mutating()),
            None => Err(HarnessError::ToolNotFound(name.to_string())),
        }
    }

    async fn preview(&self, call: &ToolCall) -> Option<ToolPreview> {
        let tool = {
            let map = self.tools.read().await;
            map.get(&call.name).cloned()
        };
        match tool {
            Some(tool) => tool.preview(&call.arguments).await,
            None => None,
        }
    }

    async fn execute(
        &self,
        call: &ToolCall,
        cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let tool = {
            let map = self.tools.read().await;
            map.get(&call.name).cloned()
        };

        match tool {
            Some(t) => t.execute(&call.id, &call.arguments, cancel_token).await,
            None => Err(HarnessError::ToolNotFound(call.name.clone())),
        }
    }
}
