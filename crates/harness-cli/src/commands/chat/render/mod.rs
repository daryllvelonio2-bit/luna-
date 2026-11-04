//! Pure rendering for the interactive transcript: how one tool call, one tool result, one
//! reasoning line and one activity line are turned into terminal text.
//!
//! Everything here is a function of its arguments — no terminal handles, no cursor movement, no
//! locks — so the transcript's appearance can be asserted in tests without a pty.

use std::time::Duration;

use colored::Colorize;
use harness_core::{ApprovalRequest, Role, StoredMessage, TurnToolCall};

/// Formatted tool execution lines for a single tool call within a turn's process.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnProcessItem {
    pub call_formatted: String,
    pub result_formatted: String,
    pub diff_lines: Vec<String>,
}

/// A completed turn in the chat session, holding its query, intermediate reasoning, tool calls, and final reply.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnRecord {
    pub user_query: String,
    pub reasoning: Option<String>,
    pub tools: Vec<TurnToolCall>,
    pub tool_items: Vec<TurnProcessItem>,
    pub duration: Duration,
    pub reply: String,
}

/// Extract user guidance from an out-of-band user message envelope.
fn extract_out_of_band_steer(content: &str) -> Option<String> {
    const START_TAG: &str = "[OUT-OF-BAND USER MESSAGE";
    const END_TAG: &str = "[/OUT-OF-BAND USER MESSAGE]";
    let start_pos = content.find(START_TAG)?;
    let after_start = &content[start_pos..];
    let body_start = if let Some(newline_pos) = after_start.find('\n') {
        start_pos + newline_pos + 1
    } else if let Some(bracket_pos) = after_start.find(']') {
        start_pos + bracket_pos + 1
    } else {
        start_pos + START_TAG.len()
    };
    let body = if let Some(end_pos) = content.find(END_TAG) {
        if end_pos >= body_start {
            &content[body_start..end_pos]
        } else {
            &content[body_start..]
        }
    } else {
        &content[body_start..]
    };
    Some(body.trim().to_string())
}

/// Clean up raw user queries stored in session history for human-readable transcript display.
///
/// Historical user messages stored in SQLite include attached image base64 data URLs
/// (which multimodal models require for vision), plan mode prompt banners, and temp file paths.
/// When replaying or displaying transcripts, these raw payloads must not be dumped as thousands
/// of lines of base64 text into the terminal.
pub fn clean_user_query_for_transcript(raw: &str) -> String {
    if raw.trim().is_empty() {
        return raw.to_string();
    }

    // 1. Strip plan mode instruction prefix if present
    let mut text = raw;
    if let Some(rest) = text.strip_prefix("[PLAN MODE ACTIVE:") {
        if let Some(end) = rest.find(']') {
            text = rest[end + 1..].trim_start_matches(['\r', '\n']);
        }
    }

    // 2. Strip saved file paths from markers if present: [image: ... saved to ...] -> [image: ...]
    let text = crate::commands::prompt_images::strip_image_saved_paths(text);

    // If there's no data URL, return trimmed text
    if !text.contains("data:image/") {
        return text.trim().to_string();
    }

    let has_image_marker = text.contains("[image");

    let mut in_data_url = false;
    let mut out_lines = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if is_whole_data_url(trimmed) {
            in_data_url = true;
            if !has_image_marker {
                out_lines.push("[image]".to_string());
            }
            continue;
        }

        if in_data_url {
            if is_base64_continuation(trimmed) {
                continue;
            } else {
                in_data_url = false;
            }
        }

        if line.contains("data:image/") {
            out_lines.push(replace_data_urls_in_line(line));
        } else {
            out_lines.push(line.to_string());
        }
    }

    let joined = out_lines.join("\n");
    collapse_blank_lines(&joined)
}

fn is_whole_data_url(trimmed: &str) -> bool {
    let stripped = trimmed.trim_matches(|c: char| "\"'`<>".contains(c));
    stripped.starts_with("data:image/")
        && stripped.contains(";base64,")
        && !stripped.chars().any(char::is_whitespace)
}

fn is_base64_continuation(trimmed: &str) -> bool {
    let stripped = trimmed.trim_matches(|c: char| "\"'`<>".contains(c));
    !stripped.is_empty()
        && stripped.len() >= 4
        && stripped
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
}

fn replace_data_urls_in_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(start) = rest.find("data:image/") {
        let (before, after_start) = rest.split_at(start);
        out.push_str(before);
        let end = after_start
            .find(char::is_whitespace)
            .unwrap_or(after_start.len());
        let (token, tail) = after_start.split_at(end);
        if token.contains(";base64,") {
            let trimmed = token.trim_end_matches(|c: char| ".,;:!?)'\"`]}".contains(c));
            let punct = &token[trimmed.len()..];
            out.push_str("[image]");
            out.push_str(punct);
        } else {
            out.push_str(token);
        }
        rest = tail;
    }
    out.push_str(rest);
    out
}

fn collapse_blank_lines(text: &str) -> String {
    let mut result = Vec::new();
    let mut prev_blank = false;
    for line in text.lines() {
        let is_blank = line.trim().is_empty();
        if is_blank {
            if !prev_blank {
                result.push("");
                prev_blank = true;
            }
        } else {
            result.push(line);
            prev_blank = false;
        }
    }
    result.join("\n").trim().to_string()
}

