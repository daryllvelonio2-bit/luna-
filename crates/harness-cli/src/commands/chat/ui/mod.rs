//! The interactive composer and the live region above it.
//!
//! The implementation is split by concern so no one file carries the whole screen: terminal
//! plumbing, the live region's tail and panels, its measurements, formatting, and the `ChatUi`
//! methods grouped by what they paint or mutate.

mod chrome;
mod consts;
mod format;
mod input;
mod landing;
mod live;
mod metrics;
mod paint;
mod panels;
mod status;
mod tail;
mod terminal;
mod view;

/// Helpers shared by this crate's test modules.
///
/// The interfaces under test here draw with cursor moves rather than by writing a whole screen, so
/// the only way to see what they put where is to read the bytes they wrote.
#[cfg(test)]
pub(crate) mod test_support;

// Re-exports that keep the module paths the rest of the crate uses. The last few are only reached
// from the test module, which is why some are flagged as unused in a plain binary build.
#[allow(unused_imports)]
pub use consts::{ANIMATION_INTERVAL, LANDING_FRAME_INTERVAL, MIN_LIVE_REASONING_ROWS};
#[allow(unused_imports)]
pub(crate) use metrics::finished_reasoning_text;
#[allow(unused_imports)]
pub use metrics::{physical_line_count, visible_width};
pub use terminal::{get_terminal_height, get_terminal_width, RawTerminal};
pub use view::{ChatUi, TuiLiveSnapshot};

// Names the test module reaches through `use super::*`. They were private items of this module
// before the split and stay private to it.
#[cfg(test)]
use super::render::reasoning_rows;
#[cfg(test)]
use super::render::PROMPT;
#[cfg(test)]
use colored::Colorize;
#[cfg(test)]
use consts::{HINT_MIN_ROOM, IDLE_HINT};
#[cfg(test)]
use harness_core::tools::todo::TodoSnapshot;
#[cfg(test)]
use harness_providers::QuotaLine;
#[cfg(test)]
use metrics::{humanize_tokens, tool_tail_limit};
#[cfg(test)]
use panels::BoxDrawnMetrics;
#[cfg(test)]
use std::io::{self, Write};
#[cfg(test)]
use std::sync::atomic::Ordering;
#[cfg(test)]
use tail::TOOL_TAIL_LINES;

#[cfg(test)]
mod tests;
