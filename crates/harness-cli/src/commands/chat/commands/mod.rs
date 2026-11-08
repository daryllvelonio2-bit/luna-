//! The slash-command surface: the table the picker lists, the dispatcher, and the handlers.
//!
//! This file is the facade. It owns the two things that have to agree with each other — the
//! `SLASH_COMMANDS` table and the `match` that dispatches it — and re-exports the handlers, which
//! live in submodules grouped by what they act on: `session`, `model`, `context`, `value_prompt`.

use colored::Colorize;
use std::sync::atomic::{AtomicBool, Ordering};
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
mod worker;
pub(crate) use usage::handle_usage_command;

mod dir;
pub(crate) use dir::handle_dir_command;

pub(crate) async fn handle_fork_command(
    ui: &ChatUi,
    store: &Arc<SqliteSessionStore>,
    session_id: &mut String,
    todo_scope: &harness_core::tools::TodoScope,
    config: &Config,
    title: Option<&str>,
) -> Result<(), HarnessError> {
    match store.fork_session(session_id, title).await {
        Ok(new_id) => {
            let old_id = session_id.clone();
            let old_list = todo_scope.path();
            *session_id = new_id.clone();
            todo_scope.set_session(session_id.as_str());
            let _ = std::fs::create_dir_all(config.paths.todos_dir.as_path());
            let _ = std::fs::copy(&old_list, todo_scope.path());
            ui.set_session_name(&format!("fork:{}", &new_id[..new_id.len().min(8)]));
            todo::refresh_panel(ui, todo_scope);
            ui.emit_above(&format!(
                "{} Forked session {} -> {}",
                "✔".green().bold(),
                old_id.dimmed(),
                session_id.cyan().bold()
            ));
        }
        Err(e) => {
            ui.emit_above(&format!("{} Fork failed: {}", "✖".red().bold(), e));
        }
    }
    Ok(())
}

pub struct SlashCommand {
    pub name: &'static str,
    pub details: &'static str,
}

/// A feature the user can switch off at runtime with a slash command.
///
/// The atomics are the CLI's own copy: the menu and the non-interactive loop read them without
/// taking the agent lock, while the agent's [`harness_core::AgentConfig`] holds the authoritative
/// flags the turn loop and dispatcher check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Feature {
    Memory,
    Notes,
    Skills,
    Mcp,
}

impl Feature {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Feature::Memory => "memory",
            Feature::Notes => "notes",
            Feature::Skills => "skills",
            Feature::Mcp => "mcp",
        }
    }

    pub(crate) fn title(self) -> &'static str {
        match self {
            Feature::Memory => "Saved memory",
            Feature::Notes => "Notes",
            Feature::Skills => "Skills",
            Feature::Mcp => "MCP",
        }
    }
}

#[derive(Debug)]
pub(crate) struct FeatureToggles {
    memory: AtomicBool,
    notes: AtomicBool,
    skills: AtomicBool,
    mcp: AtomicBool,
}

impl Default for FeatureToggles {
    fn default() -> Self {
        Self {
            memory: AtomicBool::new(true),
            notes: AtomicBool::new(true),
            skills: AtomicBool::new(true),
            mcp: AtomicBool::new(true),
        }
    }
}

impl FeatureToggles {
    pub(crate) fn enabled(&self, feature: Feature) -> bool {
        match feature {
            Feature::Memory => self.memory.load(Ordering::Relaxed),
            Feature::Notes => self.notes.load(Ordering::Relaxed),
            Feature::Skills => self.skills.load(Ordering::Relaxed),
            Feature::Mcp => self.mcp.load(Ordering::Relaxed),
        }
    }

    pub(crate) fn set(&self, feature: Feature, enabled: bool) {
        let slot = match feature {
            Feature::Memory => &self.memory,
            Feature::Notes => &self.notes,
            Feature::Skills => &self.skills,
            Feature::Mcp => &self.mcp,
        };
        slot.store(enabled, Ordering::Relaxed);
    }
}

/// Parse the argument of `/memory`, `/notes`, `/skills`, or `/mcp`.
///
/// `Ok(None)` is a status query (no argument), `Err(())` is an argument the command does not
/// understand, and the message for it belongs to the caller so the usage line can name the
/// command the user actually typed.
fn parse_feature_arg(arg: Option<&str>) -> Result<Option<bool>, ()> {
    match arg
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        None | Some("") | Some("status") => Ok(None),
        Some("on") | Some("enable") | Some("enabled") | Some("true") => Ok(Some(true)),
        Some("off") | Some("disable") | Some("disabled") | Some("false") => Ok(Some(false)),
        Some(_) => Err(()),
    }
}

/// Apply a runtime feature toggle to both the CLI's copy and the live agent.
///
/// The agent holds the flags the turn loop filters on, so a command that only changed the CLI's
/// atomics would show "OFF" while the model kept its tools.
pub(crate) async fn handle_feature_toggle(
    ui: &ChatUi,
    toggles: &FeatureToggles,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    feature: Feature,
    arg: Option<&str>,
) {
    let desired = match parse_feature_arg(arg) {
        Ok(value) => value,
        Err(()) => {
            ui.emit_above(&format!("{} Usage: /{} on|off", "•".cyan(), feature.name()));
            return;
        }
    };

    let enabled = match desired {
        Some(enabled) => {
            toggles.set(feature, enabled);
            let mut agent = agent.lock().await;
            match feature {
                Feature::Memory => agent.set_memory_enabled(enabled),
                Feature::Notes => agent.set_notes_enabled(enabled),
                Feature::Skills => agent.set_skills_enabled(enabled),
                Feature::Mcp => agent.set_mcp_enabled(enabled),
            }
            enabled
        }
        None => toggles.enabled(feature),
    };

    let state = if enabled {
        "ON".green().bold().to_string()
    } else {
        "OFF".dimmed().to_string()
    };
    ui.emit_above(&format!(
        "{} {} is {} for this session",
        "•".cyan(),
        feature.title(),
        state
    ));
}

/// One row in the slash menu: a built-in command or a configured worker.
///
/// Built-ins are `&'static str`, workers are owned, so the menu works on owned rows uniformly.
#[derive(Clone)]
pub struct CommandEntry {
    pub name: String,
    pub details: String,
}

/// The built-in command table plus every configured worker, as `/<name>`.
///
/// A worker whose name collides with a built-in never appears: the dispatcher gives built-ins
/// priority, so listing it as a worker would be a lie.
pub fn command_entries(
    config: &Config,
    skills: &[harness_plugins::Skill],
    skills_enabled: bool,
) -> Vec<CommandEntry> {
    let mut entries: Vec<CommandEntry> = SLASH_COMMANDS
        .iter()
        .map(|command| CommandEntry {
            name: command.name.to_string(),
            details: command.details.to_string(),
        })
        .collect();

    for (name, worker) in &config.workers {
        let command = format!("/{name}");
        if entries
            .iter()
            .any(|entry| entry.name.eq_ignore_ascii_case(&command))
            || worker::RESERVED_COMMAND_NAMES.contains(&command.as_str())
        {
            continue;
        }
        entries.push(CommandEntry {
            name: command,
            details: format!("Worker · {} ({})", worker.model, worker.provider),
        });
    }

    // Installed skills are invokable too: `/<skill-name> [input]` runs the skill's instructions as
    // the next turn. Built-ins and workers keep their names; a skill cannot shadow them. With
    // `/skills off` the skill commands disappear from the menu and from `/help`, but the built-in
    // `/skills` command stays so the feature can be turned back on.
    if skills_enabled {
        for skill in skills {
            let command = format!("/{}", skill.name);
            if skill.name.is_empty()
                || skill.name.contains(char::is_whitespace)
                || skill.name.contains('/')
                || entries
                    .iter()
                    .any(|entry| entry.name.eq_ignore_ascii_case(&command))
            {
                continue;
            }
            entries.push(CommandEntry {
                name: command,
                details: skill_command_details(&skill.description),
            });
        }
    }

    entries
}

/// Whether a line that begins with `/` is a filesystem path rather than a slash command.
///
/// The composer routes every line starting with `/` to the command dispatcher, which rejects an
/// absolute path such as `/home/me/notes.txt` as an unknown command before it can reach the agent.
/// A line is a path when its first token — the only part that can name a command — starts with `/`
/// and either contains a second separator or names an existing absolute path. A command with a
/// path argument (`/dir /tmp`) is still a command because only the first token is inspected.
pub(crate) fn looks_like_path(line: &str) -> bool {
    // Quotes are how a path with spaces arrives from a paste; they are not part of the path.
    let unquoted = line.trim_start().trim_start_matches(['"', '\'', '`']);
    let token = unquoted.split_whitespace().next().unwrap_or_default();
    let Some(rest) = token.strip_prefix('/') else {
        return false;
    };
    if rest.is_empty() {
        // A bare `/` opens the command menu; it is not a path.
        return false;
    }
    // `/home/me/notes.txt` — a second separator settles it without touching the filesystem.
    if rest.contains('/') {
        return true;
    }
    // `/tmp`, `/etc`: a single component can still be a real path.
    std::path::Path::new(token).exists()
}

/// Keep the slash menu and `/help` scannable: skill descriptions are written for the model and can
/// run to a paragraph, so the command list shows only the first line, capped.
fn skill_command_details(description: &str) -> String {
    const MAX_CHARS: usize = 80;
    let first_line = description.lines().next().unwrap_or("").trim();
    let mut text: String = first_line.chars().take(MAX_CHARS).collect();
    if first_line.chars().count() > MAX_CHARS {
        text.push('…');
    }
    format!("Skill · {text}")
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
        name: "/default_model",
        details: "Choose the default provider and model (this session or global)",
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
        details: "List available skills, or turn skills on/off (/skills on|off)",
    },
    SlashCommand {
        name: "/mcp",
        details: "List MCP servers and tools, or turn MCP on/off (/mcp on|off)",
    },
    SlashCommand {
        name: "/memory",
        details: "Turn saved memory on/off for this session (/memory on|off)",
    },
    SlashCommand {
        name: "/notes",
        details: "Turn notes on/off for this session (/notes on|off)",
    },
    SlashCommand {
        name: "/remember",
        details: "Run a memory pass over this session now instead of waiting for the interval",
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
        details: "Inspect background task output and delegated subagents (Ctrl+Shift+B)",
    },
    SlashCommand {
        name: "/jobs",
        details: "Inspect background task output and delegated subagents (Ctrl+Shift+B)",
    },
    SlashCommand {
        name: "/subagents",
        details: "Inspect background task output and delegated subagents (Ctrl+Shift+B)",
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
        name: "/worker",
        details: "Create, list, or delete named workers invoked as /<name> <task>",
    },
    SlashCommand {
        name: "/exit",
        details: "Exit Luna chat session",
    },
];

