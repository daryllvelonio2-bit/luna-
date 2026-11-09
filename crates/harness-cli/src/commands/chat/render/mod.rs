//! Pure rendering for the interactive transcript: how one tool call, one tool result, one
//! reasoning line and one activity line are turned into terminal text.
//!
//! Everything here is a function of its arguments — no terminal handles, no cursor movement, no
//! locks — so the transcript's appearance can be asserted in tests without a pty.

use std::time::Duration;

use colored::Colorize;
use harness_core::tasks::JobCompletion;
use harness_core::{ApprovalRequest, Role, StoredMessage, TurnToolCall};

/// The first line of the prompt the harness sends itself when a background job finishes.
///
/// It is a sentinel, not prose: [`clean_user_query_for_transcript`] drops a stored prompt that
/// starts with it, so replaying a session does not show a user message nobody typed.
pub const BACKGROUND_JOB_RESUME_PREFIX: &str = "[background job finished]";

/// How much of a finished job's output the stored system message carries. The log keeps the
/// rest; this is enough to see what happened without pinning a huge line in every replay.
const MAX_JOB_TAIL_CHARS: usize = 4_000;

/// Formatted tool execution lines for a single tool call within a turn's process.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnProcessItem {
    pub call_formatted: String,
    pub result_formatted: String,
    pub diff_lines: Vec<String>,
    /// A bounded copy of what the call printed, kept so Ctrl+O can show it after the live tail
    /// is gone. Bounded because a transcript lives for the whole session.
    pub output: String,
    /// Lines dropped from the front of `output` when it was bounded.
    pub output_dropped: usize,
}

/// How many lines of a finished tool's output the transcript retains for Ctrl+O.
const MAX_RETAINED_OUTPUT_LINES: usize = 40;

/// Hard cap on retained output characters, so a single-line payload cannot pin megabytes.
const MAX_RETAINED_OUTPUT_CHARS: usize = 8 * 1024;

