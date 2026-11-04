use async_trait::async_trait;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use crate::stdio::McpStdioClient;
use crate::types::McpToolInfo;
use harness_core::{HarnessError, Tool, ToolResult};

/// One tool advertised by an MCP server, presented to the harness as a `Tool`.
///
/// Two names matter and they are not the same string. The registry name is what
/// the model sees and what the harness uses to keep two servers apart
/// (`mcp__<server>__<tool>`); the remote name is what the server itself calls the
/// tool. A `tools/call` must carry the remote name — the server has never heard
/// of the namespace — so the two are kept side by side rather than derived from
/// one another.
pub struct McpToolAdapter {
    registry_name: String,
    info: McpToolInfo,
    client: Arc<McpStdioClient>,
}

impl McpToolAdapter {
    /// Adapter that registers under the tool's own name.
    pub fn new(info: McpToolInfo, client: Arc<McpStdioClient>) -> Self {
        let registry_name = info.name.clone();
        Self {
            registry_name,
            info,
            client,
        }
    }

    /// Adapter registered as `registry_name`, dispatching to `info.name`.
    pub fn namespaced(
        registry_name: impl Into<String>,
        info: McpToolInfo,
        client: Arc<McpStdioClient>,
    ) -> Self {
        Self {
            registry_name: registry_name.into(),
            info,
            client,
        }
    }

    /// The name the MCP server knows this tool by.
    pub fn remote_name(&self) -> &str {
        &self.info.name
    }
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.registry_name
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::stdio::McpStdioClient;

    /// A stub MCP server in POSIX shell: it answers the handshake, advertises one
    /// tool, and logs every request it receives so the test can see exactly which
    /// tool name arrived on the wire.
    const STUB_SERVER: &str = r#"#!/bin/sh
log="$1"
while IFS= read -r line; do
  printf '%s\n' "$line" >> "$log"
  id=`printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p'`
  case "$line" in
    *'"initialize"'*) body='{"protocolVersion":"2024-11-05","capabilities":{},"serverInfo":{"name":"stub","version":"0"}}' ;;
    *'"tools/list"'*) body='{"tools":[{"name":"luna_start","description":"stub","inputSchema":{"type":"object"}}]}' ;;
    *) body='{"content":[{"type":"text","text":"called"}]}' ;;
  esac
  printf '{"jsonrpc":"2.0","id":%s,"result":%s}\n' "$id" "$body"
done
"#;

    struct Stub {
        dir: std::path::PathBuf,
        script: std::path::PathBuf,
        log: std::path::PathBuf,
    }

    impl Stub {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "harness-mcp-adapter-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            let script = dir.join("stub.sh");
            let log = dir.join("requests.log");
            std::fs::write(&script, STUB_SERVER).unwrap();
            std::fs::write(&log, "").unwrap();
            Self { dir, script, log }
        }

        fn script(&self) -> String {
            self.script.to_string_lossy().into_owned()
        }

        fn log_path(&self) -> String {
            self.log.to_string_lossy().into_owned()
        }

        /// `sh <script> <logfile>` — the script takes the log path as `$1` so the
        /// test can read back the exact requests the server received.
        fn argv(&self) -> Vec<String> {
            vec![self.script(), self.log_path()]
        }

        fn requests(&self) -> Vec<String> {
            std::fs::read_to_string(&self.log)
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect()
        }
    }

    impl Drop for Stub {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// The regression this exists for: a tool registered as
    /// `mcp__<server>__<tool>` must still dispatch `<tool>`. Sending the
    /// namespaced name reaches a server that has never heard of it, and every
    /// call fails with "unknown tool".
    #[tokio::test]
    async fn namespaced_adapter_dispatches_the_remote_tool_name() {
        let stub = Stub::new();
        let argv = stub.argv();
        let args: Vec<&str> = argv.iter().map(String::as_str).collect();
        let client = McpStdioClient::spawn(
            "sh",
            &args,
            &std::collections::BTreeMap::new(),
            None,
            10,
            10,
        )
        .await
        .expect("stub server should start");

        let tools = client.list_tools().await.expect("stub should list a tool");
        assert_eq!(tools.len(), 1);
        let info = tools[0].clone();
        assert_eq!(info.name, "luna_start");

        let adapter =
            McpToolAdapter::namespaced("mcp__stub__luna_start".to_string(), info, client.clone());
        assert_eq!(adapter.name(), "mcp__stub__luna_start");
        assert_eq!(adapter.remote_name(), "luna_start");

        let result = adapter
            .execute(
                "call-1",
                &serde_json::json!({ "text": "hi" }),
                &CancellationToken::new(),
            )
            .await
            .expect("execute should not fail at the transport level");
        assert!(
            !result.is_error,
            "stub reported an error: {}",
            result.content
        );

        let requests = stub.requests();
        let call = requests
            .iter()
            .find(|line| line.contains("tools/call"))
            .expect("a tools/call should have reached the server");
        assert!(
            call.contains(r#""name":"luna_start""#),
            "the server must be asked for its own tool name, got: {call}"
        );
        assert!(
            !call.contains("mcp__stub__luna_start"),
            "the namespace is a harness-local concept and must not be sent: {call}"
        );
    }

    /// `new` keeps the tool's own name as the registry name, for callers that
    /// have no namespace to apply.
    #[tokio::test]
    async fn plain_adapter_registers_under_the_tool_name() {
        let stub = Stub::new();
        let argv = stub.argv();
        let args: Vec<&str> = argv.iter().map(String::as_str).collect();
        let client = McpStdioClient::spawn(
            "sh",
            &args,
            &std::collections::BTreeMap::new(),
            None,
            10,
            10,
        )
        .await
        .expect("stub server should start");
        let info = client.list_tools().await.unwrap()[0].clone();
        let adapter = McpToolAdapter::new(info, client);
        assert_eq!(adapter.name(), "luna_start");
        assert_eq!(adapter.remote_name(), "luna_start");
    }
}