/// What the dispatcher wants the chat loop to do after a slash command.
pub(crate) enum SlashOutcome {
    /// Command handled; keep the loop running.
    Continue,
    /// The user asked to leave the chat.
    Exit,
    /// A skill invocation: run this prompt as a normal turn, echoing `display` in the transcript.
    Turn { prompt: String, display: String },
}

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
    // Installed skills, so `/<skill-name>` can appear in help and dispatch as a turn.
    skills: &[harness_plugins::Skill],
    // Runtime on/off state for memory, notes, skills, and MCP.
    toggles: &FeatureToggles,
    plan_mode: &Arc<std::sync::atomic::AtomicBool>,
    bg_queue: Option<&harness_core::tasks::BackgroundTaskQueue>,
    delegate_tool: Option<&harness_core::tools::DelegateTaskTool>,
    // Clipboard images waiting to ride the next prompt. Worker invocations consume them the same
    // way a normal turn does, so `/carbon look at this` can carry a pasted screenshot.
    pending_images: Option<&Arc<std::sync::Mutex<Vec<String>>>>,
) -> Result<SlashOutcome, HarnessError> {
    let parts: Vec<&str> = cmd.trim().splitn(2, ' ').collect();
    let name = parts[0];
    let arg = parts
        .get(1)
        .copied()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    match name {
        "/default_model" | "/default-model" => {
            ui.redraw_input_line();
            if let Err(e) = handle_default_model_switch(
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
                arg,
            )
            .await
            {
                ui.emit_above(&format!(
                    "{} Failed to set default model: {}",
                    "✖".red().bold(),
                    e
                ));
            }
            Ok(SlashOutcome::Continue)
        }
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
            Ok(SlashOutcome::Continue)
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
            Ok(SlashOutcome::Continue)
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
            Ok(SlashOutcome::Continue)
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
            Ok(SlashOutcome::Continue)
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
            Ok(SlashOutcome::Continue)
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
            Ok(SlashOutcome::Continue)
        }
        "/dir" | "/workspace" => {
            if let Err(e) = handle_dir_command(key_rx, ui, config, agent, arg).await {
                ui.emit_above(&format!(
                    "{} Failed to switch workspace: {}",
                    "✖".red().bold(),
                    e
                ));
            }
            Ok(SlashOutcome::Continue)
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
            Ok(SlashOutcome::Continue)
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
            Ok(SlashOutcome::Continue)
        }
        "/rewind" => {
            if let Err(e) = handle_rewind_command(ui, agent, session_id, arg).await {
                ui.emit_above(&format!("{} Failed to rewind: {}", "✖".red().bold(), e));
            }
            Ok(SlashOutcome::Continue)
        }
        "/diff" => {
            if let Err(e) = handle_diff_command(ui, agent, session_id, arg).await {
                ui.emit_above(&format!("{} Failed to diff: {}", "✖".red().bold(), e));
            }
            Ok(SlashOutcome::Continue)
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
            Ok(SlashOutcome::Continue)
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
            Ok(SlashOutcome::Continue)
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
            Ok(SlashOutcome::Continue)
        }
        "/expand" => {
            *process_expanded = true;
            if ui.is_attended {
                crate::commands::clear_screen();
                draw_transcript(turns, *process_expanded);
                ui.draw_initial_box();
            }
            Ok(SlashOutcome::Continue)
        }
        "/collapse" => {
            *process_expanded = false;
            if ui.is_attended {
                crate::commands::clear_screen();
                draw_transcript(turns, *process_expanded);
                ui.draw_initial_box();
            }
            Ok(SlashOutcome::Continue)
        }
        "/fork" => {
            handle_fork_command(ui, store, session_id, todo_scope, config, arg).await?;
            Ok(SlashOutcome::Continue)
        }
        "/todo" => {
            // The same fold Ctrl+Shift+O does, for the terminals that cannot tell the
            // two apart. The panel is chrome, so it is redrawn in place.
            ui.toggle_todo_collapsed();
            todo::refresh_panel(ui, todo_scope);
            Ok(SlashOutcome::Continue)
        }
        "/clear" => {
            turns.clear();
            let _ = std::fs::remove_file(todo_scope.path());
            ui.set_todo_panel(None);
            crate::commands::clear_screen();
            ui.draw_initial_box();
            Ok(SlashOutcome::Continue)
        }
        // `/steer`, `/queue` and `/interrupt` are not matched here: they act on the chat loop's own
        // state — the pending queue, the steer handle, the cancellation token of the turn in flight
        // — and `chat::turn::handle_composer_command` answers all three, typed or picked from the menu,
        // before dispatch ever reaches this function. A `/steer` arm here would have to take the
        // agent lock, which the running turn holds for its whole duration: the guidance would land
        // only once the turn it was meant to steer had already ended.
        "/skills" => {
            // `/skills on|off` is a toggle; a bare `/skills` keeps its original job of listing.
            if arg.is_some() {
                handle_feature_toggle(ui, toggles, agent, Feature::Skills, arg).await;
                return Ok(SlashOutcome::Continue);
            }
            if !toggles.enabled(Feature::Skills) {
                ui.emit_above(&format!(
                    "{} Skills are OFF for this session. Use /skills on to enable them.",
                    "•".cyan()
                ));
                return Ok(SlashOutcome::Continue);
            }
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
            Ok(SlashOutcome::Continue)
        }
        "/memory" => {
            handle_feature_toggle(ui, toggles, agent, Feature::Memory, arg).await;
            Ok(SlashOutcome::Continue)
        }
        "/notes" => {
            handle_feature_toggle(ui, toggles, agent, Feature::Notes, arg).await;
            Ok(SlashOutcome::Continue)
        }
        "/remember" => {
            let report = {
                let agent = agent.lock().await;
                agent.run_memory_pass_now().await
            };
            match report {
                Ok(report) => {
                    if report.is_empty() && report.skipped == 0 && report.declined == 0 {
                        ui.emit_above(&format!("{} Nothing new worth remembering.", "•".cyan()));
                    } else {
                        ui.emit_above(&format!("{} Memory pass: {}", "•".cyan(), report.summary()));
                    }
                }
                Err(error) => ui.emit_above(&format!(
                    "{} Memory pass failed: {}",
                    "✖".red().bold(),
                    error
                )),
            }
            Ok(SlashOutcome::Continue)
        }
        "/mcp" => {
            let sub = arg.unwrap_or("").trim();
            if matches!(
                sub,
                "on" | "off" | "enable" | "disable" | "enabled" | "disabled" | "true" | "false"
            ) {
                handle_feature_toggle(ui, toggles, agent, Feature::Mcp, Some(sub)).await;
                return Ok(SlashOutcome::Continue);
            }
            if !toggles.enabled(Feature::Mcp) {
                ui.emit_above(&format!(
                    "{} MCP is OFF for this session. Use /mcp on to enable it.",
                    "•".cyan()
                ));
                return Ok(SlashOutcome::Continue);
            }
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
            Ok(SlashOutcome::Continue)
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
            Ok(SlashOutcome::Continue)
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
            Ok(SlashOutcome::Continue)
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
            Ok(SlashOutcome::Continue)
        }
        "/plan" => {
            let current = plan_mode.load(std::sync::atomic::Ordering::SeqCst);
            let next = !current;
            plan_mode.store(next, std::sync::atomic::Ordering::SeqCst);
            ui.set_plan_mode(next);
            ui.emit_above(&format!(
                "{} Plan mode {}",
                "•".cyan(),
                if next {
                    "ON (read-only planning, mutating tools gated)"
                        .cyan()
                        .bold()
                } else {
                    "OFF".dimmed()
                }
            ));
            Ok(SlashOutcome::Continue)
        }
        "/tasks" | "/jobs" | "/subagents" => {
            super::viewer::show_background_task_viewer(
                key_rx,
                bg_queue,
                delegate_tool,
                store.as_ref(),
                ui,
                turns,
                *process_expanded,
            )
            .await;
            Ok(SlashOutcome::Continue)
        }
        "/help" | "/commands" | "/?" => {
            let mut help_text = String::from("\nAvailable commands:\n");
            for cmd in command_entries(config, skills, toggles.enabled(Feature::Skills)) {
                help_text.push_str(&format!(
                    "  {:<16} {}\n",
                    cmd.name.cyan(),
                    cmd.details.dimmed()
                ));
            }
            ui.emit_above(&help_text);
            Ok(SlashOutcome::Continue)
        }
        "/worker" => {
            ui.redraw_input_line();
            if let Err(e) = worker::handle_worker_command(
                Some(key_rx),
                ui,
                config,
                cred_manager,
                discovery,
                current_provider,
                current_model,
                arg,
            )
            .await
            {
                ui.emit_above(&format!("{} Worker setup failed: {}", "✖".red().bold(), e));
            }
            Ok(SlashOutcome::Continue)
        }
        "exit" | "quit" | "/exit" | "/quit" => {
            ui.clear_box_on_exit();
            clear_on_exit();
            Ok(SlashOutcome::Exit)
        }
        _ => {
            if let Some(worker_name) = name.strip_prefix('/') {
                let worker_name = worker_name.to_ascii_lowercase();
                if config.workers.contains_key(&worker_name) {
                    match arg {
                        Some(task) => {
                            if let Err(e) = worker::handle_worker_invocation(
                                &worker_name,
                                task,
                                ui,
                                config,
                                cred_manager,
                                discovery,
                                agent,
                                session_id,
                                delegate_tool,
                                pending_images,
                            )
                            .await
                            {
                                ui.emit_above(&format!(
                                    "{} Worker {} failed to start: {}",
                                    "✖".red().bold(),
                                    worker_name.cyan().bold(),
                                    e
                                ));
                            }
                        }
                        None => ui.emit_above(&format!(
                            "{} Usage: {}",
                            "•".cyan(),
                            format!("/{worker_name} <task>").yellow()
                        )),
                    }
                    return Ok(SlashOutcome::Continue);
                }
            }
            // An installed skill is a command too. The caller turns this prompt into a normal
            // turn, so the skill runs with the session's tools, history, and approvals. With
            // `/skills off` the names are not commands, so a typed one falls through to the
            // unrecognized-command message instead of running.
            if toggles.enabled(Feature::Skills) {
                if let Some(prompt) = crate::commands::skills::invocation_prompt(cmd, skills) {
                    return Ok(SlashOutcome::Turn {
                        prompt,
                        display: cmd.trim().to_string(),
                    });
                }
            }
            if name.starts_with('/') {
                ui.emit_above(&format!(
                    "{} Unrecognized command '{}'. Type {} for available commands.",
                    "✖".red().bold(),
                    name,
                    "/help".cyan()
                ));
            }
            Ok(SlashOutcome::Continue)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_model_switch(
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    store: &SqliteSessionStore,
    session_id: &str,
    current_provider: &mut String,
    current_model: &mut String,
    compact_model: &Option<String>,
) -> Result<(), HarnessError> {
    let Some((chosen_provider, chosen_model)) = pick_provider_and_model(
        key_rx.as_deref_mut(),
        ui,
        cred_manager,
        discovery,
        current_provider,
        current_model,
        true,
    )
    .await?
    else {
        ui.clear_box_on_exit();
        ui.draw_initial_box();
        return Ok(());
    };

    // Reasoning effort selection
    let reasoning_options = [
        ("Keep current / Default", None),
        ("minimal", Some(ReasoningEffort::Minimal)),
        ("low", Some(ReasoningEffort::Low)),
        ("medium", Some(ReasoningEffort::Medium)),
        ("high", Some(ReasoningEffort::High)),
        ("xhigh", Some(ReasoningEffort::Xhigh)),
        ("max", Some(ReasoningEffort::Max)),
        ("off / disabled", None),
    ];

    let reasoning_items: Vec<String> = reasoning_options
        .iter()
        .map(|(label, _)| label.to_string())
        .collect();
    let current_effort = agent.lock().await.reasoning_effort();
    let default_reasoning_idx = match current_effort {
        None => 0,
        Some(ReasoningEffort::Minimal) => 1,
        Some(ReasoningEffort::Low) => 2,
        Some(ReasoningEffort::Medium) => 3,
        Some(ReasoningEffort::High) => 4,
        Some(ReasoningEffort::Xhigh) => 5,
        Some(ReasoningEffort::Max) => 6,
    };

    let reasoning_sel = if let Some(ref mut rx) = key_rx {
        while rx.try_recv().is_ok() {}
        match select_from_rx(
            rx,
            ui,
            "Select Reasoning Effort",
            &reasoning_items,
            default_reasoning_idx,
            true,
            false,
        )
        .await
        {
            Some(i) => i,
            None => {
                ui.clear_box_on_exit();
                ui.draw_initial_box();
                return Ok(());
            }
        }
    } else {
        default_reasoning_idx
    };

    let chosen_effort = if reasoning_sel == 0 {
        current_effort
    } else {
        reasoning_options[reasoning_sel].1
    };

    let new_provider = Arc::new(GenericProviderClient::new(
        chosen_provider.clone(),
        chosen_model.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&chosen_provider),
    )?);

    {
        let mut ag = agent.lock().await;
        ag.set_provider(new_provider);
        ag.set_reasoning_effort(chosen_effort);
        ag.set_context_window(Some(context_window(
            config,
            discovery,
            &chosen_provider,
            &chosen_model,
        ) as usize));
    }

    *current_provider = chosen_provider.clone();
    *current_model = chosen_model.clone();

    // The switch is session state, not just process state: stamping the row means a later
    // `luna --session <id>` resumes on this pair instead of the config default. A failure to
    // persist is surfaced as a warning — the in-memory switch already happened.
    if let Err(error) = store
        .set_model(session_id, &chosen_provider, &chosen_model)
        .await
    {
        ui.emit_above(&format!(
            "{} Model switched in this session, but persisting it failed: {}",
            "⚠".yellow(),
            error
        ));
    }

    // If compaction model is not custom set, default it to the used model
    if compact_model.is_none() {
        if let Ok(comp_client) = GenericProviderClient::new(
            chosen_provider.clone(),
            chosen_model.clone(),
            cred_manager.clone(),
            discovery.clone(),
            config.providers.get(&chosen_provider),
        ) {
            agent
                .lock()
                .await
                .set_compaction(Some(harness_core::CompactionSetup {
                    provider: Arc::new(comp_client),
                    model: chosen_model.clone(),
                }));
        }
    }

    // The chained pickers redraw the box as they tear down — with the pre-switch model. Erase
    // that stale box so the status bar is drawn once, showing the model now in use.
    let context_max = context_window(config, discovery, &chosen_provider, &chosen_model);
    // Switching models does not touch the transcript, so keep the real usage rather than resetting
    // the bar to 0.
    let context_used = agent
        .lock()
        .await
        .estimate_session_tokens()
        .await
        .unwrap_or(0);
    ui.set_model(&chosen_model, context_used, context_max);
    super::refresh_antigravity_status_if_needed(ui, &chosen_provider, &chosen_model, cred_manager)
        .await;
    ui.clear_box_on_exit();
    ui.draw_initial_box();

    Ok(())
}

/// `/default_model`: choose the provider/model new sessions start on.
///
/// The picker chain is the one `/model` uses — provider, then model — with a trailing scope
/// choice. Session scope stamps the session row, exactly like `/model`; global scope writes
/// `model.provider` / `model.default` and clears the session pin so every future session follows
/// the configured default.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_default_model_switch(
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    config: &mut Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    store: &SqliteSessionStore,
    session_id: &str,
    current_provider: &mut String,
    current_model: &mut String,
    compact_model: &Option<String>,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let scope = arg.map(|a| a.trim().to_ascii_lowercase());
    let explicit_global = scope
        .as_deref()
        .is_some_and(|a| matches!(a, "global" | "--global" | "g"));
    let explicit_session = scope
        .as_deref()
        .is_some_and(|a| matches!(a, "session" | "--session" | "this" | "local" | "s"));

    let Some((chosen_provider, chosen_model)) = pick_provider_and_model(
        key_rx.as_deref_mut(),
        ui,
        cred_manager,
        discovery,
        current_provider.as_str(),
        current_model.as_str(),
        true,
    )
    .await?
    else {
        ui.clear_box_on_exit();
        ui.draw_initial_box();
        return Ok(());
    };

    // Scope comes after the model picker, like `/compact-model`. An explicit argument skips the
    // menu so `/default_model global` remains usable without another prompt.
    let is_global = if explicit_global {
        true
    } else if explicit_session {
        false
    } else if let Some(ref mut rx) = key_rx {
        let scope_options = [
            "This session only (remember for this session)".to_string(),
            "Globally (save as default for all sessions)".to_string(),
        ];
        while rx.try_recv().is_ok() {}
        match select_from_rx(
            rx,
            ui,
            "Default Model Scope",
            &scope_options,
            0,
            true,
            false,
        )
        .await
        {
            Some(1) => true,
            Some(0) => false,
            None => {
                ui.clear_box_on_exit();
                ui.draw_initial_box();
                return Ok(());
            }
            _ => false,
        }
    } else {
        false
    };

    // Resolve the config layers before touching live state: a broken config file should fail the
    // command rather than half-apply it.
    let layers = if is_global {
        Some(harness_core::config::process_layers(
            crate::commands::config::catalog_defaults(),
        )?)
    } else {
        None
    };

    let new_provider = Arc::new(GenericProviderClient::new(
        chosen_provider.clone(),
        chosen_model.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&chosen_provider),
    )?);

    {
        let mut ag = agent.lock().await;
        ag.set_provider(new_provider);
        ag.set_context_window(Some(context_window(
            config,
            discovery,
            &chosen_provider,
            &chosen_model,
        ) as usize));
    }

    *current_provider = chosen_provider.clone();
    *current_model = chosen_model.clone();

    let mut persisted_globally = false;
    if let Some(layers) = layers {
        let provider_write =
            harness_core::config::set_in_file(&layers, "model.provider", &chosen_provider);
        let model_write =
            harness_core::config::set_in_file(&layers, "model.default", &chosen_model);

        match (&provider_write, &model_write) {
            (Ok(_), Ok(_)) => {
                config.model.provider = chosen_provider.clone();
                config.model.default = chosen_model.clone();
                // The session pin outranks the config default on resume, so drop it: "global"
                // means this session follows the configured default too.
                if let Err(error) = store.clear_model(session_id).await {
                    ui.emit_above(&format!(
                        "{} Default model saved globally, but clearing this session's model pin failed: {}",
                        "⚠".yellow(),
                        error
                    ));
                }
                persisted_globally = true;
            }
            _ => {
                if let Err(error) = &provider_write {
                    ui.emit_above(&format!(
                        "{} Failed to persist global provider: {}",
                        "⚠".yellow(),
                        error
                    ));
                }
                if let Err(error) = &model_write {
                    ui.emit_above(&format!(
                        "{} Failed to persist global model: {}",
                        "⚠".yellow(),
                        error
                    ));
                }
                // The global write failed, so at least keep the pair on this session's row.
                if let Err(error) = store
                    .set_model(session_id, &chosen_provider, &chosen_model)
                    .await
                {
                    ui.emit_above(&format!(
                        "{} Model switched, but persisting it for this session failed: {}",
                        "⚠".yellow(),
                        error
                    ));
                }
            }
        }
    } else if let Err(error) = store
        .set_model(session_id, &chosen_provider, &chosen_model)
        .await
    {
        ui.emit_above(&format!(
            "{} Model switched, but persisting it for this session failed: {}",
            "⚠".yellow(),
            error
        ));
    }

    // Keep compaction on the active model while no dedicated compaction model is configured,
    // matching `/model`.
    if compact_model.is_none() {
        if let Ok(comp_client) = GenericProviderClient::new(
            chosen_provider.clone(),
            chosen_model.clone(),
            cred_manager.clone(),
            discovery.clone(),
            config.providers.get(&chosen_provider),
        ) {
            agent
                .lock()
                .await
                .set_compaction(Some(harness_core::CompactionSetup {
                    provider: Arc::new(comp_client),
                    model: chosen_model.clone(),
                }));
        }
    }

    let context_max = context_window(config, discovery, &chosen_provider, &chosen_model);
    let context_used = agent
        .lock()
        .await
        .estimate_session_tokens()
        .await
        .unwrap_or(0);
    ui.set_model(&chosen_model, context_used, context_max);
    super::refresh_antigravity_status_if_needed(ui, &chosen_provider, &chosen_model, cred_manager)
        .await;
    ui.clear_box_on_exit();
    ui.draw_initial_box();

    if persisted_globally {
        ui.emit_above(&format!(
            "{} Default model set globally: {} ({})",
            "✓".green(),
            chosen_model,
            chosen_provider
        ));
    } else if is_global {
        // The warning above explains the failed write; name the live switch too.
        ui.emit_above(&format!(
            "{} Default model switched for this session: {} ({})",
            "✓".green(),
            chosen_model,
            chosen_provider
        ));
    } else {
        ui.emit_above(&format!(
            "{} Default model set for this session: {} ({})",
            "✓".green(),
            chosen_model,
            chosen_provider
        ));
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_compact_model_switch(
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    store: &SqliteSessionStore,
    session_id: &str,
    current_provider: &str,
    current_model: &str,
    compact_provider: &mut Option<String>,
    compact_model: &mut Option<String>,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    if let Some(a) = arg {
        if a.eq_ignore_ascii_case("reset") || a.eq_ignore_ascii_case("default") {
            *compact_provider = None;
            *compact_model = None;
            let _ = store.clear_compact_model(session_id).await;
            let comp_client = GenericProviderClient::new(
                current_provider.to_string(),
                current_model.to_string(),
                cred_manager.clone(),
                discovery.clone(),
                config.providers.get(current_provider),
            )?;
            agent
                .lock()
                .await
                .set_compaction(Some(harness_core::CompactionSetup {
                    provider: Arc::new(comp_client),
                    model: current_model.to_string(),
                }));
            ui.clear_box_on_exit();
            ui.draw_initial_box();
            ui.emit_above(&format!(
                "{} Compaction model reset to active model: {} ({})",
                "✓".green(),
                current_model,
                current_provider
            ));
            return Ok(());
        }
    }

    let initial_p = compact_provider.as_deref().unwrap_or(current_provider);
    let initial_m = compact_model.as_deref().unwrap_or(current_model);

    let Some((chosen_provider, chosen_model)) = pick_provider_and_model(
        key_rx.as_deref_mut(),
        ui,
        cred_manager,
        discovery,
        initial_p,
        initial_m,
        false,
    )
    .await?
    else {
        ui.clear_box_on_exit();
        ui.draw_initial_box();
        return Ok(());
    };

    // Prompt for scope: this session only vs globally
    let is_global = if let Some(a) = arg {
        a.eq_ignore_ascii_case("global") || a.eq_ignore_ascii_case("--global")
    } else if let Some(ref mut rx) = key_rx {
        let scope_options = [
            "This session only (remember for this session)".to_string(),
            "Globally (save as default for all sessions)".to_string(),
        ];
        while rx.try_recv().is_ok() {}
        let sel = select_from_rx(
            rx,
            ui,
            "Compaction Model Scope",
            &scope_options,
            0,
            true,
            false,
        )
        .await;
        match sel {
            Some(1) => true,
            Some(0) => false,
            None => {
                ui.clear_box_on_exit();
                ui.draw_initial_box();
                return Ok(());
            }
            _ => false,
        }
    } else {
        false
    };

    let comp_client = GenericProviderClient::new(
        chosen_provider.clone(),
        chosen_model.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&chosen_provider),
    )?;

    agent
        .lock()
        .await
        .set_compaction(Some(harness_core::CompactionSetup {
            provider: Arc::new(comp_client),
            model: chosen_model.clone(),
        }));

    *compact_provider = Some(chosen_provider.clone());
    *compact_model = Some(chosen_model.clone());

    if is_global {
        let layers =
            harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
        if let Err(e) = harness_core::config::set_in_file(
            &layers,
            "agent.compaction_provider",
            &chosen_provider,
        ) {
            ui.emit_above(&format!(
                "{} Failed to persist global compaction provider: {}",
                "⚠".yellow(),
                e
            ));
        }
        if let Err(e) =
            harness_core::config::set_in_file(&layers, "agent.compaction_model", &chosen_model)
        {
            ui.emit_above(&format!(
                "{} Failed to persist global compaction model: {}",
                "⚠".yellow(),
                e
            ));
        }
        let _ = store.clear_compact_model(session_id).await;
        ui.emit_above(&format!(
            "{} Compaction model set globally: {} ({})",
            "✓".green(),
            chosen_model,
            chosen_provider
        ));
    } else {
        if let Err(e) = store
            .set_compact_model(session_id, &chosen_provider, &chosen_model)
            .await
        {
            ui.emit_above(&format!(
                "{} Compaction model switched in this session, but persisting it failed: {}",
                "⚠".yellow(),
                e
            ));
        }
        ui.emit_above(&format!(
            "{} Compaction model set for this session: {} ({})",
            "✓".green(),
            chosen_model,
            chosen_provider
        ));
    }

    ui.clear_box_on_exit();
    ui.draw_initial_box();

    Ok(())
}

