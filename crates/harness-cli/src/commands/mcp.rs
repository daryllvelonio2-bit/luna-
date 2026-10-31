//! `luna mcp list` / `luna mcp tools`: report the MCP surface the current config produces.
//!
//! `list` reads the config only — it never spawns a process, so it is safe to run anywhere.
//! `tools` is the live view: it spawns each enabled server, performs the `initialize`/`tools/list`
//! handshake and prints the namespaced tool names the agent will actually see.

use clap::Subcommand;
use colored::Colorize;
use serde_json::json;

use harness_core::{Config, HarnessError};
use harness_mcp::McpStdioClient;

#[derive(Subcommand, Debug)]
pub enum McpCommand {
    /// List configured MCP servers from the `mcp.servers` block
    List {
        /// One JSON object on stdout instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Spawn each enabled server and list the tools it advertises
    Tools {
        /// One JSON object on stdout instead of a table
        #[arg(long)]
        json: bool,
    },
}

/// `luna mcp`: report what MCP surface the current config produces.
pub async fn handle_mcp_command(cmd: McpCommand, config: &Config) -> Result<(), HarnessError> {
    match cmd {
        McpCommand::List { json } => {
            list_servers(config, json);
            Ok(())
        }
        McpCommand::Tools { json } => list_tools(config, json).await,
    }
}

/// The configured servers, from the config alone. No process is spawned.
fn list_servers(config: &Config, json_out: bool) {
    let mcp = &config.mcp;

    if json_out {
        let servers: Vec<_> = mcp
            .servers
            .iter()
            .map(|server| {
                json!({
                    "name": server.name,
                    "command": server.command,
                    "args": server.args,
                    "enabled": server.enabled,
                    "tools": server.tools,
                })
            })
            .collect();
        let value = json!({
            "enabled": mcp.enabled,
            "startup_timeout_secs": mcp.startup_timeout_secs,
            "call_timeout_secs": mcp.call_timeout_secs,
            "servers": servers,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
        );
        return;
    }

    if mcp.servers.is_empty() {
        println!(
            "no MCP servers configured — add an `mcp.servers` block to the config to enable them"
        );
        return;
    }

    if !mcp.enabled {
        println!(
            "{}",
            "mcp.enabled = false — no server is spawned until it is set to true".yellow()
        );
    }

    println!("{:<16} {:<8} {:<8} COMMAND", "NAME", "ENABLED", "TOOLS");
    for server in &mcp.servers {
        // An empty allowlist means every tool the server advertises.
        let tools = if server.tools.is_empty() {
            "all".to_string()
        } else {
            server.tools.len().to_string()
        };
        let mut command = server.command.clone();
        if !server.args.is_empty() {
            command.push(' ');
            command.push_str(&server.args.join(" "));
        }
        println!(
            "{:<16} {:<8} {:<8} {}",
            server.name,
            if server.enabled { "yes" } else { "no" },
            tools,
            command
        );
    }
}

/// The live tool list: spawn each enabled server and read back its `tools/list`.
async fn list_tools(config: &Config, json_out: bool) -> Result<(), HarnessError> {
    if config.mcp.servers.is_empty() {
        if json_out {
            println!("{}", json!({ "servers": [] }));
        } else {
            println!(
                "no MCP servers configured — add an `mcp.servers` block to the config to enable them"
            );
        }
        return Ok(());
    }

    if !config.mcp.enabled && !json_out {
        println!(
            "{}",
            "mcp.enabled = false — no server is spawned until it is set to true".yellow()
        );
        return Ok(());
    }

    // (server name, advertised tool names namespaced as `mcp__<server>__<tool>`) or the reason the
    // server could not be queried. A single unreachable server must not hide the others.
    let mut report: Vec<(String, Result<Vec<String>, String>)> = Vec::new();

    for server in &config.mcp.servers {
        if !server.enabled {
            continue;
        }
        let args: Vec<&str> = server.args.iter().map(|s| s.as_str()).collect();
        let cwd = server
            .cwd
            .as_ref()
            .map(|dir| std::path::PathBuf::from(harness_core::config::expand_tilde(&dir.to_string_lossy())));
        let outcome = match McpStdioClient::spawn(
            &server.command,
            &args,
            &server.env,
            cwd.as_deref(),
            config.mcp.startup_timeout_secs,
            config.mcp.call_timeout_secs,
        )
        .await
        {
            Ok(client) => client.list_tools().await.map(|tools| {
                let allowed: std::collections::HashSet<&str> =
                    server.tools.iter().map(|s| s.as_str()).collect();
                tools
                    .into_iter()
                    .filter(|t| allowed.is_empty() || allowed.contains(t.name.as_str()))
                    .map(|t| format!("mcp__{}__{}", server.name, t.name))
                    .collect::<Vec<_>>()
            }),
            Err(e) => Err(e),
        };
        report.push((server.name.clone(), outcome.map_err(|e| e.to_string())));
    }

    if json_out {
        let servers: Vec<_> = report
            .iter()
            .map(|(name, outcome)| match outcome {
                Ok(tools) => json!({ "name": name, "tools": tools }),
                Err(error) => json!({ "name": name, "error": error }),
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "servers": servers }))
                .unwrap_or_else(|_| "{}".to_string())
        );
        return Ok(());
    }

    if report.is_empty() {
        println!("no enabled MCP servers to query (all entries have `enabled: false`)");
        return Ok(());
    }

    for (name, outcome) in &report {
        match outcome {
            Ok(tools) if tools.is_empty() => {
                println!("{} {}: no tools advertised", "•".dimmed(), name.cyan());
            }
            Ok(tools) => {
                println!(
                    "{} {} — {} tool(s):",
                    "•".dimmed(),
                    name.cyan().bold(),
                    tools.len()
                );
                for tool in tools {
                    println!("    {}", tool);
                }
            }
            Err(error) => {
                println!("{} {}: {}", "✖".red().bold(), name.cyan(), error);
            }
        }
    }

    Ok(())
}
