//! The slash-command surface: the table the picker lists, the dispatcher, and the handlers.
//!
//! This file is the facade. It owns the two things that have to agree with each other — the
//! `SLASH_COMMANDS` table and the `match` that dispatches it — and re-exports the handlers, which
//! live in submodules grouped by what they act on: `session`, `model`, `context`, `value_prompt`.

use colored::Colorize;
use std::sync::Arc;
use tokio::sync::mpsc;

use harness_core::{
    Config, HarnessError, Message, ReasoningEffort, Role, SessionBinding, SessionStore,
    SqliteSessionStore,
};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};

use super::clear_on_exit;
use super::keys::AppKey;
use super::render::{parse_turns_from_history, render_transcript, TurnRecord};
use super::todo;
use super::ui::{get_terminal_width, ChatUi};
use super::ChatAgent;
use crate::commands::chat::modal::{paint_centred, ModalScreen};
use crate::commands::context_window::{context_window, context_window_for};
use crate::commands::sessions::{find_by_name, resolve_session, SessionSelection};

mod picker;
use picker::pick_provider_and_model;
pub(crate) use picker::{input_from_rx, select_from_rx, select_slash_command};
mod usage;
pub(crate) use usage::handle_usage_command;

mod context;
mod dir;
mod model;
mod session;
mod value_prompt;

pub(crate) use context::{
    handle_auto_compact_command, handle_compact_command, handle_diff_command,
};
pub(crate) use dir::handle_dir_command;
pub(crate) use model::{
    handle_compact_model_switch, handle_model_switch, handle_reasoning_switch,
    handle_thinking_switch,
};
pub(crate) use session::{
    handle_delete_command, handle_fork_command, handle_name_command, handle_new_command,
    handle_rewind_command, handle_sessions_switch, handle_summarize_and_new_command,
};

pub struct SlashCommand {
    pub name: &'static str,
    pub details: &'static str,
}

/// Draw the transcript.
///
/// The todo panel is not part of it: the panel is chrome, drawn above the composer.
pub(super) fn draw_transcript(turns: &[TurnRecord], expanded: bool) {
    render_transcript(turns, expanded, get_terminal_width());
}

pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/model",
        details: "Switch provider and model",
    },
    SlashCommand {
        name: "/compact-model",
        details: "Choose provider and model for context compaction",
    },
    SlashCommand {
        name: "/thinking",
        details: "Set thinking level (off, low, medium, high)",
    },
    SlashCommand {
        name: "/think",
        details: "Alias for /thinking",
    },
    SlashCommand {
        name: "/reasoning",
        details: "Set reasoning effort (alias: /thinking)",
    },
    SlashCommand {
        name: "/name",
        details: "Set or view session name",
    },
    SlashCommand {
        name: "/sessions",
        details: "Interactive session picker to switch chat sessions (alias: /session)",
    },
    SlashCommand {
        name: "/new",
        details: "Start a fresh chat session",
    },
    SlashCommand {
        name: "/summarize",
        details: "Summarize and start a new session with it",
    },
    SlashCommand {
        name: "/rewind",
        details: "List turn checkpoints, or restore the files of one (/rewind 3)",
    },
    SlashCommand {
        name: "/diff",
        details: "Review what a turn changed on disk (/diff, /diff 3, /diff --all)",
    },
    SlashCommand {
        name: "/compact",
        details: "Fold oldest turns into a summary in this session",
    },
    SlashCommand {
        name: "/auto-compact",
        details: "Set auto-compaction in a box: threshold, then turns (on|off to toggle)",
    },
    SlashCommand {
        name: "/expand",
        details: "Expand intermediate tool and reasoning processes (or Ctrl+O)",
    },
    SlashCommand {
        name: "/collapse",
        details: "Collapse intermediate tool and reasoning processes (or Ctrl+O)",
    },
    SlashCommand {
        name: "/skills",
        details: "List available specialized skills",
    },
    SlashCommand {
        name: "/mcp",
        details: "List configured MCP servers and tools (/mcp, /mcp tools)",
    },
    SlashCommand {
        name: "/plan",
        details: "Toggle read-only plan mode (or Tab key)",
    },
    SlashCommand {
        name: "/fork",
        details: "Fork current session into a new branch session",
    },
    SlashCommand {
        name: "/tasks",
        details: "Inspect running background tasks and delegated subagents (Ctrl+B, or /jobs)",
    },
    SlashCommand {
        name: "/jobs",
        details: "Inspect running background tasks and delegated subagents (Ctrl+B, or /tasks)",
    },
    SlashCommand {
        name: "/subagents",
        details: "Inspect running background tasks and delegated subagents (Ctrl+B, or /tasks)",
    },
    SlashCommand {
        name: "/usage",
        details: "Show tokens, cache, cost and context for this session",
    },
    SlashCommand {
        name: "/todo",
        details: "Fold or unfold the todo list (Ctrl+T)",
    },
    SlashCommand {
        name: "/clear",
        details: "Clear terminal screen",
    },
    SlashCommand {
        name: "/steer",
        details: "Insert guidance that lands right after the next model call (/steer <text>)",
    },
    SlashCommand {
        name: "/queue",
        details: "Hold a message and send it when the turn in flight finishes (/queue <text>)",
    },
    SlashCommand {
        name: "/interrupt",
        details: "Abort the model request in flight and give the prompt back",
    },
    SlashCommand {
        name: "/heartbeat",
        details: "Show harness liveness, system health, and background status (or /hb)",
    },
    SlashCommand {
        name: "/hb",
        details: "Alias for /heartbeat",
    },
    SlashCommand {
        name: "/connect",
        details: "Add providers and API keys",
    },
    SlashCommand {
        name: "/dir",
        details: "Inspect, change, or reset workspace directory (/dir reset to restore default)",
    },
    SlashCommand {
        name: "/workspace",
        details: "Inspect, change, or reset workspace directory (alias: /dir)",
    },
    SlashCommand {
        name: "/delete",
        details: "Delete current session and start a fresh one",
    },
    SlashCommand {
        name: "/exit",
        details: "Exit Luna chat session",
    },
];

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_slash_command(
    cmd: &str,
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
    config: &mut Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    store: &Arc<SqliteSessionStore>,
    session_id: &mut String,
    current_provider: &mut String,
    current_model: &mut String,
    compact_provider: &mut Option<String>,
    compact_model: &mut Option<String>,
    turns: &mut Vec<TurnRecord>,
    process_expanded: &mut bool,
    todo_scope: &harness_core::tools::TodoScope,
    // The rendered skill list (`None` when none are installed); `/skills` prints it or explains
    // where skills would go, instead of falling through the match and saying nothing.
    skills_readout: Option<&str>,
) -> Result<bool, HarnessError> {
    let parts: Vec<&str> = cmd.trim().splitn(2, ' ').collect();
    let name = parts[0];
    let arg = parts
        .get(1)
        .copied()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    match name {
        "/model" | "/models" | "/provider" => {
            ui.redraw_input_line();
            if let Err(e) = handle_model_switch(
                Some(key_rx),
                ui,
                config,
                cred_manager,
                discovery,
                agent,
                store,
                session_id,
                current_provider,
                current_model,
                compact_model,
            )
            .await
            {
                ui.emit_above(&format!(
                    "{} Failed to switch model: {}",
                    "✖".red().bold(),
                    e
                ));
            }
            Ok(false)
        }
        "/compact-model" => {
            ui.redraw_input_line();
            if let Err(e) = handle_compact_model_switch(
                Some(key_rx),
                ui,
                config,
                cred_manager,
                discovery,
                agent,
                store.as_ref(),
                session_id,
                current_provider,
                current_model,
                compact_provider,
                compact_model,
                arg,
            )
            .await
            {
                ui.emit_above(&format!(
                    "{} Failed to set compaction model: {}",
                    "✖".red().bold(),
                    e
                ));
            }
            Ok(false)
        }
        "/reasoning" => {
            ui.redraw_input_line();
            if let Err(e) = handle_reasoning_switch(Some(key_rx), ui, agent, arg).await {
                ui.emit_above(&format!(
                    "{} Failed to set reasoning effort: {}",
                    "✖".red().bold(),
                    e
                ));
            }
            Ok(false)
        }
        "/thinking" | "/think" => {
            ui.redraw_input_line();
            if let Err(e) = handle_thinking_switch(Some(key_rx), ui, agent, arg).await {
                ui.emit_above(&format!(
                    "{} Failed to set thinking level: {}",
                    "✖".red().bold(),
                    e
                ));
            }
            Ok(false)
        }
        "/connect" => {
            ui.redraw_input_line();
            // handle_connect_command restores the composer on every exit path; drawing it again
            // here would print a second box below the first.
            if let Err(e) = super::connect::handle_connect_command(key_rx, ui, cred_manager).await {
                ui.emit_above(&format!(
                    "{} Failed to open provider setup: {}",
                    "✖".red().bold(),
                    e
                ));
            }
            super::refresh_antigravity_status_if_needed(
                ui,
                current_provider,
                current_model,
                cred_manager,
            )
            .await;
            Ok(false)
        }
        "/name" => {
            if let Some(a) = arg {
                if let Err(e) = handle_name_command(ui, store.as_ref(), session_id, Some(a)).await {
                    ui.emit_above(&format!(
                        "{} Failed to set session name: {}",
                        "✖".red().bold(),
                        e
                    ));
                }
            } else {
                if let Ok(mut buf) = ui.input_buf.lock() {
                    buf.clear();
                    buf.push_str("/name ");
                }
                ui.redraw_input_line();
            }
            Ok(false)
        }
        "/dir" | "/workspace" => {
            if let Err(e) = handle_dir_command(key_rx, ui, config, agent, arg).await {
                ui.emit_above(&format!(
                    "{} Failed to switch workspace: {}",
                    "✖".red().bold(),
                    e
                ));
            }
            Ok(false)
        }
        "/sessions" | "/session" => {
            ui.redraw_input_line();
            if let Err(e) = handle_sessions_switch(
                Some(key_rx),
                ui,
                config,
                cred_manager,
                discovery,
                agent,
                store,
                session_id,
                current_provider,
                current_model,
                compact_provider,
                compact_model,
                turns,
                process_expanded,
                todo_scope,
                arg,
            )
            .await
            {
                ui.emit_above(&format!(
                    "{} Failed to switch session: {}",
                    "✖".red().bold(),
                    e
                ));
            }
            Ok(false)
        }
        "/summarize" => {
            turns.clear();
            ui.clear_box_on_exit();
            if let Err(e) = handle_summarize_and_new_command(
                ui,
                store,
                config,
                agent,
                session_id,
                current_provider,
                current_model,
                todo_scope,
                arg,
            )
            .await
            {
                ui.draw_initial_box();
                ui.emit_above(&format!(
                    "{} Failed to summarize session: {}",
                    "✖".red().bold(),
                    e
                ));
            }
            Ok(false)
        }
        "/rewind" => {
            if let Err(e) = handle_rewind_command(ui, agent, session_id, arg).await {
                ui.emit_above(&format!("{} Failed to rewind: {}", "✖".red().bold(), e));
            }
            Ok(false)
        }
        "/diff" => {
            if let Err(e) = handle_diff_command(ui, agent, session_id, arg).await {
                ui.emit_above(&format!("{} Failed to diff: {}", "✖".red().bold(), e));
            }
            Ok(false)
        }
        "/compact" => {
            if let Err(e) =
                handle_compact_command(ui, config, agent, current_provider, current_model).await
            {
                ui.emit_above(&format!(
                    "{} Failed to compact session: {}",
                    "✖".red().bold(),
                    e
                ));
            }
            Ok(false)
        }
        "/auto-compact" => {
            // The wizard reads the composer's own keys, so it is handed the channel the menu and
            // this dispatch already drained.
            if let Err(e) = handle_auto_compact_command(ui, agent, arg, Some(key_rx)).await {
                ui.emit_above(&format!(
                    "{} Failed to update auto-compact: {}",
                    "✖".red().bold(),
                    e
                ));
            }
            Ok(false)
        }
        "/new" => {
            turns.clear();
            let (is_summarize, remaining_arg) = if let Some(a) = arg {
                if a.starts_with("-s ") || a == "-s" {
                    (true, a.strip_prefix("-s").unwrap().trim())
                } else if a.starts_with("--summarize ") || a == "--summarize" {
                    (true, a.strip_prefix("--summarize").unwrap().trim())
                } else if a.starts_with("--summary ") || a == "--summary" {
                    (true, a.strip_prefix("--summary").unwrap().trim())
                } else {
                    (false, a)
                }
            } else {
                (false, "")
            };
            let rem_opt = if remaining_arg.is_empty() {
                None
            } else {
                Some(remaining_arg)
            };
            ui.clear_box_on_exit();
            if is_summarize {
                if let Err(e) = handle_summarize_and_new_command(
                    ui,
                    store,
                    config,
                    agent,
                    session_id,
                    current_provider,
                    current_model,
                    todo_scope,
                    rem_opt,
                )
                .await
                {
                    ui.draw_initial_box();
                    ui.emit_above(&format!(
                        "{} Failed to summarize session: {}",
                        "✖".red().bold(),
                        e
                    ));
                }
            } else {
                if let Err(e) = handle_new_command(
                    ui,
                    store,
                    config,
                    agent,
                    session_id,
                    current_provider,
                    current_model,
                    todo_scope,
                    rem_opt,
                )
                .await
                {
                    ui.draw_initial_box();
                    ui.emit_above(&format!(
                        "{} Failed to start new session: {}",
                        "✖".red().bold(),
                        e
                    ));
                }
            }
            Ok(false)
        }
        "/expand" => {
            *process_expanded = true;
            if ui.is_attended {
                crate::commands::clear_screen();
                draw_transcript(turns, *process_expanded);
                ui.draw_initial_box();
            }
            Ok(false)
        }
        "/collapse" => {
            *process_expanded = false;
            if ui.is_attended {
                crate::commands::clear_screen();
                draw_transcript(turns, *process_expanded);
                ui.draw_initial_box();
            }
            Ok(false)
        }
        "/fork" => {
            handle_fork_command(ui, store, session_id, todo_scope, config, arg).await?;
            Ok(false)
        }
        "/todo" => {
            // The same fold Ctrl+Shift+O does, for the terminals that cannot tell the
            // two apart. The panel is chrome, so it is redrawn in place.
            ui.toggle_todo_collapsed();
            todo::refresh_panel(ui, todo_scope);
            Ok(false)
        }
        "/clear" => {
            turns.clear();
            let _ = std::fs::remove_file(todo_scope.path());
            ui.set_todo_panel(None);
            crate::commands::clear_screen();
            ui.draw_initial_box();
            Ok(false)
        }
        // `/steer`, `/queue` and `/interrupt` are not matched here: they act on the chat loop's own
        // state — the pending queue, the steer handle, the cancellation token of the turn in flight
        // — and `chat::turn::handle_composer_command` answers all three, typed or picked from the menu,
        // before dispatch ever reaches this function. A `/steer` arm here would have to take the
        // agent lock, which the running turn holds for its whole duration: the guidance would land
        // only once the turn it was meant to steer had already ended.
        "/skills" => {
            // Every exit path answers something: a silent command reads as a dead key. Both of them
            // answer through `emit_above`, which restores the composer itself — drawing it again
            // here would print a second box below the first.
            match skills_readout {
                Some(readout) if !readout.is_empty() => {
                    ui.emit_above(&format!("\n{}\n{}\n", "Available skills:".bold(), readout));
                }
                _ => {
                    ui.emit_above(&format!(
                        "{} No skills installed — drop SKILL.md directories into {} and restart",
                        "•".cyan(),
                        config.paths.skills_dir.display()
                    ));
                }
            }
            Ok(false)
        }
        "/mcp" => {
            let sub = arg.unwrap_or("").trim();
            if sub == "list" {
                let readout = crate::commands::mcp::format_mcp_servers(config);
                ui.emit_above(&format!("\n{}", readout.trim_end()));
            } else if sub == "tools" {
                ui.emit_above(&format!("{} Querying MCP tools...", "•".cyan()));
                let readout = crate::commands::mcp::format_mcp_tools(config).await;
                ui.emit_above(&format!("\n{}", readout.trim_end()));
            } else {
                let servers = crate::commands::mcp::format_mcp_servers(config);
                ui.emit_above(&format!("\n{}", servers.trim_end()));
                if config.mcp.enabled && config.mcp.servers.iter().any(|s| s.enabled) {
                    ui.emit_above(&format!("\n{} Querying MCP tools...", "•".cyan()));
                    let tools = crate::commands::mcp::format_mcp_tools(config).await;
                    ui.emit_above(&format!("\n{}", tools.trim_end()));
                }
            }
            Ok(false)
        }
        "/heartbeat" | "/hb" => {
            crate::commands::heartbeat::handle_chat_heartbeat(
                arg,
                ui,
                config,
                agent,
                session_id,
                current_provider,
                current_model,
                turns,
            )
            .await;
            if ui.is_attended {
                ui.draw_initial_box();
            }
            Ok(false)
        }
        "/usage" => {
            if let Err(e) = handle_usage_command(
                ui,
                config,
                agent,
                session_id.as_str(),
                current_provider.as_str(),
                current_model.as_str(),
            )
            .await
            {
                ui.emit_above(&format!("{} Failed to read usage: {}", "✖".red().bold(), e));
            }
            Ok(false)
        }
        "/delete" => {
            turns.clear();
            ui.clear_box_on_exit();
            if let Err(e) = handle_delete_command(
                ui,
                store,
                config,
                agent,
                session_id,
                current_provider,
                current_model,
            )
            .await
            {
                ui.draw_initial_box();
                ui.emit_above(&format!(
                    "{} Failed to delete session: {}",
                    "✖".red().bold(),
                    e
                ));
            }
            Ok(false)
        }
        "exit" | "quit" | "/exit" | "/quit" => {
            ui.clear_box_on_exit();
            clear_on_exit();
            Ok(true)
        }
        _ => Ok(false),
    }
}