/// `/rewind [turn]`: list this session's file checkpoints, or restore one turn's files.
///
/// Restoring is about *files*. The transcript is untouched — a rewind puts the workspace back so
/// the conversation can continue from a known state, it does not un-say what was said.
pub(crate) async fn handle_rewind_command(
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &str,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let controller = {
        let agent = agent.lock().await;
        agent.checkpoints()
    };
    let Some(controller) = controller else {
        ui.emit_above(&format!(
            "{} Checkpoints are off — enable them with {}",
            "•".cyan(),
            "checkpoints.enabled = true".bold()
        ));
        return Ok(());
    };
    if !controller.enabled() {
        ui.emit_above(&format!(
            "{} Checkpoints are off — enable them with {}",
            "•".cyan(),
            "checkpoints.enabled = true".bold()
        ));
        return Ok(());
    }

    let store = controller.store();
    let checkpoints = store.list(session_id)?;

    let Some(seq) = arg.and_then(|raw| raw.parse::<u32>().ok()) else {
        if checkpoints.is_empty() {
            ui.emit_above(&format!(
                "{} No checkpoints yet — the next turn that writes a file creates one",
                "•".cyan()
            ));
            return Ok(());
        }
        ui.emit_above(&format!(
            "{} {} checkpoint(s) for this session (newest last):",
            "•".cyan(),
            checkpoints.len()
        ));
        let len = checkpoints.len();
        for (index, checkpoint) in checkpoints.iter().enumerate() {
            // The counts come from the *bytes*, so a turn that wrote a file back unchanged reads
            // as unchanged rather than as a scary modification.
            let stat = store
                .diff(session_id, checkpoint.seq)
                .map(|turn| turn.summary().stat_line())
                .unwrap_or_else(|_| format!("{} file(s)", checkpoint.files.len()));
            let marker = if index + 1 == len { " (latest)" } else { "" };
            let prompt = checkpoint
                .prompt
                .as_deref()
                .map(|prompt| {
                    let flat = prompt.replace('\n', " ");
                    let short: String = flat.chars().take(60).collect();
                    format!(" — {}", short.dimmed())
                })
                .unwrap_or_default();
            ui.emit_above(&format!(
                "    {}  {}{}{}",
                format!("turn {}", checkpoint.seq).bold(),
                stat.cyan(),
                marker.green(),
                prompt
            ));
        }
        ui.emit_above(&format!(
            "    Restore one with {}",
            "/rewind <turn>".to_string().bold()
        ));
        return Ok(());
    };

    let report = store.restore(session_id, seq)?;
    if report.is_empty() {
        ui.emit_above(&format!(
            "{} Turn {} changed nothing restorable",
            "•".cyan(),
            seq
        ));
    } else {
        ui.emit_above(&format!(
            "{} Rewound turn {}: {} file(s) restored, {} deleted",
            "✔".green().bold(),
            seq,
            report.restored.len(),
            report.deleted.len()
        ));
        for path in report.restored.iter().chain(report.deleted.iter()) {
            ui.emit_above(&format!("    {}", path.dimmed()));
        }
    }
    for skipped in &report.skipped {
        ui.emit_above(&format!("    {} {}", "skipped".yellow(), skipped.dimmed()));
    }
    ui.emit_above(&format!(
        "{} The transcript is unchanged — files only",
        "•".cyan()
    ));
    Ok(())
}

