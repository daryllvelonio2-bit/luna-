use std::time::Duration;

use colored::Colorize;

use super::landing::PROMPT;
use super::transcript::clean_user_query_for_transcript;

/// Luna's reply label. Kept beside [`PROMPT`] so the two sides of the transcript are edited
/// in one place rather than hunting for scattered UI literals.
pub const LUNA_LABEL: &str = "Luna";

/// Longest a tool argument may render before it is ellipsised, so one call stays on one line.
pub const MAX_TOOL_ARG_CHARS: usize = 68;

/// Longest a tool result summary line may render before it is ellipsised.
pub const MAX_TOOL_RESULT_CHARS: usize = 120;

/// A result this short is printed in full; anything longer collapses to its first line plus
/// a line count so a large tool payload cannot flood the transcript.
pub const MAX_INLINE_RESULT_LINES: usize = 3;

/// Frames for the activity spinner. Braille cells animate smoothly in any terminal that has a
/// Braille-capable font, and they carry no colour, which keeps the transcript black-and-white.
pub(crate) const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Floor for the call text in a live activity row: on a very narrow terminal the call is still
/// shown, truncated, rather than the row collapsing to a bare spinner.
pub const LIVE_CALL_MIN_CHARS: usize = 12;

/// Floor for the compaction label. Lower than [`LIVE_CALL_MIN_CHARS`] because this row also carries
/// a bar and a reason: on a narrow terminal the label gives way before they do, and the row still
/// has to fit one physical line.
pub const MIN_COMPACTION_LABEL_CELLS: usize = 4;

/// Cells in the compaction sweep bar.
pub const COMPACTION_BAR_CELLS: usize = 14;
/// Cells of that bar lit at once. A wider head reads as a pulse; a narrower one disappears at a
/// glance between frames.
pub const COMPACTION_BAR_SWEEP: usize = 5;
/// Narrowest the bar may shrink to before it is dropped altogether: fewer cells than the lit run
/// and the bar says less than the words beside it.
pub const COMPACTION_BAR_MIN_CELLS: usize = 6;
/// Frames each step of the sweep holds. The bar moves one cell per two ticks, so the eye follows a
/// travelling highlight instead of a flicker.
pub const COMPACTION_BAR_HOLD: usize = 2;

/// Share of the terminal the user's own message may occupy. Staying below 100% is what puts the
/// message — including every wrapped row — visibly on the user's side of the transcript.
pub const USER_BLOCK_PERCENT: usize = 70;

/// Floor for that block on a narrow terminal, so the message does not become a one-word column.
pub const USER_BLOCK_MIN_CHARS: usize = 24;

/// One transcript block kind, tracked so blank lines land only between differing blocks
/// (user → reasoning, reasoning → tool) and never between repeats (tool → tool).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputBlockKind {
    None,
    User,
    Thinking,
    Tool,
    #[allow(dead_code)]
    Reply,
}

/// A blank line separates blocks of different kinds. Identical consecutive blocks stay
/// adjacent, and the first block of a turn is not preceded by one.
pub fn needs_separator(prev: OutputBlockKind, next: OutputBlockKind) -> bool {
    prev != OutputBlockKind::None && prev != next
}

/// Truncate `text` to at most `max` visible characters, marking the cut with an ellipsis.
pub fn truncate_visible(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut truncated: String = text.chars().take(max - 1).collect();
    truncated.push('…');
    truncated
}

/// Shortens `text` to at most `max` terminal *cells* — the unit the terminal actually renders.
///
/// `truncate_visible` counts chars, but a char is not a cell: `💻` is one char and two cells.
/// A live-region row truncated by chars can print wider than the terminal, soft-wrap into a
/// second physical row, and desync the cursor arithmetic that erases the row on the next
/// animation frame — the live region then stacks one orphan frame per tick.
pub fn truncate_to_cells(text: &str, max: usize) -> String {
    if dialoguer::console::measure_text_width(text) <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let ch_cells = dialoguer::console::measure_text_width(&ch.to_string());
        if width + ch_cells > max.saturating_sub(1) {
            break;
        }
        out.push(ch);
        width += ch_cells;
    }
    out.push('…');
    out
}

