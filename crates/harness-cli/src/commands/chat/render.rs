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
                current_query = msg.content.clone();
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
                    current_tools.push(TurnProcessItem {
                        call_formatted: format_tool_call(&call.name, &call.arguments),
                        result_formatted: String::new(),
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
                    if let Some(last) = current_tools.last_mut() {
                        if !last.call_formatted.ends_with("(success)") && !last.call_formatted.ends_with("(failed)") {
                            last.call_formatted.push_str(&status);
                        }
                        last.result_formatted.clear();
                    }
                }
            }
            Role::System if stored.is_summary => {
                if let Some(ref c) = msg.content {
                    turns.push(TurnRecord {
                        user_query: format!("• {}", c),
                        reasoning: None,
                        tools: Vec::new(),
                        tool_items: Vec::new(),
                        duration: Duration::ZERO,
                        reply: String::new(),
                    });
                }
            }
            _ => {}
        }
    }

    if let Some(q) = current_query {
        turns.push(TurnRecord {
            user_query: q,
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
        println!("{}", right_aligned_user_message(&turn.user_query, width));

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
                        it.call_formatted
                            .split_whitespace()
                            .nth(1)
                            .map(|s| s.to_string())
                    })
                    .collect()
            };
            let tools_count = turn.tools.len().max(turn.tool_items.len());

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
                        if !call.ends_with("(success)") && !call.ends_with("(failed)") {
                            let is_err = item.result_formatted.contains('✖');
                            let status = if is_err {
                                " (failed)".red().to_string()
                            } else {
                                " (success)".green().to_string()
                            };
                            call.push_str(&status);
                        }
                        println!("{}", call);
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

/// One transcript block kind, tracked so blank lines land only between differing blocks
/// (user → reasoning, reasoning → tool) and never between repeats (tool → tool).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputBlockKind {
    None,
    User,
    Thinking,
    Tool,
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