/// `/diff [turn|--all]`: what the workspace actually changed, read from the turn's checkpoint.
///
/// `/rewind` is a write. This is the read that belongs before it: the same snapshot, diffed against
/// the files as they are now, so a person can see what a turn did before deciding to undo it.
pub(crate) async fn handle_diff_command(
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &str,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let controller = {
        let agent = agent.lock().await;
        agent.checkpoints()
    };
    let Some(controller) = controller.filter(|controller| controller.enabled()) else {
        ui.emit_above(&format!(
            "{} Checkpoints are off — enable them with {}",
            "•".cyan(),
            "checkpoints.enabled = true".bold()
        ));
        return Ok(());
    };

    let store = controller.store();
    let checkpoints = store.list(session_id)?;
    if checkpoints.is_empty() {
        ui.emit_above(&format!(
            "{} No checkpoints yet — the next turn that writes a file creates one",
            "•".cyan()
        ));
        return Ok(());
    }

    let raw = arg.unwrap_or("").trim();
    let all = raw == "--all" || raw == "all";
    let seq = if all {
        None
    } else {
        match raw.split_whitespace().next().map(str::parse::<u32>) {
            Some(Ok(seq)) => Some(seq),
            _ => checkpoints.last().map(|checkpoint| checkpoint.seq),
        }
    };

    let turns = match seq {
        Some(seq) => vec![store.diff(session_id, seq)?],
        None => store.diff_all(session_id)?,
    };

    for turn in &turns {
        let summary = turn.summary();
        let prompt = turn
            .prompt
            .as_deref()
            .map(|prompt| prompt.replace('\n', " "))
            .map(|prompt| format!(" — {}", prompt.dimmed()))
            .unwrap_or_default();
        ui.emit_above(&format!(
            "{}  {}{}",
            format!("turn {}", turn.seq).bold(),
            summary.stat_line().cyan(),
            prompt
        ));

        if turn.is_empty() && turn.unrecorded.is_empty() {
            ui.emit_above(&format!(
                "    {}",
                "nothing changed (the workspace matches this turn's snapshot)".dimmed()
            ));
            continue;
        }

        for file in turn.changed() {
            ui.emit_above(&format!(
                "    {} {:<9} {} {}",
                file.kind.marker(),
                file.kind.as_str().yellow(),
                file.path.bold(),
                format!("+{} -{}", file.additions, file.deletions).dimmed()
            ));
            if let Some(note) = &file.note {
                ui.emit_above(&format!("        {}", note.dimmed()));
            }
            // Hunks are the point of a diff, but a chat pane is not a pager: cap them and say so
            // rather than letting one regenerated lockfile scroll the conversation away.
            let mut budget = DIFF_HUNK_BUDGET;
            for line in file.diff.trim_end().lines() {
                if budget == 0 {
                    ui.emit_above(&format!(
                        "        {}",
                        "… more hunks not shown in chat — pipe `luna diff` for all of them"
                            .dimmed()
                    ));
                    break;
                }
                budget -= 1;
                let painted =
                    if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@")
                    {
                        line.cyan()
                    } else if line.starts_with('+') {
                        line.green()
                    } else if line.starts_with('-') {
                        line.red()
                    } else {
                        line.normal()
                    };
                ui.emit_above(&format!("        {}", painted));
            }
        }
        if !turn.unrecorded.is_empty() {
            ui.emit_above(&format!(
                "    {}",
                format!(
                    "! {} file(s) changed by a shell command — no snapshot, /rewind cannot undo these:",
                    turn.unrecorded.len()
                )
                .yellow()
            ));
            for file in &turn.unrecorded {
                ui.emit_above(&format!(
                    "      {:<9} {}",
                    file.kind.as_str(),
                    file.path.dimmed()
                ));
            }
        }
        ui.emit_above("");
    }

    Ok(())
}

