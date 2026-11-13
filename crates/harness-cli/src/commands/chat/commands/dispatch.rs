//! The dispatcher: one table pairing each command name with the handler that answers it.
//!
//! `execute_slash_command` keeps the parsing and the match together, but every arm delegates to a
//! small private method on `SlashContext`, so a command can be read on its own. The methods are
//! grouped by what they touch under `dispatch/`.

use colored::Colorize;
use std::sync::Arc;
use tokio::sync::mpsc;

use harness_core::{Config, HarnessError, SqliteSessionStore};
use harness_providers::{CredentialManager, ModelDiscovery};

use crate::commands::chat::clear_on_exit;
use crate::commands::chat::connect;
use crate::commands::chat::keys::{self, AppKey};
use crate::commands::chat::render::TurnRecord;
use crate::commands::chat::status::refresh_antigravity_status_if_needed;
use crate::commands::chat::todo;
use crate::commands::chat::ui::ChatUi;
use crate::commands::chat::viewer;
use crate::commands::chat::ChatAgent;

use super::{
    command_entries, draw_transcript, handle_auto_compact_command, handle_bgsaver_model_command,
    handle_compact_command, handle_compact_model_switch, handle_default_model_switch,
    handle_delete_command, handle_diff_command, handle_dir_command, handle_feature_toggle,
    handle_fork_command, handle_goal_command, handle_model_switch, handle_name_command,
    handle_new_command, handle_reasoning_switch, handle_rewind_command, handle_sessions_switch,
    handle_settings_command, handle_summarize_and_new_command, handle_thinking_switch,
    handle_usage_command, parse_profile_command, profile_wizard, worker, Feature, FeatureToggles,
};

mod context;
mod feature;
mod misc;
mod model;
mod session;
mod ui;

/// What the dispatcher wants the chat loop to do after a slash command.
pub(crate) enum SlashOutcome {
    /// Command handled; keep the loop running.
    Continue,
    /// The user asked to leave the chat.
    Exit,
    /// A skill invocation: run this prompt as a normal turn, echoing `display` in the transcript.
    Turn { prompt: String, display: String },
    /// A `/goal` invocation: start an autonomous loop at iteration 1 and run the first turn.
    StartGoal {
        state: harness_core::GoalRunState,
        prompt: String,
        display: String,
    },
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn execute_slash_command(
    cmd: &str,
    // `None` means no key channel is wired to a composer (the TUI owns the terminal and reads its
    // own events). Interactive arms then report instead of waiting on a channel nobody writes to.
    key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
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
    active_goal: Option<&harness_core::GoalRunState>,
    bg_queue: Option<&harness_core::tasks::BackgroundTaskQueue>,
    delegate_tool: Option<&harness_core::tools::DelegateTaskTool>,
    // Clipboard images waiting to ride the next prompt. Worker invocations consume them the same
    // way a normal turn does, so `/carbon look at this` can carry a pasted screenshot.
    pending_images: Option<&Arc<std::sync::Mutex<Vec<String>>>>,
    display_settings: &Arc<std::sync::RwLock<harness_core::DisplaySettings>>,
) -> Result<SlashOutcome, HarnessError> {
    let parts: Vec<&str> = cmd.trim().splitn(2, ' ').collect();
    let name = parts[0];
    let arg = parts
        .get(1)
        .copied()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty());

    SlashContext {
        key_rx,
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
        skills_readout,
        skills,
        toggles,
        plan_mode,
        active_goal,
        bg_queue,
        delegate_tool,
        pending_images,
        display_settings,
    }
    .dispatch(cmd, name, arg)
    .await
}

/// Everything one slash command may touch, borrowed for the duration of the command.
struct SlashContext<'a> {
    key_rx: Option<&'a mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &'a ChatUi,
    config: &'a mut Config,
    cred_manager: &'a Arc<CredentialManager>,
    discovery: &'a Arc<ModelDiscovery>,
    agent: &'a Arc<tokio::sync::Mutex<ChatAgent>>,
    store: &'a Arc<SqliteSessionStore>,
    session_id: &'a mut String,
    current_provider: &'a mut String,
    current_model: &'a mut String,
    compact_provider: &'a mut Option<String>,
    compact_model: &'a mut Option<String>,
    turns: &'a mut Vec<TurnRecord>,
    process_expanded: &'a mut bool,
    todo_scope: &'a harness_core::tools::TodoScope,
    skills_readout: Option<&'a str>,
    skills: &'a [harness_plugins::Skill],
    toggles: &'a FeatureToggles,
    plan_mode: &'a Arc<std::sync::atomic::AtomicBool>,
    active_goal: Option<&'a harness_core::GoalRunState>,
    bg_queue: Option<&'a harness_core::tasks::BackgroundTaskQueue>,
    delegate_tool: Option<&'a harness_core::tools::DelegateTaskTool>,
    pending_images: Option<&'a Arc<std::sync::Mutex<Vec<String>>>>,
    display_settings: &'a Arc<std::sync::RwLock<harness_core::DisplaySettings>>,
}

impl<'a> SlashContext<'a> {
    async fn dispatch(
        &mut self,
        cmd: &str,
        name: &str,
        arg: Option<&str>,
    ) -> Result<SlashOutcome, HarnessError> {
        match name {
            "/default_model" | "/default-model" => self.cmd_default_model(arg).await,
            "/model" | "/models" | "/provider" => self.cmd_model().await,
            "/compact-model" => self.cmd_compact_model(arg).await,
            "/bgsaver-model" => self.cmd_bgsaver_model(arg).await,
            "/reasoning" => self.cmd_reasoning(arg).await,
            "/thinking" | "/think" => self.cmd_thinking(arg).await,
            "/connect" => self.cmd_connect().await,
            "/name" => self.cmd_name(arg).await,
            "/dir" | "/workspace" => self.cmd_dir(arg).await,
            "/sessions" | "/session" => self.cmd_sessions(arg).await,
            "/summarize" => self.cmd_summarize(arg).await,
            "/rewind" => self.cmd_rewind(arg).await,
            "/diff" => self.cmd_diff(arg).await,
            "/compact" => self.cmd_compact().await,
            "/auto-compact" => self.cmd_auto_compact(arg).await,
            "/new" => self.cmd_new(arg).await,
            "/expand" => self.cmd_expand().await,
            "/collapse" => self.cmd_collapse().await,
            "/fork" => self.cmd_fork(arg).await,
            "/todo" => self.cmd_todo().await,
            "/clear" => self.cmd_clear().await,
            "/skills" => self.cmd_skills(arg).await,
            "/memory" => self.cmd_memory(arg).await,
            "/notes" => self.cmd_notes(arg).await,
            "/remember" => self.cmd_remember().await,
            "/mcp" => self.cmd_mcp(arg).await,
            "/heartbeat" | "/hb" => self.cmd_heartbeat(arg).await,
            "/usage" => self.cmd_usage().await,
            "/delete" => self.cmd_delete().await,
            "/plan" => self.cmd_plan().await,
            "/goal" => self.cmd_goal(arg).await,
            "/tasks" | "/jobs" | "/subagents" => self.cmd_tasks().await,
            "/help" | "/commands" | "/?" => self.cmd_help().await,
            "/profile" | "/profiles" => self.cmd_profile(arg).await,
            "/worker" => self.cmd_worker(arg).await,
            "/settings" => self.cmd_settings(arg).await,
            "exit" | "quit" | "/exit" | "/quit" => self.cmd_exit().await,
            _ => self.cmd_fallback(cmd, name, arg).await,
        }
    }
}
