//! Pure rendering for the interactive transcript: how one tool call, one tool result, one
//! reasoning line and one activity line are turned into terminal text.
//!
//! Everything here is a function of its arguments — no terminal handles, no cursor movement, no
//! locks — so the transcript's appearance can be asserted in tests without a pty.

use std::time::Duration;

use colored::Colorize;
use harness_core::ApprovalRequest;

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

/// Right-align the user's own message so the transcript reads as a chat — the user on the
/// right, Luna on the left. Padding is derived from the *visible* width, never the ANSI codes.
pub fn right_aligned_user_message(query: &str, width: usize) -> String {
    // The message and its glyph are one unit, so they are bolded together.
    let line = format!("{} {}", query, PROMPT.trim_end());
    let pad = width.saturating_sub(1).saturating_sub(line.chars().count());
    format!("{}{}", " ".repeat(pad), line.bold())
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

/// Render a tool call as one transcript line that names the tool *and* the operation it is
/// performing. A search spells out its mode (`search_files(content)`) so the reader never has
/// to guess whether a scan went over file contents or file names.
pub fn format_tool_call(name: &str, arguments: &serde_json::Value) -> String {
    let bullet = "•".cyan();
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

    match name {
        "terminal" | "shell" | "bash" => {
            if let Some(command) = text("command") {
                let first_line = command.lines().next().unwrap_or(command).trim();
                let cwd = text("workdir")
                    .map(|w| suffix(format!("in {}", w)))
                    .unwrap_or_default();
                return format!(
                    "{} {} {}{}",
                    bullet,
                    "$".bold().cyan(),
                    truncate_visible(first_line, MAX_TOOL_ARG_CHARS).bold(),
                    cwd
                );
            }
        }
        "read_file" => {
            if let Some(path) = text("path") {
                let range = match (number("offset"), number("limit")) {
                    (Some(offset), Some(limit)) => {
                        suffix(format!("lines {}-{}", offset, offset + limit.saturating_sub(1)))
                    }
                    (Some(offset), None) => suffix(format!("from line {}", offset)),
                    (None, Some(limit)) => suffix(format!("first {} lines", limit)),
                    _ => String::new(),
                };
                return format!("{} {} {}{}", bullet, "read".bold(), path.cyan(), range);
            }
        }
        "write_file" => {
            if let Some(path) = text("path") {
                let lines = text("content").map(|c| c.lines().count()).unwrap_or(0);
                return format!(
                    "{} {} {}{}",
                    bullet,
                    "write".bold(),
                    path.cyan(),
                    suffix(format!("{} lines", lines))
                );
            }
        }
        "patch" => {
            if let Some(path) = text("path") {
                let every = match arguments.get("replace_all").and_then(|v| v.as_bool()) {
                    Some(true) => suffix("all occurrences".to_string()),
                    _ => String::new(),
                };
                return format!("{} {} {}{}", bullet, "patch".bold(), path.cyan(), every);
            }
        }
        "search_files" => {
            let pattern = text("pattern").unwrap_or("");
            let path = text("path").unwrap_or(".");
            let mode = text("output_mode").unwrap_or("content");
            let include = text("file_glob")
                // `*` filters nothing, so it carries no information for the reader.
                .filter(|glob| *glob != "*")
                .map(|g| suffix(format!("matching {}", g)))
                .unwrap_or_default();
            return format!(
                "{} {} {} {} {}{}",
                bullet,
                format!("search_files({})", mode).bold(),
                quoted(pattern),
                "in".dimmed(),
                path.cyan(),
                include
            );
        }
        "save_memory" => {
            let subject = text("subject").unwrap_or("?");
            let predicate = text("predicate").unwrap_or("?");
            let object = text("object").unwrap_or("?");
            return format!(
                "{} {} {}{}{}",
                bullet,
                "save_memory".bold(),
                format!("{}.{}", subject, predicate).cyan(),
                " = ".dimmed(),
                quoted(object)
            );
        }
        "update_memory" | "forget_memory" => {
            if let Some(id) = text("memory_id") {
                return format!("{} {} {}", bullet, name.bold(), id.cyan());
            }
        }
        "save_note" => {
            let title = text("title").unwrap_or("");
            let lines = text("content").map(|c| c.lines().count()).unwrap_or(0);
            return format!(
                "{} {} {}{}",
                bullet,
                "save_note".bold(),
                quoted(title),
                suffix(format!("{} lines", lines))
            );
        }
        "search_notes" => {
            let query = text("query").unwrap_or("");
            let limit = number("limit")
                .map(|l| suffix(format!("limit {}", l)))
                .unwrap_or_default();
            return format!("{} {} {}{}", bullet, "search_notes".bold(), quoted(query), limit);
        }
        "read_note" | "update_note" | "delete_note" => {
            if let Some(target) = text("title").or_else(|| text("id")) {
                return format!("{} {} {}", bullet, name.bold(), quoted(target));
            }
        }
        "inspect_harness_source" => {
            if let Some(path) = text("path") {
                return format!("{} {} {}", bullet, name.bold(), path.cyan());
            }
        }
        "propose_harness_patch" => {
            if let Some(path) = text("target_file") {
                let lines = text("content").map(|c| c.lines().count()).unwrap_or(0);
                return format!(
                    "{} {} {}{}",
                    bullet,
                    name.bold(),
                    path.cyan(),
                    suffix(format!("{} lines", lines))
                );
            }
        }
        "apply_harness_update" => {
            if let Some(id) = text("patch_id") {
                return format!("{} {} {}", bullet, name.bold(), id.cyan());
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
                            format!("\"{}\"", truncate_visible(first_line, MAX_TOOL_ARG_CHARS))
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

    format!("{} {}{}", bullet, display_name.bold(), args_str.dimmed())
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
        assert!(call("read_file", json!({ "path": "src/lib.rs", "offset": 10, "limit": 20 }))
            .contains("lines 10-29"));
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
        assert!(content.contains("search_files(content)"), "got: {}", content);
        assert!(content.contains("\"needle\""), "got: {}", content);
        assert!(content.contains("in ."), "an empty path renders as the workspace root");
        assert!(!content.contains("matching"), "a `*` glob filters nothing and is hidden");

        let files = call("search_files", json!({ "pattern": "needle", "output_mode": "files_only" }));
        assert!(files.contains("search_files(files_only)"), "got: {}", files);

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
}