/// How many diff lines a chat reply shows per turned file.
const DIFF_HUNK_BUDGET: usize = 40;

/// `/name [name]`: set the current session's user-assigned name, or report it when no name is given.
pub(crate) async fn handle_name_command(
    ui: &ChatUi,
    store: &SqliteSessionStore,
    session_id: &str,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    if let Some(raw) = arg {
        store.set_name(session_id, raw).await?;
    }

    let name = store.get(session_id).await?.name;
    ui.set_session_name(&name);

    if name.is_empty() {
        if arg.is_some() {
            ui.emit_above(&format!("{} Session name cleared", "✔".green().bold()));
        } else {
            ui.emit_above(&format!(
                "{} This session has no name — set one with {}",
                "•".cyan(),
                "/name <name>".bold()
            ));
        }
    } else if arg.is_some() {
        ui.emit_above(&format!(
            "{} Session name set to '{}'",
            "✔".green().bold(),
            name.cyan()
        ));
    } else {
        ui.emit_above(&format!("{} Session name: {}", "•".cyan(), name.cyan()));
    }

    Ok(())
}

/// `/new [name]`: start a fresh chat session, rebinding the agent and clearing the screen.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_new_command(
    ui: &ChatUi,
    store: &Arc<SqliteSessionStore>,
    config: &Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &mut String,
    current_provider: &str,
    current_model: &str,
    todo_scope: &harness_core::tools::TodoScope,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let new_id = resolve_session(store.as_ref(), SessionSelection::New).await?;
    let name = arg.map(|s| s.trim()).unwrap_or("");
    if !name.is_empty() {
        store.set_name(&new_id, name).await?;
    }
    // The new session inherits the model currently in use, so the switch carried into it is
    // not lost the next time this session is resumed.
    store
        .set_model(&new_id, current_provider, current_model)
        .await?;

    *session_id = new_id.clone();
    agent.lock().await.set_session(SessionBinding {
        store: store.clone(),
        session_id: new_id.clone(),
        resume_max_messages: config.session.resume_max_messages,
    });

    let context_max = context_window_for(config, current_provider, current_model);
    ui.set_status(current_model, 0, context_max);
    ui.set_session_name(name);

    // The new conversation starts with an empty list, and the list belongs to the
    // session: the scope follows the session, and the panel above the composer goes
    // with it.
    todo_scope.set_session(&new_id);
    todo::refresh_panel(ui, todo_scope);

    if ui.is_attended {
        crate::commands::clear_screen();
        ui.draw_landing();
        ui.draw_initial_box();
        if name.is_empty() {
            ui.emit_above(&format!(
                "{} Started new session {}",
                "✔".green().bold(),
                session_id.cyan()
            ));
        } else {
            ui.emit_above(&format!(
                "{} Started new session '{}' ({})",
                "✔".green().bold(),
                name.cyan(),
                session_id.dimmed()
            ));
        }
    } else {
        if name.is_empty() {
            println!(
                "{} Started new session {}",
                "✔".green().bold(),
                session_id.cyan()
            );
        } else {
            println!(
                "{} Started new session '{}' ({})",
                "✔".green().bold(),
                name.cyan(),
                session_id.dimmed()
            );
        }
    }

    Ok(())
}

/// `/delete`: permanently delete the current chat session and start a fresh one.
pub(crate) async fn handle_delete_command(
    ui: &ChatUi,
    store: &Arc<SqliteSessionStore>,
    config: &Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &mut String,
    current_provider: &str,
    current_model: &str,
) -> Result<(), HarnessError> {
    let old_id = session_id.clone();
    let old_name = store.get(&old_id).await.map(|s| s.name).unwrap_or_default();

    // Delete current session from SQLite store
    store.delete_session(&old_id).await?;

    // Start a fresh new session
    let new_id = resolve_session(store.as_ref(), SessionSelection::New).await?;
    store
        .set_model(&new_id, current_provider, current_model)
        .await?;

    *session_id = new_id.clone();
    agent.lock().await.set_session(SessionBinding {
        store: store.clone(),
        session_id: new_id.clone(),
        resume_max_messages: config.session.resume_max_messages,
    });

    let context_max = context_window_for(config, current_provider, current_model);
    ui.set_status(current_model, 0, context_max);
    ui.set_session_name("");

    let desc = if old_name.is_empty() {
        old_id.cyan().to_string()
    } else {
        format!("'{}' ({})", old_name.cyan(), old_id.dimmed())
    };

    if ui.is_attended {
        crate::commands::clear_screen();
        ui.draw_landing();
        ui.draw_initial_box();
        ui.emit_above(&format!(
            "{} Deleted session {} and started fresh session {}",
            "✔".green().bold(),
            desc,
            new_id.cyan()
        ));
    } else {
        println!(
            "{} Deleted session {} and started fresh session {}",
            "✔".green().bold(),
            desc,
            new_id.cyan()
        );
    }

    Ok(())
}

/// `/sessions [id|name]`: interactive session picker to switch chat sessions (alias: `/session`).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_sessions_switch(
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    config: &Config,
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
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let target_id = if let Some(target) = arg.filter(|s| !s.is_empty()) {
        if store.get(target).await.is_ok() {
            target.to_string()
        } else if let Some(id) = find_by_name(store.as_ref(), target).await? {
            id
        } else {
            return Err(HarnessError::SessionError(format!(
                "Session '{}' not found",
                target
            )));
        }
    } else {
        let sessions: Vec<_> = store
            .list()
            .await?
            .into_iter()
            .filter(|session| !session.name.starts_with("worker:"))
            .collect();
        if sessions.is_empty() {
            ui.emit_above(&format!("{} No saved sessions found", "•".cyan()));
            return Ok(());
        }

        let items: Vec<String> = sessions
            .iter()
            .map(|s| {
                let is_current = s.id == *session_id;
                let tag = if is_current { " *" } else { "" };
                let name = if s.name.is_empty() {
                    "-".to_string()
                } else {
                    format!("\"{}\"", s.name)
                };
                let title = if s.title.is_empty() {
                    "(new session)"
                } else {
                    s.title.as_str()
                };
                let time = s.updated_at.format("%m/%d %H:%M").to_string();
                format!(
                    "{:<9} {:<14} {:>3} msgs  {:<11} {}{}",
                    s.id, name, s.message_count, time, title, tag
                )
            })
            .collect();

        let default_idx = sessions
            .iter()
            .position(|s| s.id == *session_id)
            .unwrap_or(0);

        let chosen_idx = if let Some(ref mut rx) = key_rx {
            while rx.try_recv().is_ok() {}
            match select_from_rx(rx, ui, "Select Session", &items, default_idx, false, false).await
            {
                Some(i) => i,
                None => {
                    ui.clear_box_on_exit();
                    ui.draw_initial_box();
                    return Ok(());
                }
            }
        } else {
            println!("Available sessions:");
            for (i, item) in items.iter().enumerate() {
                println!("  {}. {}", i + 1, item);
            }
            return Ok(());
        };

        sessions[chosen_idx].id.clone()
    };

    if target_id == *session_id {
        ui.emit_above(&format!(
            "{} Already on session {}",
            "•".cyan(),
            session_id.cyan()
        ));
        return Ok(());
    }

    *session_id = target_id.clone();
    agent.lock().await.set_session(SessionBinding {
        store: store.clone(),
        session_id: target_id.clone(),
        resume_max_messages: config.session.resume_max_messages,
    });

    let record = store.get(&target_id).await?;
    if let Some((ref prov, ref mdl)) = record.model {
        if prov != current_provider || mdl != current_model {
            match GenericProviderClient::new(
                prov.clone(),
                mdl.clone(),
                cred_manager.clone(),
                discovery.clone(),
                config.providers.get(prov),
            ) {
                Ok(client) => {
                    agent.lock().await.set_provider(Arc::new(client));
                    *current_provider = prov.clone();
                    *current_model = mdl.clone();
                }
                Err(e) => {
                    ui.emit_above(&format!(
                        "{} Warning: could not restore session model {} ({}): {}",
                        "⚠".yellow().bold(),
                        mdl,
                        prov,
                        e
                    ));
                }
            }
        }
    }

    // Restore target session's compaction model (or fall back to global config / active model)
    let (target_comp_p, target_comp_m) = match &record.compact_model {
        Some((p, m)) => (p.clone(), m.clone()),
        None => {
            let p = config
                .agent
                .compaction_provider
                .clone()
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| current_provider.clone());
            let m = config
                .agent
                .compaction_model
                .clone()
                .filter(|m| !m.is_empty())
                .unwrap_or_else(|| current_model.clone());
            (p, m)
        }
    };
    if let Ok(client) = GenericProviderClient::new(
        target_comp_p.clone(),
        target_comp_m.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&target_comp_p),
    ) {
        agent
            .lock()
            .await
            .set_compaction(Some(harness_core::CompactionSetup {
                provider: Arc::new(client),
                model: target_comp_m.clone(),
            }));
    }
    *compact_provider = record
        .compact_model
        .as_ref()
        .map(|(p, _)| p.clone())
        .or_else(|| {
            config
                .agent
                .compaction_provider
                .clone()
                .filter(|p| !p.is_empty())
        });
    *compact_model = record
        .compact_model
        .as_ref()
        .map(|(_, m)| m.clone())
        .or_else(|| {
            config
                .agent
                .compaction_model
                .clone()
                .filter(|m| !m.is_empty())
        });

    todo_scope.set_session(&target_id);
    todo::refresh_panel(ui, todo_scope);

    let replayed = store
        .replay_transcript(&target_id, config.session.resume_max_messages)
        .await?;
    *turns = parse_turns_from_history(&replayed.history);

    if ui.is_attended {
        crate::commands::clear_screen();
        if replayed.replayed > 0 {
            if replayed.truncated() {
                println!(
                    "{}\n",
                    format!(
                        "… {} earlier messages not shown ({} shown)",
                        replayed.total - replayed.replayed,
                        replayed.replayed
                    )
                    .dimmed()
                );
            }
            draw_transcript(turns, *process_expanded);
            println!();
        }
        ui.draw_initial_box();
    }

    let context_max = context_window_for(config, current_provider, current_model);
    let context_used = agent
        .lock()
        .await
        .estimate_session_tokens()
        .await
        .unwrap_or(0);
    ui.set_status(current_model, context_used, context_max);
    ui.set_session_name(&record.name);

    let display_name = if !record.name.is_empty() {
        format!("'{}' ({})", record.name.cyan(), target_id.dimmed())
    } else if !record.title.is_empty() {
        format!("\"{}\" ({})", record.title.cyan(), target_id.dimmed())
    } else {
        target_id.cyan().to_string()
    };

    ui.emit_above(&format!(
        "{} Switched to session {}",
        "✔".green().bold(),
        display_name
    ));

    Ok(())
}

