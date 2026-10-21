use std::io::{self, Write};
use std::time::Duration;
use colored::Colorize;
use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use tokio::sync::mpsc;

use harness_core::tasks::{BackgroundTaskQueue, JobSummary};
use harness_evolver::{CodebaseEvolver, EvolverEvent, JobRecord, JobStatus};

use super::keys::AppKey;
use super::render::{render_all_turns, TurnRecord};
use super::ui::ChatUi;

enum TaskEntry {
    Evolver(JobRecord),
    Process(JobSummary),
}

impl TaskEntry {
    fn id(&self) -> &str {
        match self {
            Self::Evolver(j) => &j.id,
            Self::Process(j) => &j.id,
        }
    }

    #[allow(dead_code)]
    fn title(&self) -> String {
        match self {
            Self::Evolver(j) => {
                let short_task = if j.task.len() > 40 {
                    format!("{}…", &j.task[..39])
                } else {
                    j.task.clone()
                };
                format!("evolver/{} • {}", j.id, short_task)
            }
            Self::Process(j) => {
                let short_cmd = if j.command.len() > 40 {
                    format!("{}…", &j.command[..39])
                } else {
                    j.command.clone()
                };
                format!("proc/{} • {}", j.id, short_cmd)
            }
        }
    }

    fn is_running(&self) -> bool {
        match self {
            Self::Evolver(j) => matches!(j.status, JobStatus::Running | JobStatus::Queued),
            Self::Process(j) => j.status == "running",
        }
    }
}