/// Parse historical stored messages into structured `TurnRecord`s for collapsible rendering.
pub fn parse_turns_from_history(history: &[StoredMessage]) -> Vec<TurnRecord> {
    let mut turns = Vec::new();
    let mut current_query: Option<String> = None;
    let mut current_reasoning: Option<String> = None;
    let mut current_tools: Vec<TurnProcessItem> = Vec::new();
    let mut current_reply: Option<String> = None;

    for stored in history {
        let msg = &stored.message;
        match msg.role {
            Role::User => {
                if let Some(ref content) = msg.content {
                    if let Some(steer_text) = extract_out_of_band_steer(content) {
                        current_tools.push(TurnProcessItem {
                            call_formatted: format!(
                                "{} Steered into active turn: {}",
                                "⏩".cyan().bold(),
                                steer_text
                            ),
                            result_formatted: String::new(),
                            diff_lines: Vec::new(),
                        });
                        continue;
                    }
                }
                if let Some(q) = current_query.take() {
                    turns.push(TurnRecord {
                        user_query: q,
                        reasoning: current_reasoning.take(),
                        tools: Vec::new(),
                        tool_items: std::mem::take(&mut current_tools),
                        duration: Duration::ZERO,
                        reply: current_reply.take().unwrap_or_default(),
                    });
                }
                current_query = msg.content.as_deref().map(clean_user_query_for_transcript);
            }
            Role::Assistant => {
                if let Some(ref r) = msg.reasoning {
                    if let Some(ref mut cur_r) = current_reasoning {
                        cur_r.push('\n');
                        cur_r.push_str(r);
                    } else {
                        current_reasoning = Some(r.clone());
                    }
                }
                for call in &msg.tool_calls {
                    let diff_lines = if call.name == "patch" {
                        let old_str = call
                            .arguments
                            .get("old_string")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        let new_str = call
                            .arguments
                            .get("new_string")
                            .and_then(|v| v.as_str())
                            .unwrap_or("");
                        format_patch_diff(old_str, new_str)
                    } else {
                        Vec::new()
                    };
                    current_tools.push(TurnProcessItem {
                        call_formatted: format_tool_call(&call.name, &call.arguments),
                        result_formatted: String::new(),
                        diff_lines,
                    });
                }
                if let Some(ref content) = msg.content {
                    if !content.trim().is_empty() {
                        if let Some(ref mut cur_reply) = current_reply {
                            cur_reply.push_str("\n\n");
                            cur_reply.push_str(content);
                        } else {
                            current_reply = Some(content.clone());
                        }
                    }
                }
            }
            Role::Tool => {
                if let Some(ref res) = msg.tool_result {
                    let status = if res.is_error {
                        " (failed)".red().to_string()
                    } else {
                        " (success)".green().to_string()
                    };
                    if let Some(target) = current_tools.iter_mut().find(|it| {
                        !is_non_tool_event(&it.call_formatted) && !has_verdict(&it.call_formatted)
                    }) {
                        target.call_formatted.push_str(&status);
                        target.result_formatted.clear();
                    }
                }
            }
            Role::System => {}
        }
    }

    // A history that opens inside a turn — the replay window landed past the user message that
    // anchors it — still holds Luna's words. Dropping them would leave the resumed session
    // showing nothing at all, so the leftover becomes a turn of its own with no user line.
    if current_query.is_some()
        || current_reasoning.is_some()
        || !current_tools.is_empty()
        || current_reply.is_some()
    {
        turns.push(TurnRecord {
            user_query: current_query.unwrap_or_default(),
            reasoning: current_reasoning,
            tools: Vec::new(),
            tool_items: current_tools,
            duration: Duration::ZERO,
            reply: current_reply.unwrap_or_default(),
        });
    }

    turns
}

/// Renders all completed turns into the transcript.
/// When `expanded` is false, intermediate processes (reasoning + tools) collapse into a single line.
/// When `expanded` is true (toggled via Ctrl+O), full reasoning and tool logs are rendered inline.
pub fn render_all_turns(turns: &[TurnRecord], expanded: bool, width: usize) {
    for (i, turn) in turns.iter().enumerate() {
        if i > 0 {
            println!();
        }
        // A turn that carries no user line (the replay opened inside a turn) shows Luna's side
        // alone: there was no user message to put on the right.
        if !turn.user_query.trim().is_empty() {
            println!("{}", right_aligned_user_message(&turn.user_query, width));
        }

        let has_reasoning = turn
            .reasoning
            .as_ref()
            .map(|r| !r.trim().is_empty())
            .unwrap_or(false);
        let has_tools = !turn.tools.is_empty() || !turn.tool_items.is_empty();

        if has_reasoning || has_tools {
            println!();
            let tool_names: Vec<String> = if !turn.tools.is_empty() {
                turn.tools.iter().map(|t| t.name.clone()).collect()
            } else {
                turn.tool_items
                    .iter()
                    .filter_map(|it| {
                        let plain = dialoguer::console::strip_ansi_codes(&it.call_formatted);
                        if plain.starts_with('⏩')
                            || plain.starts_with('✖')
                            || plain.starts_with('✔')
                        {
                            None
                        } else {
                            plain.split_whitespace().nth(1).map(|s| s.to_string())
                        }
                    })
                    .collect()
            };
            let tools_count = if !turn.tools.is_empty() {
                turn.tools.len()
            } else {
                tool_names.len()
            };

            println!(
                "{}",
                format_process_header(
                    expanded,
                    tools_count,
                    &tool_names,
                    Some(turn.duration),
                    has_reasoning
                )
            );

            if expanded {
                if let Some(r) = &turn.reasoning {
                    if !r.trim().is_empty() {
                        for row in reasoning_rows(r, width) {
                            println!("{}", row);
                        }
                    }
                }
                for item in &turn.tool_items {
                    if !item.call_formatted.is_empty() {
                        let mut call = item.call_formatted.clone();
                        let plain = dialoguer::console::strip_ansi_codes(&call);
                        let is_non_tool_event = plain.starts_with('⏩')
                            || plain.starts_with('✖')
                            || plain.starts_with('✔');
                        if !has_verdict(&call) && !is_non_tool_event {
                            let is_err = item.result_formatted.contains('✖');
                            let status = if is_err {
                                " (failed)".red().to_string()
                            } else {
                                " (success)".green().to_string()
                            };
                            call.push_str(&status);
                        }
                        println!("{}", call);
                        for diff_line in &item.diff_lines {
                            println!("{}", diff_line);
                        }
                    }
                }
            }
        }

        if !turn.reply.is_empty() {
            println!();
            println!("{}\n{}", LUNA_LABEL.bold().green(), turn.reply);
        }
    }
}

