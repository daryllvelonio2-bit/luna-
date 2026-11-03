use colored::Colorize;
use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use std::io::{self, Write};
use std::time::Duration;
use tokio::sync::mpsc;

use harness_core::tasks::{BackgroundTaskQueue, JobSummary};

use super::keys::AppKey;
use super::render::{render_all_turns, TurnRecord};
use super::ui::ChatUi;

pub async fn show_background_task_viewer(
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    bg_queue: Option<&BackgroundTaskQueue>,
    delegate_tool: Option<&harness_core::tools::DelegateTaskTool>,
    ui: &ChatUi,
    turns: &[TurnRecord],
    process_expanded: bool,
) {
    // 1. Collect all available background tasks and subagents
    let mut tasks = collect_tasks(bg_queue, delegate_tool).await;
    if tasks.is_empty() {
        ui.emit_above(&format!(
            "{} No background tasks or subagents active or recorded",
            "•".dimmed()
        ));
        return;
    }

    let _ = execute!(io::stdout(), EnterAlternateScreen, Hide);

    let mut selected_idx: usize = 0;
    // Prefer selecting an actively running task if available
    if let Some(pos) = tasks.iter().position(|t| t.status == "running") {
        selected_idx = pos;
    }

    let mut scroll_offset: usize = 0;
    let mut auto_scroll = true;
    let mut refresh_interval = tokio::time::interval(Duration::from_millis(500));

    let render = |task: &JobSummary,
                  all_tasks: &[JobSummary],
                  cur_idx: usize,
                  offset: usize|
     -> (usize, usize) {
        print!("\x1b[H\x1b[2J");
        let width = super::ui::get_terminal_width().max(40);
        let term_height = super::ui::get_terminal_height().max(10);

        let divider = "─".repeat(width).dimmed().to_string();
        let mut header_lines = Vec::new();

        // 1. Top bar: Summary & Controls
        let running_count = all_tasks.iter().filter(|t| t.status == "running").count();
        let header_left = format!(
            "{} {} {}",
            "●".cyan().bold(),
            "BACKGROUND TASKS".bold(),
            format!("[{} active • {} total]", running_count, all_tasks.len()).dimmed()
        );
        let header_right = "[Tab: next • x: kill • q: close]".dimmed().to_string();
        let left_len = dialoguer::console::strip_ansi_codes(&header_left)
            .chars()
            .count();
        let right_len = dialoguer::console::strip_ansi_codes(&header_right)
            .chars()
            .count();
        let spaces = width.saturating_sub(left_len + right_len);
        header_lines.push(format!(
            "  {}{}{}",
            header_left,
            " ".repeat(spaces.max(1)),
            header_right
        ));
        header_lines.push(divider.clone());

        // 2. Task switcher tabs (if multiple tasks)
        if all_tasks.len() > 1 {
            let mut tabs_line = String::from("  ");
            for (i, t) in all_tasks.iter().enumerate() {
                let is_sel = i == cur_idx;
                let num = i + 1;
                let dot = if t.status == "running" {
                    "●".cyan()
                } else {
                    "○".dimmed()
                };
                let short_id = t.id.as_str();
                let tab_label = if is_sel {
                    format!("[{num}] {dot} {short_id}*")
                        .bold()
                        .cyan()
                        .to_string()
                } else {
                    format!("[{num}] {dot} {short_id}").dimmed().to_string()
                };
                tabs_line.push_str(&tab_label);
                tabs_line.push_str("   ");
            }
            let truncated_tabs = dialoguer::console::truncate_str(&tabs_line, width - 2, "…");
            header_lines.push(truncated_tabs.to_string());
            header_lines.push(divider.clone());
        }

        // 3. One compact summary line. The old metadata card spent five rows on goal, status,
        // type and log path; the status and what the task is doing fit on one, leaving the rest
        // of the screen for the log itself.
        header_lines.push(format_task_summary(task, width));
        header_lines.push(divider.clone());

        // 4. Output / Logs Content
        let mut content_lines = Vec::new();
        match std::fs::read_to_string(&task.log_path) {
            Ok(s) => {
                let lines: Vec<&str> = s.lines().collect();
                if lines.is_empty() {
                    content_lines.push("  (waiting for output...)".dimmed().to_string());
                } else {
                    for l in lines {
                        content_lines.push(format!("  {}", l));
                    }
                }
            }
            Err(e) => {
                content_lines.push(
                    format!("  (unable to read log file: {})", e)
                        .dimmed()
                        .to_string(),
                );
            }
        }

        // The log ends when the child stops writing, not when its last tool call returned. A
        // finished task therefore gets an explicit terminal marker, so the last "▶ Tool Call"
        // line cannot read as work that is still in flight.
        if task.status != "running" {
            content_lines.push(String::new());
            content_lines.push(format!(
                "  {}  {}",
                format_process_status(&task.status),
                format!("finished after {}s", task.elapsed_secs).dimmed()
            ));
        }

        let footer_lines_count = 2;
        let view_height = term_height
            .saturating_sub(header_lines.len() + footer_lines_count)
            .max(3);

        let total_lines = content_lines.len();
        let max_offset = total_lines.saturating_sub(view_height);
        let eff_offset = offset.min(max_offset);

        for h in &header_lines {
            println!("{}", h);
        }

        let visible = &content_lines[eff_offset..total_lines.min(eff_offset + view_height)];
        for line in visible {
            let truncated = dialoguer::console::truncate_str(line, width - 1, "…");
            println!("{}", truncated);
        }

        for _ in visible.len()..view_height {
            println!();
        }

        println!("{}", divider);
        let pos_str = format!(
            "Lines {}-{} of {} • [↑/↓ j/k: scroll • Tab: switch task • x: kill • q: exit]",
            if total_lines == 0 { 0 } else { eff_offset + 1 },
            (eff_offset + visible.len()).min(total_lines),
            total_lines
        );
        print!("  {}", pos_str.dimmed());
        let _ = io::stdout().flush();

        (eff_offset, max_offset)
    };

    // Initial render
    let (eff, mut max_offset) = render(
        &tasks[selected_idx],
        &tasks,
        selected_idx,
        if auto_scroll {
            usize::MAX
        } else {
            scroll_offset
        },
    );
    scroll_offset = eff;

    loop {
        tokio::select! {
            _ = refresh_interval.tick() => {
                // Periodically refresh job statuses and logs
                tasks = collect_tasks(bg_queue, delegate_tool).await;
                if selected_idx >= tasks.len() {
                    selected_idx = tasks.len().saturating_sub(1);
                }
                if let Some(current) = tasks.get(selected_idx) {
                    let target_offset = if auto_scroll { usize::MAX } else { scroll_offset };
                    let (eff, max) = render(current, &tasks, selected_idx, target_offset);
                    scroll_offset = eff;
                    max_offset = max;
                }
            }
            maybe_key = key_rx.recv() => {
                let key = match maybe_key {
                    Some(k) => k,
                    None => break,
                };
                match key {
                    AppKey::Up | AppKey::Char('k') | AppKey::Char('K') => {
                        auto_scroll = false;
                        if scroll_offset > 0 {
                            scroll_offset = scroll_offset.saturating_sub(1);
                            if let Some(current) = tasks.get(selected_idx) {
                                let (eff, _) = render(current, &tasks, selected_idx, scroll_offset);
                                scroll_offset = eff;
                            }
                        }
                    }
                    AppKey::Down | AppKey::Char('j') | AppKey::Char('J') => {
                        if scroll_offset < max_offset {
                            scroll_offset = scroll_offset.saturating_add(1);
                            if scroll_offset >= max_offset {
                                auto_scroll = true;
                            }
                            if let Some(current) = tasks.get(selected_idx) {
                                let (eff, _) = render(current, &tasks, selected_idx, scroll_offset);
                                scroll_offset = eff;
                            }
                        }
                    }
                    AppKey::PageUp | AppKey::Char('b') => {
                        auto_scroll = false;
                        scroll_offset = scroll_offset.saturating_sub(10);
                        if let Some(current) = tasks.get(selected_idx) {
                            let (eff, _) = render(current, &tasks, selected_idx, scroll_offset);
                            scroll_offset = eff;
                        }
                    }
                    AppKey::PageDown | AppKey::Char('f') | AppKey::Char(' ') => {
                        scroll_offset = (scroll_offset + 10).min(max_offset);
                        if scroll_offset >= max_offset {
                            auto_scroll = true;
                        }
                        if let Some(current) = tasks.get(selected_idx) {
                            let (eff, _) = render(current, &tasks, selected_idx, scroll_offset);
                            scroll_offset = eff;
                        }
                    }
                    AppKey::Home | AppKey::Char('g') => {
                        auto_scroll = false;
                        scroll_offset = 0;
                        if let Some(current) = tasks.get(selected_idx) {
                            let (eff, _) = render(current, &tasks, selected_idx, scroll_offset);
                            scroll_offset = eff;
                        }
                    }
                    AppKey::End | AppKey::Char('G') => {
                        auto_scroll = true;
                        scroll_offset = max_offset;
                        if let Some(current) = tasks.get(selected_idx) {
                            let (eff, _) = render(current, &tasks, selected_idx, scroll_offset);
                            scroll_offset = eff;
                        }
                    }
                    AppKey::Tab | AppKey::Right | AppKey::Char('l') | AppKey::Char('L') => {
                        if !tasks.is_empty() {
                            selected_idx = (selected_idx + 1) % tasks.len();
                            auto_scroll = true;
                            scroll_offset = usize::MAX;
                            if let Some(current) = tasks.get(selected_idx) {
                                let (eff, max) = render(current, &tasks, selected_idx, scroll_offset);
                                scroll_offset = eff;
                                max_offset = max;
                            }
                        }
                    }
                    AppKey::BackTab | AppKey::Left | AppKey::Char('h') | AppKey::Char('H') => {
                        if !tasks.is_empty() {
                            selected_idx = if selected_idx == 0 {
                                tasks.len() - 1
                            } else {
                                selected_idx - 1
                            };
                            auto_scroll = true;
                            scroll_offset = usize::MAX;
                            if let Some(current) = tasks.get(selected_idx) {
                                let (eff, max) = render(current, &tasks, selected_idx, scroll_offset);
                                scroll_offset = eff;
                                max_offset = max;
                            }
                        }
                    }
                    AppKey::Char(c @ '1'..='9') => {
                        let idx = (c as usize) - ('1' as usize);
                        if idx < tasks.len() {
                            selected_idx = idx;
                            auto_scroll = true;
                            scroll_offset = usize::MAX;
                            if let Some(current) = tasks.get(selected_idx) {
                                let (eff, max) = render(current, &tasks, selected_idx, scroll_offset);
                                scroll_offset = eff;
                                max_offset = max;
                            }
                        }
                    }
                    AppKey::Char('x') | AppKey::Char('X') => {
                        // Kill / cancel active job or subagent
                        if let Some(task) = tasks.get(selected_idx) {
                            if let Some(q) = bg_queue {
                                let _ = q.kill(&task.id);
                            }
                            if let Some(dt) = delegate_tool {
                                let _ = dt.kill(&task.id).await;
                            }
                            tasks = collect_tasks(bg_queue, delegate_tool).await;
                            if let Some(current) = tasks.get(selected_idx) {
                                let (eff, max) = render(current, &tasks, selected_idx, scroll_offset);
                                scroll_offset = eff;
                                max_offset = max;
                            }
                        }
                    }
                    AppKey::Escape
                    | AppKey::Char('q')
                    | AppKey::Char('Q')
                    | AppKey::CtrlC
                    | AppKey::Ctrl('c')
                    | AppKey::Char('\x02') => {
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
    render_all_turns(turns, process_expanded, super::ui::get_terminal_width());
    if let Some(ref err) = ui.transient_error() {
        println!();
        println!("{}", err);
    }
    ui.draw_initial_box();
}

pub(crate) async fn collect_tasks(
    bg_queue: Option<&BackgroundTaskQueue>,
    delegate_tool: Option<&harness_core::tools::DelegateTaskTool>,
) -> Vec<JobSummary> {
    let mut tasks = Vec::new();

    // Terminal Background Jobs
    if let Some(q) = bg_queue {
        for job in q.list() {
            tasks.push(job);
        }
    }

    // Delegated Subagents
    if let Some(dt) = delegate_tool {
        for sub in dt.list_tasks().await {
            // A finished task's elapsed time is frozen at its finish; reading `Utc::now()` made a
            // completed child look like it was still running (the timer kept climbing) even
            // though its status said COMPLETED.
            let elapsed_secs = task_elapsed_secs(&sub.status, sub.started_at, chrono::Utc::now());
            let status = match &sub.status {
                harness_core::tools::delegate::SubagentStatus::Running => "running".to_string(),
                harness_core::tools::delegate::SubagentStatus::Completed { .. } => {
                    "completed".to_string()
                }
                harness_core::tools::delegate::SubagentStatus::Failed { error, .. } => {
                    format!("failed: {error}")
                }
            };
            // Worker records are recognizable by the marker `spawn_worker` puts in their context;
            // their ids are ordinary forked session ids, not the `sub_*` shape.
            let is_worker = sub
                .context
                .as_deref()
                .is_some_and(|context| context.starts_with("worker profile:"));
            let kind = if is_worker { "worker" } else { "subagent" };
            tasks.push(JobSummary {
                id: sub.id.clone(),
                command: format!("{kind}: {}", sub.goal),
                workdir: if is_worker {
                    "worker (forked conversation)".to_string()
                } else {
                    "delegated subagent".to_string()
                },
                status,
                elapsed_secs,
                log_path: sub.log_path.unwrap_or_else(|| format!("{}.log", sub.id)),
            });
        }
    }

    tasks
}

/// A delegated child or worker runs as an agent turn, not a shell job.
fn task_is_subagent(task: &JobSummary) -> bool {
    task.id.starts_with("sub_")
        || task.command.starts_with("worker:")
        || task.command.starts_with("subagent:")
}

/// How long the task ran: `now` while it runs, and the recorded finish time once it stops.
fn task_elapsed_secs(
    status: &harness_core::tools::delegate::SubagentStatus,
    started_at: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> u64 {
    let ended_at = match status {
        harness_core::tools::delegate::SubagentStatus::Running => now,
        harness_core::tools::delegate::SubagentStatus::Completed { finished_at, .. }
        | harness_core::tools::delegate::SubagentStatus::Failed { finished_at, .. } => *finished_at,
    };
    (ended_at - started_at).num_seconds().max(0) as u64
}

/// The one-line header for the selected task: status, elapsed time, kind, and its subject.
///
/// Deliberately compact: the screen is for the log, so the old five-row metadata card
/// (goal/status/type/log) is collapsed to this.
fn format_task_summary(task: &JobSummary, width: usize) -> String {
    let kind = if task.command.starts_with("worker:") {
        "worker"
    } else if task_is_subagent(task) {
        "subagent"
    } else {
        "job"
    };
    let subject = task
        .command
        .strip_prefix("worker: ")
        .or_else(|| task.command.strip_prefix("subagent: "))
        .unwrap_or(&task.command);
    format!(
        "  {}  {}  {}  {}",
        format_process_status(&task.status),
        format!("{}s", task.elapsed_secs).dimmed(),
        kind.dimmed(),
        dialoguer::console::truncate_str(subject, width.saturating_sub(34), "…")
    )
}

fn format_process_status(status: &str) -> colored::ColoredString {
    if status == "running" {
        "● RUNNING".cyan().bold()
    } else if status.contains("completed") || status.contains("exit 0") {
        "✔ COMPLETED".green().bold()
    } else if status.contains("failed") || status.contains("exit") {
        status.red().bold()
    } else if status == "killed" {
        "✖ KILLED".dimmed()
    } else {
        status.normal()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(command: &str, status: &str) -> JobSummary {
        JobSummary {
            id: "s41253240".to_string(),
            command: command.to_string(),
            workdir: "worker (forked conversation)".to_string(),
            status: status.to_string(),
            elapsed_secs: 485,
            log_path: "/home/janelle/.luna/jobs/s41253240.log".to_string(),
        }
    }

    #[test]
    fn the_task_header_is_one_compact_status_line() {
        let line = dialoguer::console::strip_ansi_codes(&format_task_summary(
            &task("worker: fix the failing analytics tests", "completed"),
            100,
        ))
        .to_string();

        assert_eq!(line.matches('\n').count(), 0, "the header is one row");
        assert!(line.contains("COMPLETED"), "{line}");
        assert!(line.contains("485s"), "{line}");
        assert!(line.contains("worker"), "{line}");
        assert!(line.contains("fix the failing analytics tests"), "{line}");
        for removed in ["Goal:", "Status:", "Type:", "Log:"] {
            assert!(
                !line.contains(removed),
                "the old metadata card label {removed} is gone: {line}"
            );
        }
    }

    #[test]
    fn a_finished_tasks_elapsed_time_stops_at_its_finish() {
        use harness_core::tools::delegate::SubagentStatus;

        let started_at = chrono::Utc::now();
        let finished_at = started_at + chrono::Duration::seconds(10);
        let now = started_at + chrono::Duration::seconds(500);

        let completed = SubagentStatus::Completed {
            reply: "done".to_string(),
            finished_at,
        };
        assert_eq!(task_elapsed_secs(&completed, started_at, now), 10);

        // A running task keeps counting; only finish freezes the clock.
        assert_eq!(
            task_elapsed_secs(&SubagentStatus::Running, started_at, now),
            500
        );
    }
}
