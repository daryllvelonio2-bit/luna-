use super::super::render::reasoning_rows;
use super::consts::MIN_LIVE_REASONING_ROWS;
use super::panels::LiveRegion;
use super::tail::TOOL_TAIL_BUFFER_LINES;
use super::terminal::get_terminal_height;

pub fn visible_width(s: &str) -> usize {
    dialoguer::console::measure_text_width(s)
}

/// The thought rows the live region shows, given everything else the region carries and everything
/// the composer draws below it.
///
/// The whole block is shown while the model thinks: the live region is the only place reasoning is
/// visible at all, so a fixed tail is a fixed blind spot. The screen is the only bound, and when
/// the block cannot fit, the rows kept are the *newest* — the interesting end of a stream is its
/// end. `chrome` counts the rows drawn below the region, so a tall block pushes the transcript up
/// instead of pushing the prompt off the screen.
/// The rows the live tool tail may occupy: the default tail, or the whole buffer once expanded.
pub(super) fn tool_tail_limit(live: &LiveRegion) -> usize {
    if live.tool_output_expanded {
        TOOL_TAIL_BUFFER_LINES
    } else if let Some(limit) = live.block_limit {
        limit
    } else {
        TOOL_TAIL_BUFFER_LINES
    }
}

pub(super) fn reasoning_window(live: &LiveRegion, width: usize, chrome: usize) -> Vec<String> {
    let all = if live.reasoning.is_empty() {
        Vec::new()
    } else {
        reasoning_rows(&live.reasoning, width)
    };
    let height = get_terminal_height();
    let reserved = usize::from(live.activity.is_some())
        + usize::from(live.compaction.is_some())
        + live.tool_output.rows(width, tool_tail_limit(live)).len();
    let budget = height
        .saturating_sub(chrome + reserved + 1)
        .max(MIN_LIVE_REASONING_ROWS);
    let start = all.len().saturating_sub(budget);
    all[start..].to_vec()
}

/// The transcript's form of a finished thought block: every row, in order.
///
/// The live region is the only place reasoning is visible while it streams, so the transcript
/// keeps it whole rather than showing a tail behind a fold. The stored turn re-renders the same
/// block when Ctrl+O expands it.
pub(crate) fn finished_reasoning_text(rows: &[String]) -> String {
    rows.join("\n")
}

pub fn physical_line_count(visible_len: usize, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    if visible_len == 0 {
        1
    } else {
        visible_len.div_ceil(width)
    }
}

/// Shortens a token count for the status line: 0→"0", 999→"999", 12_345→"12.3k", 272_000→"272k".
/// Sub-k counts stay exact because that is the range where the digits still matter.
pub(super) fn humanize_tokens(count: u32) -> String {
    if count >= 1_000_000 {
        let m = count as f64 / 1_000_000.0;
        if count.is_multiple_of(1_000_000) {
            format!("{}M", (count / 1_000_000))
        } else {
            let rounded = (m * 10.0).round() / 10.0;
            if rounded.fract() == 0.0 {
                format!("{}M", rounded as u64)
            } else {
                format!("{:.1}M", rounded)
            }
        }
    } else if count >= 10_000 {
        let k = count as f64 / 1_000.0;
        if k >= 100.0 || count.is_multiple_of(1_000) {
            format!("{}k", k.round() as u64)
        } else {
            let rounded = (k * 10.0).round() / 10.0;
            if rounded.fract() == 0.0 {
                format!("{}k", rounded as u64)
            } else {
                format!("{:.1}k", rounded)
            }
        }
    } else {
        count.to_string()
    }
}