/// Whether a formatted line is a non-tool event (steer, approval rejection, etc.)
pub fn is_non_tool_event(call: &str) -> bool {
    let plain = dialoguer::console::strip_ansi_codes(call);
    plain.starts_with('⏩') || plain.starts_with('✖') || plain.starts_with('✔')
}

/// Whether a formatted call already carries a verdict.
///
/// The verdict is written in colour, so the string ends in the reset escape rather than
/// in "(success)": comparing the *visible* text is what keeps a call from being
/// stamped with its verdict twice — once when the tool returned, once when the
/// transcript was drawn.
pub fn has_verdict(call: &str) -> bool {
    let plain = dialoguer::console::strip_ansi_codes(call);
    let trimmed = plain.trim_end();
    trimmed.ends_with("(success)") || trimmed.ends_with("(failed)")
}

/// The transcript.
///
/// The todo panel is not part of it: the panel is chrome, drawn above the composer, so
/// it stays put while the conversation scrolls under it.
pub fn render_transcript(turns: &[TurnRecord], expanded: bool, width: usize) {
    render_all_turns(turns, expanded, width);
}

/// The composer prompt. The `User >` label is gone, so Luna's crescent marks the input line
/// instead. U+263E LAST QUARTER MOON is a plain text glyph — terminals keep it
/// black-and-white, unlike the colour crescent U+1F319 which renders as an emoji.
pub const PROMPT: &str = "\u{263E} ";

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
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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

/// Format a clean single-line title from a subagent goal.
pub fn format_delegate_title(goal: &str) -> String {
    let first = goal
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(goal)
        .trim();
    if first.chars().count() > 50 {
        format!("{}…", first.chars().take(49).collect::<String>())
    } else {
        first.to_string()
    }
}

/// Format duration into compact human-readable string (e.g. "4s", "1m 12s").
pub fn format_delegate_duration(dur: std::time::Duration) -> String {
    let secs = dur.as_secs();
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Render a delegated task notification line:
/// "delegate: title > last tool call (total running time)"
pub fn format_delegate_notification(
    glyph: &str,
    title: &str,
    last_tool: &str,
    running_time: &str,
) -> String {
    format!("{glyph} delegate: {title} > {last_tool} ({running_time})")
}

/// What the activity line can say about a call beyond "still running": the size of the edit a
/// mutating tool is about to make, so a patch shows its shape while it is applied.
pub fn tool_activity_detail(name: &str, arguments: &serde_json::Value) -> Option<String> {
    let text = |key: &str| {
        arguments
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };

    match name {
        "patch" => {
            let removed = text("old_string").map(|s| s.lines().count()).unwrap_or(0);
            let added = text("new_string").map(|s| s.lines().count()).unwrap_or(0);
            Some(format!("−{} +{}", removed, added))
        }
        "write_file" => {
            let added = text("content").map(|c| c.lines().count()).unwrap_or(0);
            Some(format!("+{}", added))
        }
        "summarize" => Some("summarizing session...".to_string()),
        _ => None,
    }
}

/// Formats the diff between `old_string` and `new_string` for a patch tool call.
/// Truncates a formatted patch diff line to fit within `width` terminal cells so it never wraps in the live region.
pub fn fit_diff_line_to_width(line: &str, width: usize) -> String {
    let visible = dialoguer::console::measure_text_width(line);
    if visible <= width {
        return line.to_string();
    }
    let raw = dialoguer::console::strip_ansi_codes(line);
    let trimmed = raw.trim_start();
    let truncated_raw = truncate_to_cells(&raw, width);
    if trimmed.starts_with("- ") {
        format!(
            "\x1b[48;2;200;0;0m\x1b[38;2;255;255;255;1m{}\x1b[0m",
            truncated_raw
        )
    } else if trimmed.starts_with("+ ") {
        format!(
            "\x1b[48;2;0;160;0m\x1b[38;2;255;255;255;1m{}\x1b[0m",
            truncated_raw
        )
    } else {
        format!("\x1b[2m{}\x1b[0m", truncated_raw)
    }
}

/// Deletions are styled with a red background; additions are styled with a green background.
pub fn format_patch_diff(old_string: &str, new_string: &str) -> Vec<String> {
    if old_string.is_empty() && new_string.is_empty() {
        return Vec::new();
    }
    let diff = similar::TextDiff::from_lines(old_string, new_string);
    let mut lines = Vec::new();

    for change in diff.iter_all_changes() {
        let val = change.value().trim_end_matches(&['\r', '\n'][..]);
        match change.tag() {
            similar::ChangeTag::Delete => {
                // Pure red background with bold white text
                lines.push(format!(
                    "\x1b[48;2;200;0;0m\x1b[38;2;255;255;255;1m  - {}\x1b[0m",
                    val
                ));
            }
            similar::ChangeTag::Insert => {
                // Pure green background with bold white text
                lines.push(format!(
                    "\x1b[48;2;0;160;0m\x1b[38;2;255;255;255;1m  + {}\x1b[0m",
                    val
                ));
            }
            similar::ChangeTag::Equal => {
                lines.push(format!("\x1b[2m    {}\x1b[0m", val));
            }
        }
    }
    lines
}

/// Shortens any file or directory path to at most 3 path components (e.g. `downloads/rust-agent-harness/agents.md`).
pub fn shorten_path(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "." {
        return trimmed.to_string();
    }
    let parts: Vec<&str> = trimmed
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != "." && *s != "~")
        .collect();
    if parts.len() > 3 {
        parts[parts.len() - 3..].join("/")
    } else if parts.is_empty() {
        trimmed.to_string()
    } else if trimmed.starts_with('/') {
        format!("/{}", parts.join("/"))
    } else {
        parts.join("/")
    }
}

