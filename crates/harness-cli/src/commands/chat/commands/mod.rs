//! The slash-command surface: the table the picker lists, the dispatcher, and the handlers.
//!
//! This file is the facade. The table lives in `catalog`, the toggles in `features`, the
//! dispatcher in `dispatch`, and the handlers in submodules grouped by what they act on:
//! `models`, `session_ops`, `reasoning`, and the shared `value_prompt`.

mod bgsaver;
mod catalog;
mod dir;
mod dispatch;
mod features;
mod models;
mod picker;
mod profile_wizard;
mod reasoning;
mod session_ops;
mod usage;
mod value_prompt;
mod worker;

pub(crate) use catalog::looks_like_path;
#[allow(unused_imports)]
pub use catalog::{command_entries, CommandEntry, SlashCommand, SLASH_COMMANDS};
pub(crate) use dispatch::{execute_slash_command, SlashOutcome};
use features::parse_profile_command;
pub(crate) use features::{
    handle_feature_toggle, is_bare_feature_command, Feature, FeatureToggles,
};
pub(crate) use models::{
    handle_compact_model_switch, handle_default_model_switch, handle_model_switch,
};
pub(crate) use reasoning::{handle_reasoning_switch, handle_thinking_switch};
pub(super) use session_ops::draw_transcript;
pub(crate) use session_ops::{
    handle_auto_compact_command, handle_compact_command, handle_delete_command,
    handle_diff_command, handle_fork_command, handle_name_command, handle_new_command,
    handle_rewind_command, handle_sessions_switch, handle_summarize_and_new_command,
};

pub(crate) use bgsaver::handle_bgsaver_model_command;
pub(crate) use dir::handle_dir_command;
pub(crate) use picker::{input_from_rx, select_from_rx, select_slash_command};
pub(crate) use usage::handle_usage_command;
