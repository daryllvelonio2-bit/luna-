use std::time::Duration;

use colored::Colorize;
use harness_core::ApprovalRequest;

use super::blocks::{
    truncate_visible, MAX_INLINE_RESULT_LINES, MAX_TOOL_ARG_CHARS, MAX_TOOL_RESULT_CHARS,
};
use super::paths::{lookup_note_title, shorten_path};
use super::tools::{arg_text, tool_activity_detail};

/// Formats the single-line verdict when a human approves or cancels a tool call.
/// "Approved:" is styled in bold blue, file paths in cyan, and command arguments
/// are structured cleanly rather than dumped as raw unparsed blobs.
pub fn format_approval_verdict(is_approved: bool, request: &ApprovalRequest) -> String {
    let tag = if is_approved {
        format!("{} {}", "✔".blue().bold(), "Approved:".blue().bold())
    } else {
        format!("{} {}", "✖".red().bold(), "Cancelled:".red().bold())
    };

    format!("{} {}", tag, approval_body(request))
}

/// The tool-specific half of the verdict.
fn approval_body(request: &ApprovalRequest) -> String {
    match request.tool.as_str() {
        "write_file" => approval_write(request),
        "patch" => approval_patch(request),
        "terminal" | "shell" | "bash" => approval_shell(request),
        "save_note" => approval_save_note(request),
        "update_note" | "delete_note" | "read_note" => approval_note(request),
        "update_memory" | "forget_memory" | "save_memory" | "memory" => approval_memory(request),
        "apply_harness_update" => approval_update(request),
        other => approval_generic(other, request),
    }
}

fn approval_write(request: &ApprovalRequest) -> String {
    let path = request
        .target
        .as_deref()
        .or_else(|| arg_text(&request.arguments, "path"))
        .unwrap_or("file");
    let lines = arg_text(&request.arguments, "content")
        .map(|c| c.lines().count())
        .unwrap_or(0);
    let suffix = if lines > 0 {
        format!(" {}", format!("(+{} lines)", lines).dimmed())
    } else {
        String::new()
    };
    format!("{} {}{}", "write".bold(), path.cyan().bold(), suffix)
}

fn approval_patch(request: &ApprovalRequest) -> String {
    let path = request
        .target
        .as_deref()
        .or_else(|| arg_text(&request.arguments, "path"))
        .unwrap_or("file");
    let suffix = match tool_activity_detail("patch", &request.arguments) {
        Some(d) => format!(" ({})", d.dimmed()),
        None => String::new(),
    };
    format!("{} {}{}", "patch".bold(), path.cyan().bold(), suffix)
}

fn approval_shell(request: &ApprovalRequest) -> String {
    let cmd = request
        .target
        .as_deref()
        .or_else(|| arg_text(&request.arguments, "command"))
        .unwrap_or("");
    let first_line = cmd.lines().next().unwrap_or(cmd).trim();
    let display_cmd = truncate_visible(first_line, 65);
    let cwd = arg_text(&request.arguments, "workdir")
        .map(|w| format!(" {}", format!("(in {})", w).dimmed()))
        .unwrap_or_default();
    format!("{} {}{}", "$".bold().cyan(), display_cmd.bold(), cwd)
}

fn approval_save_note(request: &ApprovalRequest) -> String {
    let title = arg_text(&request.arguments, "title").unwrap_or("note");
    let lines = arg_text(&request.arguments, "content")
        .map(|c| c.lines().count())
        .unwrap_or(0);
    let suffix = if lines > 0 {
        format!(" {}", format!("({} lines)", lines).dimmed())
    } else {
        String::new()
    };
    format!("{} \"{}\"{}", "note".bold(), title.cyan().bold(), suffix)
}

