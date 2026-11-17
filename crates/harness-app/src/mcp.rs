//! MCP server registration shared across CLI and GUI.

use std::path::PathBuf;
use std::sync::Arc;

use harness_core::{Config, DefaultToolRegistry, HarnessError, ToolRegistry};
use harness_mcp::{McpStdioClient, McpToolAdapter};

/// Register MCP servers from configuration and add their tools to the registry.
pub async fn register_mcp_servers(
    registry: &Arc<DefaultToolRegistry>,
    config: &Config,
) -> Result<(), HarnessError> {
    if !config.mcp.enabled || config.mcp.servers.is_empty() {
        return Ok(());
    }

    for server_config in &config.mcp.servers {
        if !server_config.enabled {
            continue;
        }

        let args: Vec<&str> = server_config.args.iter().map(|s| s.as_str()).collect();
        let cwd = server_config
            .cwd
            .as_ref()
            .map(|dir| PathBuf::from(harness_core::config::expand_tilde(&dir.to_string_lossy())));
        let client = match McpStdioClient::spawn(
            &server_config.command,
            &args,
            &server_config.env,
            cwd.as_deref(),
            config.mcp.startup_timeout_secs,
            config.mcp.call_timeout_secs,
        )
        .await
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("failed to start MCP server '{}': {}", server_config.name, e);
                continue;
            }
        };

        let tools = match client.list_tools().await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    "failed to list tools from MCP server '{}': {}",
                    server_config.name,
                    e
                );
                continue;
            }
        };

        let allowed: std::collections::HashSet<_> = server_config.tools.iter().cloned().collect();
        let filtered_tools: Vec<_> = tools
            .into_iter()
            .filter(|t| allowed.is_empty() || allowed.contains(&t.name))
            .collect();

        for tool_info in &filtered_tools {
            let namespaced_name = format!("mcp__{}__{}", server_config.name, tool_info.name);
            let adapter = McpToolAdapter::namespaced(
                namespaced_name.clone(),
                tool_info.clone(),
                client.clone(),
            );
            if let Err(e) = registry.register(Arc::new(adapter)).await {
                tracing::warn!("failed to register MCP tool '{}': {}", namespaced_name, e);
            }
        }
    }

    Ok(())
}

/// Spawn and register MCP servers on demand if none are currently registered yet.
pub async fn ensure_mcp_servers_registered(
    registry: &Arc<DefaultToolRegistry>,
    config: &Config,
) -> Result<(), HarnessError> {
    if config.mcp.servers.is_empty() {
        return Ok(());
    }
    let has_mcp_tools = registry
        .list_tools()
        .await?
        .iter()
        .any(|tool| tool.name.starts_with("mcp__"));
    if has_mcp_tools {
        return Ok(());
    }
    let mut forced = config.clone();
    forced.mcp.enabled = true;
    register_mcp_servers(registry, &forced).await
}
