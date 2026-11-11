use colored::Colorize;

use super::blocks::{truncate_visible, MAX_TOOL_ARG_CHARS};
use super::paths::{lookup_memory_from_db, lookup_note_title, shorten_path, shorten_paths_in_text};

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

pub(crate) fn resolve_memory_name(arguments: &serde_json::Value) -> String {
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
pub(crate) const VERB_COLUMN: usize = 12;

/// Emoji glyph and short verb per tool: the transcript reads as an action log
/// (`📖 read      chat.rs L500-524`) instead of a JSON argument dump.
pub(crate) fn tool_glyph_and_verb(name: &str) -> (&'static str, &'static str) {
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

/// An argument that is present and non-empty: a model that sends `"path": ""` means "no path
/// given", not "a path called nothing".
pub(crate) fn arg_text<'a>(arguments: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    arguments
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
}

fn arg_number(arguments: &serde_json::Value, key: &str) -> Option<u64> {
    arguments.get(key).and_then(|v| v.as_u64())
}

/// A secondary detail, dimmed and separated from the target by one space.
fn arg_suffix(detail: String) -> String {
    format!(" {}", detail.dimmed())
}

fn quoted_arg(value: &str) -> String {
    format!("\"{}\"", truncate_visible(value, MAX_TOOL_ARG_CHARS))
        .cyan()
        .to_string()
}

/// `{:width$}` pads but never cuts, so a long fallback name still renders in full.
fn verb_column(label: &str) -> String {
    format!("{:<w$}", label, w = VERB_COLUMN).bold().to_string()
}

/// Render a tool call as one transcript line: glyph, a padded short verb, the target, and the
/// secondary detail the call itself implies (a read's line range, a patch's edit size).
pub fn format_tool_call(name: &str, arguments: &serde_json::Value) -> String {
    let (glyph, verb) = tool_glyph_and_verb(name);
    format_known_tool_call(name, glyph, verb, arguments)
        .unwrap_or_else(|| format_fallback_call(glyph, name, arguments))
}

/// Dispatch a call to the renderer for its tool family.
///
/// `None` keeps the old arm's fall-through: a call whose identifying argument is missing is drawn
/// by the generic renderer instead.
fn format_known_tool_call(
    name: &str,
    glyph: &str,
    verb: &str,
    arguments: &serde_json::Value,
) -> Option<String> {
    match name {
        "terminal" | "shell" | "bash" => format_shell_call(glyph, verb, arguments),
        "read_file" | "read" => format_read_call(glyph, verb, arguments),
        "list_files" | "ls" => Some(format_list_call(glyph, verb, arguments)),
        "write_file" | "write" => format_write_call(glyph, verb, arguments),
        "patch" | "edit" | "apply_patch" => format_patch_call(glyph, verb, arguments),
        "search_files" => Some(format_search_call(glyph, verb, arguments)),
        "save_memory" | "update_memory" | "forget_memory" | "memory" => {
            Some(format_memory_call(name, glyph, arguments))
        }
        "save_note" => Some(format_save_note_call(glyph, verb, arguments)),
        "search_notes" => Some(format_search_notes_call(glyph, verb, arguments)),
        "skill_view" | "skill" => Some(format_skill_call(glyph, verb, arguments)),
        "skills_list" => Some(format_skills_list_call(glyph, verb)),
        "clarify" => Some(format_clarify_call(glyph, verb, arguments)),
        "process" => Some(format_process_call(glyph, arguments)),
        "delegate_task" => Some(format_delegate_call(glyph, verb, arguments)),
        "list_notes" => Some(format_list_notes_call(glyph, verb, arguments)),
        "read_note" | "update_note" | "delete_note" => {
            format_note_action_call(name, glyph, verb, arguments)
        }
        _ => None,
    }
}

fn format_shell_call(glyph: &str, verb: &str, arguments: &serde_json::Value) -> Option<String> {
    let command = arg_text(arguments, "command")?;
    let first_line = command.lines().next().unwrap_or(command).trim();
    let short_cmd = shorten_paths_in_text(first_line);
    let cwd = arg_text(arguments, "workdir")
        .map(|w| arg_suffix(format!("in {}", shorten_path(w))))
        .unwrap_or_default();
    Some(format!(
        "{} {} {}{}",
        glyph,
        verb_column(verb),
        truncate_visible(&short_cmd, MAX_TOOL_ARG_CHARS).bold(),
        cwd
    ))
}

fn format_read_call(glyph: &str, verb: &str, arguments: &serde_json::Value) -> Option<String> {
    let path = arg_text(arguments, "path")
        .or_else(|| arg_text(arguments, "file"))
        .or_else(|| arg_text(arguments, "filepath"))?;
    let short = shorten_path(path);
    let range = match (
        arg_number(arguments, "offset"),
        arg_number(arguments, "limit"),
    ) {
        (Some(offset), Some(limit)) => {
            arg_suffix(format!("L{}-{}", offset, offset + limit.saturating_sub(1)))
        }
        (Some(offset), None) => arg_suffix(format!("L{}+", offset)),
        (None, Some(limit)) => arg_suffix(format!("L1-{}", limit)),
        _ => String::new(),
    };
    Some(format!(
        "{} {} {}{}",
        glyph,
        verb_column(verb),
        short.cyan(),
        range
    ))
}

fn format_list_call(glyph: &str, verb: &str, arguments: &serde_json::Value) -> String {
    let path = arg_text(arguments, "path").unwrap_or(".");
    let short = shorten_path(path);
    let glob = arg_text(arguments, "glob")
        // `*` filters nothing, so it carries no information for the reader.
        .filter(|g| *g != "*")
        .map(|g| arg_suffix(format!("matching {}", g)))
        .unwrap_or_default();
    format!("{} {} {}{}", glyph, verb_column(verb), short.cyan(), glob)
}

fn format_write_call(glyph: &str, verb: &str, arguments: &serde_json::Value) -> Option<String> {
    let path = arg_text(arguments, "path")
        .or_else(|| arg_text(arguments, "file"))
        .or_else(|| arg_text(arguments, "filepath"))?;
    let short = shorten_path(path);
    let lines = arg_text(arguments, "content")
        .map(|c| c.lines().count())
        .unwrap_or(0);
    Some(format!(
        "{} {} {}{}",
        glyph,
        verb_column(verb),
        short.cyan(),
        arg_suffix(format!("{} lines", lines))
    ))
}

fn format_patch_call(glyph: &str, verb: &str, arguments: &serde_json::Value) -> Option<String> {
    let path_opt = arg_text(arguments, "path")
        .or_else(|| arg_text(arguments, "filePath"))
        .or_else(|| arg_text(arguments, "filepath"))
        .or_else(|| arg_text(arguments, "file"))
        .map(shorten_path)
        .or_else(|| patch_path_from_diff(arguments));

    let short = path_opt?;
    let mut extra = tool_activity_detail("patch", arguments)
        .map(arg_suffix)
        .unwrap_or_default();
    if matches!(
        arguments
            .get("replace_all")
            .or_else(|| arguments.get("replaceAll"))
            .and_then(|v| v.as_bool()),
        Some(true)
    ) {
        extra.push_str(&arg_suffix("all occurrences".to_string()));
    }
    Some(format!(
        "{} {} {}{}",
        glyph,
        verb_column(verb),
        short.cyan(),
        extra
    ))
}

/// The path a patch is aimed at, read from the first `---`/`+++` header of its diff.
fn patch_path_from_diff(arguments: &serde_json::Value) -> Option<String> {
    let diff_str = arg_text(arguments, "diff")
        .or_else(|| arg_text(arguments, "patch"))
        .or_else(|| arg_text(arguments, "patchText"))?;
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
}

fn format_search_call(glyph: &str, verb: &str, arguments: &serde_json::Value) -> String {
    let pattern = arg_text(arguments, "pattern").unwrap_or("");
    let path = arg_text(arguments, "path").unwrap_or(".");
    let short = shorten_path(path);
    let mode = arg_text(arguments, "output_mode").unwrap_or("content");
    let mode_tag = if mode == "content" {
        String::new()
    } else {
        arg_suffix(format!("({})", mode))
    };
    let include = arg_text(arguments, "file_glob")
        .filter(|glob| *glob != "*")
        .map(|g| arg_suffix(format!("matching {}", g)))
        .unwrap_or_default();
    format!(
        "{} {} {} in {}{}{}",
        glyph,
        verb_column(verb),
        quoted_arg(pattern),
        short.cyan(),
        mode_tag,
        include
    )
}

fn format_memory_call(name: &str, glyph: &str, arguments: &serde_json::Value) -> String {
    let action = match name {
        "save_memory" => "save",
        "update_memory" => "update",
        "forget_memory" => "forget",
        _ => arg_text(arguments, "action").unwrap_or("update"),
    };
    let verb_label = format!("memory {}", action);
    let mem_name = resolve_memory_name(arguments);
    let detail = if action == "save" || action == "update" {
        if let Some(obj) = arg_text(arguments, "object") {
            format!("{}{}", " = ".dimmed(), quoted_arg(obj))
        } else {
            String::new()
        }
    } else {
        String::new()
    };
    format!(
        "{} {} {}{}",
        glyph,
        verb_column(&verb_label),
        mem_name.cyan(),
        detail
    )
}

fn format_save_note_call(glyph: &str, verb: &str, arguments: &serde_json::Value) -> String {
    let title = arg_text(arguments, "title").unwrap_or("");
    let lines = arg_text(arguments, "content")
        .map(|c| c.lines().count())
        .unwrap_or(0);
    format!(
        "{} {} {}{}",
        glyph,
        verb_column(verb),
        quoted_arg(title),
        arg_suffix(format!("{} lines", lines))
    )
}

fn format_search_notes_call(glyph: &str, verb: &str, arguments: &serde_json::Value) -> String {
    let query = arg_text(arguments, "query").unwrap_or("");
    let limit = arg_number(arguments, "limit")
        .map(|l| arg_suffix(format!("limit {}", l)))
        .unwrap_or_default();
    format!(
        "{} {} {}{}",
        glyph,
        verb_column(verb),
        quoted_arg(query),
        limit
    )
}

fn format_skill_call(glyph: &str, verb: &str, arguments: &serde_json::Value) -> String {
    let skill_name = arg_text(arguments, "name")
        .or_else(|| arg_text(arguments, "skill"))
        .unwrap_or("");
    format!("{} {} {}", glyph, verb_column(verb), quoted_arg(skill_name))
}

fn format_skills_list_call(glyph: &str, verb: &str) -> String {
    format!("{} {} (all)", glyph, verb_column(verb))
}

fn format_clarify_call(glyph: &str, verb: &str, arguments: &serde_json::Value) -> String {
    // The question is the clarify box's job, and it is on screen directly above this line:
    // echoing it here (`question="…"`) put the same paragraph in the transcript twice, and a
    // multi-question call dumped the whole `questions` array as raw JSON beside it. What the
    // transcript keeps is that a question was asked, and how many.
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
    format!("{} {}{}", glyph, verb_column(verb), arg_suffix(label))
}

fn format_process_call(glyph: &str, arguments: &serde_json::Value) -> String {
    let action = arg_text(arguments, "action").unwrap_or("list");
    let job_id = arg_text(arguments, "job_id")
        .map(|j| format!(" {}", j.cyan()))
        .unwrap_or_default();
    format!("{} {}{}", glyph, verb_column(action), job_id)
}

fn format_delegate_call(glyph: &str, verb: &str, arguments: &serde_json::Value) -> String {
    let action = arg_text(arguments, "action").unwrap_or("spawn");
    if action != "spawn" {
        let detail = arg_text(arguments, "task_id")
            .map(|id| format!(" [{}]", id.cyan()))
            .unwrap_or_default();
        return format!(
            "{} {} ({}){}",
            glyph,
            verb_column(verb),
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
    format!("{} {} {}", glyph, verb_column(verb), goal_preview.cyan())
}

fn format_list_notes_call(glyph: &str, verb: &str, arguments: &serde_json::Value) -> String {
    let limit = arg_number(arguments, "limit").unwrap_or(20);
    let offset = arg_number(arguments, "offset").unwrap_or(0);
    let detail = if offset > 0 {
        arg_suffix(format!("limit {} offset {}", limit, offset))
    } else {
        arg_suffix(format!("limit {}", limit))
    };
    format!("{} {} (all){}", glyph, verb_column(verb), detail)
}

fn format_note_action_call(
    name: &str,
    glyph: &str,
    verb: &str,
    arguments: &serde_json::Value,
) -> Option<String> {
    let target = arg_text(arguments, "title").or_else(|| arg_text(arguments, "id"))?;
    let display_title = lookup_note_title(target).unwrap_or_else(|| target.to_string());
    let action = match name {
        "update_note" => arg_suffix("(update)".to_string()),
        "delete_note" => arg_suffix("(delete)".to_string()),
        _ => String::new(),
    };
    Some(format!(
        "{} {} {}{}",
        glyph,
        verb_column(verb),
        quoted_arg(&display_title),
        action
    ))
}

/// The generic renderer: `tool arg=value`, skipping nulls so unset optionals stay invisible.
/// MCP tools arrive as `mcp__server__tool`.
fn format_fallback_call(glyph: &str, name: &str, arguments: &serde_json::Value) -> String {
    let display_name = match name.strip_prefix("mcp__") {
        Some(stripped) => stripped.replace("__", "/"),
        None => name.to_string(),
    };

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

    format!(
        "{} {}{}",
        glyph,
        verb_column(&display_name),
        args_str.dimmed()
    )
}