pub async fn show_background_task_viewer(
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    evolver: &CodebaseEvolver,
    bg_queue: Option<&BackgroundTaskQueue>,
    ui: &ChatUi,
    turns: &[TurnRecord],
    process_expanded: bool,
) {
    // 1. Collect all available background tasks
    let mut tasks = collect_tasks(evolver, bg_queue).await;
    if tasks.is_empty() {
        ui.emit_above(&format!("{} No background tasks active or recorded", "•".dimmed()));
        return;
    }

    let _ = execute!(io::stdout(), EnterAlternateScreen, Hide);

    let mut selected_idx: usize = 0;
    // Prefer selecting an actively running task if available
    if let Some(pos) = tasks.iter().position(|t| t.is_running()) {
        selected_idx = pos;
    }

    let mut scroll_offset: usize = 0;
    let mut auto_scroll = true;
    let mut evolver_rx = evolver.subscribe();
    let mut refresh_interval = tokio::time::interval(Duration::from_millis(500));

    let render = |task: &TaskEntry, all_tasks: &[TaskEntry], cur_idx: usize, offset: usize| -> (usize, usize) {
        print!("\x1b[H\x1b[2J");
        let width = super::ui::get_terminal_width().max(40);
        let term_height = super::ui::get_terminal_height().max(10);

        let divider = "─".repeat(width).dimmed().to_string();
        let mut header_lines = Vec::new();

        // 1. Top bar: Summary & Controls
        let running_count = all_tasks.iter().filter(|t| t.is_running()).count();
        let header_left = format!(
            "{} {} {}",
            "●".cyan().bold(),
            "BACKGROUND TASKS".bold(),
            format!("[{} active • {} total]", running_count, all_tasks.len()).dimmed()
        );
        let header_right = "[Tab: next • x: kill • q: close]".dimmed().to_string();
        let left_len = dialoguer::console::strip_ansi_codes(&header_left).chars().count();
        let right_len = dialoguer::console::strip_ansi_codes(&header_right).chars().count();
        let spaces = width.saturating_sub(left_len + right_len);
        header_lines.push(format!("  {}{}{}", header_left, " ".repeat(spaces.max(1)), header_right));
        header_lines.push(divider.clone());

        // 2. Task switcher tabs (if multiple tasks)
        if all_tasks.len() > 1 {
            let mut tabs_line = String::from("  ");
            for (i, t) in all_tasks.iter().enumerate() {
                let is_sel = i == cur_idx;
                let num = i + 1;
                let dot = if t.is_running() { "●".cyan() } else { "○".dimmed() };
                let short_id = t.id();
                let tab_label = if is_sel {
                    format!("[{num}] {dot} {short_id}*").bold().cyan().to_string()
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

        // 3. Task Metadata Card
        match task {
            TaskEntry::Evolver(rec) => {
                let status_badge = format_evolver_status(&rec.status);
                let current_step = rec.current_tool.as_deref().unwrap_or("idle");
                header_lines.push(format!("  {} {}", "Task:".bold(), dialoguer::console::truncate_str(&rec.task, width.saturating_sub(10), "…")));
                header_lines.push(format!("  {} {}   {} {}", "Status:".bold(), status_badge, "Step:".bold(), current_step.cyan()));
                header_lines.push(format!("  {} {}   {} {}", "Branch:".dimmed(), rec.branch.dimmed(), "Worktree:".dimmed(), rec.worktree_dir.dimmed()));
                if !rec.modified_files.is_empty() {
                    header_lines.push(format!("  {} {}", "Changes:".bold(), dialoguer::console::truncate_str(&rec.modified_files.join(", "), width.saturating_sub(12), "…").dimmed()));
                }
                if let Some(ref sum) = rec.summary {
                    let first_line = sum.lines().next().unwrap_or(sum);
                    header_lines.push(format!("  {} {}", "Summary:".green().bold(), dialoguer::console::truncate_str(first_line, width.saturating_sub(12), "…")));
                }
            }
            TaskEntry::Process(proc) => {
                let status_badge = format_process_status(&proc.status);
                header_lines.push(format!("  {} {}", "Command:".bold(), dialoguer::console::truncate_str(&proc.command, width.saturating_sub(12), "…")));
                header_lines.push(format!("  {} {}   {} {}", "Status:".bold(), status_badge, "Elapsed:".dimmed(), format!("{}s", proc.elapsed_secs).dimmed()));
                header_lines.push(format!("  {} {}", "Workdir:".dimmed(), dialoguer::console::truncate_str(&proc.workdir, width.saturating_sub(12), "…")));
                header_lines.push(format!("  {} {}", "Log:".dimmed(), dialoguer::console::truncate_str(&proc.log_path, width.saturating_sub(12), "…")));
            }
        }
        header_lines.push(divider.clone());

        // 4. Output / Logs Content
        let mut content_lines = Vec::new();
        match task {
            TaskEntry::Evolver(rec) => {
                if let Some(ref ev) = rec.evidence {
                    content_lines.push(format!("Context Evidence: {}", ev).dimmed().to_string());
                    content_lines.push(String::new());
                }
                for log in &rec.logs {
                    for sub in log.lines() {
                        let formatted = if sub.starts_with("• Tool:") {
                            format!("  {}", sub.cyan().bold())
                        } else if sub.starts_with("✔") {
                            format!("  {}", sub.green().bold())
                        } else if sub.starts_with("✖") || sub.to_lowercase().contains("error:") {
                            format!("  {}", sub.red())
                        } else {
                            format!("  {}", sub)
                        };
                        content_lines.push(formatted);
                    }
                }
                if content_lines.is_empty() {
                    content_lines.push("  (no execution logs recorded yet)".dimmed().to_string());
                }
            }
            TaskEntry::Process(proc) => {
                match std::fs::read_to_string(&proc.log_path) {
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
                        content_lines.push(format!("  (unable to read log file: {})", e).dimmed().to_string());
                    }
                }
            }
        }

        let footer_lines_count = 2;
        let view_height = term_height.saturating_sub(header_lines.len() + footer_lines_count).max(3);

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
    let (eff, mut max_offset) = render(&tasks[selected_idx], &tasks, selected_idx, if auto_scroll { usize::MAX } else { scroll_offset });
    scroll_offset = eff;

    loop {
        tokio::select! {
            _ = refresh_interval.tick() => {
                // Periodically refresh job statuses and logs
                tasks = collect_tasks(evolver, bg_queue).await;
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
            maybe_ev = evolver_rx.recv() => {
                if let Ok(ev) = maybe_ev {
                    match ev {
                        EvolverEvent::JobProgress { .. } | EvolverEvent::JobCompleted { .. } => {
                            tasks = collect_tasks(evolver, bg_queue).await;
                            if let Some(current) = tasks.get(selected_idx) {
                                let target_offset = if auto_scroll { usize::MAX } else { scroll_offset };
                                let (eff, max) = render(current, &tasks, selected_idx, target_offset);
                                scroll_offset = eff;
                                max_offset = max;
                            }
                        }
                        _ => {}
                    }
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
                        // Kill / cancel active job
                        if let Some(task) = tasks.get(selected_idx) {
                            match task {
                                TaskEntry::Process(p) => {
                                    if let Some(q) = bg_queue {
                                        let _ = q.kill(&p.id);
                                    }
                                }
                                TaskEntry::Evolver(_) => {
                                    // Evolver jobs run in background tasks
                                }
                            }
                            tasks = collect_tasks(evolver, bg_queue).await;
                            if let Some(current) = tasks.get(selected_idx) {
                                let (eff, max) = render(current, &tasks, selected_idx, scroll_offset);
                                scroll_offset = eff;
                                max_offset = max;
                            }
                        }
                    }
                    AppKey::Escape | AppKey::Char('q') | AppKey::Char('Q') | AppKey::CtrlC | AppKey::Char('\x02') => {
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

async fn collect_tasks(
    evolver: &CodebaseEvolver,
    bg_queue: Option<&BackgroundTaskQueue>,
) -> Vec<TaskEntry> {
    let mut tasks = Vec::new();

    // 1. Evolver jobs
    if let Ok(ev_jobs) = evolver.list_jobs().await {
        for job in ev_jobs {
            tasks.push(TaskEntry::Evolver(job));
        }
    } else if let Ok(latest) = evolver.latest_job().await {
        tasks.push(TaskEntry::Evolver(latest));
    }

    // 2. Terminal Background Jobs
    if let Some(q) = bg_queue {
        for job in q.list() {
            tasks.push(TaskEntry::Process(job));
        }
    }

    tasks
}

fn format_evolver_status(status: &JobStatus) -> colored::ColoredString {
    match status {
        JobStatus::Queued => "⏳ QUEUED".yellow(),
        JobStatus::Running => "● RUNNING".cyan().bold(),
        JobStatus::FailedTest { error } => format!("✖ TEST FAILED ({})", error).red().bold(),
        JobStatus::ReadyForReenactment { .. } => "✔ READY".green().bold(),
        JobStatus::Verified => "✔ VERIFIED".green().bold(),
        JobStatus::FailedVerification { reason } => format!("✖ FAILED ({})", reason).red().bold(),
        JobStatus::ScheduledUpdate { .. } => "✔ STAGED FOR UPDATE".green().bold(),
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