/// Keep the newest lines of a tool result, plus a count of what was dropped.
///
/// The end of a build or test run is where the verdict is; the beginning is already represented by
/// [`format_tool_result`]'s first-line summary.
pub fn bounded_tool_output(output: &str) -> (String, usize) {
    let trimmed = output.trim_end();
    if trimmed.is_empty() {
        return (String::new(), 0);
    }

    let lines: Vec<&str> = trimmed.lines().collect();
    let start = lines.len().saturating_sub(MAX_RETAINED_OUTPUT_LINES);
    let mut kept = lines[start..].join("\n");
    let mut dropped = start;

    if kept.len() > MAX_RETAINED_OUTPUT_CHARS {
        // Trim from the front on a character boundary: the tail is what matters.
        let mut cut = kept.len() - MAX_RETAINED_OUTPUT_CHARS;
        while cut < kept.len() && !kept.is_char_boundary(cut) {
            cut += 1;
        }
        dropped += kept[..cut].matches('\n').count();
        kept = kept[cut..].to_string();
    }

    (kept, dropped)
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

    // A turn the harness started for itself is not something the user said. The prompt exists
    // only to carry the completion to the model; the transcript already has the notification.
    if raw.trim_start().starts_with(BACKGROUND_JOB_RESUME_PREFIX) {
        return String::new();
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
                            output: String::new(),
                            output_dropped: 0,
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
                    let diff_lines = if call.name == "patch"
                        || call.name == "edit"
                        || call.name == "apply_patch"
                    {
                        extract_patch_diff(&call.arguments)
                    } else {
                        Vec::new()
                    };
                    current_tools.push(TurnProcessItem {
                        call_formatted: format_tool_call(&call.name, &call.arguments),
                        result_formatted: String::new(),
                        diff_lines,
                        output: String::new(),
                        output_dropped: 0,
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
                        target.result_formatted = format_tool_result(res.is_error, &res.content);
                        (target.output, target.output_dropped) = bounded_tool_output(&res.content);
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
    for line in render_turn_lines(turns, expanded, width) {
        println!("{line}");
    }
}

/// The transcript as physical rows, one entry per terminal line.
///
/// [`render_all_turns`] prints this vector; the background-task viewer paints the same rows in
/// its own frame. Sharing the vector is what keeps a delegated child's conversation identical to
/// the parent CLI transcript instead of a second, log-shaped renderer.
pub fn render_turn_lines(turns: &[TurnRecord], expanded: bool, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for (i, turn) in turns.iter().enumerate() {
        if i > 0 {
            lines.push(String::new());
        }
        // A turn that carries no user line (the replay opened inside a turn) shows Luna's side
        // alone: there was no user message to put on the right.
        if !turn.user_query.trim().is_empty() {
            push_rows(
                &mut lines,
                &right_aligned_user_message(&turn.user_query, width),
            );
        }

        let has_reasoning = turn
            .reasoning
            .as_ref()
            .map(|r| !r.trim().is_empty())
            .unwrap_or(false);
        let has_tools = !turn.tools.is_empty() || !turn.tool_items.is_empty();

        if has_reasoning || has_tools {
            lines.push(String::new());
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

            push_rows(
                &mut lines,
                &format_process_header(
                    expanded,
                    tools_count,
                    &tool_names,
                    Some(turn.duration),
                    has_reasoning,
                ),
            );

            if expanded {
                if let Some(r) = &turn.reasoning {
                    if !r.trim().is_empty() {
                        for row in reasoning_rows(r, width) {
                            push_rows(&mut lines, &row);
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
                        push_rows(&mut lines, &call);
                        for diff_line in &item.diff_lines {
                            push_rows(&mut lines, diff_line);
                        }
                        // The retained result: a one-line verdict/summary always, then the bounded
                        // body when there is more to it. This is what keeps a failing build's last
                        // lines — and an MCP tool's output — available after the live tail clears.
                        if !item.result_formatted.is_empty() {
                            push_rows(&mut lines, &item.result_formatted);
                        }
                        if !item.output.is_empty() && item.output.lines().count() > 1 {
                            if item.output_dropped > 0 {
                                lines.push(format!(
                                    "  {} {}",
                                    "…".dimmed(),
                                    format!(
                                        "{} earlier line{}",
                                        item.output_dropped,
                                        if item.output_dropped == 1 { "" } else { "s" }
                                    )
                                    .dimmed()
                                ));
                            }
                            for row in item.output.lines() {
                                lines.push(format!(
                                    "  {} {}",
                                    "│".dimmed(),
                                    truncate_to_cells(row, width.saturating_sub(4).max(8)).dimmed()
                                ));
                            }
                        }
                    }
                }
            }
        }

        if !turn.reply.is_empty() {
            lines.push(String::new());
            push_rows(&mut lines, &LUNA_LABEL.bold().green().to_string());
            push_rows(&mut lines, reply_for_display(&turn.reply));
        }
    }
    lines
}

/// Add a rendered block as physical rows, so a block that already contains newlines does not make
/// the viewer's scroll arithmetic count one row where the terminal draws several.
fn push_rows(lines: &mut Vec<String>, text: &str) {
    lines.extend(text.split('\n').map(str::to_string));
}

/// The core stores a neutral placeholder when a model reasons but emits no text. `Ctrl+O` is this
/// host's binding, so the hint is added at render time instead of being written into the transcript
/// and sent back to the provider on every later turn.
pub fn reply_for_display(reply: &str) -> &str {
    if reply == harness_core::types::NO_TEXT_RESPONSE {
        "(The model completed reasoning but produced no text output. Press Ctrl+O to inspect reasoning.)"
    } else {
        reply
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

/// The moon drawn above the composer when a session has no transcript yet.
///
/// The crescent is deliberately made of block glyphs rather than ASCII punctuation: a fresh
/// session is the one screen with nothing else on it, and the denser shape reads as a mark
/// instead of stray characters. Quadrant blocks give the concave edge sub-cell steps, so the
/// curve stays smooth instead of turning into the square notch a half-block crescent leaves.
/// It is deliberately compact — thirteen cells wide and ten rows tall — so the mark hugs the
/// left margin of a wide terminal instead of filling it.
const LANDING_MOON: [&str; 10] = [
    "   ▗▄▟█████▀▀",
    " ▗▟█████▀▘",
    "▗█████▛▘",
    "██████",
    "█████▌",
    "█████▙",
    "▜█████▖",
    " ▜█████▙",
    "  ▝▜█████▙▄",
    "     ▝▀▀▀▀▀▀▀",
];

/// The wordmark that sits beside the moon on a wide terminal.
const LANDING_TITLE: &str = "L U N A";

/// Column the title starts at, measured from the left edge of the moon block.
const LANDING_TEXT_COL: usize = 17;

/// Cells kept empty between the left edge of the terminal and the mark.
const LANDING_LEFT_MARGIN: usize = 2;

/// Narrowest terminal that can hold the moon and its wordmark side by side.
///
/// One column wider than the lockup itself on purpose: a row that fills the terminal exactly sits
/// in the pending-wrap state, and the next newline moves twice. The animation moves the cursor by
/// the block's height, so a wrapped row would desynchronise every frame after it.
const LANDING_FULL_MIN_WIDTH: usize =
    LANDING_LEFT_MARGIN + LANDING_TEXT_COL + LANDING_TITLE.len() + 1;

/// Narrowest terminal that can hold the stacked moon and still leave the last column clear.
const LANDING_MOON_MIN_WIDTH: usize = LANDING_TEXT_COL + 1;

/// Narrower than this, even the compact wordmark does not fit with a margin.
const LANDING_COMPACT_MIN_WIDTH: usize = 12;

/// Return the rows of the fresh-session mark, or an empty vector when the terminal is too
/// small to say anything.
///
/// A brand-new session has no transcript to anchor the screen, so the first frame is the only
/// chance to show what the user has opened. This is printed once above the composer (not kept
/// as chrome), so it scrolls away with the first turn rather than being redrawn on every paint.
/// Two layouts are available so a narrow or short terminal still gets a recognisable mark:
/// the full moon-and-wordmark lockup and a stacked moon. Every layout is left-aligned, so the
/// mark grows out of the same margin Luna's replies use instead of floating in the far corner of
/// a wide terminal.
pub fn landing_rows(width: usize, available_height: usize) -> Vec<String> {
    if available_height == 0 {
        return Vec::new();
    }
    if width >= LANDING_FULL_MIN_WIDTH && available_height >= LANDING_MOON.len() {
        return left_align_rows(full_landing_rows(), width);
    }
    if width >= LANDING_MOON_MIN_WIDTH && available_height > LANDING_MOON.len() {
        return left_align_rows(stacked_landing_rows(), width);
    }
    if width >= LANDING_COMPACT_MIN_WIDTH {
        return left_align_rows(
            vec![format!("{}  {}", "☾".cyan().bold(), LANDING_TITLE.bold())],
            width,
        );
    }
    Vec::new()
}

/// The wide lockup: the moon on the left, the wordmark on its right.
fn full_landing_rows() -> Vec<String> {
    LANDING_MOON
        .iter()
        .enumerate()
        .map(|(index, art)| {
            let text = match index {
                1 => Some(LANDING_TITLE.cyan().bold().to_string()),
                _ => None,
            };
            landing_row(art, text.as_deref())
        })
        .collect()
}

/// The narrow lockup: the moon with its name underneath, stacked as one block.
fn stacked_landing_rows() -> Vec<String> {
    let mut rows: Vec<String> = LANDING_MOON
        .iter()
        .map(|art| art.white().bold().to_string())
        .collect();
    rows.push(LANDING_TITLE.cyan().bold().to_string());
    rows
}

/// One row of the wide lockup: the moon in place, then the text at its own column.
fn landing_row(art: &str, text: Option<&str>) -> String {
    let art = art.trim_end();
    let mut row = art.white().bold().to_string();
    if let Some(text) = text {
        let pad = LANDING_TEXT_COL.saturating_sub(dialoguer::console::measure_text_width(art));
        row.push_str(&" ".repeat(pad));
        row.push_str(text);
    }
    row
}

/// Visible width of a row, ignoring its colour codes.
fn visible_width(row: &str) -> usize {
    dialoguer::console::measure_text_width(&dialoguer::console::strip_ansi_codes(row))
}

/// Left-align every row of a block on the terminal margin, using the block's widest row so the
/// pieces keep their relative alignment.
///
/// The margin shrinks on a terminal too narrow to hold the block and the full margin, so the
/// narrow layouts still fit instead of spilling a column off the edge. Where there is room it
/// also keeps one cell clear on the right: a row that fills the terminal sits in the pending-wrap
/// state, and the animation walks the cursor back up by row count, so a wrapped row would
/// desynchronise every frame after it.
fn left_align_rows(rows: Vec<String>, width: usize) -> Vec<String> {
    let block_width = rows.iter().map(|row| visible_width(row)).max().unwrap_or(0);
    let spare = width.saturating_sub(block_width);
    let margin = LANDING_LEFT_MARGIN.min(spare.saturating_sub(1));
    rows.into_iter()
        .map(|row| format!("{}{}", " ".repeat(margin), row))
        .collect()
}

/// The glyph a falling star draws: U+2726 BLACK FOUR-POINTED STAR.
///
/// A plain text star, not an emoji, for the same reason the moon is block glyphs: the mark should
/// keep the terminal's own font all the way through rather than pulling in a colour emoji.
pub const LANDING_STAR: &str = "\u{2726}";

/// The small dot a meteor leaves behind it. U+00B7 is one cell in every terminal font that draws
/// the rest of the mark, and it reads as distance rather than as another falling star.
pub const LANDING_TRAIL: &str = "\u{00b7}";

/// Falling stars in one frame. Nine read as a shower — enough to feel alive, few enough that the
/// moon still reads as the subject rather than vanishing under a starfield.
pub const LANDING_STARS: usize = 9;

/// Cells of trail behind each falling star. Two is the shortest tail that reads as motion at the
/// animation tick instead of a second star travelling in formation.
pub const LANDING_TRAIL_CELLS: usize = 2;

/// Frames in one full loop of the shower. At the animation tick this is a little under three
/// seconds, and the shower keeps looping for as long as the fresh session stays on screen.
pub const LANDING_FALL_FRAMES: usize = 72;

/// How much faster (or slower) each successive star crosses the block. Different speeds pull the
/// shower apart over time, so the stars do not stay locked in one rigid diagonal.
const LANDING_STAR_SPEEDS: [f64; 5] = [0.65, 0.95, 0.75, 1.1, 0.85];

/// The mark with the shower overlaid at one point of its fall.
///
/// The stars enter at the top-right of the block and drop diagonally toward the lower-left, so
/// the eye follows a meteor across the moon instead of a cursor blinking inside it. Each star has
/// its own phase, speed and lane, which is what turns the group into a shower rather than one
/// head with a tail.
///
/// A meteor passes behind the wordmark instead of through it: a cell is only replaced when it is
/// blank or part of the moon, so the title keeps every letter. The eye still reads the path
/// because the star reappears below the text.
///
/// Every frame replaces characters rather than inserting them, which keeps every row exactly as
/// wide as the static mark: the caller rewrites the block in place, and a row that changed width
/// would leave the previous frame's tail behind it.
///
/// ANSI sequences in `rows` are copied through untouched, so each star takes on the colour of
/// whatever it is passing over — a white cut-out on the moon, cyan on the wordmark — and the rest
/// of the row keeps its own styling.
pub fn landing_star_rows(rows: &[String], frame: usize) -> Vec<String> {
    if rows.is_empty() {
        return Vec::new();
    }

    // Every row is padded to the block's width first: then a star always replaces a cell instead
    // of being appended, and every frame is the same width as the last one.
    let block_width = rows.iter().map(|row| visible_width(row)).max().unwrap_or(0);
    if block_width == 0 {
        return rows.to_vec();
    }

    // `&str` glyphs keep the heads and trails in one table so a meteor and a trail that land on
    // the same cell are deduplicated instead of overwriting one another.
    let mut slots: Vec<Vec<(usize, &'static str)>> = vec![Vec::new(); rows.len()];
    let stars = landing_star_positions(rows, frame);
    // Every head goes in before any trail. The layers are deduplicated by insertion order, and a
    // trail from an earlier star must not claim the cell a later star's head is about to occupy.
    for &(row, col) in &stars {
        let Some(row_text) = rows.get(row) else {
            continue;
        };
        // The star only lands on sky or on the moon. A letter is never replaced: the meteor
        // passes behind the wordmark and comes back out underneath it.
        if can_host_star(row_text, col) {
            slots[row].push((col, LANDING_STAR));
        }
    }
    for &(row, col) in &stars {
        let Some(row_text) = rows.get(row) else {
            continue;
        };
        if !can_host_star(row_text, col) {
            continue;
        }
        for step in 1..=LANDING_TRAIL_CELLS {
            let Some(trail_row) = row.checked_sub(step) else {
                break;
            };
            let trail_col = col.saturating_add(step);
            let Some(trail_text) = rows.get(trail_row) else {
                break;
            };
            if trail_col < block_width && can_host_star(trail_text, trail_col) {
                slots[trail_row].push((trail_col, LANDING_TRAIL));
            }
        }
    }
    rows.iter()
        .enumerate()
        .map(|(index, row)| {
            let mut row = row.clone();
            let width = visible_width(&row);
            row.push_str(&" ".repeat(block_width.saturating_sub(width)));

            let cells = &mut slots[index];
            // Stable sort: a meteor head is pushed before any trail, so it wins a cell two
            // stars both picked.
            cells.sort_by_key(|(col, _)| *col);
            cells.dedup_by_key(|(col, _)| *col);
            if cells.is_empty() {
                row
            } else {
                overlay_glyphs(&row, cells)
            }
        })
        .collect()
}

/// Where each falling star sits on `frame`, as `(row, visible column)` pairs.
///
/// Each star is a point on its own diagonal: `phase` spreads them along the fall, `speed` pulls
/// them apart over time, and `lane` shifts the whole track left so they do not all ride the same
/// line. `fract` sends a star that reaches the bottom back to the top-right, so the shower keeps
/// going for the whole loop instead of emptying out halfway through.
///
/// Exposed within the module so the tests can reason about individual tracks instead of scraping
/// glyphs back out of the rendered rows.
pub fn landing_star_positions(rows: &[String], frame: usize) -> Vec<(usize, usize)> {
    if rows.is_empty() {
        return Vec::new();
    }
    let last_row = rows.len() - 1;
    let block_width = rows.iter().map(|row| visible_width(row)).max().unwrap_or(0);
    if block_width == 0 {
        return Vec::new();
    }

    let cycle = LANDING_FALL_FRAMES.max(1);
    let progress = (frame % cycle) as f64 / cycle as f64;
    let base_start = block_width.saturating_sub(2);

    (0..LANDING_STARS)
        .map(|star| {
            let phase = star as f64 / LANDING_STARS as f64;
            let speed = LANDING_STAR_SPEEDS[star % LANDING_STAR_SPEEDS.len()];
            let t = (progress * speed + phase).fract();

            // Four lanes, two cells apart, each ending a little earlier than the one before it.
            let lane = (star % 4) * 2;
            let start_col = base_start.saturating_sub(lane);
            let end_col = (block_width / 4).max(1).saturating_sub(lane / 2).max(1);
            let span = start_col.saturating_sub(end_col) as f64;

            let row = (t * last_row as f64).round() as usize;
            let col = start_col.saturating_sub((span * t) as usize);
            (row.min(last_row), col)
        })
        .collect()
}

/// Whether the glyph at visible column `col` is the moon's body, which a star may cut into.
fn is_moon_block(ch: char) -> bool {
    ('\u{2580}'..='\u{259f}').contains(&ch)
}

/// Whether a star may replace the cell at `col` without destroying a letter.
///
/// Blank cells are sky, and block glyphs are the moon: a meteor cut-out there reads as the star
/// passing in front of the body. Everything else — the wordmark, punctuation — is left alone, so
/// the star passes behind the text and reappears below it. A column past the end of the row is
/// padded sky.
fn can_host_star(row: &str, col: usize) -> bool {
    let mut visible = 0usize;
    let mut chars = row.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }

        let width = dialoguer::console::measure_text_width(&ch.to_string()).max(1);
        if visible == col {
            if is_moon_block(ch) {
                return true;
            }
            if ch != ' ' {
                return false;
            }
            // A single space inside the wordmark is not open sky: a star there reads as
            // punctuation between two letters. The wide gaps before and after the text are.
            let blanks = blank_columns(row);
            let row_width = visible_width(row);
            let left_filled = col > 0 && !blanks.contains(&(col - 1));
            let right_blank = col + 1 < row_width && blanks.contains(&(col + 1));
            return !(left_filled && !right_blank);
        }
        if col < visible.saturating_add(width) {
            // The column lands inside a double-width glyph, so the whole cell is taken.
            return false;
        }
        visible = visible.saturating_add(width);
    }
    true
}

/// The visible columns of `row` that hold a space, ignoring ANSI sequences.
fn blank_columns(row: &str) -> Vec<usize> {
    let mut columns = Vec::new();
    let mut column = 0usize;
    let mut chars = row.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        let width = dialoguer::console::measure_text_width(&ch.to_string()).max(1);
        if ch == ' ' {
            columns.push(column);
        }
        column += width;
    }
    columns
}

/// Replace the cells at the visible columns in `cells` with their glyphs, leaving ANSI sequences
/// in place so each glyph takes on the styling of the text around it.
///
/// On a block glyph the cell is drawn in reverse video: filling the cell with the moon's own
/// colour turns the star into a cut-out instead of a black square punched through the mark. On
/// text and empty cells the plain glyph is invisible enough against the terminal background.
///
/// Widths are counted in terminal cells rather than bytes: the moon is block glyphs, which are
/// several bytes each, and slicing by byte would land in the middle of one. `cells` is sorted and
/// deduplicated by the caller; an out-of-range column appends rather than panics so a caller that
/// miscalculates still draws something.
fn overlay_glyphs(row: &str, cells: &[(usize, &str)]) -> String {
    let mut out = String::with_capacity(row.len() + 2 * cells.len());
    let mut visible = 0usize;
    let mut next = 0usize;
    let mut chars = row.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            out.push(ch);
            // Only CSI sequences reach these rows; copy one whole so its digits are never
            // mistaken for visible characters.
            if chars.peek() == Some(&'[') {
                out.push(chars.next().unwrap_or('['));
                for c in chars.by_ref() {
                    out.push(c);
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }

        let width = dialoguer::console::measure_text_width(&ch.to_string()).max(1);
        if let Some((_, glyph)) = cells.get(next).filter(|(col, _)| *col == visible) {
            // A block is part of the moon's body, so keep its cell filled and let the star read
            // as a cut-out. Anything else sits on the terminal background already.
            if is_moon_block(ch) {
                out.push_str("\x1b[7m");
                out.push_str(glyph);
                out.push_str("\x1b[27m");
            } else {
                out.push_str(glyph);
            }
            // A double-width glyph we replaced still occupies two cells: pad the second one so
            // the row keeps its visible width.
            if width > 1 {
                out.push_str(&" ".repeat(width - 1));
            }
            next += 1;
            visible += width;
            // The character under the star is dropped: this is a replacement, not an insertion.
            continue;
        }

        out.push(ch);
        visible += width;
    }

    for (_, glyph) in &cells[next..] {
        out.push_str(glyph);
    }
    out
}

/// Replace the cells at `cols` with [`LANDING_STAR`]. Kept as the narrow entry point for tests
/// that only need the falling layer.
#[cfg(test)]
fn overlay_stars(row: &str, cols: &[usize]) -> String {
    let cells: Vec<(usize, &str)> = cols.iter().map(|&col| (col, LANDING_STAR)).collect();
    overlay_glyphs(row, &cells)
}

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
    format_elapsed_secs(dur.as_secs())
}

/// The same compact duration, from whole seconds.
pub fn format_elapsed_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// One line of a shell command, bounded, for a job notice.
pub fn format_job_command(command: &str) -> String {
    let first = command
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(command)
        .trim();
    if first.chars().count() > 60 {
        format!("{}…", first.chars().take(59).collect::<String>())
    } else {
        first.to_string()
    }
}

/// Render the line that appears above the composer when a background job stops.
pub fn format_job_notification(
    glyph: &str,
    id: &str,
    command: &str,
    status: &str,
    elapsed: &str,
) -> String {
    format!(
        "{glyph} {id}: {} — {status}, {elapsed}",
        format_job_command(command)
    )
}

/// The stored system message a finished job leaves for the model.
///
/// It carries everything the model needs to decide what to do next without another `process
/// poll`: what ran, how it ended, how long it took, where the full log is, and the end of the
/// output. The first line is the resume sentinel so replay can recognise the turn it started.
pub fn format_job_system_message(completion: &JobCompletion) -> String {
    let tail = completion.output_tail.trim_end();
    let tail = if tail.is_empty() {
        "(no output)".to_string()
    } else {
        let chars: Vec<char> = tail.chars().collect();
        if chars.len() > MAX_JOB_TAIL_CHARS {
            let kept: String = chars[chars.len() - MAX_JOB_TAIL_CHARS..].iter().collect();
            format!(
                "… ({} earlier characters trimmed; see the log)\n{}",
                chars.len() - MAX_JOB_TAIL_CHARS,
                kept
            )
        } else {
            tail.to_string()
        }
    };
    format!(
        "{BACKGROUND_JOB_RESUME_PREFIX}\n\
         Background job {} stopped: {}\n\
         Command: {}\n\
         Duration: {}\n\
         Log: {}\n\
         \n\
         Output tail:\n{}",
        completion.id,
        completion.status,
        completion.command,
        format_elapsed_secs(completion.elapsed_secs),
        completion.log_path,
        tail,
    )
}

/// The prompt that starts the harness-owned turn reacting to a completion.
///
/// The system message carries the data; this only tells the model what to do with it. It is
/// hidden from replay by the sentinel prefix.
pub fn background_job_resume_prompt() -> String {
    format!(
        "{BACKGROUND_JOB_RESUME_PREFIX}\n\
         A background shell job you started has just stopped. Read its result above, then continue \
         the task: report what happened, or take the next step the result calls for. Do not repeat \
         work that already finished."
    )
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
    let text_any = |keys: &[&str]| -> Option<&str> {
        for k in keys {
            if let Some(s) = arguments
                .get(*k)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                return Some(s);
            }
        }
        None
    };

    match name {
        "patch" | "edit" | "apply_patch" => {
            let old_str = text_any(&["old_string", "oldString", "old_str", "old"]);
            let new_str = text_any(&["new_string", "newString", "new_str", "new"]);
            if old_str.is_some() || new_str.is_some() {
                let removed = old_str.map(|s| s.lines().count()).unwrap_or(0);
                let added = new_str.map(|s| s.lines().count()).unwrap_or(0);
                Some(format!("−{} +{}", removed, added))
            } else if let Some(diff_str) = text_any(&["diff", "patch", "patchText"]) {
                let mut added = 0;
                let mut removed = 0;
                for line in diff_str.lines() {
                    if line.starts_with('+') && !line.starts_with("+++") {
                        added += 1;
                    } else if line.starts_with('-') && !line.starts_with("---") {
                        removed += 1;
                    }
                }
                Some(format!("−{} +{}", removed, added))
            } else {
                Some("−0 +0".to_string())
            }
        }
        "write_file" => {
            let added = text("content").map(|c| c.lines().count()).unwrap_or(0);
            Some(format!("+{}", added))
        }
        "summarize" => Some("summarizing session...".to_string()),
        _ => None,
    }
}

/// Extracts formatted diff lines from tool call arguments (handles old/new strings, diff/patchText,
/// and both snake_case and camelCase parameters).
pub fn extract_patch_diff(arguments: &serde_json::Value) -> Vec<String> {
    let text_any = |keys: &[&str]| -> Option<&str> {
        for k in keys {
            if let Some(s) = arguments
                .get(*k)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                return Some(s);
            }
        }
        None
    };

    let old_str = text_any(&["old_string", "oldString", "old_str", "old"]);
    let new_str = text_any(&["new_string", "newString", "new_str", "new"]);

    if let (Some(old), Some(new)) = (old_str, new_str) {
        return format_patch_diff(old, new);
    }

    if let Some(diff_str) = text_any(&["diff", "patch", "patchText"]) {
        return format_unified_diff(diff_str);
    }

    if let Some(old) = old_str {
        return format_patch_diff(old, "");
    }
    if let Some(new) = new_str {
        return format_patch_diff("", new);
    }

    Vec::new()
}

/// Formats a unified diff string with red deletions and green additions.
pub fn format_unified_diff(diff_str: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for line in diff_str.lines() {
        let val = line.trim_end_matches(&['\r', '\n'][..]);
        if val.starts_with("+++") || val.starts_with("---") {
            lines.push(format!("\x1b[2m    {}\x1b[0m", val));
        } else if val.starts_with('+') {
            lines.push(format!(
                "\x1b[48;2;0;160;0m\x1b[38;2;255;255;255;1m  + {}\x1b[0m",
                val.strip_prefix('+').unwrap_or(val).trim_start()
            ));
        } else if val.starts_with('-') {
            lines.push(format!(
                "\x1b[48;2;200;0;0m\x1b[38;2;255;255;255;1m  - {}\x1b[0m",
                val.strip_prefix('-').unwrap_or(val).trim_start()
            ));
        } else if val.starts_with("@@") {
            lines.push(format!("\x1b[2m  {}\x1b[0m", val));
        } else if !val.is_empty() {
            let content = val.strip_prefix(' ').unwrap_or(val);
            lines.push(format!("\x1b[2m    {}\x1b[0m", content));
        }
    }
    lines
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
    // `C:\...` — the drive counts as a component so the shortened form still says which disk.
    let drive = trimmed
        .as_bytes()
        .get(0..2)
        .filter(|head| head[0].is_ascii_alphabetic() && head[1] == b':')
        .map(|_| &trimmed[..2]);
    let parts: Vec<&str> = trimmed
        .split(['/', '\\'])
        .filter(|s| !s.is_empty() && *s != "." && *s != "~")
        .collect();
    if let Some(drive) = drive {
        let keep = 2.min(parts.len());
        return format!("{drive}/{}", parts[parts.len() - keep..].join("/"));
    }
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
        // A Windows path is only treated as one when it is absolute: a bare token with
        // backslashes is far more likely to be an escape sequence or regex than a path.
        let drive_absolute = clean
            .as_bytes()
            .get(0..3)
            .is_some_and(|head| head[0].is_ascii_alphabetic() && &head[1..] == b":\\");
        let looks_like_path = clean.starts_with('/')
            || clean.starts_with("~/")
            || clean.starts_with("~\\")
            || clean.starts_with(r"\\")
            || clean.contains('/')
            || drive_absolute;
        if looks_like_path
            && clean
                .split(['/', '\\'])
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
        "patch" | "edit" | "apply_patch" => ("✏️", "patch"),
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
        "patch" | "edit" | "apply_patch" => {
            let path_opt = text("path")
                .or_else(|| text("filePath"))
                .or_else(|| text("filepath"))
                .or_else(|| text("file"))
                .map(shorten_path)
                .or_else(|| {
                    let diff_str = text("diff")
                        .or_else(|| text("patch"))
                        .or_else(|| text("patchText"))?;
                    for line in diff_str.lines() {
                        if let Some(rest) = line.strip_prefix("--- ") {
                            let p = rest
                                .trim()
                                .trim_start_matches("a/")
                                .trim_start_matches("b/");
                            if !p.is_empty() && p != "/dev/null" {
                                return Some(shorten_path(p));
                            }
                        }
                        if let Some(rest) = line.strip_prefix("+++ ") {
                            let p = rest
                                .trim()
                                .trim_start_matches("a/")
                                .trim_start_matches("b/");
                            if !p.is_empty() && p != "/dev/null" {
                                return Some(shorten_path(p));
                            }
                        }
                    }
                    None
                });

            if let Some(short) = path_opt {
                let mut extra = tool_activity_detail("patch", arguments)
                    .map(suffix)
                    .unwrap_or_default();
                if matches!(
                    arguments
                        .get("replace_all")
                        .or_else(|| arguments.get("replaceAll"))
                        .and_then(|v| v.as_bool()),
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
            if let Some(status_str) = val.get("status").and_then(|s| s.as_str()) {
                if status_str == "updated" || status_str == "saved" {
                    let mem_id = val
                        .get("new_memory_id")
                        .or_else(|| val.get("memory_id"))
                        .and_then(|m| m.as_str())
                        .unwrap_or("");
                    let object = val.get("object").and_then(|o| o.as_str()).unwrap_or("");
                    let obj_str = if !object.is_empty() {
                        format!(": \"{}\"", truncate_visible(object, 40))
                    } else {
                        String::new()
                    };
                    return format!(
                        "  {} Memory \"{}\" {}{}",
                        "✓".green(),
                        mem_id.bold(),
                        status_str,
                        obj_str.dimmed()
                    );
                }
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
