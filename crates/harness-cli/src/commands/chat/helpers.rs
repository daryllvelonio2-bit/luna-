//! Small pieces of the chat surface that the setup and REPL phases share.
//!
//! They live here rather than beside the REPL so `mod.rs` stays a thin facade and the
//! interactive loop keeps to its own concerns.

use std::path::PathBuf;
use std::sync::Arc;

use harness_core::{Config, DefaultToolRegistry, HarnessError, ToolRegistry};
use harness_mcp::{McpStdioClient, McpToolAdapter};

use super::keys::AppKey;
use super::render::{render_transcript, TurnRecord};
use super::ui::{get_terminal_width, ChatUi};

/// The idle memory-pass timer.
///
/// The background saver counts completed turns, so a short session can end before the interval
/// ever fires. This notices a finished conversation that has gone quiet and asks for one pass.
/// It is a tiny state machine rather than three locals in the event loop so the reset rules —
/// a pass per transcript, no pass while busy, no pass before anything was said — are testable.
pub(crate) struct IdleMemoryPass {
    idle_since: std::time::Instant,
    last_pass_turns: usize,
    was_active: bool,
}

impl IdleMemoryPass {
    pub(super) fn new(turns: usize) -> Self {
        Self {
            idle_since: std::time::Instant::now(),
            last_pass_turns: turns,
            was_active: false,
        }
    }

    /// Record that an interval pass already covered `turns`, so the idle timer does not run a
    /// duplicate pass for the same transcript slice.
    pub(super) fn mark_passed(&mut self, turns: usize) {
        self.last_pass_turns = turns;
        self.idle_since = std::time::Instant::now();
    }

    /// Advance one tick. `busy` means a turn, a compaction, or a queued message is in progress.
    /// Returns `true` at most once per new transcript, after it has been quiet for `idle_secs`.
    pub(super) fn tick(
        &mut self,
        turns: usize,
        busy: bool,
        idle_secs: u64,
        now: std::time::Instant,
    ) -> bool {
        if busy {
            self.was_active = true;
            self.idle_since = now;
            return false;
        }
        if self.was_active {
            // The turn that was in flight has landed, so the quiet clock starts now rather than
            // at the last keystroke.
            self.was_active = false;
            self.idle_since = now;
            return false;
        }
        if turns < self.last_pass_turns {
            // A cleared or compacted transcript must not strand the timer: without this the
            // `>` test below could never be true again.
            self.last_pass_turns = turns;
            self.idle_since = now;
            return false;
        }
        if turns > self.last_pass_turns
            && idle_secs > 0
            && now.duration_since(self.idle_since) >= std::time::Duration::from_secs(idle_secs)
        {
            self.last_pass_turns = turns;
            self.idle_since = now;
            return true;
        }
        false
    }
}

/// Draw the transcript.
///
/// The todo panel is not drawn here: it is chrome, drawn above the composer by
/// [`super::ui::ChatUi`], so it stays put while the conversation scrolls under it.
pub(crate) fn redraw_transcript(turns: &[TurnRecord], expanded: bool) {
    render_transcript(turns, expanded, get_terminal_width());
}

/// Keep a slash command's output in the transcript, then show it the way Ctrl+O would.
///
/// `execute_slash_command` prints its answer above the composer, which is enough until the screen
/// is redrawn: those lines were never part of `turns`, so a Ctrl+O toggle or a later turn erased
/// them. The capture turns them into a normal exchange — the typed command on the user's side,
/// the printed text as Luna's reply — and an idle screen is redrawn immediately so the command
/// and its output sit together instead of arriving as loose scrollback.
pub(crate) fn finish_slash_command(
    ui: &ChatUi,
    turns: &mut Vec<TurnRecord>,
    command: &str,
    process_expanded: bool,
    redraw: bool,
) {
    let Some(lines) = ui.end_command_capture() else {
        return;
    };
    let reply = lines.join("\n");
    if reply.trim().is_empty() {
        return;
    }
    turns.push(TurnRecord {
        user_query: command.trim().to_string(),
        reasoning: None,
        tools: Vec::new(),
        tool_items: Vec::new(),
        duration: std::time::Duration::ZERO,
        reply,
    });
    if redraw && ui.is_attended && !ui.is_tui_mode() {
        crate::commands::clear_screen();
        redraw_transcript(turns, process_expanded);
        ui.draw_initial_box();
    }
}

/// Register MCP servers from configuration and add their tools to the registry.
pub(crate) async fn register_mcp_servers(
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

        // Spawn the MCP stdio server
        let args: Vec<&str> = server_config.args.iter().map(|s| s.as_str()).collect();
        // `~` is expanded here rather than in the config type: the config keeps
        // what the user wrote, and only the spawn needs a real path.
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
                eprintln!(
                    "warning: failed to start MCP server '{}': {}",
                    server_config.name, e
                );
                continue;
            }
        };

        // List tools from the server
        let tools = match client.list_tools().await {
            Ok(t) => t,
            Err(e) => {
                eprintln!(
                    "warning: failed to list tools from MCP server '{}': {}",
                    server_config.name, e
                );
                continue;
            }
        };

        // Filter tools if an allowlist is configured
        let allowed: std::collections::HashSet<_> = server_config.tools.iter().cloned().collect();
        let filtered_tools: Vec<_> = tools
            .into_iter()
            .filter(|t| allowed.is_empty() || allowed.contains(&t.name))
            .collect();

        // Register each tool under the namespace, dispatching to the server's own
        // name: the namespace is local to this process and means nothing to the
        // server, which would reject `mcp__<server>__<tool>` as an unknown tool.
        for tool_info in &filtered_tools {
            let namespaced_name = format!("mcp__{}__{}", server_config.name, tool_info.name);
            let adapter = McpToolAdapter::namespaced(
                namespaced_name.clone(),
                tool_info.clone(),
                client.clone(),
            );
            if let Err(e) = registry.register(Arc::new(adapter)).await {
                eprintln!(
                    "warning: failed to register MCP tool '{}': {}",
                    namespaced_name, e
                );
            }
        }

        if !server_config.tools.is_empty() {
            eprintln!(
                "info: MCP server '{}' registered {} tool(s)",
                server_config.name,
                filtered_tools.len()
            );
        }
    }

    Ok(())
}

/// Whether a keypress can be handled without moving the mark.
///
/// Typing, deleting and moving the caret only rewrite the prompt row, which the landing repaint
/// already accounts for. Everything else can open a picker, clear the screen or start a turn, and
/// those move the rows the shower rewrites, so the mark settles before the key is handled.
pub(crate) fn key_keeps_landing(key: &AppKey, prompt_empty: bool) -> bool {
    match key {
        // `/` on an empty prompt opens the command menu, which owns the screen.
        AppKey::Char('/') => !prompt_empty,
        AppKey::Char(_)
        | AppKey::Backspace
        | AppKey::Delete
        | AppKey::Left
        | AppKey::Right
        | AppKey::Home
        | AppKey::End
        | AppKey::Up
        | AppKey::Down => true,
        _ => false,
    }
}
