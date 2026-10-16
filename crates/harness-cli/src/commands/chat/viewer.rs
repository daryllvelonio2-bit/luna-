use std::io::{self, Write};
use colored::Colorize;
use dialoguer::console::{Key, Term};
use tokio::sync::mpsc;

use harness_evolver::{CodebaseEvolver, EvolverEvent, JobRecord};

use super::render::{render_all_turns, TurnRecord};
use super::ui::ChatUi;

pub async fn show_background_task_viewer(
    key_rx: &mut mpsc::UnboundedReceiver<Key>,
    evolver: &CodebaseEvolver,
    job_id: &str,
    ui: &ChatUi,
    turns: &[TurnRecord],
    process_expanded: bool,
) {
    let mut scroll_offset: usize = 0;
    let mut evolver_rx = evolver.subscribe();

    let render_screen = |record: &JobRecord, offset: usize| -> (usize, usize) {
        crate::commands::clear_screen();
        let width = super::ui::get_terminal_width();
        let (_, term_height) = Term::stdout().size();
        let term_height = (term_height as usize).max(10);
        let header_lines = 6;
        let footer_lines = 2;
        let view_height = term_height.saturating_sub(header_lines + footer_lines);

        let divider = "─".repeat(width).dimmed().to_string();
        println!("  {} {} {}", "⚙".cyan().bold(), "Background Engineer Task:".bold(), record.id.cyan().bold());
        println!("  {} {}", "Task:".bold(), record.task);
        println!("  {} {:?}  {} {}", "Status:".bold(), record.status, "Branch:".dimmed(), record.branch.dimmed());
        println!("  {} {}", "Worktree:".dimmed(), record.worktree_dir.dimmed());
        println!("{}", divider);

        let mut lines = Vec::new();
        if let Some(ref evidence) = record.evidence {
            lines.push(format!("Evidence context: {}", evidence).dimmed().to_string());
            lines.push(String::new());
        }
        lines.push("Execution Logs & Conversation:".bold().to_string());
        for log in &record.logs {
            for sub in log.lines() {
                lines.push(format!("  {}", sub));
            }
        }

        let total_lines = lines.len();
        let max_offset = total_lines.saturating_sub(view_height);
        let effective_offset = offset.min(max_offset);

        let visible_lines = &lines[effective_offset..total_lines.min(effective_offset + view_height)];
        for l in visible_lines {
            println!("{}", l);
        }

        for _ in visible_lines.len()..view_height {
            println!();
        }

        println!("{}", divider);
        let scroll_info = format!(
            "Line {}/{} • [↑/↓ j/k scroll • PgUp/PgDn • q / Esc / Ctrl+B to return]",
            effective_offset + 1,
            total_lines
        );
        println!("  {}", scroll_info.dimmed());
        let _ = io::stdout().flush();

        (effective_offset, max_offset)
    };

    let mut current_job = match evolver.check_job(job_id).await {
        Ok(j) => j,
        Err(_) => return,
    };

    let (effective_offset, mut max_offset) = render_screen(&current_job, scroll_offset);
    scroll_offset = effective_offset;

    loop {
        tokio::select! {
            maybe_ev = evolver_rx.recv() => {
                if let Ok(ev) = maybe_ev {
                    match ev {
                        EvolverEvent::JobProgress { ref job_id, .. } | EvolverEvent::JobCompleted { ref job_id, .. } => {
                            if job_id == &current_job.id {
                                if let Ok(updated) = evolver.check_job(&current_job.id).await {
                                    current_job = updated;
                                    let (eff, max) = render_screen(&current_job, scroll_offset);
                                    scroll_offset = eff;
                                    max_offset = max;
                                }
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
                    Key::ArrowUp | Key::Char('k') | Key::Char('K') => {
                        if scroll_offset > 0 {
                            scroll_offset = scroll_offset.saturating_sub(1);
                            let (eff, _) = render_screen(&current_job, scroll_offset);
                            scroll_offset = eff;
                        }
                    }
                    Key::ArrowDown | Key::Char('j') | Key::Char('J') => {
                        if scroll_offset < max_offset {
                            scroll_offset = scroll_offset.saturating_add(1);
                            let (eff, _) = render_screen(&current_job, scroll_offset);
                            scroll_offset = eff;
                        }
                    }
                    Key::PageUp | Key::Char('b') => {
                        scroll_offset = scroll_offset.saturating_sub(10);
                        let (eff, _) = render_screen(&current_job, scroll_offset);
                        scroll_offset = eff;
                    }
                    Key::PageDown | Key::Char('f') | Key::Char(' ') => {
                        scroll_offset = (scroll_offset + 10).min(max_offset);
                        let (eff, _) = render_screen(&current_job, scroll_offset);
                        scroll_offset = eff;
                    }
                    Key::Home | Key::Char('g') => {
                        scroll_offset = 0;
                        let (eff, _) = render_screen(&current_job, scroll_offset);
                        scroll_offset = eff;
                    }
                    Key::End | Key::Char('G') => {
                        scroll_offset = max_offset;
                        let (eff, _) = render_screen(&current_job, scroll_offset);
                        scroll_offset = eff;
                    }
                    Key::Escape | Key::Char('q') | Key::Char('Q') | Key::CtrlC | Key::Char('\x02') => {
                        break;
                    }
                    _ => {}
                }
            }
        }
    }

    crate::commands::clear_screen();
    render_all_turns(turns, process_expanded, super::ui::get_terminal_width());
    if let Some(ref err) = ui.transient_error() {
        println!();
        println!("{}", err);
    }
    ui.draw_initial_box();
}