fn approval_note(request: &ApprovalRequest) -> String {
    let id = request
        .target
        .as_deref()
        .or_else(|| arg_text(&request.arguments, "id"))
        .or_else(|| arg_text(&request.arguments, "title"))
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

fn approval_memory(request: &ApprovalRequest) -> String {
    let action = match request.tool.as_str() {
        "save_memory" => "save",
        "update_memory" => "update",
        "forget_memory" => "forget",
        _ => arg_text(&request.arguments, "action").unwrap_or("update"),
    };
    let target = request
        .target
        .as_deref()
        .or_else(|| arg_text(&request.arguments, "name"))
        .or_else(|| arg_text(&request.arguments, "subject"))
        .unwrap_or("memory");
    format!(
        "{} {} {}",
        "memory".bold(),
        action.bold(),
        target.cyan().bold()
    )
}

fn approval_update(request: &ApprovalRequest) -> String {
    let id = request
        .target
        .as_deref()
        .or_else(|| arg_text(&request.arguments, "patch_id"))
        .unwrap_or("patch");
    format!("{} {}", "update".bold(), id.cyan().bold())
}

fn approval_generic(other: &str, request: &ApprovalRequest) -> String {
    let target = request.target.as_deref().unwrap_or("-");
    let clean_target = truncate_visible(target, 60);
    format!("{} ({})", other.bold(), clean_target.cyan())
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
    format_tool_result_for(None, is_error, output)
}

/// Render a tool result with tool-specific formatting (e.g. for search_files, read_file).
pub fn format_tool_result_for(tool: Option<&str>, is_error: bool, output: &str) -> String {
    let trimmed = output.trim();
    if is_error {
        return format_error_result(trimmed);
    }

    if is_search_tool(tool, trimmed) {
        return format_search_result(trimmed);
    }

    if is_read_tool(tool, trimmed) {
        return format_read_result(trimmed);
    }

    if trimmed.is_empty() {
        return format!("  {} {}", "✓".green(), "done".dimmed());
    }

    if let Some(result) = format_json_result(trimmed) {
        return result;
    }

    format_summary_result(trimmed)
}

/// An error shows its first line: the rest is usually a log the live tail already has.
fn format_error_result(trimmed: &str) -> String {
    let first_line = trimmed.lines().next().unwrap_or(trimmed).trim();
    format!(
        "  {} {}",
        "✖".red(),
        truncate_visible(first_line, MAX_TOOL_RESULT_CHARS).red()
    )
}

/// The saved-note and saved-memory payloads, which carry their own summary fields.
fn format_json_result(trimmed: &str) -> Option<String> {
    if !trimmed.starts_with('{') {
        return None;
    }
    let val = serde_json::from_str::<serde_json::Value>(trimmed).ok()?;
    if let Some(title) = val.get("title").and_then(|t| t.as_str()) {
        let id_suffix = val
            .get("id")
            .and_then(|i| i.as_str())
            .map(|id| format!(" ({})", id))
            .unwrap_or_default();
        return Some(format!(
            "  {} Note \"{}\" saved{}",
            "✓".green(),
            title.bold(),
            id_suffix.dimmed()
        ));
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
            return Some(format!(
                "  {} Memory \"{}\" {}{}",
                "✓".green(),
                mem_id.bold(),
                status_str,
                obj_str.dimmed()
            ));
        }
    }
    None
}

