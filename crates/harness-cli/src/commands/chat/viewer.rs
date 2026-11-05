use colored::Colorize;
use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use std::io::{self, Write};
use std::time::Duration;
use tokio::sync::mpsc;

use harness_core::session::SessionStore;
use harness_core::tasks::{BackgroundTaskQueue, JobSummary};

use super::keys::AppKey;
use super::render::{parse_turns_from_history, render_all_turns, render_turn_lines, TurnRecord};
use super::ui::ChatUi;

/// What the viewer paints below its header for one task.
struct TaskBody {
    /// Physical rows, already formatted for the terminal.
    lines: Vec<String>,
    /// The compact figures shown beside the task's status and goal.
    progress: String,
}

pub async fn show_background_task_viewer(
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    bg_queue: Option<&BackgroundTaskQueue>,
    delegate_tool: Option<&harness_core::tools::DelegateTaskTool>,
    store: &dyn SessionStore,
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

    let mut width = super::ui::get_terminal_width().max(40);
    let mut body = task_body(&tasks[selected_idx], store, process_expanded, width).await;

    // Initial render
    let (eff, mut max_offset) = render_viewer_frame(
        &tasks[selected_idx],
        &tasks,
        selected_idx,
        if auto_scroll {
            usize::MAX
        } else {
            scroll_offset
        },
        &body,
        width,
    );
    scroll_offset = eff;

    loop {
        tokio::select! {
            _ = refresh_interval.tick() => {
                // Periodically refresh job statuses and transcripts. A child still writing gets a
                // new stored row, so the CLI transcript below the header follows it live.
                tasks = collect_tasks(bg_queue, delegate_tool).await;
                if tasks.is_empty() {
                    break;
                }
                if selected_idx >= tasks.len() {
                    selected_idx = tasks.len().saturating_sub(1);
                }
                width = super::ui::get_terminal_width().max(40);
                body = task_body(&tasks[selected_idx], store, process_expanded, width).await;
                let target_offset = if auto_scroll { usize::MAX } else { scroll_offset };
                let (eff, max) = render_viewer_frame(
                    &tasks[selected_idx],
                    &tasks,
                    selected_idx,
                    target_offset,
                    &body,
                    width,
                );
                scroll_offset = eff;
                max_offset = max;
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
                            let (eff, _) = render_viewer_frame(
                                &tasks[selected_idx],
                                &tasks,
                                selected_idx,
                                scroll_offset,
                                &body,
                                width,
                            );
                            scroll_offset = eff;
                        }
                    }
                    AppKey::Down | AppKey::Char('j') | AppKey::Char('J') => {
                        if scroll_offset < max_offset {
                            scroll_offset = scroll_offset.saturating_add(1);
                            if scroll_offset >= max_offset {
                                auto_scroll = true;
                            }
                            let (eff, _) = render_viewer_frame(
                                &tasks[selected_idx],
                                &tasks,
                                selected_idx,
                                scroll_offset,
                                &body,
                                width,
                            );
                            scroll_offset = eff;
                        }
                    }
                    AppKey::PageUp | AppKey::Char('b') => {
                        auto_scroll = false;
                        scroll_offset = scroll_offset.saturating_sub(10);
                        let (eff, _) = render_viewer_frame(
                            &tasks[selected_idx],
                            &tasks,
                            selected_idx,
                            scroll_offset,
                            &body,
                            width,
                        );
                        scroll_offset = eff;
                    }
                    AppKey::PageDown | AppKey::Char('f') | AppKey::Char(' ') => {
                        scroll_offset = (scroll_offset + 10).min(max_offset);
                        if scroll_offset >= max_offset {
                            auto_scroll = true;
                        }
                        let (eff, _) = render_viewer_frame(
                            &tasks[selected_idx],
                            &tasks,
                            selected_idx,
                            scroll_offset,
                            &body,
                            width,
                        );
                        scroll_offset = eff;
                    }
                    AppKey::Home | AppKey::Char('g') => {
                        auto_scroll = false;
                        scroll_offset = 0;
                        let (eff, _) = render_viewer_frame(
                            &tasks[selected_idx],
                            &tasks,
                            selected_idx,
                            scroll_offset,
                            &body,
                            width,
                        );
                        scroll_offset = eff;
                    }
                    AppKey::End | AppKey::Char('G') => {
                        auto_scroll = true;
                        scroll_offset = max_offset;
                        let (eff, _) = render_viewer_frame(
                            &tasks[selected_idx],
                            &tasks,
                            selected_idx,
                            scroll_offset,
                            &body,
                            width,
                        );
                        scroll_offset = eff;
                    }
                    AppKey::Tab | AppKey::Right | AppKey::Char('l') | AppKey::Char('L') => {
                        selected_idx = (selected_idx + 1) % tasks.len();
                        auto_scroll = true;
                        scroll_offset = usize::MAX;
                        width = super::ui::get_terminal_width().max(40);
                        body = task_body(&tasks[selected_idx], store, process_expanded, width).await;
                        let (eff, max) = render_viewer_frame(
                            &tasks[selected_idx],
                            &tasks,
                            selected_idx,
                            scroll_offset,
                            &body,
                            width,
                        );
                        scroll_offset = eff;
                        max_offset = max;
                    }
                    AppKey::BackTab | AppKey::Left | AppKey::Char('h') | AppKey::Char('H') => {
                        selected_idx = if selected_idx == 0 {
                            tasks.len() - 1
                        } else {
                            selected_idx - 1
                        };
                        auto_scroll = true;
                        scroll_offset = usize::MAX;
                        width = super::ui::get_terminal_width().max(40);
                        body = task_body(&tasks[selected_idx], store, process_expanded, width).await;
                        let (eff, max) = render_viewer_frame(
                            &tasks[selected_idx],
                            &tasks,
                            selected_idx,
                            scroll_offset,
                            &body,
                            width,
                        );
                        scroll_offset = eff;
                        max_offset = max;
                    }
                    AppKey::Char(c @ '1'..='9') => {
                        let idx = (c as usize) - ('1' as usize);
                        if idx < tasks.len() {
                            selected_idx = idx;
                            auto_scroll = true;
                            scroll_offset = usize::MAX;
                            width = super::ui::get_terminal_width().max(40);
                            body = task_body(&tasks[selected_idx], store, process_expanded, width).await;
                            let (eff, max) = render_viewer_frame(
                                &tasks[selected_idx],
                                &tasks,
                                selected_idx,
                                scroll_offset,
                                &body,
                                width,
                            );
                            scroll_offset = eff;
                            max_offset = max;
                        }
                    }
                    AppKey::Char('x') | AppKey::Char('X') => {
                        // Kill / cancel active job or subagent
                        let task = tasks.get(selected_idx);
                        if let Some(task) = task {
                            if let Some(q) = bg_queue {
                                let _ = q.kill(&task.id);
                            }
                            if let Some(dt) = delegate_tool {
                                let _ = dt.kill(&task.id).await;
                            }
                        }
                        tasks = collect_tasks(bg_queue, delegate_tool).await;
                        if tasks.is_empty() {
                            break;
                        }
                        if selected_idx >= tasks.len() {
                            selected_idx = tasks.len().saturating_sub(1);
                        }
                        width = super::ui::get_terminal_width().max(40);
                        body = task_body(&tasks[selected_idx], store, process_expanded, width).await;
                        let (eff, max) = render_viewer_frame(
                            &tasks[selected_idx],
                            &tasks,
                            selected_idx,
                            scroll_offset,
                            &body,
                            width,
                        );
                        scroll_offset = eff;
                        max_offset = max;
                    }
                    AppKey::Escape
                    | AppKey::Char('q')
                    | AppKey::Char('Q')
                    | AppKey::CtrlC
                    | AppKey::Ctrl('c')
                    | AppKey::CtrlShiftB
                    | AppKey::Ctrl('b')
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

/// The rows for one task.
///
/// A delegated subagent or a worker is an actual agent turn stored in `sessions.db`, so its body
/// is the CLI transcript itself: the same user line, process header, tool calls, reasoning and
/// Luna reply that the parent chat renders. Shell jobs have no conversation, so they keep the
/// raw log they stream.
async fn task_body(
    task: &JobSummary,
    store: &dyn SessionStore,
    expanded: bool,
    width: usize,
) -> TaskBody {
    if task_is_subagent(task) {
        return transcript_body(task, store, expanded, width).await;
    }
    log_body(task)
}

/// Render the child session behind a delegate/worker with the parent CLI's own renderer.
///
/// The viewer deliberately reads the display transcript (`replay_transcript`) rather than the
/// model replay: compacted rows still belong on screen, and summary rows are internal artifacts,
/// not conversation turns.
async fn transcript_body(
    task: &JobSummary,
    store: &dyn SessionStore,
    expanded: bool,
    width: usize,
) -> TaskBody {
    match store.replay_transcript(&task.id, usize::MAX).await {
        Ok(outcome) => {
            let messages = outcome.history.len();
            // A worker is a fork: its session holds the parent conversation as context before the
            // task prompt. The viewer is about what the worker did, so show the latest turn — the
            // task and its work — rather than pasting the parent transcript underneath a header
            // that says "worker". A delegate child starts empty, so this is its one turn too.
            let mut turns = parse_turns_from_history(&outcome.history);
            let turn_count = turns.len();
            let turns = turns.split_off(turn_count.saturating_sub(1));
            let mut lines = render_turn_lines(&turns, expanded, width);
            if lines.is_empty() {
                lines.push("  (waiting for the first message...)".dimmed().to_string());
            }
            let progress = format!(
                "{} message{} · {} turn{}",
                messages,
                if messages == 1 { "" } else { "s" },
                turns.len(),
                if turns.len() == 1 { "" } else { "s" }
            );
            TaskBody { lines, progress }
        }
        Err(error) => TaskBody {
            lines: vec![format!("  (unable to load the child transcript: {error})")
                .dimmed()
                .to_string()],
            progress: "transcript unavailable".to_string(),
        },
    }
}

/// The raw output of a shell background job, which has no stored conversation.
fn log_body(task: &JobSummary) -> TaskBody {
    let log = std::fs::read_to_string(&task.log_path);
    let (log_lines, log_bytes) = match &log {
        Ok(text) => (text.lines().count(), text.len()),
        Err(_) => (0, 0),
    };
    let updated_secs = std::fs::metadata(&task.log_path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| std::time::SystemTime::now().duration_since(modified).ok())
        .map(|age| age.as_secs_f64())
        .unwrap_or(0.0);
    let progress = if task.status == "running" {
        format!(
            "{} lines · {} · updated {:.1}s ago",
            log_lines,
            format_bytes(log_bytes),
            updated_secs
        )
    } else {
        format!("{} lines · {}", log_lines, format_bytes(log_bytes))
    };

    let mut lines = Vec::new();
    match &log {
        Ok(text) => {
            let log_lines: Vec<&str> = text.lines().collect();
            if log_lines.is_empty() {
                lines.push("  (waiting for output...)".dimmed().to_string());
            } else {
                for line in log_lines {
                    lines.push(format!("  {line}"));
                }
            }
        }
        Err(error) => lines.push(
            format!("  (unable to read log file: {error})")
                .dimmed()
                .to_string(),
        ),
    }

    // The log ends when the child stops writing, not when its last tool call returned. A finished
    // task therefore gets an explicit terminal marker, so the last "▶ Tool Call" line cannot read
    // as work that is still in flight.
    if task.status != "running" {
        lines.push(String::new());
        lines.push(format!(
            "  {}  {}",
            format_process_status(&task.status),
            format!("finished after {}s", task.elapsed_secs).dimmed()
        ));
    }

    TaskBody { lines, progress }
}

/// Paint one frame: viewer chrome, the selected task's body, and the scroll position.
fn render_viewer_frame(
    task: &JobSummary,
    all_tasks: &[JobSummary],
    cur_idx: usize,
    offset: usize,
    body: &TaskBody,
    width: usize,
) -> (usize, usize) {
    print!("\x1b[H\x1b[2J");
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

    // 3. One compact summary line: status, progress, kind and subject.
    header_lines.push(format_task_summary(task, width, &body.progress));
    header_lines.push(divider.clone());

    let footer_lines_count = 2;
    let view_height = term_height
        .saturating_sub(header_lines.len() + footer_lines_count)
        .max(3);

    let total_lines = body.lines.len();
    let max_offset = total_lines.saturating_sub(view_height);
    let eff_offset = offset.min(max_offset);

    for header in &header_lines {
        println!("{header}");
    }

    let visible = &body.lines[eff_offset..total_lines.min(eff_offset + view_height)];
    for line in visible {
        let truncated = dialoguer::console::truncate_str(line, width - 1, "…");
        println!("{truncated}");
    }

    for _ in visible.len()..view_height {
        println!();
    }

    println!("{divider}");
    let pos_str = format!(
        "Lines {}-{} of {} • [↑/↓ j/k: scroll • Tab: switch task • x: kill • q: exit]",
        if total_lines == 0 { 0 } else { eff_offset + 1 },
        (eff_offset + visible.len()).min(total_lines),
        total_lines
    );
    print!("  {}", pos_str.dimmed());
    let _ = io::stdout().flush();

    (eff_offset, max_offset)
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
fn format_task_summary(task: &JobSummary, width: usize, progress: &str) -> String {
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
    let lead = format!(
        "  {}  {}  {}  ",
        format_process_status(&task.status),
        format!("{}s", task.elapsed_secs).dimmed(),
        kind.dimmed()
    );
    let lead_len = dialoguer::console::strip_ansi_codes(&lead).chars().count();
    let progress = if progress.is_empty() {
        String::new()
    } else {
        format!("{}  ", progress.dimmed())
    };
    let progress_len = dialoguer::console::strip_ansi_codes(&progress)
        .chars()
        .count();
    let subject_width = width.saturating_sub(lead_len + progress_len + 1);
    format!(
        "{}{}{}",
        lead,
        progress,
        dialoguer::console::truncate_str(subject, subject_width, "…")
    )
}

/// Compact log sizes for the progress line; a task's progress is easier to read as "1.2 KiB"
/// than as a raw byte count.
fn format_bytes(bytes: usize) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    if bytes as f64 >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB)
    } else if bytes as f64 >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes} B")
    }
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
            log_path: ".luna/jobs/s41253240.log".to_string(),
        }
    }

    #[test]
    fn the_task_header_is_one_compact_status_line() {
        let line = dialoguer::console::strip_ansi_codes(&format_task_summary(
            &task("worker: fix the failing analytics tests", "completed"),
            100,
            "42 lines · 3.1 KiB",
        ))
        .to_string();

        assert_eq!(line.matches('\n').count(), 0, "the header is one row");
        assert!(line.contains("COMPLETED"), "{line}");
        assert!(line.contains("485s"), "{line}");
        assert!(line.contains("worker"), "{line}");
        assert!(line.contains("42 lines · 3.1 KiB"), "{line}");
        assert!(line.contains("fix the failing analytics tests"), "{line}");
        for removed in ["Goal:", "Status:", "Type:", "Log:"] {
            assert!(
                !line.contains(removed),
                "the old metadata card label {removed} is gone: {line}"
            );
        }
    }

    #[test]
    fn log_sizes_stay_compact() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
        assert_eq!(format_bytes(2 * 1024 * 1024), "2.0 MiB");
    }

    #[test]
    fn workers_and_subagents_share_the_transcript_body() {
        let mut subagent = task("subagent: audit the parser", "completed");
        subagent.id = "sub_20261105_120000_1".to_string();
        assert!(task_is_subagent(&subagent));

        let worker = task("worker: fix the tests", "running");
        assert!(task_is_subagent(&worker));

        let job = task("shell: cargo test", "running");
        assert!(!task_is_subagent(&job));
    }

    #[tokio::test]
    async fn a_worker_body_is_the_cli_transcript_not_its_log() {
        use harness_core::{Message, SessionStore, SqliteSessionStore};

        let dir = tempfile::tempdir().unwrap();
        let store = SqliteSessionStore::new(Some(dir.path().join("sessions.db"))).unwrap();
        store.ensure_session("se42f23").await.unwrap();
        // The fork carries the parent's turns as context; the viewer only shows the worker's own.
        store
            .append("se42f23", &Message::user("parent question"), false)
            .await
            .unwrap();
        store
            .append("se42f23", &Message::assistant("parent answer"), false)
            .await
            .unwrap();
        store
            .append("se42f23", &Message::user("remove the model name"), false)
            .await
            .unwrap();
        store
            .append(
                "se42f23",
                &Message::assistant("Removed it from .env."),
                false,
            )
            .await
            .unwrap();

        let mut worker = task("worker: remove the model name", "completed");
        worker.id = "se42f23".to_string();
        worker.log_path = dir.path().join("missing.log").display().to_string();

        let body = task_body(&worker, &store, true, 100).await;
        let text = body
            .lines
            .iter()
            .map(|row| dialoguer::console::strip_ansi_codes(row).to_string())
            .collect::<Vec<_>>()
            .join("\n");

        assert!(text.contains("remove the model name"), "{text}");
        assert!(text.contains("Luna"), "the CLI reply label is used: {text}");
        assert!(text.contains("Removed it from .env."), "{text}");
        assert!(
            !text.contains("parent question"),
            "the viewer shows the worker's turn, not the fork's parent history: {text}"
        );
        assert!(
            !text.contains("unable to read log"),
            "the worker no longer reads its log: {text}"
        );
        assert_eq!(body.progress, "4 messages · 1 turn");
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