/// Scans any string and shortens any embedded path with more than 3 segments to at most 3 segments.
pub fn shorten_paths_in_text(raw: &str) -> String {
    let mut out = Vec::new();
    for token in raw.split(' ') {
        let clean =
            token.trim_matches(|c| c == '\'' || c == '"' || c == '`' || c == '(' || c == ')');
        if (clean.starts_with('/') || clean.starts_with("~/") || clean.contains('/'))
            && clean
                .split('/')
                .filter(|s| !s.is_empty() && *s != "~" && *s != ".")
                .count()
                > 3
        {
            let short = shorten_path(clean);
            out.push(token.replace(clean, &short));
        } else {
            out.push(token.to_string());
        }
    }
    out.join(" ")
}

/// The effective runtime paths, resolved once per process from the config file and the
/// environment.
///
/// The transcript renders tool calls by looking a record up in the stores it was written to, so
/// the paths have to be the *configured* ones (`paths.memory_db`, `paths.notes_dir`) — a hardcoded
/// `~/.luna` would show nothing for an install that moved its state elsewhere.
fn effective_paths() -> &'static harness_core::config::PathsSettings {
    static PATHS: std::sync::LazyLock<harness_core::config::PathsSettings> =
        std::sync::LazyLock::new(|| {
            harness_core::config::process_layers(harness_core::Config::default())
                .ok()
                .and_then(|layers| harness_core::config::resolve(&layers).ok())
                .map(|resolved| resolved.into_config().paths)
                .unwrap_or_default()
        });
    &PATHS
}

fn lookup_memory_from_db(id: &str) -> Option<(String, String)> {
    let db_path = effective_paths().memory_db.clone();
    if !db_path.exists() {
        return None;
    }
    let conn =
        rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    let mut stmt = conn
        .prepare("SELECT subject, predicate FROM memories WHERE id = ?1 LIMIT 1")
        .ok()?;
    let mut rows = stmt.query([id]).ok()?;
    if let Ok(Some(row)) = rows.next() {
        let subject: String = row.get(0).ok()?;
        let predicate: String = row.get(1).ok()?;
        return Some((subject, predicate));
    }
    None
}

/// The title of a note, by its id (file stem) or by its exact title.
///
/// File-backed notes are plain markdown, so the title comes from the front matter of
/// `<notes_dir>/<id>.md`; a title that is not a file name is matched by scanning the folder.
fn lookup_note_title(id: &str) -> Option<String> {
    let paths = effective_paths();
    let dir = &paths.notes_dir;

    let direct = dir.join(format!("{}.md", id));
    if direct.exists() {
        if let Some(title) = read_note_title(&direct) {
            return Some(title);
        }
    }

    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            if let Some(title) = read_note_title(&path) {
                if title.eq_ignore_ascii_case(id) {
                    return Some(title);
                }
            }
        }
    }

    // Legacy store, before the import ran (or for a note the import skipped).
    let db_path = if paths.notes_db.exists() {
        paths.notes_db.clone()
    } else {
        paths.notes_db.with_extension("db.imported")
    };
    if !db_path.exists() {
        return None;
    }
    let conn =
        rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .ok()?;
    let mut stmt = conn
        .prepare("SELECT title FROM notes WHERE id = ?1 OR title = ?1 COLLATE NOCASE LIMIT 1")
        .ok()?;
    let mut rows = stmt.query([id]).ok()?;
    if let Ok(Some(row)) = rows.next() {
        let title: String = row.get(0).ok()?;
        return Some(title);
    }
    None
}

