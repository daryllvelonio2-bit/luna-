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
                val.strip_prefix('+').unwrap_or(val)
            ));
        } else if val.starts_with('-') {
            lines.push(format!(
                "\x1b[48;2;200;0;0m\x1b[38;2;255;255;255;1m  - {}\x1b[0m",
                val.strip_prefix('-').unwrap_or(val)
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

/// Extracts formatted diff lines from a write_file or write tool call.
///
/// If an existing file is present on disk and differs from the new content,
/// formats a unified diff against the previous on-disk content.
/// Otherwise, formats the written lines as green addition lines.
pub fn extract_write_diff(arguments: &serde_json::Value) -> Vec<String> {
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

    let new_content = text_any(&[
        "content",
        "contents",
        "text",
        "new_string",
        "newString",
        "code",
    ]);
    let path = text_any(&["path", "filePath", "filepath", "file"]);

    let Some(new) = new_content else {
        return Vec::new();
    };

    // If explicit old content was provided in arguments
    if let Some(old) = text_any(&[
        "old_content",
        "oldContent",
        "old_string",
        "oldString",
        "old_str",
        "old",
    ]) {
        return format_patch_diff(old, new);
    }

    if let Some(p) = path {
        let path_obj = std::path::Path::new(p);
        let expanded = if let Some(stripped) = p.strip_prefix("~/") {
            if let Some(home) = std::env::var_os("HOME") {
                std::path::PathBuf::from(home).join(stripped)
            } else {
                path_obj.to_path_buf()
            }
        } else {
            path_obj.to_path_buf()
        };
        let on_disk = if expanded.is_file() {
            std::fs::read_to_string(&expanded).ok()
        } else if let Ok(cwd) = std::env::current_dir() {
            let joined = cwd.join(&expanded);
            if joined.is_file() {
                std::fs::read_to_string(joined).ok()
            } else {
                None
            }
        } else {
            None
        };

        if let Some(old) = on_disk {
            if old != new {
                let diff_str = similar::TextDiff::from_lines(old.as_str(), new)
                    .unified_diff()
                    .context_radius(3)
                    .to_string();
                let lines = format_unified_diff(&diff_str);
                if !lines.is_empty() {
                    return lines;
                }
            }
        }
    }

    let mut diff = format_patch_diff("", new);
    if diff.len() > 2000 {
        diff.truncate(2000);
        diff.push(format!(
            "\x1b[2m    ... ({} more lines omitted)\x1b[0m",
            diff.len().saturating_sub(2000)
        ));
    }
    diff
}

/// Returns true if the tool modifies files and produces diffs.
pub fn is_diff_or_write_tool(name: &str) -> bool {
    matches!(
        name,
        "patch" | "edit" | "apply_patch" | "write_file" | "write" | "create_file"
    )
}

/// Extracts formatted diff lines for any patch or write tool.
pub fn extract_tool_diff(name: &str, arguments: &serde_json::Value) -> Vec<String> {
    match name {
        "patch" | "edit" | "apply_patch" => extract_patch_diff(arguments),
        "write_file" | "write" | "create_file" => extract_write_diff(arguments),
        _ => Vec::new(),
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

/// Strip ANSI CSI escape sequences so line prefixes can be cleanly inspected.
pub fn strip_ansi_codes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_esc = false;
    for c in s.chars() {
        if c == '\x1b' {
            in_esc = true;
        } else if in_esc {
            if c.is_ascii_alphabetic() {
                in_esc = false;
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Count additions and deletions from formatted or unified diff lines.
pub fn count_diff_lines(diff_lines: &[String]) -> (usize, usize) {
    let mut added = 0;
    let mut deleted = 0;
    for line in diff_lines {
        let clean = strip_ansi_codes(line);
        let trimmed = clean.trim_start();
        if trimmed.starts_with('+') && !trimmed.starts_with("+++") {
            added += 1;
        } else if trimmed.starts_with('-') && !trimmed.starts_with("---") {
            deleted += 1;
        }
    }
    (added, deleted)
}

/// Count additions and deletions produced by a turn item.
pub fn item_diff_counts(item: &crate::commands::chat::render::TurnProcessItem) -> (usize, usize) {
    if item.is_error {
        return (0, 0);
    }
    if !item.diff_lines.is_empty() {
        return count_diff_lines(&item.diff_lines);
    }
    if let Some(ref args) = item.arguments {
        if is_diff_or_write_tool(&item.tool_name) {
            let lines = extract_tool_diff(&item.tool_name, args);
            return count_diff_lines(&lines);
        }
    }
    (0, 0)
}

/// Count total additions and deletions produced across all tool items in a turn.
pub fn turn_diff_counts(turn: &crate::commands::chat::render::TurnRecord) -> (usize, usize) {
    let mut total_a = 0;
    let mut total_d = 0;
    for item in &turn.tool_items {
        let (a, d) = item_diff_counts(item);
        total_a += a;
        total_d += d;
    }
    (total_a, total_d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_count_diff_lines_ansi_and_plain() {
        let lines = vec![
            "\x1b[48;2;200;0;0m\x1b[38;2;255;255;255;1m  - old line 1\x1b[0m".to_string(),
            "\x1b[48;2;200;0;0m\x1b[38;2;255;255;255;1m  - old line 2\x1b[0m".to_string(),
            "\x1b[48;2;0;160;0m\x1b[38;2;255;255;255;1m  + new line 1\x1b[0m".to_string(),
            "\x1b[48;2;0;160;0m\x1b[38;2;255;255;255;1m  + new line 2\x1b[0m".to_string(),
            "\x1b[48;2;0;160;0m\x1b[38;2;255;255;255;1m  + new line 3\x1b[0m".to_string(),
            "\x1b[2m    context line\x1b[0m".to_string(),
            "--- a/file.rs".to_string(),
            "+++ b/file.rs".to_string(),
            "@@ -1,3 +1,4 @@".to_string(),
        ];
        let (added, deleted) = count_diff_lines(&lines);
        assert_eq!(added, 3);
        assert_eq!(deleted, 2);
    }
}