/// Right-align the user's own message so the transcript reads as a chat — the user on the
/// right, Luna on the left. Padding is derived from the *visible* width, never the ANSI codes.
///
/// The message is wrapped here, to a block narrower than the terminal, rather than left to the
/// terminal's own soft wrap. A line wider than the terminal used to pad to zero (so it started at
/// column 0) and then wrap with its tail at column 0 as well — the continuation landed in the
/// same column as Luna's replies and read as assistant text. Wrapping to a block and right-aligning
/// *every* row keeps the whole message, wrap included, on the user's side.
pub fn right_aligned_user_message(query: &str, width: usize) -> String {
    let prompt = PROMPT.trim_end();
    if query.trim().is_empty() {
        let pad = width
            .saturating_sub(1)
            .saturating_sub(prompt.chars().count());
        return format!("{}{}", " ".repeat(pad), prompt.blue().bold());
    }

    let cleaned = clean_user_query_for_transcript(query);

    // Two columns of the block are held back so " ☾" always fits on the final row.
    let block = user_block_width(width)
        .saturating_sub(prompt.chars().count() + 1)
        .max(8);
    let mut rows = wrap_rows(&cleaned, block);

    if let Some(last) = rows.last_mut() {
        last.push(' ');
        last.push_str(prompt);
    }

    // Blue, not another attribute, because it is the only colour a user message carries: the
    // transcript is otherwise achromatic, so the whole message reads at a glance as the user's.
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let pad = width.saturating_sub(1).saturating_sub(row.chars().count());
        out.push(format!("{}{}", " ".repeat(pad), row.blue().bold()));
    }
    out.join("\n")
}

/// Widest the user's block may be: a fraction of the terminal, so a visible gap separates it from
/// the left column where Luna's replies live.
pub fn user_block_width(width: usize) -> usize {
    let scaled = width * USER_BLOCK_PERCENT / 100;
    scaled
        .max(USER_BLOCK_MIN_CHARS)
        .min(width.saturating_sub(1))
        .max(1)
}

/// Soft-wrap `text` to `width` columns, returning the visible rows. Wrapping here rather than
/// letting the terminal do it keeps every drawn row one physical row, which is what makes the
/// live region's cursor arithmetic correct.
pub fn wrap_rows(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in text.split('\n') {
        if line.is_empty() {
            rows.push(String::new());
            continue;
        }
        let mut row = String::new();
        for word in line.split_inclusive(' ') {
            // Cells, not chars: an emoji-bearing word is wider on the terminal than it is long,
            // and a row wrapped by chars would still soft-wrap physically and desync the live
            // region's cursor arithmetic.
            let word_width = dialoguer::console::measure_text_width(word);
            let row_width = dialoguer::console::measure_text_width(&row);
            if !row.is_empty() && row_width + word_width > width {
                rows.push(std::mem::take(&mut row));
            }
            // A single word longer than the row is hard-split so it cannot overflow the row.
            for ch in word.chars() {
                let ch_cells = dialoguer::console::measure_text_width(&ch.to_string());
                if dialoguer::console::measure_text_width(&row) + ch_cells > width {
                    rows.push(std::mem::take(&mut row));
                }
                row.push(ch);
            }
        }
        rows.push(row);
    }
    rows
}

/// The dimmed rendering of reasoning text as transcript rows.
pub fn reasoning_rows(text: &str, width: usize) -> Vec<String> {
    let wrap_width = width.saturating_sub(4).max(20);
    wrap_rows(text, wrap_width)
        .into_iter()
        .map(|row| format!("  {}", row.dimmed()))
        .collect()
}

/// One animation frame of the activity line: a spinner cell, the call itself, the elapsed time,
/// and whatever detail the tool knows about the work (a patch's line deltas).
///
/// The call is part of the line — not just its result — so the invocation is visible from the
/// instant it starts instead of appearing only once the tool has already finished. The call is
/// measured and truncated in *visible* characters because this row is drawn inside the live
/// region, where a row that wraps past the terminal width breaks the cursor arithmetic.
pub fn activity_line(
    frame: usize,
    call: &str,
    detail: Option<&str>,
    elapsed: Duration,
    width: usize,
) -> String {
    let glyph = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()].cyan();
    let seconds = format!("{:.1}s", elapsed.as_secs_f32());
    let tail = match detail {
        Some(detail) => format!("{} {}", detail, seconds),
        None => seconds,
    };
    // "  ", the spinner cell, then " <call> <tail>" — everything except the call itself.
    // The glyph is 2 cells on the terminal but 1 char, so the budget is measured in cells
    // (measure_text_width) throughout: a row wider than the terminal soft-wraps into a second
    // physical row, and the erase then leaves one orphan frame behind per animation tick.
    let fixed = 3 + 1 + dialoguer::console::measure_text_width(&tail) + 1;
    let budget = width.saturating_sub(fixed).max(LIVE_CALL_MIN_CHARS);
    let plain = dialoguer::console::strip_ansi_codes(call).to_string();
    let label = if plain.trim().is_empty() {
        "running".to_string()
    } else {
        truncate_to_cells(&plain, budget)
    };
    format!("  {} {} {}", glyph, label.dimmed(), tail.dimmed())
}