/// The `title:` field of a note's front matter, if the file has one.
fn read_note_title(path: &std::path::Path) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    for line in raw.lines() {
        if line.trim() == "---" {
            // The front matter ended without a title.
            break;
        }
        if let Some((key, value)) = line.split_once(':') {
            if key.trim() == "title" {
                let title = value.trim().to_string();
                if !title.is_empty() {
                    return Some(title);
                }
            }
        }
    }
    None
}

fn resolve_memory_name(arguments: &serde_json::Value) -> String {
    let text = |key: &str| {
        arguments
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };

    if let Some(name) = text("name")
        .or_else(|| text("key"))
        .or_else(|| text("title"))
    {
        return format!("\"{}\"", truncate_visible(name, MAX_TOOL_ARG_CHARS));
    }

    let subj = text("subject");
    let pred = text("predicate");
    match (subj, pred) {
        (Some(s), Some(p)) => return format!("{}.{}", s, p),
        (Some(s), None) => return s.to_string(),
        _ => {}
    }

    if let Some(target) = text("target") {
        return format!("\"{}\"", truncate_visible(target, MAX_TOOL_ARG_CHARS));
    }

    if let Some(id) = text("memory_id").or_else(|| text("id")) {
        if let Some((sub, pred)) = lookup_memory_from_db(id) {
            if !pred.is_empty() {
                return format!("{}.{}", sub, pred);
            } else if !sub.is_empty() {
                return sub;
            }
        }
        let short = if id.len() > 16 { &id[..16] } else { id };
        return short.to_string();
    }

    "memory".to_string()
}

/// How wide the verb column of a tool-call line is, so targets line up across calls.
const VERB_COLUMN: usize = 12;

/// Emoji glyph and short verb per tool: the transcript reads as an action log
/// (`📖 read      chat.rs L500-524`) instead of a JSON argument dump.
fn tool_glyph_and_verb(name: &str) -> (&'static str, &'static str) {
    match name {
        "read_file" | "read" => ("📖", "read"),
        "write_file" | "write" => ("📝", "write"),
        "patch" | "edit" => ("✏️", "patch"),
        "search_files" | "grep" | "find" => ("🔍", "search"),
        "list_files" | "ls" => ("📂", "list"),
        "terminal" | "shell" | "bash" => ("💻", "run"),
        "save_note" | "read_note" | "update_note" | "delete_note" => ("📑", "note"),
        "search_notes" | "list_notes" => ("🔍", "notes"),
        "save_memory" | "update_memory" | "forget_memory" | "memory" => ("🧠", "memory"),
        "skill_view" | "skill" => ("📖", "skill"),
        "skills_list" => ("📋", "skills"),
        "clarify" => ("❓", "ask"),
        "process" => ("⚙️", "proc"),
        "delegate_task" => ("🤖", "delegate"),
        "apply_harness_update" => ("🚀", "update"),
        _ => ("🔧", ""),
    }
}