/// `/summarize [name]`, `/compact [name]`, or `/new -s [name]`:
/// Summarize the current session's history and start a new session seeded with the summary.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_summarize_and_new_command(
    ui: &ChatUi,
    store: &Arc<SqliteSessionStore>,
    config: &Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &mut String,
    current_provider: &str,
    current_model: &str,
    todo_scope: &harness_core::tools::TodoScope,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let replayed = store.replay(session_id, usize::MAX).await?;
    let has_content = replayed
        .history
        .iter()
        .any(|s| !s.is_summary && s.message.role != Role::System);

    if !has_content {
        ui.draw_initial_box();
        ui.emit_above(&format!(
            "{} Nothing to summarize in this session",
            "•".cyan()
        ));
        return Ok(());
    }

    let old_id = session_id.clone();
    let name = arg.map(|s| s.trim()).unwrap_or("");

    ui.start_activity(
        "summarize session",
        "summarize",
        &serde_json::json!({ "session": old_id }),
    );

    let summary_res = {
        let ag = agent.lock().await;
        ag.summarize_current_session(current_model).await
    };

    ui.stop_activity();

    let summary = match summary_res {
        Ok(s) => s,
        Err(e) => {
            ui.draw_initial_box();
            ui.emit_above(&format!(
                "{} Failed to summarize session: {}",
                "✖".red().bold(),
                e
            ));
            return Ok(());
        }
    };

    let new_id = resolve_session(store.as_ref(), SessionSelection::New).await?;
    if !name.is_empty() {
        store.set_name(&new_id, name).await?;
    }
    // As with `/new`: the follow-up session inherits the model in use when it was created.
    store
        .set_model(&new_id, current_provider, current_model)
        .await?;

    let summary_header = if !name.is_empty() {
        format!(
            "Context summary from session '{}' ({}):\n{}",
            name, old_id, summary
        )
    } else {
        format!("Context summary from session {}:\n{}", old_id, summary)
    };
    let summary_msg = Message::system(summary_header);
    store.append(&new_id, &summary_msg, true).await?;

    *session_id = new_id.clone();
    agent.lock().await.set_session(SessionBinding {
        store: store.clone(),
        session_id: new_id.clone(),
        resume_max_messages: config.session.resume_max_messages,
    });

    let context_max = context_window_for(config, current_provider, current_model);
    let summary_tokens = harness_core::heuristic_tokens(&[summary_msg]);
    ui.set_status(current_model, summary_tokens, context_max);
    ui.set_session_name(name);

    // The summarized conversation carries its work into the new session, so the list
    // comes with it.
    let old_list = todo_scope.path();
    todo_scope.set_session(&new_id);
    let _ = std::fs::create_dir_all(config.paths.todos_dir.as_path());
    let _ = std::fs::copy(&old_list, todo_scope.path());
    todo::refresh_panel(ui, todo_scope);

    if ui.is_attended {
        crate::commands::clear_screen();
        ui.draw_initial_box();
        if name.is_empty() {
            ui.emit_above(&format!(
                "{} Started new session {} with summary from {}",
                "✔".green().bold(),
                session_id.cyan(),
                old_id.dimmed()
            ));
        } else {
            ui.emit_above(&format!(
                "{} Started new session '{}' ({}) with summary from {}",
                "✔".green().bold(),
                name.cyan(),
                session_id.dimmed(),
                old_id.dimmed()
            ));
        }
        ui.emit_above(&format!("\n{}\n{}\n", "Summary:".bold(), summary.dimmed()));
    } else {
        if name.is_empty() {
            println!(
                "{} Started new session {} with summary from {}",
                "✔".green().bold(),
                session_id.cyan(),
                old_id.dimmed()
            );
        } else {
            println!(
                "{} Started new session '{}' ({}) with summary from {}",
                "✔".green().bold(),
                name.cyan(),
                session_id.dimmed(),
                old_id.dimmed()
            );
        }
        println!("\n{}\n{}\n", "Summary:".bold(), summary);
    }

    Ok(())
}

/// `/compact`: fold the current session's oldest turns into a summary in place — the session
/// keeps its id and name, only the context shrinks.
///
/// The fold runs as its own task and reports through the agent's compaction events, so the
/// composer stays live and animates while the summary call is in flight. The agent lock is what
/// arbitrates: a turn holds it for its whole run, so a fold asked for mid-turn is refused instead
/// of being silently parked behind the turn.
pub(crate) async fn handle_compact_command(
    ui: &ChatUi,
    config: &Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    current_provider: &str,
    current_model: &str,
) -> Result<(), HarnessError> {
    let ui = ui.clone();
    let agent = agent.clone();
    // Resolved here, on the caller's thread: the fold runs in a task that has no config, and the
    // answer is the same before and after it (the window belongs to the model, not the fold).
    let context_max = context_window_for(config, current_provider, current_model);
    tokio::spawn(async move {
        let Ok(ag) = agent.try_lock() else {
            ui.emit_above(&format!(
                "{} Cannot compact while a turn is running: interrupt it (send a message or press Escape), then run /compact",
                "✖".red().bold()
            ));
            return;
        };
        // Everything that happens inside the fold — the animation, the report, the failure — is
        // emitted as compaction events, so this task only refreshes the gauge afterwards.
        if ag.compact_current_session().await.is_ok() {
            // The summary replaced the folded turns, so the gauge drops for real.
            let context_used = ag.estimate_session_tokens().await.unwrap_or(0);
            ui.update_status(context_used, context_max);
        }
    });

    Ok(())
}

/// `/auto-compact`: open the setup box — the threshold, then the turns — and turn the feature on.
///
/// The two values used to be arguments (`/auto-compact set threshold 200k`). They are asked for in
/// a box instead, the way the approval gate asks for a verdict, so the bounds, an example and the
/// current value are in front of the user while they type. `on` and `off` stay as arguments: a
/// toggle has no value to choose, and a box for it would be a box for nothing.
///
/// `key_rx` is the composer's key channel, so the wizard reads the same keys the chat does. A host
/// with no composer (piped stdin) passes `None` and gets the settings printed instead of a prompt
/// nobody could type into.
pub(crate) async fn handle_auto_compact_command(
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    arg: Option<&str>,
    key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
) -> Result<(), HarnessError> {
    let arg = arg.unwrap_or("").trim();

    let layers = harness_core::config::process_layers(Config::default())?;

    if arg.eq_ignore_ascii_case("on") || arg.eq_ignore_ascii_case("enable") {
        {
            let mut ag = agent.lock().await;
            ag.set_auto_compact_enabled(true);
        }
        let _ = harness_core::config::set_in_file(&layers, "auto_compact_enabled", "true")?;
        let (threshold, percent) = {
            let ag = agent.lock().await;
            let (_, th, pct) = ag.auto_compact_settings();
            (th, pct)
        };
        ui.emit_above(&format!(
            "{} Auto-compact enabled (threshold: {} tokens, compact: {}%)",
            "✓".green().bold(),
            threshold,
            percent
        ));
        return Ok(());
    }

    if arg.eq_ignore_ascii_case("off") || arg.eq_ignore_ascii_case("disable") {
        {
            let mut ag = agent.lock().await;
            ag.set_auto_compact_enabled(false);
        }
        let _ = harness_core::config::set_in_file(&layers, "auto_compact_enabled", "false")?;
        ui.emit_above(&format!("{} Auto-compact disabled", "✓".green().bold()));
        return Ok(());
    }

    // No composer to type into: report the settings and the two arguments this host can use,
    // rather than opening a box that would read keys from a channel no one is writing to.
    let Some(key_rx) = key_rx else {
        let (enabled, threshold, percent) = {
            let ag = agent.lock().await;
            ag.auto_compact_settings()
        };
        let status = if enabled {
            "enabled".green().bold()
        } else {
            "disabled".dimmed()
        };
        ui.emit_above(&format!(
            "Auto-compact is {} (threshold: {} tokens, compact: {}%)",
            status, threshold, percent
        ));
        ui.emit_above(
            "Usage: /auto-compact opens the setup box in an interactive chat; here it also takes on | off",
        );
        return Ok(());
    };

    if !arg.is_empty() {
        ui.emit_above(&format!(
            "{} Unknown auto-compact option '{}' — {} opens the setup box, {} and {} toggle it",
            "✖".red().bold(),
            arg,
            "/auto-compact".bold(),
            "on".bold(),
            "off".bold()
        ));
        return Ok(());
    }

    let (_, current_threshold, current_percent) = {
        let ag = agent.lock().await;
        ag.auto_compact_settings()
    };

    // Two boxes, one value each. Esc in either leaves the config exactly as it was: a half
    // answered wizard must not write a threshold the user never confirmed.
    let Some(threshold) = prompt_value(
        ui,
        key_rx,
        &THRESHOLD_PROMPT,
        current_threshold,
        harness_onboarding::compact_step::parse_threshold,
        |_text, _val| None,
    )
    .await
    else {
        ui.emit_above(&format!("{} Auto-compact unchanged", "•".dimmed()));
        return Ok(());
    };

    let Some(percent) = prompt_value(
        ui,
        key_rx,
        &PERCENT_PROMPT,
        current_percent,
        harness_onboarding::compact_step::parse_percent,
        |_text, pct| {
            if pct <= 10 {
                Some(format!(
                    "{} ≤{}%: frees very little context; will quickly hit threshold again",
                    "⚠".yellow().bold(),
                    pct
                ))
            } else if pct <= 60 {
                Some(format!(
                    "{} {}%: good balance of history retention and token headroom",
                    "✓".green().bold(),
                    pct
                ))
            } else {
                Some(format!(
                    "{} {}%: aggressive compaction, clears majority of context",
                    "⚡".cyan().bold(),
                    pct
                ))
            }
        },
    )
    .await
    else {
        ui.emit_above(&format!("{} Auto-compact unchanged", "•".dimmed()));
        return Ok(());
    };

    {
        let mut ag = agent.lock().await;
        ag.set_auto_compact_threshold(threshold);
        ag.set_auto_compact_percent(percent);
        ag.set_auto_compact_enabled(true);
    }
    let _ = harness_core::config::set_in_file(&layers, "auto_compact_enabled", "true")?;
    let _ = harness_core::config::set_in_file(
        &layers,
        "auto_compact_threshold",
        &threshold.to_string(),
    )?;
    let _ =
        harness_core::config::set_in_file(&layers, "auto_compact_percent", &percent.to_string())?;

    ui.emit_above(&format!(
        "{} Auto-compact enabled (threshold: {} tokens, compact: {}%)",
        "✓".green().bold(),
        threshold,
        percent
    ));
    Ok(())
}

