use std::time::Duration;

use colored::Colorize;
use harness_core::{Message, Role, StoredMessage, ToolCall, TurnToolCall};

use super::blocks::{reasoning_rows, right_aligned_user_message, truncate_to_cells, LUNA_LABEL};
use super::diff::extract_patch_diff;
use super::notices::BACKGROUND_JOB_RESUME_PREFIX;
use super::results::{format_process_header, format_tool_result_for};
use super::tools::format_tool_call;

/// Formatted tool execution lines for a single tool call within a turn's process.
#[derive(Clone, Debug, PartialEq)]
pub struct TurnProcessItem {
    pub tool_name: String,
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
pub(crate) const MAX_RETAINED_OUTPUT_LINES: usize = 40;

/// Hard cap on retained output characters, so a single-line payload cannot pin megabytes.
pub(crate) const MAX_RETAINED_OUTPUT_CHARS: usize = 8 * 1024;

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
pub(crate) fn extract_out_of_band_steer(content: &str) -> Option<String> {
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

pub(crate) fn is_whole_data_url(trimmed: &str) -> bool {
    let stripped = trimmed.trim_matches(|c: char| "\"'`<>".contains(c));
    stripped.starts_with("data:image/")
        && stripped.contains(";base64,")
        && !stripped.chars().any(char::is_whitespace)
}

pub(crate) fn is_base64_continuation(trimmed: &str) -> bool {
    let stripped = trimmed.trim_matches(|c: char| "\"'`<>".contains(c));
    !stripped.is_empty()
        && stripped.len() >= 4
        && stripped
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=')
}

pub(crate) fn replace_data_urls_in_line(line: &str) -> String {
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

pub(crate) fn collapse_blank_lines(text: &str) -> String {
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
        match stored.message.role {
            Role::User => apply_user_message(
                &stored.message,
                &mut turns,
                &mut current_query,
                &mut current_reasoning,
                &mut current_tools,
                &mut current_reply,
            ),
            Role::Assistant => apply_assistant_message(
                &stored.message,
                &mut current_reasoning,
                &mut current_tools,
                &mut current_reply,
            ),
            Role::Tool => apply_tool_message(&stored.message, &mut current_tools),
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
        turns.push(build_turn(
            current_query.unwrap_or_default(),
            current_reasoning,
            current_tools,
            current_reply.unwrap_or_default(),
        ));
    }

    turns
}

/// One completed turn from the fragments accumulated so far.
fn build_turn(
    user_query: String,
    reasoning: Option<String>,
    tool_items: Vec<TurnProcessItem>,
    reply: String,
) -> TurnRecord {
    TurnRecord {
        user_query,
        reasoning,
        tools: Vec::new(),
        tool_items,
        duration: Duration::ZERO,
        reply,
    }
}

/// Fold a user message into the turn being built: an out-of-band steer joins the current turn's
/// process, and a real query closes the turn before opening the next one.
fn apply_user_message(
    msg: &Message,
    turns: &mut Vec<TurnRecord>,
    current_query: &mut Option<String>,
    current_reasoning: &mut Option<String>,
    current_tools: &mut Vec<TurnProcessItem>,
    current_reply: &mut Option<String>,
) {
    if let Some(content) = msg.content.as_deref() {
        if let Some(item) = steer_item(content) {
            current_tools.push(item);
            return;
        }
    }
    if let Some(q) = current_query.take() {
        turns.push(build_turn(
            q,
            current_reasoning.take(),
            std::mem::take(current_tools),
            current_reply.take().unwrap_or_default(),
        ));
    }
    *current_query = msg.content.as_deref().map(clean_user_query_for_transcript);
}

/// An out-of-band steer is part of the running turn, not a new one.
fn steer_item(content: &str) -> Option<TurnProcessItem> {
    let steer_text = extract_out_of_band_steer(content)?;
    Some(TurnProcessItem {
        tool_name: "steer".to_string(),
        call_formatted: format!(
            "{} Steered into active turn: {}",
            "⏩".cyan().bold(),
            steer_text
        ),
        result_formatted: String::new(),
        diff_lines: Vec::new(),
        output: String::new(),
        output_dropped: 0,
    })
}

/// Fold an assistant message into the turn: reasoning, tool calls, then reply text.
fn apply_assistant_message(
    msg: &Message,
    current_reasoning: &mut Option<String>,
    current_tools: &mut Vec<TurnProcessItem>,
    current_reply: &mut Option<String>,
) {
    if let Some(r) = &msg.reasoning {
        if let Some(cur_r) = current_reasoning {
            cur_r.push('\n');
            cur_r.push_str(r);
        } else {
            *current_reasoning = Some(r.clone());
        }
    }
    for call in &msg.tool_calls {
        current_tools.push(tool_call_item(call));
    }
    if let Some(content) = &msg.content {
        if !content.trim().is_empty() {
            if let Some(cur_reply) = current_reply {
                cur_reply.push_str("\n\n");
                cur_reply.push_str(content);
            } else {
                *current_reply = Some(content.clone());
            }
        }
    }
}

/// The formatted call for one stored tool call, with its diff when it is a patch tool.
fn tool_call_item(call: &ToolCall) -> TurnProcessItem {
    let diff_lines = if call.name == "patch" || call.name == "edit" || call.name == "apply_patch" {
        extract_patch_diff(&call.arguments)
    } else {
        Vec::new()
    };
    TurnProcessItem {
        tool_name: call.name.clone(),
        call_formatted: format_tool_call(&call.name, &call.arguments),
        result_formatted: String::new(),
        diff_lines,
        output: String::new(),
        output_dropped: 0,
    }
}

/// Attach a tool result to the call it answers, stamping its verdict once.
fn apply_tool_message(msg: &Message, current_tools: &mut [TurnProcessItem]) {
    let Some(res) = msg.tool_result.as_ref() else {
        return;
    };
    let status = if res.is_error {
        " (failed)".red().to_string()
    } else {
        " (success)".green().to_string()
    };
    if let Some(target) = current_tools
        .iter_mut()
        .find(|it| !is_non_tool_event(&it.call_formatted) && !has_verdict(&it.call_formatted))
    {
        target.call_formatted.push_str(&status);
        target.result_formatted =
            format_tool_result_for(Some(&target.tool_name), res.is_error, &res.content);
        (target.output, target.output_dropped) = bounded_tool_output(&res.content);
    }
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
        push_user_turn(&mut lines, turn, width);
        push_process_turn(&mut lines, turn, expanded, width);
        push_turn_reply(&mut lines, turn);
    }
    lines
}

/// The user's own message, right-aligned. A turn that carries no user line (the replay opened
/// inside a turn) shows Luna's side alone: there was no user message to put on the right.
fn push_user_turn(lines: &mut Vec<String>, turn: &TurnRecord, width: usize) {
    if !turn.user_query.trim().is_empty() {
        push_rows(lines, &right_aligned_user_message(&turn.user_query, width));
    }
}

/// The collapsed process header, plus the reasoning and tool log when expanded.
fn push_process_turn(lines: &mut Vec<String>, turn: &TurnRecord, expanded: bool, width: usize) {
    let has_reasoning = turn
        .reasoning
        .as_ref()
        .map(|r| !r.trim().is_empty())
        .unwrap_or(false);
    let has_tools = !turn.tools.is_empty() || !turn.tool_items.is_empty();
    if !(has_reasoning || has_tools) {
        return;
    }

    lines.push(String::new());
    let tool_names = turn_tool_names(turn);
    let tools_count = if !turn.tools.is_empty() {
        turn.tools.len()
    } else {
        tool_names.len()
    };

    push_rows(
        lines,
        &format_process_header(
            expanded,
            tools_count,
            &tool_names,
            Some(turn.duration),
            has_reasoning,
        ),
    );

    if expanded {
        push_expanded_process(lines, turn, width);
    }
}

/// The tool names the header summarises, from the structured calls or — for a replayed history
/// that only kept formatted lines — from the call text itself.
fn turn_tool_names(turn: &TurnRecord) -> Vec<String> {
    if !turn.tools.is_empty() {
        turn.tools.iter().map(|t| t.name.clone()).collect()
    } else {
        turn.tool_items
            .iter()
            .filter_map(|it| {
                let plain = dialoguer::console::strip_ansi_codes(&it.call_formatted);
                if plain.starts_with('⏩') || plain.starts_with('✖') || plain.starts_with('✔')
                {
                    None
                } else {
                    plain.split_whitespace().nth(1).map(|s| s.to_string())
                }
            })
            .collect()
    }
}

/// The expanded body of the process: the reasoning rows, then one block per tool call.
fn push_expanded_process(lines: &mut Vec<String>, turn: &TurnRecord, width: usize) {
    if let Some(r) = &turn.reasoning {
        if !r.trim().is_empty() {
            for row in reasoning_rows(r, width) {
                push_rows(lines, &row);
            }
        }
    }
    for item in &turn.tool_items {
        push_tool_item(lines, item, width);
    }
}

/// One tool call, its diff, its verdict line and the output retained for Ctrl+O.
fn push_tool_item(lines: &mut Vec<String>, item: &TurnProcessItem, width: usize) {
    if item.call_formatted.is_empty() {
        return;
    }
    let mut call = item.call_formatted.clone();
    if !has_verdict(&call) && !is_non_tool_event(&call) {
        let is_err = item.result_formatted.contains('✖');
        let status = if is_err {
            " (failed)".red().to_string()
        } else {
            " (success)".green().to_string()
        };
        call.push_str(&status);
    }
    push_rows(lines, &call);
    for diff_line in &item.diff_lines {
        push_rows(lines, diff_line);
    }
    // The retained result: a one-line verdict/summary always, then the bounded body when there
    // is more to it. This is what keeps a failing build's last lines — and an MCP tool's output
    // — available after the live tail clears.
    if !item.result_formatted.is_empty() {
        push_rows(lines, &item.result_formatted);
    }
    if !item.output.is_empty() && item.output.lines().count() > 1 {
        push_retained_output(lines, item, width);
    }
}

/// The bounded tail of a tool's output, under the folded line count.
fn push_retained_output(lines: &mut Vec<String>, item: &TurnProcessItem, width: usize) {
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

/// Luna's side of the turn: a blank line, the label, then the reply.
fn push_turn_reply(lines: &mut Vec<String>, turn: &TurnRecord) {
    if !turn.reply.is_empty() {
        lines.push(String::new());
        push_rows(lines, &LUNA_LABEL.bold().green().to_string());
        push_rows(lines, reply_for_display(&turn.reply));
    }
}

/// Add a rendered block as physical rows, so a block that already contains newlines does not make
/// the viewer's scroll arithmetic count one row where the terminal draws several.
pub(crate) fn push_rows(lines: &mut Vec<String>, text: &str) {
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