/// Everything else: one line, a handful of lines in full, or a first line plus a count.
fn format_summary_result(trimmed: &str) -> String {
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

fn is_search_tool(tool: Option<&str>, output: &str) -> bool {
    if let Some(t) = tool {
        match t {
            "search_files" | "search" | "find_in_files" | "grep" => return true,
            "read_file" | "read" | "write_file" | "patch" | "terminal" => return false,
            _ => {}
        }
    }
    let mut matches = 0;
    for line in output.lines().take(5) {
        if parse_search_content_line(line).is_some() || parse_search_count_line(line).is_some() {
            matches += 1;
        }
    }
    matches >= 1
}

fn is_read_tool(tool: Option<&str>, output: &str) -> bool {
    if let Some(t) = tool {
        match t {
            "read_file" | "read" | "cat" | "view" => return true,
            "search_files" | "search" | "write_file" | "patch" | "terminal" => return false,
            _ => {}
        }
    }
    let mut matches = 0;
    for line in output.lines().take(5) {
        if parse_read_line(line).is_some() {
            matches += 1;
        }
    }
    matches >= 1
}

fn parse_search_content_line(line: &str) -> Option<(&str, usize, &str)> {
    let line = line.trim();
    if line.starts_with('[') && line.contains("search stopped") {
        return None;
    }
    let start_idx =
        if line.len() > 2 && line.as_bytes()[1] == b':' && line.as_bytes()[0].is_ascii_alphabetic()
        {
            2
        } else {
            0
        };
    let colon1 = line[start_idx..].find(':').map(|idx| start_idx + idx)?;
    let rest = &line[colon1 + 1..];
    let digits_len = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits_len == 0 {
        return None;
    }
    let line_no: usize = rest[..digits_len].parse().ok()?;
    if rest[digits_len..].starts_with(':') {
        let content = &rest[digits_len + 1..];
        let path = &line[..colon1];
        Some((path, line_no, content))
    } else {
        None
    }
}

fn parse_search_count_line(line: &str) -> Option<(&str, usize)> {
    let line = line.trim();
    if line.ends_with(" matches") || line.ends_with(" match") {
        let idx = line.rfind(':')?;
        let path = line[..idx].trim();
        let count_str = line[idx + 1..]
            .trim_end_matches(" matches")
            .trim_end_matches(" match")
            .trim();
        let count = count_str.parse::<usize>().ok()?;
        Some((path, count))
    } else {
        None
    }
}

fn format_search_result(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return format!("  {} {}", "✓".green(), "0 matches found".dimmed());
    }

    // Check count mode lines first
    let mut total_count_matches = 0;
    let mut count_files = 0;
    let mut count_mode = true;
    for line in trimmed.lines() {
        if line.starts_with('[') && line.contains("search stopped") {
            continue;
        }
        if let Some((_, count)) = parse_search_count_line(line) {
            total_count_matches += count;
            count_files += 1;
        } else {
            count_mode = false;
            break;
        }
    }
    if count_mode && count_files > 0 {
        let file_label = if count_files == 1 { "file" } else { "files" };
        let match_label = if total_count_matches == 1 {
            "match"
        } else {
            "matches"
        };
        return format!(
            "  {} {}",
            "✓".green(),
            format!("{total_count_matches} {match_label} across {count_files} {file_label}")
                .dimmed()
        );
    }

    // Content mode lines
    let mut total_matches = 0;
    let mut files_seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut first_match: Option<(String, usize, String)> = None;

    for line in trimmed.lines() {
        if line.starts_with('[') && line.contains("search stopped") {
            continue;
        }
        if let Some((path, line_no, content)) = parse_search_content_line(line) {
            total_matches += 1;
            files_seen.insert(path.to_string());
            if first_match.is_none() {
                let clean_content = content.trim();
                let clean_content = if clean_content.is_empty() {
                    "..."
                } else {
                    clean_content
                };
                first_match = Some((path.to_string(), line_no, clean_content.to_string()));
            }
        }
    }

    if let Some((path, line_no, content_snippet)) = first_match {
        let short_file = {
            let short = shorten_path(&path);
            if short.len() > 30 {
                std::path::Path::new(&path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(&short)
                    .to_string()
            } else {
                short
            }
        };
        let truncated_snippet = truncate_visible(&content_snippet, 55);
        let snippet_quoted =
            if truncated_snippet.starts_with('"') && truncated_snippet.ends_with('"') {
                truncated_snippet
            } else {
                format!("\"{}\"", truncated_snippet)
            };

        let file_count = files_seen.len();
        let file_label = if file_count == 1 { "file" } else { "files" };
        let match_label = if total_matches == 1 {
            "match"
        } else {
            "matches"
        };
        let summary = if total_matches == 1 {
            format!("1 match in {short_file}:{line_no} · {snippet_quoted}")
        } else if file_count <= 1 {
            format!("{total_matches} {match_label} in {short_file} · L{line_no} · {snippet_quoted}")
        } else {
            format!("{total_matches} {match_label} in {file_count} {file_label} · {short_file}:{line_no} · {snippet_quoted}")
        };
        return format!("  {} {}", "✓".green(), summary.dimmed());
    }

    // files_only mode or paths
    let file_paths: Vec<&str> = trimmed
        .lines()
        .filter(|l| !l.starts_with('[') || !l.contains("search stopped"))
        .filter(|l| !l.trim().is_empty())
        .collect();
    if !file_paths.is_empty() {
        let count = file_paths.len();
        let first_short = shorten_path(file_paths[0]);
        let summary = if count == 1 {
            format!("1 matching file: {first_short}")
        } else {
            format!("{count} matching files (first: {first_short})")
        };
        return format!("  {} {}", "✓".green(), summary.dimmed());
    }

    format!("  {} {}", "✓".green(), "0 matches found".dimmed())
}

fn parse_read_line(line: &str) -> Option<(usize, &str)> {
    let pipe_idx = line.find('|')?;
    if pipe_idx == 0 || pipe_idx > 8 {
        return None;
    }
    let num_str = line[..pipe_idx].trim();
    if num_str.is_empty() || !num_str.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let line_no = num_str.parse::<usize>().ok()?;
    let content = &line[pipe_idx + 1..];
    Some((line_no, content))
}

fn is_code_declaration_or_heading(s: &str) -> bool {
    let lower = s.to_ascii_lowercase();
    let trimmed = lower.trim();
    trimmed.starts_with("# ")
        || trimmed.starts_with("## ")
        || trimmed.starts_with("class ")
        || trimmed.starts_with("abstract class ")
        || trimmed.starts_with("interface ")
        || trimmed.starts_with("trait ")
        || trimmed.starts_with("function ")
        || trimmed.starts_with("public function ")
        || trimmed.starts_with("protected function ")
        || trimmed.starts_with("private function ")
        || trimmed.starts_with("static function ")
        || trimmed.starts_with("fn ")
        || trimmed.starts_with("pub fn ")
        || trimmed.starts_with("pub async fn ")
        || trimmed.starts_with("async fn ")
        || trimmed.starts_with("struct ")
        || trimmed.starts_with("pub struct ")
        || trimmed.starts_with("enum ")
        || trimmed.starts_with("pub enum ")
        || trimmed.starts_with("def ")
        || trimmed.starts_with("async def ")
        || trimmed.starts_with("type ")
        || trimmed.starts_with("export ")
        || trimmed.starts_with("\"name\":")
        || trimmed.starts_with("\"title\":")
}

fn is_trivial_delimiter(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() {
        return true;
    }
    matches!(
        t,
        "<?php"
            | "?>"
            | "{"
            | "}"
            | "["
            | "]"
            | "]);"
            | ");"
            | "};"
            | "/**"
            | "*/"
            | "*"
            | "---"
            | "..."
            | "()"
            | "[]"
            | "{}"
            | ";"
    )
}

fn format_read_result(output: &str) -> String {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return format!("  {} {}", "✓".green(), "0 lines (empty file)".dimmed());
    }

    let mut start_line = None;
    let mut end_line = 0;
    let mut total_lines = 0;
    let mut lines_data: Vec<(usize, String)> = Vec::new();

    for line in trimmed.lines() {
        if let Some((num, content)) = parse_read_line(line) {
            if start_line.is_none() {
                start_line = Some(num);
            }
            end_line = num;
            total_lines += 1;
            lines_data.push((num, content.trim().to_string()));
        }
    }

    if total_lines == 0 {
        // Fallback if line numbering was not present
        let count = trimmed.lines().count();
        let first = trimmed.lines().next().unwrap_or("").trim();
        return format!(
            "  {} {}",
            "✓".green(),
            format!("{count} lines · \"{}\"", truncate_visible(first, 50)).dimmed()
        );
    }

    let start_line = start_line.unwrap_or(1);
    let range_str = if start_line == end_line {
        format!("L{}", start_line)
    } else {
        format!("L{}-{}", start_line, end_line)
    };
    let line_str = if total_lines == 1 { "line" } else { "lines" };

    // Select the best representative snippet:
    // Priority 1: Key declarations or headings
    let mut chosen_snippet = None;
    for (_num, content) in &lines_data {
        if is_code_declaration_or_heading(content) {
            chosen_snippet = Some(content.as_str());
            break;
        }
    }

    // Priority 2: First non-trivial line (skipping trivial delimiters/comments/empty)
    if chosen_snippet.is_none() {
        for (_num, content) in &lines_data {
            if !is_trivial_delimiter(content) {
                chosen_snippet = Some(content.as_str());
                break;
            }
        }
    }

    // Priority 3: First non-empty line
    if chosen_snippet.is_none() {
        for (_num, content) in &lines_data {
            if !content.is_empty() {
                chosen_snippet = Some(content.as_str());
                break;
            }
        }
    }

    let snippet = chosen_snippet.unwrap_or("...");
    let snippet_clean = truncate_visible(snippet, 60);

    let summary = format!("{total_lines} {line_str} ({range_str}) · {snippet_clean}");
    format!("  {} {}", "✓".green(), summary.dimmed())
}
