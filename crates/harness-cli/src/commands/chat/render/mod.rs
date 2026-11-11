//! Pure rendering for the interactive transcript: how one tool call, one tool result, one
//! reasoning line and one activity line are turned into terminal text.
//!
//! Everything here is a function of its arguments — no terminal handles, no cursor movement, no
//! locks — so the transcript's appearance can be asserted in tests without a pty.

#[cfg(test)]
use colored::Colorize;
#[cfg(test)]
use harness_core::{ApprovalRequest, Role, StoredMessage};
#[cfg(test)]
use std::time::Duration;

mod blocks;
mod diff;
mod landing;
mod notices;
mod paths;
mod results;
mod tools;
mod transcript;

/// Re-exports keep every item reachable as `crate::commands::chat::render::X`; some are only
/// called from outside the binary's own paths, hence the unused-import allowance.
#[allow(unused_imports)]
pub use blocks::{
    activity_line, compaction_bar, compaction_line, compaction_report, needs_separator,
    reasoning_rows, right_aligned_user_message, truncate_to_cells, truncate_visible,
    user_block_width, wrap_rows, OutputBlockKind, COMPACTION_BAR_CELLS, COMPACTION_BAR_HOLD,
    COMPACTION_BAR_MIN_CELLS, COMPACTION_BAR_SWEEP, LIVE_CALL_MIN_CHARS, LUNA_LABEL,
    MAX_INLINE_RESULT_LINES, MAX_TOOL_ARG_CHARS, MAX_TOOL_RESULT_CHARS, MIN_COMPACTION_LABEL_CELLS,
    USER_BLOCK_MIN_CHARS, USER_BLOCK_PERCENT,
};
#[allow(unused_imports)]
pub use diff::{extract_patch_diff, format_patch_diff, format_unified_diff};
#[allow(unused_imports)]
pub use landing::{
    landing_rows, landing_star_positions, landing_star_rows, LANDING_FALL_FRAMES, LANDING_STAR,
    LANDING_STARS, LANDING_TRAIL, LANDING_TRAIL_CELLS, PROMPT,
};
#[allow(unused_imports)]
pub use notices::{
    background_job_resume_prompt, format_delegate_duration, format_delegate_notification,
    format_delegate_title, format_elapsed_secs, format_job_command, format_job_notification,
    format_job_system_message, BACKGROUND_JOB_RESUME_PREFIX,
};
#[allow(unused_imports)]
pub use paths::{shorten_path, shorten_paths_in_text};
#[allow(unused_imports)]
pub use results::{
    format_approval_verdict, format_process_header, format_tool_result, format_tool_result_for,
};
pub use tools::{format_tool_call, tool_activity_detail};
pub use transcript::{
    bounded_tool_output, clean_user_query_for_transcript, has_verdict, is_non_tool_event,
    parse_turns_from_history, render_all_turns, render_transcript, render_turn_lines,
    reply_for_display, TurnProcessItem, TurnRecord,
};

/// Helpers the rendering tests reach through the facade. They are internal to the module, so
/// they are re-exported at crate scope rather than widening the public API.
#[cfg(test)]
pub(crate) use blocks::SPINNER_FRAMES;
#[cfg(test)]
pub(crate) use landing::{
    can_host_star, overlay_stars, LANDING_COMPACT_MIN_WIDTH, LANDING_FULL_MIN_WIDTH,
    LANDING_LEFT_MARGIN, LANDING_MOON, LANDING_MOON_MIN_WIDTH, LANDING_TITLE,
};
#[cfg(test)]
pub(crate) use transcript::MAX_RETAINED_OUTPUT_LINES;

#[cfg(test)]
mod tests;