/// Left-align the user's message with a blue `User` label, so multi-line input reads
/// naturally from the left edge — Luna's reply sits directly below it.
pub fn right_aligned_user_message(query: &str, _width: usize) -> String {
    let label = "User >".blue().bold();
    let lines: Vec<&str> = query.lines().collect();
    if lines.is_empty() {
        return format!("{} {}", label, PROMPT.trim_end());
    }
    let mut out = Vec::new();
    for line in lines {
        out.push(format!("{} {}", label, line.bold()));
    }
    out.join("\n")
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
            let word_width = word.chars().count();
            if !row.is_empty() && row.chars().count() + word_width > width {
                rows.push(std::mem::take(&mut row));
            }
            // A single word longer than the row is hard-split so it cannot overflow the row.
            for ch in word.chars() {
                if row.chars().count() == width {
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

/// One animation frame of the activity line: a spinner cell, the operation still in flight,
/// the elapsed time, and whatever detail the tool knows about the work (a patch's line deltas).
pub fn activity_line(frame: usize, detail: Option<&str>, elapsed: Duration) -> String {
    let glyph = SPINNER_FRAMES[frame % SPINNER_FRAMES.len()].cyan();
    let seconds = format!("{:.1}s", elapsed.as_secs_f32());
    match detail {
        Some(detail) => format!("  {} {} {}", glyph, detail.dimmed(), seconds.dimmed()),
        None => format!("  {} {}", glyph, seconds.dimmed()),
    }
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

/// Shortens any file or directory path to at most 3 path components (e.g. `downloads/rust-agent-harness/agents.md`).
pub fn shorten_path(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "." {
        return trimmed.to_string();
    }
    let parts: Vec<&str> = trimmed
        .split(|c| c == '/' || c == '\\')
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
        let clean = token.trim_matches(|c| c == '\'' || c == '"' || c == '`' || c == '(' || c == ')');
        if (clean.starts_with('/') || clean.starts_with("~/") || clean.contains('/'))
            && clean.split('/').filter(|s| !s.is_empty() && *s != "~" && *s != ".").count() > 3
        {
            let short = shorten_path(clean);
            out.push(token.replace(clean, &short));
        } else {
            out.push(token.to_string());
        }
    }
    out.join(" ")
}

/// How wide the verb column of a tool-call line is, so targets line up across calls.
const VERB_COLUMN: usize = 12;

/// Emoji glyph and short verb per tool: the transcript reads as an action log
/// (`📖 read      chat.rs L500-524`) instead of a JSON argument dump.
fn tool_glyph_and_verb(name: &str) -> (&'static str, &'static str) {
    match name {
        "read_file" => ("📖", "read"),
        "write_file" => ("📝", "write"),
        "patch" => ("✏️", "patch"),
        "search_files" => ("🔍", "search"),
        "list_files" => ("📂", "list"),
        "terminal" | "shell" | "bash" => ("💻", "run"),
        "save_note" | "read_note" | "update_note" | "delete_note" => ("📑", "note"),
        "search_notes" | "list_notes" => ("🔍", "notes"),
        "save_memory" | "update_memory" | "forget_memory" => ("🧠", "memory"),
        "skill_view" => ("📖", "skill"),
        "skills_list" => ("📋", "skills"),
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
    let quoted = |value: &str| format!("\"{}\"", truncate_visible(value, MAX_TOOL_ARG_CHARS)).cyan();

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
        "read_file" => {
            if let Some(path) = text("path") {
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
        "list_files" => {
            let path = text("path").unwrap_or(".");
            let short = shorten_path(path);
            let glob = text("glob")
                // `*` filters nothing, so it carries no information for the reader.
                .filter(|g| *g != "*")
                .map(|g| suffix(format!("matching {}", g)))
                .unwrap_or_default();
            return format!("{} {} {}{}", glyph, verb_col(verb), short.cyan(), glob);
        }
        "write_file" => {
            if let Some(path) = text("path") {
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
        "patch" => {
            if let Some(path) = text("path") {
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
        "save_memory" => {
            let subject = text("subject").unwrap_or("?");
            let predicate = text("predicate").unwrap_or("?");
            let object = text("object").unwrap_or("?");
            return format!(
                "{} {} {}{}{}",
                glyph,
                verb_col(verb),
                format!("{}.{}", subject, predicate).cyan(),
                " = ".dimmed(),
                quoted(object)
            );
        }
        "update_memory" | "forget_memory" => {
            if let Some(id) = text("memory_id") {
                return format!("{} {} {}", glyph, verb_col(verb), id.cyan());
            }
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
        "skill_view" => {
            let skill_name = text("name").unwrap_or("");
            return format!("{} {} {}", glyph, verb_col(verb), quoted(skill_name));
        }
        "skills_list" => {
            return format!("{} {} (all)", glyph, verb_col(verb));
        }
        "process" => {
            let action = text("action").unwrap_or("list");
            let job_id = text("job_id").map(|j| format!(" {}", j.cyan())).unwrap_or_default();
            return format!("{} {}{}", glyph, verb_col(action), job_id);
        }
        "delegate_task" => {
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
                format!("\"{}\" (+{} tasks)", truncate_visible(first_goal, 30), task_count - 1)
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
                let action = match name {
                    "update_note" => suffix("(update)".to_string()),
                    "delete_note" => suffix("(delete)".to_string()),
                    _ => String::new(),
                };
                return format!("{} {} {}{}", glyph, verb_col(verb), quoted(target), action);
            }
        }
        "inspect_harness_source" => {
            if let Some(path) = text("path") {
                return format!("{} {} {}", glyph, verb_col(name), path.cyan());
            }
        }
        "propose_harness_patch" => {
            if let Some(path) = text("target_file") {
                let lines = text("content").map(|c| c.lines().count()).unwrap_or(0);
                return format!(
                    "{} {} {}{}",
                    glyph,
                    verb_col(name),
                    path.cyan(),
                    suffix(format!("{} lines", lines))
                );
            }
        }
        "apply_harness_update" => {
            if let Some(id) = text("patch_id") {
                return format!("{} {} {}", glyph, verb_col(verb), id.cyan());
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
                .unwrap_or("note");
            let short_id = if id.len() > 16 && id.starts_with("note_") {
                format!("{}…", &id[..13])
            } else {
                id.to_string()
            };
            let action = match request.tool.as_str() {
                "update_note" => " (update)",
                "delete_note" => " (delete)",
                _ => "",
            };
            format!("{} {}{}", "note".bold(), short_id.cyan().bold(), action.dimmed())
        }
        "update_memory" | "forget_memory" | "save_memory" => {
            let target = request
                .target
                .as_deref()
                .or_else(|| text_arg("subject"))
                .unwrap_or("memory");
            format!("{} {}", "memory".bold(), target.cyan().bold())
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
            format!("1 tool: {}", unique_names.first().map(|s| s.as_str()).unwrap_or("tool"))
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
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn shorten_path_keeps_at_most_three_segments() {
        assert_eq!(
            shorten_path("~/Downloads/rust-agent-harness/crates/harness-core/src/tools/todo.rs"),
            "src/tools/todo.rs"
        );
        assert_eq!(
            shorten_path("downloads/rust-agent-harness/agents.md"),
            "downloads/rust-agent-harness/agents.md"
        );
        assert_eq!(
            shorten_path("/home/janelle/Downloads/rust-agent-harness/crates/harness-core/prompts.md"),
            "crates/harness-core/prompts.md"
        );
        assert_eq!(shorten_path("agents.md"), "agents.md");
        assert_eq!(shorten_path("."), ".");
    }

    fn call(name: &str, arguments: serde_json::Value) -> String {
        format_tool_call(name, &arguments)
    }

    #[test]
    fn terminal_call_shows_the_command_and_directory() {
        let line = call("terminal", json!({ "command": "cargo test\n--all", "workdir": "/repo" }));
        assert!(line.contains("cargo test"), "got: {}", line);
        assert!(line.contains("in /repo"), "got: {}", line);
        assert!(!line.contains("--all"), "only the first command line is shown");
    }

    #[test]
    fn read_call_shows_the_line_range() {
        let ranged = call("read_file", json!({ "path": "src/lib.rs", "offset": 10, "limit": 20 }));
        assert!(ranged.contains("L10-29"), "got: {}", ranged);
        assert!(call("read_file", json!({ "path": "src/lib.rs" })).contains("src/lib.rs"));
    }

    #[test]
    fn write_call_shows_the_payload_size() {
        let line = call("write_file", json!({ "path": "a.rs", "content": "one\ntwo\nthree" }));
        assert!(line.contains("3 lines"), "got: {}", line);
    }

    #[test]
    fn patch_call_flags_replace_all() {
        let one = call("patch", json!({ "path": "a.rs", "old_string": "x", "new_string": "y" }));
        assert!(!one.contains("all occurrences"), "got: {}", one);

        let all = call(
            "patch",
            json!({ "path": "a.rs", "old_string": "x", "new_string": "y", "replace_all": true }),
        );
        assert!(all.contains("all occurrences"), "got: {}", all);
    }

    /// The reader must be able to tell which search mode ran, and empty optionals must not
    /// render as blank gaps.
    #[test]
    fn search_call_names_the_mode_and_defaults_the_path() {
        let content = call("search_files", json!({ "pattern": "needle", "path": "", "file_glob": "*" }));
        assert!(content.contains("\"needle\""), "got: {}", content);
        assert!(content.contains("in ."), "an empty path renders as the workspace root");
        assert!(!content.contains("matching"), "a `*` glob filters nothing and is hidden");
        assert!(!content.contains("(content)"), "the default mode carries no tag: {}", content);

        let files = call("search_files", json!({ "pattern": "needle", "output_mode": "files_only" }));
        assert!(files.contains("(files_only)"), "got: {}", files);

        let scoped = call("search_files", json!({ "pattern": "needle", "file_glob": "*.rs" }));
        assert!(scoped.contains("matching *.rs"), "got: {}", scoped);
    }

    #[test]
    fn memory_and_note_calls_show_their_subject() {
        let memory = call(
            "save_memory",
            json!({ "subject": "user", "predicate": "editor", "object": "neovim" }),
        );
        assert!(memory.contains("user.editor"), "got: {}", memory);
        assert!(memory.contains("neovim"), "got: {}", memory);

        assert!(call("search_notes", json!({ "query": "budget", "limit": 5 })).contains("limit 5"));
        assert!(call("read_note", json!({ "id": "note_123" })).contains("note_123"));
    }

    #[test]
    fn unknown_and_mcp_tools_fall_back_to_named_arguments() {
        let mcp = call("mcp__filesystem__read", json!({ "path": "/etc/hosts", "missing": null }));
        assert!(mcp.contains("filesystem/read"), "got: {}", mcp);
        assert!(mcp.contains("path=\"/etc/hosts\""), "got: {}", mcp);
        assert!(!mcp.contains("missing"), "null arguments are hidden");
    }

    #[test]
    fn long_arguments_are_clipped_to_one_line() {
        let line = call("terminal", json!({ "command": "x".repeat(500) }));
        assert!(
            line.chars().count() < 500,
            "a long command must not wrap the transcript: {} chars",
            line.chars().count()
        );
        assert!(line.contains('…'));
    }

    #[test]
    fn long_results_collapse_to_a_line_count() {
        let output = (0..40).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        let rendered = format_tool_result(false, &output);
        assert!(rendered.contains("line 0"), "got: {}", rendered);
        assert!(rendered.contains("(+39 lines)"), "got: {}", rendered);
    }

    #[test]
    fn errors_report_the_first_line() {
        let rendered = format_tool_result(true, "boom: file not found\nmore detail");
        assert!(rendered.contains("boom: file not found"), "got: {}", rendered);
        assert!(!rendered.contains("more detail"), "got: {}", rendered);
    }

    #[test]
    fn activity_line_animates_the_frame_and_reports_elapsed_time() {
        let first = activity_line(0, None, Duration::from_millis(1400));
        let second = activity_line(1, None, Duration::from_millis(1500));
        assert_ne!(first, second, "each frame must differ, or nothing animates");
        assert!(first.contains("1.4s"), "got: {}", first);

        let patch = activity_line(0, Some("−3 +1"), Duration::from_millis(0));
        assert!(patch.contains("−3 +1"), "got: {}", patch);
    }

    #[test]
    fn a_patch_reports_the_size_of_its_edit() {
        let detail = tool_activity_detail(
            "patch",
            &json!({ "old_string": "a\nb\nc", "new_string": "d" }),
        );
        assert_eq!(detail.as_deref(), Some("−3 +1"));

        assert_eq!(
            tool_activity_detail("write_file", &json!({ "content": "x\ny" })).as_deref(),
            Some("+2")
        );
        assert_eq!(tool_activity_detail("terminal", &json!({ "command": "ls" })), None);
    }

    /// Every row must fit one physical terminal row, because the live region moves the cursor by
    /// counting rows and a wrapped row would desynchronise it.
    #[test]
    fn wrapping_never_exceeds_the_width() {
        let text = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu";
        for row in wrap_rows(text, 20) {
            assert!(row.chars().count() <= 20, "row overflowed: {:?}", row);
        }

        let long_word = "x".repeat(50);
        for row in wrap_rows(&long_word, 12) {
            assert!(row.chars().count() <= 12);
        }

        assert_eq!(wrap_rows("line one\nline two", 40).len(), 2);
        assert!(wrap_rows("", 40).len() == 1, "an empty line still occupies a row");
    }

    #[test]
    fn separators_only_appear_between_different_blocks() {
        use OutputBlockKind::*;
        assert!(!needs_separator(None, User));
        assert!(needs_separator(User, Thinking));
        assert!(needs_separator(Thinking, Tool));
        assert!(!needs_separator(Tool, Tool), "consecutive tool calls stay adjacent");
        assert!(needs_separator(Reply, User), "a new user message starts after a blank line");
    }

    #[test]
    fn approval_verdict_colors_approved_blue_and_file_cyan() {
        let req = ApprovalRequest {
            tool: "write_file".to_string(),
            call_id: "c1".to_string(),
            target: Some("/home/janelle/fts-verify/src/main.rs".to_string()),
            diff: None,
            arguments: json!({ "path": "/home/janelle/fts-verify/src/main.rs", "content": "fn main() {}\n" }),
        };
        let line = format_approval_verdict(true, &req);
        assert!(line.contains("Approved:"), "got: {}", line);
        assert!(line.contains("/home/janelle/fts-verify/src/main.rs"), "got: {}", line);
        assert!(line.contains("write"), "got: {}", line);
    }

    #[test]
    fn approval_verdict_formats_terminal_unbloated() {
        let req = ApprovalRequest {
            tool: "terminal".to_string(),
            call_id: "c2".to_string(),
            target: Some("cargo run --quiet 2>&1 | tail -110".to_string()),
            diff: None,
            arguments: json!({ "command": "cargo run --quiet 2>&1 | tail -110" }),
        };
        let line = format_approval_verdict(true, &req);
        assert!(line.contains("$"), "got: {}", line);
        assert!(line.contains("cargo run --quiet"), "got: {}", line);
        assert!(!line.contains("terminal (cargo"), "must not be wrapped in bloated tool parentheses");
    }

    #[test]
    fn approval_verdict_formats_note_uuid_shortened() {
        let req = ApprovalRequest {
            tool: "update_note".to_string(),
            call_id: "c3".to_string(),
            target: Some("note_5d26a9f8-b905-43aa-8168-3f286a96ee3f".to_string()),
            diff: None,
            arguments: json!({ "id": "note_5d26a9f8-b905-43aa-8168-3f286a96ee3f" }),
        };
        let line = format_approval_verdict(true, &req);
        assert!(line.contains("note_5d26a9f8…"), "got: {}", line);
        assert!(!line.contains("note_5d26a9f8-b905-43aa-8168-3f286a96ee3f"), "raw UUID must be shortened");
    }

    #[test]
    fn process_header_formats_collapsed_with_tools_and_duration() {
        let tools = vec!["read_file".to_string(), "patch".to_string()];
        let header = format_process_header(false, 2, &tools, Some(Duration::from_millis(2400)), true);
        assert!(header.contains("Process"), "got: {}", header);
        assert!(header.contains("2 tools: read_file, patch"), "got: {}", header);
        assert!(header.contains("thought for 2.4s"), "got: {}", header);
        assert!(header.contains("Ctrl+O to expand"), "got: {}", header);
        assert!(header.contains("▶"), "got: {}", header);
    }

    #[test]
    fn process_header_formats_expanded_header() {
        let tools = vec!["terminal".to_string()];
        let header = format_process_header(true, 1, &tools, Some(Duration::from_millis(500)), false);
        assert!(header.contains("▼"), "got: {}", header);
        assert!(header.contains("1 tool: terminal"), "got: {}", header);
        assert!(header.contains("Ctrl+O to collapse"), "got: {}", header);
    }

    #[test]
    fn parse_turns_from_history_handles_multistep_and_empty_queries() {
        use harness_core::Message;

        let history = vec![
            StoredMessage {
                id: 1,
                seq: 1,
                message: Message::user("first question"),
                is_summary: false,
            },
            StoredMessage {
                id: 2,
                seq: 2,
                message: Message::assistant("first reply"),
                is_summary: false,
            },
            StoredMessage {
                id: 3,
                seq: 3,
                message: Message::user("second question"),
                is_summary: false,
            },
            StoredMessage {
                id: 4,
                seq: 4,
                message: Message {
                    role: Role::Assistant,
                    content: None,
                    reasoning: Some("thinking...".to_string()),
                    tool_calls: vec![harness_core::ToolCall {
                        id: "c1".to_string(),
                        name: "terminal".to_string(),
                        arguments: json!({ "command": "ls" }),
                    }],
                    tool_result: None,
                    usage: None,
                },
                is_summary: false,
            },
            StoredMessage {
                id: 5,
                seq: 5,
                message: Message {
                    role: Role::Assistant,
                    content: Some("final reply".to_string()),
                    reasoning: None,
                    tool_calls: Vec::new(),
                    tool_result: None,
                    usage: None,
                },
                is_summary: false,
            },
        ];

        let turns = parse_turns_from_history(&history);
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].user_query, "first question");
        assert_eq!(turns[0].reply, "first reply");
        assert_eq!(turns[1].user_query, "second question");
        assert_eq!(turns[1].reply, "final reply");
        assert_eq!(turns[1].tool_items.len(), 1);
        assert_eq!(turns[1].reasoning.as_deref(), Some("thinking..."));
    }

    #[test]
    fn user_message_is_left_aligned_with_label() {
        let msg = right_aligned_user_message("hello\nworld", 80);
        let stripped: Vec<String> = msg
            .lines()
            .map(|l| dialoguer::console::strip_ansi_codes(l).to_string())
            .collect();
        assert_eq!(stripped.len(), 2);
        assert!(stripped[0].starts_with("User >"), "got: {}", stripped[0]);
        assert!(stripped[0].ends_with("hello"));
        assert!(stripped[1].starts_with("User >"), "got: {}", stripped[1]);
        assert!(stripped[1].ends_with("world"));
    }
}