/// Render a tool call as one transcript line: glyph, a padded short verb, the target, and the
/// secondary detail the call itself implies (a read's line range, a patch's edit size).
pub fn format_tool_call(name: &str, arguments: &serde_json::Value) -> String {
    // Absent and empty are the same thing to a reader: a model that sends `"path": ""` means
    // "no path given", not "a path called nothing".
    let text = |key: &str| {
        arguments
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };
    let number = |key: &str| arguments.get(key).and_then(|v| v.as_u64());
    let suffix = |detail: String| format!(" {}", detail.dimmed());
    let quoted =
        |value: &str| format!("\"{}\"", truncate_visible(value, MAX_TOOL_ARG_CHARS)).cyan();

    let (glyph, verb) = tool_glyph_and_verb(name);
    // `{:width$}` pads but never cuts, so a long fallback name still renders in full.
    let verb_col = |label: &str| format!("{:<w$}", label, w = VERB_COLUMN).bold().to_string();

    match name {
        "terminal" | "shell" | "bash" => {
            if let Some(command) = text("command") {
                let first_line = command.lines().next().unwrap_or(command).trim();
                let short_cmd = shorten_paths_in_text(first_line);
                let cwd = text("workdir")
                    .map(|w| suffix(format!("in {}", shorten_path(w))))
                    .unwrap_or_default();
                return format!(
                    "{} {} {}{}",
                    glyph,
                    verb_col(verb),
                    truncate_visible(&short_cmd, MAX_TOOL_ARG_CHARS).bold(),
                    cwd
                );
            }
        }
        "read_file" | "read" => {
            if let Some(path) = text("path")
                .or_else(|| text("file"))
                .or_else(|| text("filepath"))
            {
                let short = shorten_path(path);
                let range = match (number("offset"), number("limit")) {
                    (Some(offset), Some(limit)) => {
                        suffix(format!("L{}-{}", offset, offset + limit.saturating_sub(1)))
                    }
                    (Some(offset), None) => suffix(format!("L{}+", offset)),
                    (None, Some(limit)) => suffix(format!("L1-{}", limit)),
                    _ => String::new(),
                };
                return format!("{} {} {}{}", glyph, verb_col(verb), short.cyan(), range);
            }
        }
        "list_files" | "ls" => {
            let path = text("path").unwrap_or(".");
            let short = shorten_path(path);
            let glob = text("glob")
                // `*` filters nothing, so it carries no information for the reader.
                .filter(|g| *g != "*")
                .map(|g| suffix(format!("matching {}", g)))
                .unwrap_or_default();
            return format!("{} {} {}{}", glyph, verb_col(verb), short.cyan(), glob);
        }
        "write_file" | "write" => {
            if let Some(path) = text("path")
                .or_else(|| text("file"))
                .or_else(|| text("filepath"))
            {
                let short = shorten_path(path);
                let lines = text("content").map(|c| c.lines().count()).unwrap_or(0);
                return format!(
                    "{} {} {}{}",
                    glyph,
                    verb_col(verb),
                    short.cyan(),
                    suffix(format!("{} lines", lines))
                );
            }
        }
        "patch" | "edit" => {
            if let Some(path) = text("path")
                .or_else(|| text("file"))
                .or_else(|| text("filepath"))
            {
                let short = shorten_path(path);
                let mut extra = tool_activity_detail("patch", arguments)
                    .map(suffix)
                    .unwrap_or_default();
                if matches!(
                    arguments.get("replace_all").and_then(|v| v.as_bool()),
                    Some(true)
                ) {
                    extra.push_str(&suffix("all occurrences".to_string()));
                }
                return format!("{} {} {}{}", glyph, verb_col(verb), short.cyan(), extra);
            }
        }
        "search_files" => {
            let pattern = text("pattern").unwrap_or("");
            let path = text("path").unwrap_or(".");
            let short = shorten_path(path);
            let mode = text("output_mode").unwrap_or("content");
            let mode_tag = if mode == "content" {
                String::new()
            } else {
                suffix(format!("({})", mode))
            };
            let include = text("file_glob")
                .filter(|glob| *glob != "*")
                .map(|g| suffix(format!("matching {}", g)))
                .unwrap_or_default();
            return format!(
                "{} {} {} in {}{}{}",
                glyph,
                verb_col(verb),
                quoted(pattern),
                short.cyan(),
                mode_tag,
                include
            );
        }
        "save_memory" | "update_memory" | "forget_memory" | "memory" => {
            let action = match name {
                "save_memory" => "save",
                "update_memory" => "update",
                "forget_memory" => "forget",
                _ => text("action").unwrap_or("update"),
            };
            let verb_label = format!("memory {}", action);
            let mem_name = resolve_memory_name(arguments);
            let detail = if action == "save" || action == "update" {
                if let Some(obj) = text("object") {
                    format!("{}{}", " = ".dimmed(), quoted(obj))
                } else {
                    String::new()
                }
            } else {
                String::new()
            };
            return format!(
                "{} {} {}{}",
                glyph,
                verb_col(&verb_label),
                mem_name.cyan(),
                detail
            );
        }
        "save_note" => {
            let title = text("title").unwrap_or("");
            let lines = text("content").map(|c| c.lines().count()).unwrap_or(0);
            return format!(
                "{} {} {}{}",
                glyph,
                verb_col(verb),
                quoted(title),
                suffix(format!("{} lines", lines))
            );
        }
        "search_notes" => {
            let query = text("query").unwrap_or("");
            let limit = number("limit")
                .map(|l| suffix(format!("limit {}", l)))
                .unwrap_or_default();
            return format!("{} {} {}{}", glyph, verb_col(verb), quoted(query), limit);
        }
        "skill_view" | "skill" => {
            let skill_name = text("name").or_else(|| text("skill")).unwrap_or("");
            return format!("{} {} {}", glyph, verb_col(verb), quoted(skill_name));
        }
        "skills_list" => {
            return format!("{} {} (all)", glyph, verb_col(verb));
        }
        "clarify" => {
            // The question is the clarify box's job, and it is on screen directly above this line:
            // echoing it here (`question="…"`) put the same paragraph in the transcript twice, and
            // a multi-question call dumped the whole `questions` array as raw JSON beside it. What
            // the transcript keeps is that a question was asked, and how many.
            let asked = arguments
                .get("questions")
                .and_then(|v| v.as_array())
                .filter(|list| !list.is_empty())
                .map(|list| list.len())
                .unwrap_or(1);
            let label = if asked == 1 {
                "1 question".to_string()
            } else {
                format!("{} questions", asked)
            };
            return format!("{} {}{}", glyph, verb_col(verb), suffix(label));
        }
        "process" => {
            let action = text("action").unwrap_or("list");
            let job_id = text("job_id")
                .map(|j| format!(" {}", j.cyan()))
                .unwrap_or_default();
            return format!("{} {}{}", glyph, verb_col(action), job_id);
        }
        "delegate_task" => {
            let action = text("action").unwrap_or("spawn");
            if action != "spawn" {
                let detail = text("task_id")
                    .map(|id| format!(" [{}]", id.cyan()))
                    .unwrap_or_default();
                return format!(
                    "{} {} ({}){}",
                    glyph,
                    verb_col(verb),
                    action.dimmed(),
                    detail
                );
            }
            let task_count = arguments
                .get("tasks")
                .and_then(|t| t.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            let first_goal = arguments
                .get("tasks")
                .and_then(|t| t.as_array())
                .and_then(|a| a.first())
                .and_then(|item| item.get("goal"))
                .and_then(|g| g.as_str())
                .unwrap_or("");
            let goal_preview = if task_count > 1 {
                format!(
                    "\"{}\" (+{} tasks)",
                    truncate_visible(first_goal, 30),
                    task_count - 1
                )
            } else {
                format!("\"{}\"", truncate_visible(first_goal, 50))
            };
            return format!("{} {} {}", glyph, verb_col(verb), goal_preview.cyan());
        }
        "list_notes" => {
            let limit = number("limit").unwrap_or(20);
            let offset = number("offset").unwrap_or(0);
            let detail = if offset > 0 {
                suffix(format!("limit {} offset {}", limit, offset))
            } else {
                suffix(format!("limit {}", limit))
            };
            return format!("{} {} (all){}", glyph, verb_col(verb), detail);
        }
        "read_note" | "update_note" | "delete_note" => {
            if let Some(target) = text("title").or_else(|| text("id")) {
                let display_title = lookup_note_title(target).unwrap_or_else(|| target.to_string());
                let action = match name {
                    "update_note" => suffix("(update)".to_string()),
                    "delete_note" => suffix("(delete)".to_string()),
                    _ => String::new(),
                };
                return format!(
                    "{} {} {}{}",
                    glyph,
                    verb_col(verb),
                    quoted(&display_title),
                    action
                );
            }
        }
        _ => {}
    }

    // MCP tools arrive as `mcp__server__tool`.
    let display_name = match name.strip_prefix("mcp__") {
        Some(stripped) => stripped.replace("__", "/"),
        None => name.to_string(),
    };

    // Fallback: `tool arg=value`, skipping nulls so unset optionals stay invisible.
    let args_str = match arguments.as_object() {
        Some(object) => {
            let parts: Vec<String> = object
                .iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| {
                    let value = match v {
                        serde_json::Value::String(s) => {
                            let first_line = s.lines().next().unwrap_or(s);
                            let shortened = shorten_paths_in_text(first_line);
                            format!("\"{}\"", truncate_visible(&shortened, MAX_TOOL_ARG_CHARS))
                        }
                        other => other.to_string(),
                    };
                    format!("{}={}", k, value)
                })
                .collect();
            if parts.is_empty() {
                String::new()
            } else {
                format!(" {}", parts.join(" "))
            }
        }
        None if arguments.is_null() => String::new(),
        None => format!(" {}", arguments),
    };

    format!("{} {}{}", glyph, verb_col(&display_name), args_str.dimmed())
}

