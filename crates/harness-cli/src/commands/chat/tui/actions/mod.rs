//! Session actions, slash-command execution, turn lifecycle, and tab management for the TUI.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use harness_core::{
    ApprovalRequest, CompactionSetup, CompletionOptions, HarnessError, MemoryEngine, Message,
    ModelProvider, ReasoningEffort, SessionBinding, SessionStore, TurnOutcome,
};
use harness_providers::{GenericProviderClient, RotationStrategy};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use super::super::commands::{
    apply_auto_compact_values, apply_dir_choice, command_entries, connected_providers,
    execute_slash_command, format_settings_summary, handle_settings_command, list_keys_text,
    looks_like_path, parse_dir_args, provider_id_of, provider_items, reset_workspace, Feature,
    SlashOutcome, RESERVED_COMMAND_NAMES,
};
use super::super::helpers::{finish_slash_command, IdleMemoryPass};
use super::super::history::append_prompt_history;
use super::super::interactive::state::{Env, Flow};
use super::super::observers::agent_observer;
use super::super::render::{
    compaction_report, parse_turns_from_history, TurnProcessItem, TurnRecord,
};
use super::super::status::{refresh_antigravity_quota_owned, refresh_antigravity_status_if_needed};
use super::super::todo;
use super::super::turn::{
    cancel_running_turn, drain_queue, handle_composer_command, launch_internal_turn, launch_turn,
    turn_records, InterruptNotice,
};
use super::super::ui::ChatUi;
use super::markdown::strip_ansi;
use super::state::{
    ActiveModal, AgentMode, ApprovalReply, BackgroundSession, InputAction, LiveThinkingView,
    ModelPickerItem, ModelTarget, PaletteAction, ProviderPickerItem, SelectAction, SessionTab,
    SubagentUiEntry, SystemNotice, TuiState, TurnMeta,
};
use super::theme::{friendly_model_name, friendly_provider_name};
use super::trace_event;
use crate::commands::context_window::context_window;
use crate::commands::sessions::{resolve_session, SessionSelection};

pub mod approval;
pub mod composer;
pub mod modals_dispatch;
pub mod orchestrator;
pub mod pickers;
pub mod sessions;
pub mod settings;
pub mod subagents;
pub mod turn;
pub mod voice_actions;

#[cfg(test)]
mod tests;

pub use approval::*;
pub use composer::*;
pub use modals_dispatch::*;
pub use orchestrator::*;
pub use pickers::*;
pub use sessions::*;
pub use settings::*;
pub use subagents::*;
pub use turn::*;
pub use voice_actions::*;