/// One frame of the compaction sweep bar: a track of `cells` with [`COMPACTION_BAR_SWEEP`] cells
/// lit, travelling left to right and wrapping around the right edge.
///
/// Returned as plain text — the lit cells are `█` and the rest `░` — so the caller styles it and a
/// test can assert on the shape without stripping colour. The head walks one cell every
/// [`COMPACTION_BAR_HOLD`] frames and starts off the left edge, so the first frames show the bar
/// filling in rather than a highlight already mid-track.
pub fn compaction_bar(frame: usize, cells: usize) -> String {
    if cells == 0 {
        return String::new();
    }
    let sweep = COMPACTION_BAR_SWEEP.min(cells);
    let head = (frame / COMPACTION_BAR_HOLD.max(1)) % cells;
    (0..cells)
        .map(|cell| {
            // Distance ahead of the head, wrapping: the head leads and the sweep trails it.
            let offset = (cell + cells - head) % cells;
            if offset < sweep {
                '█'
            } else {
                '░'
            }
        })
        .collect()
}

/// One animation frame of the compaction line: a spinner, what is being folded and why, the sweep
/// bar, and the elapsed time.
///
/// Compaction is the one operation that both rewrites the transcript under the user and takes
/// seconds to do, so it gets its own row: the tool spinner is for work the user can see the shape
/// of, and a fold has none. The row is budgeted in cells for the same reason as [`activity_line`] —
/// wrapping inside the live region breaks the erase arithmetic.
pub fn compaction_line(frame: usize, forced: bool, elapsed: Duration, width: usize) -> String {
    // Slower than the tool spinner: a fold is a long wait, and a fast spinner on a long wait reads
    // as a hang.
    let glyph = SPINNER_FRAMES[(frame / COMPACTION_BAR_HOLD) % SPINNER_FRAMES.len()].cyan();
    let why = if forced { "requested" } else { "automatic" };
    let when = format!("{:.1}s", elapsed.as_secs_f32());

    // The bar is the widest part of the row, so it is what gives way first on a narrow terminal: it
    // shrinks to the room left after the words, and is dropped when there is no room for it at all.
    // The words stay — a fold with no words is a mystery.
    let words = format!("{why} {when}");
    let bar_room = width.saturating_sub(6 + dialoguer::console::measure_text_width(&words));
    let bar_cells = if bar_room >= COMPACTION_BAR_MIN_CELLS {
        COMPACTION_BAR_CELLS.min(bar_room)
    } else {
        0
    };
    let tail = if bar_cells > 0 {
        format!("{}  {}", compaction_bar(frame, bar_cells).cyan(), words)
    } else {
        words
    };
    // "  ", the glyph cell, " <label> ", then the tail: measured in cells because this row lives in
    // the live region, where a wrap breaks the erase arithmetic.
    let fixed = 5 + dialoguer::console::measure_text_width(&tail);
    let budget = width.saturating_sub(fixed).max(MIN_COMPACTION_LABEL_CELLS);
    let label = truncate_to_cells("compacting context — folding the oldest turns", budget);
    format!("  {} {} {}", glyph, label.dimmed(), tail.dimmed())
}

/// The line a finished fold leaves in the transcript: what it folded, in the terms the fold itself
/// happened in. `.turns` is zero for a fold that took no whole turn — one huge message — and saying
/// "0 turns" there would read as if nothing had happened.
pub fn compaction_report(notice: &harness_core::CompactionNotice) -> String {
    let folded = if notice.turns > 0 {
        format!("{} turns ({} messages)", notice.turns, notice.messages)
    } else {
        format!("{} messages", notice.messages)
    };
    format!("{} Compacted {} into 1 summary", "✔".green().bold(), folded)
}