/// Formats the single-line verdict when a human approves or cancels a tool call.
/// "Approved:" is styled in bold blue, file paths in cyan, and command arguments
/// are structured cleanly rather than dumped as raw unparsed blobs.
pub fn format_approval_verdict(is_approved: bool, request: &ApprovalRequest) -> String {
    let tag = if is_approved {
        format!("{} {}", "✔".blue().bold(), "Approved:".blue().bold())
    } else {
        format!("{} {}", "✖".red().bold(), "Cancelled:".red().bold())
    };

    let text_arg = |key: &str| {
        request
            .arguments
            .get(key)
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };

    let body = match request.tool.as_str() {
        "write_file" => {
            let path = request
                .target
                .as_deref()
                .or_else(|| text_arg("path"))
                .unwrap_or("file");
            let lines = text_arg("content").map(|c| c.lines().count()).unwrap_or(0);
            let suffix = if lines > 0 {
                format!(" {}", format!("(+{} lines)", lines).dimmed())
            } else {
                String::new()
            };
            format!("{} {}{}", "write".bold(), path.cyan().bold(), suffix)
        }
        "patch" => {
            let path = request
                .target
                .as_deref()
                .or_else(|| text_arg("path"))
                .unwrap_or("file");
            let detail = tool_activity_detail("patch", &request.arguments);
            let suffix = match detail {
                Some(d) => format!(" ({})", d.dimmed()),
                None => String::new(),
            };
            format!("{} {}{}", "patch".bold(), path.cyan().bold(), suffix)
        }
        "terminal" | "shell" | "bash" => {
            let cmd = request
                .target
                .as_deref()
                .or_else(|| text_arg("command"))
                .unwrap_or("");
            let first_line = cmd.lines().next().unwrap_or(cmd).trim();
            let display_cmd = truncate_visible(first_line, 65);
            let cwd = text_arg("workdir")
                .map(|w| format!(" {}", format!("(in {})", w).dimmed()))
                .unwrap_or_default();
            format!("{} {}{}", "$".bold().cyan(), display_cmd.bold(), cwd)
        }
        "save_note" => {
            let title = text_arg("title").unwrap_or("note");
            let lines = text_arg("content").map(|c| c.lines().count()).unwrap_or(0);
            let suffix = if lines > 0 {
                format!(" {}", format!("({} lines)", lines).dimmed())
            } else {
                String::new()
            };
            format!("{} \"{}\"{}", "note".bold(), title.cyan().bold(), suffix)
        }
        "update_note" | "delete_note" | "read_note" => {
            let id = request
                .target
                .as_deref()
                .or_else(|| text_arg("id"))
                .or_else(|| text_arg("title"))
                .unwrap_or("note");
            let display_name = lookup_note_title(id).unwrap_or_else(|| {
                if id.len() > 16 && id.starts_with("note_") {
                    format!("{}…", &id[..13])
                } else {
                    id.to_string()
                }
            });
            let action = match request.tool.as_str() {
                "update_note" => " (update)",
                "delete_note" => " (delete)",
                _ => "",
            };
            format!(
                "{} \"{}\"{}",
                "note".bold(),
                display_name.cyan().bold(),
                action.dimmed()
            )
        }
        "update_memory" | "forget_memory" | "save_memory" | "memory" => {
            let action = match request.tool.as_str() {
                "save_memory" => "save",
                "update_memory" => "update",
                "forget_memory" => "forget",
                _ => text_arg("action").unwrap_or("update"),
            };
            let target = request
                .target
                .as_deref()
                .or_else(|| text_arg("name"))
                .or_else(|| text_arg("subject"))
                .unwrap_or("memory");
            format!(
                "{} {} {}",
                "memory".bold(),
                action.bold(),
                target.cyan().bold()
            )
        }
        "apply_harness_update" => {
            let id = request
                .target
                .as_deref()
                .or_else(|| text_arg("patch_id"))
                .unwrap_or("patch");
            format!("{} {}", "update".bold(), id.cyan().bold())
        }
        other => {
            let target = request.target.as_deref().unwrap_or("-");
            let clean_target = truncate_visible(target, 60);
            format!("{} ({})", other.bold(), clean_target.cyan())
        }
    };

    format!("{} {}", tag, body)
}

