use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;
use tracing::{debug, info};

use harness_core::HarnessError;
use crate::types::{JsonRpcRequest, JsonRpcResponse, McpToolInfo};

pub struct McpStdioClient {
    command: String,
    stdin: Mutex<ChildStdin>,
    reader: Mutex<BufReader<ChildStdout>>,
    next_id: AtomicU64,
}

impl McpStdioClient {
    pub async fn spawn(command: &str, args: &[&str]) -> Result<Arc<Self>, HarnessError> {
        info!("Spawning MCP stdio server: '{}' with args {:?}", command, args);

        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| HarnessError::Io(format!("Failed to spawn MCP server '{}': {}", command, e)))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| HarnessError::Io("Failed to capture MCP stdin".to_string()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| HarnessError::Io("Failed to capture MCP stdout".to_string()))?;

        let client = Arc::new(Self {
            command: command.to_string(),
            stdin: Mutex::new(stdin),
            reader: Mutex::new(BufReader::new(stdout)),
            next_id: AtomicU64::new(1),
        });

        client.initialize().await?;
        Ok(client)
    }

    async fn send_request(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, HarnessError> {
        let req_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let req = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: req_id,
            method: method.to_string(),
            params,
        };

        let mut line = serde_json::to_string(&req)
            .map_err(|e| HarnessError::SerializationError(e.to_string()))?;
        line.push('\n');

        {
            let mut stdin = self.stdin.lock().await;
            stdin
                .write_all(line.as_bytes())
                .await
                .map_err(|e| HarnessError::Io(e.to_string()))?;
            stdin.flush().await.map_err(|e| HarnessError::Io(e.to_string()))?;
        }

        let mut response_line = String::new();
        {
            let mut reader = self.reader.lock().await;
            reader
                .read_line(&mut response_line)
                .await
                .map_err(|e| HarnessError::Io(e.to_string()))?;
        }

        let resp: JsonRpcResponse = serde_json::from_str(&response_line)
            .map_err(|e| HarnessError::SerializationError(format!("Failed to parse response '{}': {}", response_line, e)))?;

        if let Some(err) = resp.error {
            return Err(HarnessError::ToolExecutionFailed {
                tool: method.to_string(),
                reason: format!("MCP error {}: {}", err.code, err.message),
            });
        }

        Ok(resp.result.unwrap_or(serde_json::Value::Null))
    }

    pub async fn initialize(&self) -> Result<(), HarnessError> {
        let init_params = json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {
                "tools": {}
            },
            "clientInfo": {
                "name": "luna-agent-harness",
                "version": "0.1.0"
            }
        });

        debug!("Sending MCP initialize handshake");
        self.send_request("initialize", Some(init_params)).await?;
        Ok(())
    }

    pub async fn list_tools(&self) -> Result<Vec<McpToolInfo>, HarnessError> {
        let res = self.send_request("tools/list", None).await?;
        let tools_val = res.get("tools").and_then(|t| t.as_array()).ok_or_else(|| {
            HarnessError::SerializationError("Invalid tools/list result".to_string())
        })?;

        let mut tools = Vec::new();
        for t in tools_val {
            let tool: McpToolInfo = serde_json::from_value(t.clone())
                .map_err(|e| HarnessError::SerializationError(e.to_string()))?;
            tools.push(tool);
        }
        Ok(tools)
    }

    pub async fn call_tool(
        &self,
        name: &str,
        arguments: &serde_json::Value,
    ) -> Result<String, HarnessError> {
        let params = json!({
            "name": name,
            "arguments": arguments,
        });

        let res = self.send_request("tools/call", Some(params)).await?;

        // Extract content array or text
        if let Some(content) = res.get("content").and_then(|c| c.as_array()) {
            let mut parts = Vec::new();
            for item in content {
                if let Some(text) = item.get("text").and_then(|t| t.as_str()) {
                    parts.push(text);
                }
            }
            Ok(parts.join("\n"))
        } else {
            Ok(serde_json::to_string_pretty(&res).unwrap_or_default())
        }
    }
}