pub(crate) async fn handle_thinking_switch(
    key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    handle_reasoning_or_thinking_switch(key_rx, ui, agent, arg, true).await
}

pub(crate) async fn handle_reasoning_switch(
    key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    handle_reasoning_or_thinking_switch(key_rx, ui, agent, arg, false).await
}

async fn handle_reasoning_or_thinking_switch(
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    arg: Option<&str>,
    is_thinking: bool,
) -> Result<(), HarnessError> {
    let concept_name = if is_thinking {
        "thinking level"
    } else {
        "reasoning effort"
    };
    let title = if is_thinking {
        "Select Thinking Level"
    } else {
        "Select Reasoning Effort"
    };

    let chosen_effort = if let Some(s) = arg {
        if s.eq_ignore_ascii_case("off")
            || s.eq_ignore_ascii_case("none")
            || s.eq_ignore_ascii_case("disabled")
            || s == "0"
            || s.eq_ignore_ascii_case("false")
        {
            None
        } else {
            match ReasoningEffort::from_str_loose(s) {
                Some(e) => Some(e),
                None => {
                    ui.draw_initial_box();
                    ui.emit_above(&format!(
                        "{} Unknown {} '{}'. Available: minimal, low, medium, high, xhigh, max, off",
                        "✖".red().bold(),
                        concept_name,
                        s
                    ));
                    return Ok(());
                }
            }
        }
    } else {
        let reasoning_options = [
            ("off / default", None),
            ("minimal", Some(ReasoningEffort::Minimal)),
            ("low", Some(ReasoningEffort::Low)),
            ("medium", Some(ReasoningEffort::Medium)),
            ("high", Some(ReasoningEffort::High)),
            ("xhigh", Some(ReasoningEffort::Xhigh)),
            ("max", Some(ReasoningEffort::Max)),
        ];
        let reasoning_items: Vec<String> = reasoning_options
            .iter()
            .map(|(label, _)| label.to_string())
            .collect();
        let current_effort = agent.lock().await.reasoning_effort();
        let default_idx = match current_effort {
            None => 0,
            Some(ReasoningEffort::Minimal) => 1,
            Some(ReasoningEffort::Low) => 2,
            Some(ReasoningEffort::Medium) => 3,
            Some(ReasoningEffort::High) => 4,
            Some(ReasoningEffort::Xhigh) => 5,
            Some(ReasoningEffort::Max) => 6,
        };

        if let Some(ref mut rx) = key_rx {
            while rx.try_recv().is_ok() {}
            match select_from_rx(rx, ui, title, &reasoning_items, default_idx, false, false).await {
                Some(i) => reasoning_options[i].1,
                None => {
                    return Ok(());
                }
            }
        } else {
            let cur = agent.lock().await.reasoning_effort();
            let cur_label = cur
                .map(|e| e.to_string())
                .unwrap_or_else(|| "off".to_string());
            ui.emit_above(&format!(
                "{} Current {} is '{}'",
                "ℹ".blue().bold(),
                concept_name,
                cur_label.yellow()
            ));
            return Ok(());
        }
    };

    agent.lock().await.set_reasoning_effort(chosen_effort);
    // Same as the model switch: the picker left a box behind, so erase it before redrawing.
    ui.clear_box_on_exit();
    ui.draw_initial_box();

    let effort_label = chosen_effort
        .map(|e| e.to_string())
        .unwrap_or_else(|| "off".to_string());

    let label_capitalized = if is_thinking {
        "Thinking level"
    } else {
        "Reasoning effort"
    };

    ui.emit_above(&format!(
        "{} {} set to '{}'",
        "✔".green().bold(),
        label_capitalized,
        effort_label.yellow()
    ));

    Ok(())
}

/// What one value box of the `/auto-compact` wizard asks for.
struct ValuePrompt {
    /// Box title.
    title: &'static str,
    /// The sentence above the input panel: what the value does.
    question: &'static str,
    /// The worked example and the accepted range, shown under the panel. The user should never
    /// have to guess the bounds, which is the thing the old typed argument got wrong.
    example: &'static str,
}

const THRESHOLD_PROMPT: ValuePrompt = ValuePrompt {
    title: "Auto-Compact — Threshold",
    question: "Fold the oldest turns into a summary once the transcript reaches this many tokens.",
    example: "Example: 150k — or 150000. Range: 100k to 700k.",
};

const PERCENT_PROMPT: ValuePrompt = ValuePrompt {
    title: "Auto-Compact — Percentage",
    question: "Percentage of the transcript context to fold away into a summary when threshold is reached.",
    example: "Example: 50 — Range: 1% to 90% (e.g. 50 folds half, keeping half).",
};

/// Ask for one auto-compact value in a box, re-asking in place while it does not parse.
///
/// Returns `None` when the user backs out (Esc / Ctrl+C). A blank line is a real answer: the parse
/// functions read it as the default, and the box says so by showing the current value as the
/// placeholder the answer falls back to.
async fn prompt_value<F, A>(
    ui: &ChatUi,
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    spec: &ValuePrompt,
    current: usize,
    parse: F,
    advisory: A,
) -> Option<usize>
where
    F: Fn(&str) -> Result<usize, String>,
    A: Fn(&str, usize) -> Option<String>,
{
    // Anything typed before the box opened belongs to the composer, not to this question.
    while key_rx.try_recv().is_ok() {}

    let placeholder = current.to_string();
    let mut input = String::new();
    let mut error: Option<String> = None;

    let compute_advisory = |text: &str| -> Option<String> {
        let val_str = if text.is_empty() {
            placeholder.as_str()
        } else {
            text
        };
        parse(val_str).ok().and_then(|val| advisory(val_str, val))
    };
    let mut adv = compute_advisory(&input);

    // The question takes the screen, the way the approval gate and the clarify box do: the whole
    // frame is repainted on every keystroke, so a box that changes height (an error row, a longer
    // value) can never walk the cursor up the screen.
    ui.set_modal_active(true);
    ui.wait_for_paints().await;
    let screen = ModalScreen::enter();
    paint_centred(&render_value_box(
        spec,
        &input,
        &placeholder,
        error.as_deref(),
        adv.as_deref(),
        get_terminal_width().saturating_sub(2),
    ));

    let answer = loop {
        let key = match key_rx.recv().await {
            Some(key) => key,
            None => break None,
        };
        match key {
            AppKey::Enter => match parse(&input) {
                Ok(value) => break Some(value),
                Err(message) => {
                    error = Some(message);
                    adv = None;
                }
            },
            AppKey::Backspace => {
                input.pop();
                error = None;
                adv = compute_advisory(&input);
            }
            AppKey::Escape | AppKey::CtrlC | AppKey::Ctrl('c') => break None,
            AppKey::Char(c) if !c.is_control() => {
                input.push(c);
                error = None;
                adv = compute_advisory(&input);
            }
            _ => continue,
        }

        paint_centred(&render_value_box(
            spec,
            &input,
            &placeholder,
            error.as_deref(),
            adv.as_deref(),
            get_terminal_width().saturating_sub(2),
        ));
    };

    // Back on the transcript: the composer was never painted over, so there is nothing to restore.
    drop(screen);
    ui.set_modal_active(false);
    ui.redraw_box();
    answer
}

/// Render one value box: a rounded frame, the question, an inset panel holding what has been typed
/// (or the current value, dimmed, while the line is empty), the example, and the key hint.
///
/// Pure, and deliberately fixed-height: every row is present whatever the state, because the
/// callers repaint the whole frame — a box that grew when an error appeared would shift the block
/// off centre per keystroke.
fn render_value_box(
    spec: &ValuePrompt,
    input: &str,
    placeholder: &str,
    error: Option<&str>,
    advisory: Option<&str>,
    width: usize,
) -> String {
    let total = width.clamp(32, 76);
    let visible_len = |s: &str| dialoguer::console::strip_ansi_codes(s).chars().count();
    let cut = |s: &str, room: usize| -> String {
        if visible_len(s) > room {
            format!(
                "{}…",
                dialoguer::console::strip_ansi_codes(s)
                    .chars()
                    .take(room.saturating_sub(1))
                    .collect::<String>()
            )
        } else {
            s.to_string()
        }
    };

    let row = |content: &str| -> String {
        let max_content = total.saturating_sub(4);
        let vlen = visible_len(content);
        if vlen > max_content {
            let truncated = cut(content, max_content);
            let pad = max_content.saturating_sub(visible_len(&truncated));
            format!("│ {}{} │", truncated, " ".repeat(pad))
        } else {
            let pad = max_content - vlen;
            format!("│ {}{} │", content, " ".repeat(pad))
        }
    };
    let blank = row("");

    let inner_row = |payload: &str| -> String {
        let max_inner = total.saturating_sub(10);
        let truncated = cut(payload, max_inner);
        let pad = max_inner.saturating_sub(visible_len(&truncated));
        row(&format!(" │ {}{} │ ", truncated, " ".repeat(pad)))
    };
    let inner_top = |label: &str| -> String {
        let dashes = total.saturating_sub(label.len() + 11);
        row(&format!(" ┌─ {} {}┐ ", label.cyan(), "─".repeat(dashes)))
    };
    let inner_bottom =
        || -> String { row(&format!(" └{}┘ ", "─".repeat(total.saturating_sub(8)))) };

    let title = spec.title;
    let dashes = total.saturating_sub(title.chars().count() + 5);
    let mut lines = Vec::new();
    lines.push(format!("╭─ {} {}╮", title.bold(), "─".repeat(dashes)));
    lines.push(blank.clone());

    for line in wrap_words(spec.question, total.saturating_sub(6)) {
        lines.push(row(&format!(" {}", line)));
    }
    lines.push(blank.clone());

    lines.push(inner_top("Value"));
    let shown = if input.is_empty() {
        placeholder.dimmed().to_string()
    } else {
        input.bold().to_string()
    };
    lines.push(inner_row(&format!("{} {}", "❯".cyan().bold(), shown)));
    lines.push(inner_bottom());

    lines.push(blank.clone());
    lines.push(row(&format!(" {}", spec.example.dimmed())));
    if let Some(message) = error {
        lines.push(row(&format!(
            " {} {}",
            "✖".red().bold(),
            cut(message, total.saturating_sub(8)).red()
        )));
    } else if let Some(message) = advisory {
        lines.push(row(&format!(" {}", cut(message, total.saturating_sub(6)))));
    } else {
        // Kept as an empty row rather than dropped: the height must not depend on the state.
        lines.push(blank.clone());
    }
    lines.push(blank.clone());
    lines.push(row(&format!(
        " {}",
        "Enter confirms • blank keeps the current one • Esc cancels".dimmed()
    )));
    lines.push(format!("╰{}╯", "─".repeat(total.saturating_sub(2))));

    // The frame's height is a property of the text (`lines().count()`), so no caller is handed a
    // second copy of it to keep in step.
    lines.join("\n") + "\n"
}

