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
