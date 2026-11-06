use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use crate::approval::ToolPreview;
use crate::error::HarnessError;
use crate::traits::{Tool, ToolMiddleware, ToolProgress, ToolRegistry};
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

    async fn unregister(&self, name: &str) -> Result<bool, HarnessError> {
        let mut map = self.tools.write().await;
        Ok(map.remove(name).is_some())
    }

    async fn list_tools(&self) -> Result<Vec<ToolDefinition>, HarnessError> {
        let map = self.tools.read().await;
        let mut definitions: Vec<ToolDefinition> = map.values().map(|t| t.definition()).collect();
        // Sorted by name, because a `HashMap` iterates in an arbitrary order and the tool list is
        // the head of every request. Two turns that differ only in this order are two different
        // prompts, and a prompt cache keys on the bytes: an unstable prefix is a cache miss that
        // costs the full input price on every turn of a session.
        definitions.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(definitions)
    }

    /// An unknown tool is an error rather than `false`: the gate fails closed on it, and
    /// `execute` will report `ToolNotFound` a moment later anyway.
    async fn is_mutating(&self, name: &str) -> Result<bool, HarnessError> {
        let map = self.tools.read().await;
        let resolved_name = match name {
            "bash" if !map.contains_key("bash") => "terminal",
            "read" if !map.contains_key("read") => "read_file",
            other => other,
        };
        match map.get(resolved_name) {
            Some(tool) => Ok(tool.is_mutating()),
            None => Err(HarnessError::ToolNotFound(name.to_string())),
        }
    }

    async fn is_mutating_call(&self, call: &ToolCall) -> Result<bool, HarnessError> {
        let map = self.tools.read().await;
        let resolved_name = match call.name.as_str() {
            "bash" if !map.contains_key("bash") => "terminal",
            "read" if !map.contains_key("read") => "read_file",
            other => other,
        };
        match map.get(resolved_name) {
            Some(tool) => Ok(tool.is_mutating_call(&call.arguments)),
            None => Err(HarnessError::ToolNotFound(call.name.clone())),
        }
    }

    async fn preview(&self, call: &ToolCall) -> Option<ToolPreview> {
        let tool = {
            let map = self.tools.read().await;
            map.get(&call.name).cloned().or_else(|| {
                if call.name == "bash" {
                    map.get("terminal").cloned()
                } else if call.name == "read" {
                    map.get("read_file").cloned()
                } else {
                    None
                }
            })
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
        self.execute_with_progress(call, cancel_token, None).await
    }

    async fn execute_with_progress(
        &self,
        call: &ToolCall,
        cancel_token: &CancellationToken,
        progress: Option<&ToolProgress>,
    ) -> Result<ToolResult, HarnessError> {
        let (tool, mws) = {
            let map = self.tools.read().await;
            let mws = self.middlewares.read().await.clone();
            let t = map.get(&call.name).cloned().or_else(|| {
                if call.name == "bash" {
                    map.get("terminal").cloned()
                } else if call.name == "read" {
                    map.get("read_file").cloned()
                } else {
                    None
                }
            });
            (t, mws)
        };

        let mut active_call = call.clone();
        if active_call.name == "read" {
            if let Some(fp) = active_call.arguments.get("filePath").cloned() {
                active_call.arguments["path"] = fp;
            }
        }
        for mw in &mws {
            mw.on_pre_execute(&mut active_call).await?;
        }

        let mut result = match tool {
            Some(t) => {
                t.execute_with_progress(
                    &active_call.id,
                    &active_call.arguments,
                    cancel_token,
                    progress,
                )
                .await?
            }
            None => return Err(HarnessError::ToolNotFound(call.name.clone())),
        };

        for mw in &mws {
            mw.on_post_execute(&active_call, &mut result).await?;
        }

        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NamedTool(&'static str);

    #[async_trait]
    impl Tool for NamedTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "a tool with a name"
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }
        async fn execute(
            &self,
            call_id: &str,
            _arguments: &serde_json::Value,
            _cancel_token: &CancellationToken,
        ) -> Result<ToolResult, HarnessError> {
            Ok(ToolResult::success(call_id, "ok"))
        }
    }

    /// Registration order is not request order: the tool list must come out the same way every
    /// turn, whatever order the tools were registered in, or no prompt cache can ever hit it.
    #[tokio::test]
    async fn tool_definitions_are_listed_in_a_stable_order() {
        let registry = DefaultToolRegistry::new();
        for name in ["write_file", "read_file", "search_files"] {
            registry
                .register(Arc::new(NamedTool(name)))
                .await
                .expect("register");
        }

        let names = |definitions: Vec<crate::types::ToolDefinition>| -> Vec<String> {
            definitions
                .into_iter()
                .map(|definition| definition.name)
                .collect()
        };

        let listed = names(registry.list_tools().await.expect("list"));
        assert_eq!(listed, vec!["read_file", "search_files", "write_file"]);

        // Listed the same way twice: nothing about the iteration order leaks into the request.
        assert_eq!(listed, names(registry.list_tools().await.expect("list")));
    }
}