/// Break `text` into lines of at most `room` characters, on word boundaries where it can.
fn wrap_words(text: &str, room: usize) -> Vec<String> {
    let room = room.max(8);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{} {}", current, word)
        };
        if candidate.chars().count() > room && !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current = word.to_string();
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

#[cfg(test)]
mod value_box_tests {
    use super::*;

    fn widths_of(text: &str) -> Vec<usize> {
        text.lines()
            .map(|line| dialoguer::console::strip_ansi_codes(line).chars().count())
            .collect()
    }

    /// A popup that changes width wobbles as it repaints. Every row, at every terminal width, and
    /// with and without an error to show.
    #[test]
    fn the_value_box_holds_one_width_in_every_state() {
        let long_error = "threshold must be between 100000 and 700000 (got 50000)";
        for width in [24, 32, 40, 60, 76, 140] {
            for error in [None, Some(long_error)] {
                for input in ["", "150k", "not a number at all"] {
                    let text =
                        render_value_box(&THRESHOLD_PROMPT, input, "150000", error, None, width);
                    let widths = widths_of(&text);
                    assert!(
                        widths.iter().all(|&w| w == widths[0]),
                        "widths at width={width}, error={error:?}, input={input:?}: {widths:?}"
                    );
                    assert!(
                        widths[0] <= width.clamp(32, 76),
                        "box wider than the frame: {} > {width}",
                        widths[0]
                    );
                }
            }
        }
    }

    /// The example and the bounds are the reason this is a box and not an argument: they have to be
    /// on screen while the value is typed.
    #[test]
    fn the_value_box_carries_the_example_and_the_bounds() {
        let text = render_value_box(&THRESHOLD_PROMPT, "", "150000", None, None, 76);
        let plain = dialoguer::console::strip_ansi_codes(&text).to_string();
        assert!(plain.contains("Auto-Compact"));
        assert!(plain.contains("150k"));
        assert!(plain.contains("100k to 700k"));
        assert!(
            plain.contains("150000"),
            "the current value is shown: {plain}"
        );
        assert!(plain.contains("Esc cancels"));
    }

    /// A rejected value is answered inside the box, and the box says why instead of forgetting it.
    #[test]
    fn a_rejected_value_is_shown_in_the_box() {
        let clean = render_value_box(&PERCENT_PROMPT, "0", "50", None, None, 76);
        let failed = render_value_box(
            &PERCENT_PROMPT,
            "0",
            "50",
            Some("percentage must be between 1 and 90 (got 0)"),
            None,
            76,
        );
        let plain = dialoguer::console::strip_ansi_codes(&failed).to_string();
        assert!(plain.contains("must be between 1 and 90"));
        // Same height either way, so repainting the frame cannot shift the block on screen.
        assert_eq!(clean.lines().count(), failed.lines().count());
    }

    /// An advisory message appears cleanly in the box without changing frame height.
    #[test]
    fn an_advisory_message_holds_frame_height() {
        let clean = render_value_box(&PERCENT_PROMPT, "10", "50", None, None, 76);
        let advised = render_value_box(
            &PERCENT_PROMPT,
            "10",
            "50",
            None,
            Some("⚠ Warning: ≤10% frees very little context"),
            76,
        );
        let plain = dialoguer::console::strip_ansi_codes(&advised).to_string();
        assert!(plain.contains("Warning: ≤10%"));
        assert_eq!(clean.lines().count(), advised.lines().count());
    }

    /// Blank is an answer, not a missing one: the parsers behind the two boxes read it as the
    /// default, which is what the hint row promises.
    #[test]
    fn a_blank_line_keeps_the_default() {
        assert_eq!(
            harness_onboarding::compact_step::parse_threshold(""),
            Ok(150_000)
        );
        assert_eq!(harness_onboarding::compact_step::parse_percent(""), Ok(50));
        assert_eq!(harness_onboarding::compact_step::parse_turns(""), Ok(20));
    }

    /// The words a prompt is made of have to land inside the frame, however narrow it is. A single
    /// word longer than the room is left whole — the wrapper never splits a word; the row that
    /// prints it trims it to the frame instead.
    #[test]
    fn wrapping_never_exceeds_the_room_it_was_given() {
        for room in [8, 12, 30, 70] {
            for line in wrap_words(THRESHOLD_PROMPT.question, room) {
                assert!(
                    line.chars().count() <= room || !line.contains(' '),
                    "line longer than {room}: {line:?}"
                );
            }
        }
    }
}

#[cfg(test)]
mod path_tests {
    use super::looks_like_path;

    /// A command name and its argument are still a command, even when the argument is a path.
    #[test]
    fn commands_are_not_paths() {
        for line in [
            "/help",
            "/model",
            "/model gpt-5",
            "/dir /tmp",
            "/queue hello",
            "exit",
        ] {
            assert!(!looks_like_path(line), "{line:?} should be a command");
        }
    }

    /// Anything with a second separator in its first token is a path, and so is a quoted path
    /// whose first segment would otherwise look like a command.
    #[test]
    fn multi_segment_absolute_paths_are_paths() {
        for line in [
            "/home/janelle/notes.txt",
            "/var/log/luna/luna.log and explain it",
            "\"/home/me/a file with spaces.txt\"",
            "  /usr/local/bin/luna",
        ] {
            assert!(looks_like_path(line), "{line:?} should be a path");
        }
    }

    /// A bare slash opens the menu, and an unknown single word is still a mistyped command.
    #[test]
    fn a_bare_slash_is_not_a_path() {
        assert!(!looks_like_path("/"));
        assert!(!looks_like_path("   /   "));
        assert!(!looks_like_path("/definitely-not-a-command"));
    }

    /// A single-component absolute path that exists on disk is a path too, so `/tmp` reaches the
    /// agent instead of being rejected as an unknown command.
    #[cfg(unix)]
    #[test]
    fn an_existing_single_component_is_a_path() {
        assert!(looks_like_path("/tmp"));
        assert!(looks_like_path("/etc"));
    }
}

#[cfg(test)]
mod feature_toggle_tests {
    use super::*;

    /// `/memory off` and its siblings accept the words people actually type; a bare command is a
    /// status query, and anything else is a usage error rather than a silent no-op.
    #[test]
    fn feature_arguments_are_on_off_or_a_status_query() {
        assert_eq!(parse_feature_arg(None), Ok(None));
        assert_eq!(parse_feature_arg(Some("")), Ok(None));
        assert_eq!(parse_feature_arg(Some("status")), Ok(None));
        for on in ["on", "enable", "enabled", "true", "ON"] {
            assert_eq!(parse_feature_arg(Some(on)), Ok(Some(true)), "{on}");
        }
        for off in ["off", "disable", "disabled", "false", "OFF"] {
            assert_eq!(parse_feature_arg(Some(off)), Ok(Some(false)), "{off}");
        }
        assert_eq!(parse_feature_arg(Some("maybe")), Err(()));
    }

    /// Each toggle is independent: turning one off must not silently take the others with it.
    #[test]
    fn toggles_default_on_and_flip_independently() {
        let toggles = FeatureToggles::default();
        let all = [
            Feature::Memory,
            Feature::Notes,
            Feature::Skills,
            Feature::Mcp,
        ];
        for feature in all {
            assert!(toggles.enabled(feature), "{feature:?} starts on");
        }

        toggles.set(Feature::Memory, false);
        assert!(!toggles.enabled(Feature::Memory));
        assert!(toggles.enabled(Feature::Notes));
        assert!(toggles.enabled(Feature::Skills));
        assert!(toggles.enabled(Feature::Mcp));

        toggles.set(Feature::Memory, true);
        assert!(toggles.enabled(Feature::Memory));
    }

    /// The skill commands are the one part of the menu that belongs to a toggle: with skills off
    /// they disappear, while the built-in `/skills` command stays so they can be turned back on.
    #[test]
    fn disabled_skills_disappear_from_the_command_menu() {
        use harness_plugins::Skill;

        let skill = Skill {
            name: "review".to_string(),
            description: "Review a diff carefully.".to_string(),
            instructions: "Do the thing.".to_string(),
            file_path: std::path::PathBuf::from("review/SKILL.md"),
            command: None,
            timeout_secs: None,
            parameters: None,
        };
        let config = Config::default();

        let on = command_entries(&config, std::slice::from_ref(&skill), true);
        assert!(on.iter().any(|entry| entry.name == "/review"));
        assert!(on.iter().any(|entry| entry.name == "/skills"));

        let off = command_entries(&config, std::slice::from_ref(&skill), false);
        assert!(!off.iter().any(|entry| entry.name == "/review"));
        assert!(off.iter().any(|entry| entry.name == "/skills"));
        assert!(off.iter().any(|entry| entry.name == "/memory"));
        assert!(off.iter().any(|entry| entry.name == "/notes"));
    }
}
