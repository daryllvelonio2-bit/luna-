use std::collections::HashMap;
use std::sync::Arc;
use async_trait::async_trait;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::approval::ToolPreview;
use crate::error::HarnessError;
use crate::traits::{Tool, ToolMiddleware, ToolRegistry};
use crate::types::{ToolCall, ToolDefinition, ToolResult};

#[derive(Default)]
pub struct DefaultToolRegistry {
    tools: RwLock<HashMap<String, Arc<dyn Tool>>>,
    middlewares: RwLock<Vec<Arc<dyn ToolMiddleware>>>,
}

impl DefaultToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
            middlewares: RwLock::new(Vec::new()),
        }
    }

    /// Register a middleware hook into the tool execution loop
    pub async fn register_middleware(&self, middleware: Arc<dyn ToolMiddleware>) {
        let mut list = self.middlewares.write().await;
        list.push(middleware);
    }

    /// Retrieve a registered tool by name
    pub async fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        let map = self.tools.read().await;
        map.get(name).cloned()
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

    async fn is_mutating_call(&self, call: &ToolCall) -> Result<bool, HarnessError> {
        let map = self.tools.read().await;
        match map.get(&call.name) {
            Some(tool) => Ok(tool.is_mutating_call(&call.arguments)),
            None => Err(HarnessError::ToolNotFound(call.name.clone())),
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
        let (tool, mws) = {
            let map = self.tools.read().await;
            let mws = self.middlewares.read().await.clone();
            (map.get(&call.name).cloned(), mws)
        };

        let mut active_call = call.clone();
        for mw in &mws {
            mw.on_pre_execute(&mut active_call).await?;
        }

        let mut result = match tool {
            Some(t) => t.execute(&active_call.id, &active_call.arguments, cancel_token).await?,
            None => return Err(HarnessError::ToolNotFound(call.name.clone())),
        };

        for mw in &mws {
            mw.on_post_execute(&active_call, &mut result).await?;
        }

        Ok(result)
    }
}