/// Formats the single summary header for intermediate processes (reasoning + tools).
/// Collapsed by default: "▶ Process (N tools: ..., thought for X.Xs) · Ctrl+O to expand"
/// Expanded when toggled: "▼ Process (N tools: ..., thought for X.Xs) · Ctrl+O to collapse"
pub fn format_process_header(
    expanded: bool,
    tools_count: usize,
    tool_names: &[String],
    duration: Option<Duration>,
    has_reasoning: bool,
) -> String {
    let glyph = if expanded {
        "▼".cyan().bold()
    } else {
        "▶".cyan().bold()
    };

    let mut parts = Vec::new();

    if tools_count > 0 {
        let unique_names: Vec<String> = {
            let mut set = Vec::new();
            for name in tool_names {
                if !set.contains(name) {
                    set.push(name.clone());
                }
            }
            set
        };

        let tools_label = if tools_count == 1 {
            format!(
                "1 tool: {}",
                unique_names.first().map(|s| s.as_str()).unwrap_or("tool")
            )
        } else if unique_names.len() <= 2 {
            format!("{} tools: {}", tools_count, unique_names.join(", "))
        } else {
            format!(
                "{} tools: {}, {}, +{}",
                tools_count,
                unique_names[0],
                unique_names[1],
                unique_names.len() - 2
            )
        };
        parts.push(tools_label);
    }

    if let Some(d) = duration {
        if d.as_millis() >= 100 {
            parts.push(format!("thought for {:.1}s", d.as_secs_f32()));
        } else if has_reasoning {
            parts.push("thinking".to_string());
        }
    } else if has_reasoning {
        parts.push("thinking".to_string());
    }

    let meta = if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(" · "))
    };

    let hint = if expanded {
        " · Ctrl+O to collapse".dimmed()
    } else {
        " · Ctrl+O to expand".dimmed()
    };

    format!("{} {}{}{}", glyph, "Process".bold(), meta, hint)
}

/// Render a tool result as the line that closes a tool call: an error's first line, a short
/// result in full, or a first line plus a count for a long one.
pub fn format_tool_result(is_error: bool, output: &str) -> String {
    let trimmed = output.trim();
    if is_error {
        let first_line = trimmed.lines().next().unwrap_or(trimmed).trim();
        return format!(
            "  {} {}",
            "✖".red(),
            truncate_visible(first_line, MAX_TOOL_RESULT_CHARS).red()
        );
    }

    if trimmed.is_empty() {
        return format!("  {} {}", "✓".green(), "done".dimmed());
    }

    if !is_error && trimmed.starts_with('{') {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) {
            if let Some(title) = val.get("title").and_then(|t| t.as_str()) {
                let id_suffix = val
                    .get("id")
                    .and_then(|i| i.as_str())
                    .map(|id| format!(" ({})", id))
                    .unwrap_or_default();
                return format!(
                    "  {} Note \"{}\" saved{}",
                    "✓".green(),
                    title.bold(),
                    id_suffix.dimmed()
                );
            }
        }
    }

    let line_count = trimmed.lines().count();
    if line_count == 1 {
        return format!(
            "  {} {}",
            "✓".green(),
            truncate_visible(trimmed, MAX_TOOL_RESULT_CHARS).dimmed()
        );
    }

    if line_count <= MAX_INLINE_RESULT_LINES && trimmed.len() <= MAX_TOOL_RESULT_CHARS {
        let mut out = String::new();
        for (i, line) in trimmed.lines().enumerate() {
            if i == 0 {
                out.push_str(&format!("  {} {}\n", "✓".green(), line.dimmed()));
            } else {
                out.push_str(&format!("    {}\n", line.dimmed()));
            }
        }
        return out.trim_end().to_string();
    }

    let first = trimmed.lines().next().unwrap_or("").trim();
    format!(
        "  {} {} {}",
        "✓".green(),
        truncate_visible(first, MAX_TOOL_ARG_CHARS).dimmed(),
        format!("(+{} lines)", line_count - 1).dimmed()
    )
}

#[cfg(test)]
mod tests;
