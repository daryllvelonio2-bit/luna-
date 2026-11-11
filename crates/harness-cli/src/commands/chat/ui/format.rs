use colored::Colorize;

use super::super::render::truncate_to_cells;
use super::metrics::visible_width;
use super::panels::{DelegateTaskView, MAX_MEMORY_NOTICE_NAME_CELLS};

/// One running child as a single composer row: its id, its goal, and what it is doing now.
pub(super) fn format_delegate_task_row(
    task: &DelegateTaskView,
    more: usize,
    width: usize,
) -> String {
    let prefix = "🤖 delegate";
    let goal = task.goal.split_whitespace().collect::<Vec<_>>().join(" ");
    let more_str = if more > 0 {
        format!(" (+{} more)", more)
    } else {
        String::new()
    };
    let body = format!("{} · {} · {}{}", task.id, task.activity, goal, more_str);
    let budget = width.saturating_sub(visible_width(prefix) + 1);
    format!(
        "{} {}",
        prefix.bold(),
        truncate_to_cells(&body, budget).dimmed()
    )
}

/// One composer row for a write: `memory <name> <saved|updated>` or `note <title> <saved>`,
/// cleanly displaying changes when available.
pub(super) fn format_save_notice(
    storage: &str,
    name: &str,
    detail: Option<&str>,
    status: &str,
    width: usize,
) -> String {
    let status_str = status.trim();
    let status_styled = match status_str {
        "saved" => status_str.green(),
        "updated" => status_str.yellow(),
        "deleted" => status_str.red(),
        "merged" => status_str.cyan(),
        other => other.normal(),
    };
    let storage_styled = match storage {
        "note" => storage.magenta(),
        _ => storage.dimmed(),
    };

    let clean_name = if name.trim().is_empty() || name.trim() == "-" {
        storage
    } else {
        name.trim()
    };

    if let Some(detail_str) = detail.map(str::trim).filter(|d| !d.is_empty()) {
        let base_fixed = visible_width(storage) + 1 + 1 + visible_width(status_str) + 2;
        if width > base_fixed + 8 {
            let max_name = MAX_MEMORY_NOTICE_NAME_CELLS.min(width.saturating_sub(base_fixed) / 2);
            let truncated_name = truncate_to_cells(clean_name, max_name.max(12));
            let used_for_prefix = visible_width(storage)
                + 1
                + visible_width(&truncated_name)
                + 1
                + visible_width(status_str)
                + 2;
            let budget_detail = width.saturating_sub(used_for_prefix);
            if budget_detail >= 6 {
                let truncated_detail = truncate_to_cells(detail_str, budget_detail);
                return format!(
                    "{} {} {}: {}",
                    storage_styled,
                    truncated_name.cyan(),
                    status_styled,
                    truncated_detail.dimmed()
                );
            }
        }
    }

    let fixed = visible_width(storage) + 1 + 1 + visible_width(status_str);
    let budget = width
        .saturating_sub(fixed)
        .min(MAX_MEMORY_NOTICE_NAME_CELLS);
    let truncated_name = truncate_to_cells(clean_name, budget);
    format!(
        "{} {} {}",
        storage_styled,
        truncated_name.cyan(),
        status_styled
    )
}
