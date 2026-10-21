use std::io::{self, Write};
use std::sync::Arc;
use colored::Colorize;
use tokio::sync::mpsc;

use harness_core::{
    Config, HarnessError, Message, ReasoningEffort, Role, SessionBinding, SessionStore,
    SqliteSessionStore,
};
use harness_providers::{
    get_context_window, get_default_model, get_provider_catalog, CredentialManager,
    GenericProviderClient, ModelDiscovery,
};

use super::clear_on_exit;
use super::keys::AppKey;
use super::render::{render_transcript, TurnRecord, PROMPT};
use super::todo;
use super::ui::{get_terminal_width, ChatUi};
use super::ChatAgent;
use crate::commands::sessions::{resolve_session, SessionSelection};

pub struct SlashCommand {
    pub name: &'static str,
    pub details: &'static str,
}

/// Draw the transcript with the todo panel under it.
fn draw_transcript(
    turns: &[TurnRecord],
    expanded: bool,
    todo_collapsed: bool,
    config: &Config,
) {
    let snapshot = todo::read_snapshot(&config.paths.workspace_root);
    render_transcript(
        turns,
        expanded,
        get_terminal_width(),
        Some(&snapshot),
        todo_collapsed,
    );
}

pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "/model",
        details: "Switch provider and model",
    },
    SlashCommand {
        name: "/compact-model",
        details: "Choose provider and model for context compaction",
    },
    SlashCommand {
        name: "/reasoning",
        details: "Set reasoning effort (off, low, medium, high)",
    },
    SlashCommand {
        name: "/name",
        details: "Set or view session name",
    },
    SlashCommand {
        name: "/new",
        details: "Start a fresh chat session",
    },
    SlashCommand {
        name: "/summarize",
        details: "Summarize and start a new session with it",
    },
    SlashCommand {
        name: "/rewind",
        details: "List turn checkpoints, or restore the files of one (/rewind 3)",
    },
    SlashCommand {
        name: "/diff",
        details: "Review what a turn changed on disk (/diff, /diff 3, /diff --all)",
    },
    SlashCommand {
        name: "/compact",
        details: "Fold oldest turns into a summary in this session",
    },
    SlashCommand {
        name: "/expand",
        details: "Expand intermediate tool and reasoning processes (or Ctrl+O)",
    },
    SlashCommand {
        name: "/collapse",
        details: "Collapse intermediate tool and reasoning processes (or Ctrl+O)",
    },
    SlashCommand {
        name: "/skills",
        details: "List available specialized skills",
    },
    SlashCommand {
        name: "/plan",
        details: "Toggle read-only plan mode (or Tab key)",
    },
    SlashCommand {
        name: "/fork",
        details: "Fork current session into a new branch session",
    },
    SlashCommand {
        name: "/jobs",
        details: "List background task queue jobs",
    },
    SlashCommand {
        name: "/todo",
        details: "Fold or unfold the todo list (Ctrl+Shift+O)",
    },
    SlashCommand {
        name: "/clear",
        details: "Clear terminal screen",
    },
    SlashCommand {
        name: "/steer",
        details: "Steer active turn with mid-run guidance",
    },
    SlashCommand {
        name: "/heartbeat",
        details: "Show harness liveness, system health, and background status (or /hb)",
    },
    SlashCommand {
        name: "/hb",
        details: "Alias for /heartbeat",
    },
    SlashCommand {
        name: "/delete",
        details: "Delete current session and start a fresh one",
    },
    SlashCommand {
        name: "/exit",
        details: "Exit Luna chat session",
    },
];

pub(crate) async fn execute_slash_command(
    cmd: &str,
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    store: &Arc<SqliteSessionStore>,
    session_id: &mut String,
    current_provider: &mut String,
    current_model: &mut String,
    compact_provider: &mut Option<String>,
    compact_model: &mut Option<String>,
    turns: &mut Vec<TurnRecord>,
    process_expanded: &mut bool,
    todo_collapsed: &mut bool,
) -> Result<bool, HarnessError> {
    let parts: Vec<&str> = cmd.trim().splitn(2, ' ').collect();
    let name = parts[0];
    let arg = parts.get(1).copied().map(|s| s.trim()).filter(|s| !s.is_empty());

    match name {
        "/model" | "/models" | "/provider" => {
            ui.redraw_input_line();
            if let Err(e) = handle_model_switch(
                Some(key_rx),
                ui,
                config,
                cred_manager,
                discovery,
                agent,
                store,
                session_id,
                current_provider,
                current_model,
                compact_model,
            )
            .await
            {
                ui.emit_above(&format!("{} Failed to switch model: {}", "✖".red().bold(), e));
            }
            Ok(false)
        }
        "/compact-model" => {
            ui.redraw_input_line();
            if let Err(e) = handle_compact_model_switch(
                Some(key_rx),
                ui,
                config,
                cred_manager,
                discovery,
                agent,
                current_provider,
                current_model,
                compact_provider,
                compact_model,
                arg,
            )
            .await
            {
                ui.emit_above(&format!("{} Failed to set compaction model: {}", "✖".red().bold(), e));
            }
            Ok(false)
        }
        "/reasoning" => {
            ui.redraw_input_line();
            if let Err(e) = handle_reasoning_switch(Some(key_rx), ui, agent, arg).await {
                ui.emit_above(&format!("{} Failed to set reasoning effort: {}", "✖".red().bold(), e));
            }
            Ok(false)
        }
        "/name" => {
            if let Some(a) = arg {
                if let Err(e) = handle_name_command(ui, store.as_ref(), session_id, Some(a)).await {
                    ui.emit_above(&format!("{} Failed to set session name: {}", "✖".red().bold(), e));
                }
            } else {
                ui.input_buf.lock().unwrap().clear();
                ui.input_buf.lock().unwrap().push_str("/name ");
                ui.redraw_input_line();
            }
            Ok(false)
        }
        "/summarize" => {
            turns.clear();
            ui.clear_box_on_exit();
            if let Err(e) = handle_summarize_and_new_command(
                ui,
                store,
                config,
                agent,
                session_id,
                current_provider,
                current_model,
                arg,
            )
            .await
            {
                ui.draw_initial_box();
                ui.emit_above(&format!("{} Failed to summarize session: {}", "✖".red().bold(), e));
            }
            Ok(false)
        }
        "/rewind" => {
            if let Err(e) = handle_rewind_command(ui, agent, session_id, arg).await {
                ui.emit_above(&format!("{} Failed to rewind: {}", "✖".red().bold(), e));
            }
            Ok(false)
        }
        "/diff" => {
            if let Err(e) = handle_diff_command(ui, agent, session_id, arg).await {
                ui.emit_above(&format!("{} Failed to diff: {}", "✖".red().bold(), e));
            }
            Ok(false)
        }
        "/compact" => {
            if let Err(e) = handle_compact_command(ui, agent, current_provider, current_model).await {
                ui.emit_above(&format!("{} Failed to compact session: {}", "✖".red().bold(), e));
            }
            Ok(false)
        }
        "/new" => {
            turns.clear();
            let (is_summarize, remaining_arg) = if let Some(a) = arg {
                if a.starts_with("-s ") || a == "-s" {
                    (true, a.strip_prefix("-s").unwrap().trim())
                } else if a.starts_with("--summarize ") || a == "--summarize" {
                    (true, a.strip_prefix("--summarize").unwrap().trim())
                } else if a.starts_with("--summary ") || a == "--summary" {
                    (true, a.strip_prefix("--summary").unwrap().trim())
                } else {
                    (false, a)
                }
            } else {
                (false, "")
            };
            let rem_opt = if remaining_arg.is_empty() { None } else { Some(remaining_arg) };
            ui.clear_box_on_exit();
            if is_summarize {
                if let Err(e) = handle_summarize_and_new_command(
                    ui,
                    store,
                    config,
                    agent,
                    session_id,
                    current_provider,
                    current_model,
                    rem_opt,
                )
                .await
                {
                    ui.draw_initial_box();
                    ui.emit_above(&format!("{} Failed to summarize session: {}", "✖".red().bold(), e));
                }
            } else {
                if let Err(e) = handle_new_command(
                    ui,
                    store,
                    config,
                    agent,
                    session_id,
                    current_provider,
                    current_model,
                    rem_opt,
                )
                .await
                {
                    ui.draw_initial_box();
                    ui.emit_above(&format!("{} Failed to start new session: {}", "✖".red().bold(), e));
                }
            }
            Ok(false)
        }
        "/expand" => {
            *process_expanded = true;
            if ui.is_attended {
                crate::commands::clear_screen();
                draw_transcript(turns, *process_expanded, *todo_collapsed, config);
                ui.draw_initial_box();
            }
            Ok(false)
        }
        "/collapse" => {
            *process_expanded = false;
            if ui.is_attended {
                crate::commands::clear_screen();
                draw_transcript(turns, *process_expanded, *todo_collapsed, config);
                ui.draw_initial_box();
            }
            Ok(false)
        }
        "/todo" => {
            // The same fold Ctrl+Shift+O does, for the terminals that cannot tell the
            // two apart.
            *todo_collapsed = !*todo_collapsed;
            if ui.is_attended {
                crate::commands::clear_screen();
                draw_transcript(turns, *process_expanded, *todo_collapsed, config);
                ui.draw_initial_box();
            }
            Ok(false)
        }
        "/clear" => {
            turns.clear();
            crate::commands::clear_screen();
            ui.draw_initial_box();
            Ok(false)
        }
        "/steer" => {
            if let Some(guidance) = arg {
                let ag = agent.lock().await;
                ag.steer(guidance);
                ui.emit_above(&format!("{} Steered: {}", "⏩".cyan().bold(), guidance));
            } else {
                ui.emit_above(&format!("{} Usage: /steer <guidance>", "•".dimmed()));
            }
            Ok(false)
        }
        "/heartbeat" | "/hb" => {
            crate::commands::heartbeat::handle_chat_heartbeat(
                arg,
                ui,
                agent,
                session_id,
                current_provider,
                current_model,
                turns,
            )
            .await;
            if ui.is_attended {
                ui.draw_initial_box();
            }
            Ok(false)
        }
        "/delete" => {
            turns.clear();
            ui.clear_box_on_exit();
            if let Err(e) = handle_delete_command(
                ui,
                store,
                config,
                agent,
                session_id,
                current_provider,
                current_model,
            )
            .await
            {
                ui.draw_initial_box();
                ui.emit_above(&format!("{} Failed to delete session: {}", "✖".red().bold(), e));
            }
            Ok(false)
        }
        "exit" | "quit" | "/exit" | "/quit" => {
            ui.clear_box_on_exit();
            clear_on_exit();
            Ok(true)
        }
        _ => Ok(false),
    }
}

pub(crate) async fn select_slash_command(
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
) -> Option<String> {
    if !ui.is_attended {
        return None;
    }

    const PAGE_SIZE: usize = 5;
    const BOX_ROWS: usize = PAGE_SIZE + 2;

    let mut selected: usize = 0;

    let get_filtered = |filter: &str| -> Vec<(usize, &'static SlashCommand)> {
        let f = filter.strip_prefix('/').unwrap_or(filter).trim().to_lowercase();
        if f.is_empty() {
            SLASH_COMMANDS.iter().enumerate().collect()
        } else {
            SLASH_COMMANDS
                .iter()
                .enumerate()
                .filter(|(_, cmd)| {
                    cmd.name.to_lowercase().contains(&f)
                        || cmd.details.to_lowercase().contains(&f)
                })
                .collect()
        }
    };

    let render_box = |sel: usize, filt: &str| -> Vec<String> {
        let width = get_terminal_width().min(76).max(44);
        let inner_width = width.saturating_sub(4);
        let filtered = get_filtered(filt);

        let mut lines = Vec::with_capacity(BOX_ROWS);
        lines.push(format!("┌{}┐", "─".repeat(width.saturating_sub(2))).dimmed().to_string());

        if filtered.is_empty() {
            let empty_msg = "(no matching commands)";
            let pad = inner_width.saturating_sub(empty_msg.chars().count() + 1);
            lines.push(format!("│ {}{} │", empty_msg.dimmed(), " ".repeat(pad)));
            for _ in 1..PAGE_SIZE {
                lines.push(format!("│ {} │", " ".repeat(inner_width)));
            }
        } else {
            let start = if sel >= PAGE_SIZE {
                (sel + 1 - PAGE_SIZE).min(filtered.len().saturating_sub(PAGE_SIZE))
            } else {
                0
            };
            let end = (start + PAGE_SIZE).min(filtered.len());

            for idx in start..end {
                let (_, cmd) = filtered[idx];
                let is_sel = idx == sel;
                let pfx = if is_sel { "❯ ".cyan().bold().to_string() } else { "  ".to_string() };
                let cmd_styled = if is_sel { cmd.name.bold().to_string() } else { cmd.name.cyan().to_string() };

                let pfx_len = 2;
                let name_len = cmd.name.chars().count();
                let gap_len = 2;
                let remaining_width = inner_width.saturating_sub(pfx_len + name_len + gap_len);

                let details_disp = if cmd.details.chars().count() > remaining_width {
                    format!("{}…", cmd.details.chars().take(remaining_width.saturating_sub(1)).collect::<String>())
                } else {
                    cmd.details.to_string()
                };
                let details_len = details_disp.chars().count();
                let pad_len = inner_width.saturating_sub(pfx_len + name_len + gap_len + details_len);

                lines.push(format!(
                    "│ {}{}  {}{}{} │",
                    pfx,
                    cmd_styled,
                    details_disp.dimmed(),
                    " ".repeat(pad_len),
                    "".clear()
                ));
            }

            for _ in (end - start)..PAGE_SIZE {
                lines.push(format!("│ {} │", " ".repeat(inner_width)));
            }
        }

        lines.push(format!("└{}┘", "─".repeat(width.saturating_sub(2))).dimmed().to_string());
        lines
    };

    // Ensure space below: scroll terminal if needed to provide canvas
    let buf = ui.input_buf.lock().unwrap().clone();
    print!("\x1b[2B\r");
    for _ in 0..BOX_ROWS {
        println!();
    }
    print!("\x1b[{}A\r{}", BOX_ROWS + 2, ui.prompt_row(&buf));
    let _ = io::stdout().flush();

    let draw_menu = |sel: usize, filt: &str, buf_str: &str| {
        let lines = render_box(sel, filt);
        print!("\x1b[2B\r");
        for (i, line) in lines.iter().enumerate() {
            print!("\x1b[2K{}", line);
            if i + 1 < lines.len() {
                print!("\x1b[1B\r");
            }
        }
        print!("\x1b[{}A\r{}", BOX_ROWS + 1, ui.prompt_row(buf_str));
        let _ = io::stdout().flush();
    };

    let erase_menu = |buf_str: &str| {
        print!("\x1b[2B\r");
        for i in 0..BOX_ROWS {
            print!("\x1b[2K");
            if i + 1 < BOX_ROWS {
                print!("\x1b[1B\r");
            }
        }
        print!("\x1b[{}A\r{}", BOX_ROWS + 1, ui.prompt_row(buf_str));
        let _ = io::stdout().flush();
    };

    let cur_filter = {
        let b = ui.input_buf.lock().unwrap();
        b.clone()
    };
    draw_menu(selected, &cur_filter, &cur_filter);

    let mut result = None;

    while let Some(key) = key_rx.recv().await {
        let filter_str = ui.input_buf.lock().unwrap().clone();
        let filtered = get_filtered(&filter_str);

        match key {
            AppKey::Down => {
                if !filtered.is_empty() {
                    selected = (selected + 1) % filtered.len();
                }
                draw_menu(selected, &filter_str, &filter_str);
            }
            AppKey::Up => {
                if !filtered.is_empty() {
                    selected = if selected == 0 {
                        filtered.len().saturating_sub(1)
                    } else {
                        selected - 1
                    };
                }
                draw_menu(selected, &filter_str, &filter_str);
            }
            AppKey::Enter => {
                if !filtered.is_empty() && selected < filtered.len() {
                    result = Some(filtered[selected].1.name.to_string());
                } else if filter_str.starts_with('/') {
                    // Enter on a filter with no menu match falls through to the typed command
                    // itself, so typing "/name luna" straight through still executes.
                    result = Some(filter_str.clone());
                }
                break;
            }
            AppKey::Tab => {
                if !filtered.is_empty() && selected < filtered.len() {
                    let cmd = filtered[selected].1.name;
                    let mut b = ui.input_buf.lock().unwrap();
                    b.clear();
                    b.push_str(cmd);
                    b.push(' ');
                    drop(b);
                    break;
                }
            }
            AppKey::Escape | AppKey::CtrlC => {
                ui.input_buf.lock().unwrap().clear();
                break;
            }
            AppKey::Backspace => {
                let mut b = ui.input_buf.lock().unwrap();
                b.pop();
                let is_empty = b.is_empty();
                let new_buf = b.clone();
                drop(b);
                ui.redraw_input_line();

                if is_empty {
                    break;
                } else {
                    selected = 0;
                    draw_menu(selected, &new_buf, &new_buf);
                }
            }
            AppKey::Char(c) if c != '\n' && c != '\r' && !c.is_control() => {
                let mut b = ui.input_buf.lock().unwrap();
                b.push(c);
                let new_buf = b.clone();
                drop(b);
                ui.redraw_input_line();
                selected = 0;
                draw_menu(selected, &new_buf, &new_buf);
            }
            _ => continue,
        }
    }

    let final_buf = ui.input_buf.lock().unwrap().clone();
    erase_menu(&final_buf);
    result
}

async fn select_from_rx(
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
    prompt: &str,
    items: &[String],
    default_idx: usize,
    is_chained: bool,
    leave_open: bool,
) -> Option<usize> {
    if items.is_empty() {
        return None;
    }

    let mut filter = String::new();

    let get_filtered = |filter: &str| -> Vec<(usize, &String)> {
        if filter.is_empty() {
            items.iter().enumerate().collect()
        } else {
            let filter_lower = filter.to_lowercase();
            items
                .iter()
                .enumerate()
                .filter(|(_, item)| item.to_lowercase().contains(&filter_lower))
                .collect()
        }
    };

    let initial_filtered = get_filtered(&filter);
    let mut selected_filtered_idx = initial_filtered
        .iter()
        .position(|(orig_idx, _)| *orig_idx == default_idx)
        .unwrap_or(0);

    const PAGE_SIZE: usize = 7;
    const MENU_HEIGHT: usize = PAGE_SIZE + 1; // 1 header + 7 items

    let render_menu_lines = |selected: usize, filter: &str| -> Vec<String> {
        let term_width = get_terminal_width().saturating_sub(4).max(30);
        let filtered = get_filtered(filter);

        let mut lines = Vec::with_capacity(MENU_HEIGHT);

        let total_pages = if filtered.is_empty() {
            1
        } else {
            (filtered.len() + PAGE_SIZE - 1) / PAGE_SIZE
        };
        let current_page = if filtered.is_empty() {
            1
        } else {
            (selected / PAGE_SIZE) + 1
        };

        let filter_display = if filter.is_empty() {
            String::new()
        } else {
            format!(" [search: \"{}\"]", filter)
        };

        let page_indicator = if total_pages > 1 {
            format!(" ({}/{})", current_page, total_pages)
        } else {
            String::new()
        };

        let raw_header = format!("{}{}:{}", prompt, filter_display, page_indicator);
        let header = if raw_header.len() > term_width {
            format!("{}…", &raw_header[..term_width.saturating_sub(1)])
        } else {
            raw_header
        };
        lines.push(format!("\r\x1b[2K{}", header.bold().cyan()));

        if filtered.is_empty() {
            lines.push(format!("\r\x1b[2K  {}", "(no matching items)".dimmed()));
            for _ in 1..PAGE_SIZE {
                lines.push("\r\x1b[2K".to_string());
            }
        } else {
            let half = PAGE_SIZE / 2;
            let start = if selected > half && filtered.len() > PAGE_SIZE {
                (selected - half).min(filtered.len() - PAGE_SIZE)
            } else {
                0
            };
            let end = (start + PAGE_SIZE).min(filtered.len());

            for idx in start..end {
                let (_, item) = filtered[idx];
                let item_width = term_width.saturating_sub(6);
                let display_item = if item.len() > item_width {
                    format!("{}…", &item[..item_width.saturating_sub(1)])
                } else {
                    item.clone()
                };

                if idx == selected {
                    lines.push(format!(
                        "\r\x1b[2K  {} {}",
                        "❯".cyan().bold(),
                        display_item.bold()
                    ));
                } else {
                    lines.push(format!(
                        "\r\x1b[2K    {}",
                        display_item.dimmed()
                    ));
                }
            }

            let rendered = end - start;
            for _ in rendered..PAGE_SIZE {
                lines.push("\r\x1b[2K".to_string());
            }
        }

        lines
    };

    let width = get_terminal_width();
    let divider = "─".repeat(width).dimmed().to_string();

    if ui.is_attended {
        if is_chained {
            // Menu is already open above the box! Just overwrite the MENU_HEIGHT rows
            print!("\r\x1b[{}A", MENU_HEIGHT + 2);
            let initial_lines = render_menu_lines(selected_filtered_idx, &filter);
            for line in &initial_lines {
                println!("{}", line);
            }
            print!("\x1b[2B\r{}", PROMPT.bold().yellow());
            let _ = io::stdout().flush();
        } else {
            // Smoothly open MENU_HEIGHT lines above the status bar
            let has_status = ui.has_status();
            if has_status {
                print!("\x1b[1B\r\x1b[2K\x1b[1A\r\x1b[2K\x1b[1A\r\x1b[2K\x1b[1A\r\x1b[2K");
            } else {
                print!("\x1b[1B\r\x1b[2K\x1b[1A\r\x1b[2K\x1b[1A\r\x1b[2K");
            }

            let initial_lines = render_menu_lines(selected_filtered_idx, &filter);
            for line in &initial_lines {
                println!("{}", line);
            }

            if let Some(status_str) = ui.format_status_line(width) {
                println!("{}", status_str);
            }
            println!("{}", divider);
            println!();
            println!("{}", divider);
            print!("\x1b[2A\r{}", PROMPT.bold().yellow());
            let _ = io::stdout().flush();
        }
    } else {
        println!("{}:", prompt);
        for (i, item) in items.iter().enumerate() {
            println!("  {}. {}", i + 1, item);
        }
    }

    let mut result = None;

    while let Some(key) = key_rx.recv().await {
        let filtered = get_filtered(&filter);

        match key {
            AppKey::Up => {
                if !filtered.is_empty() {
                    if selected_filtered_idx > 0 {
                        selected_filtered_idx -= 1;
                    } else {
                        selected_filtered_idx = filtered.len() - 1;
                    }
                }
            }
            AppKey::Down => {
                if !filtered.is_empty() {
                    if selected_filtered_idx + 1 < filtered.len() {
                        selected_filtered_idx += 1;
                    } else {
                        selected_filtered_idx = 0;
                    }
                }
            }
            AppKey::Backspace => {
                if !filter.is_empty() {
                    filter.pop();
                    selected_filtered_idx = 0;
                }
            }
            AppKey::Char(c) if c != '\n' && c != '\r' && !c.is_control() => {
                filter.push(c);
                selected_filtered_idx = 0;
            }
            AppKey::Enter => {
                if !filtered.is_empty() && selected_filtered_idx < filtered.len() {
                    result = Some(filtered[selected_filtered_idx].0);
                }
                break;
            }
            AppKey::Escape | AppKey::CtrlC => {
                if !filter.is_empty() {
                    filter.clear();
                    selected_filtered_idx = 0;
                } else {
                    result = None;
                    break;
                }
            }
            _ => continue,
        }

        if ui.is_attended {
            // Distance from User > to top of menu is MENU_HEIGHT + 2 lines
            print!("\r\x1b[{}A", MENU_HEIGHT + 2);
            let updated_lines = render_menu_lines(selected_filtered_idx, &filter);
            for line in &updated_lines {
                println!("{}", line);
            }
            // Move cursor back down to User >
            print!("\x1b[2B\r{}", PROMPT.bold().yellow());
            let _ = io::stdout().flush();
        }
    }

    if ui.is_attended && (!leave_open || result.is_none()) {
        // Erase menu rows and box rows, then redraw box in its original position
        print!("\r\x1b[{}A", MENU_HEIGHT + 2);
        for _ in 0..(MENU_HEIGHT + 4) {
            print!("\r\x1b[2K\x1b[1B");
        }
        print!("\r\x1b[{}A", MENU_HEIGHT + 4);
        ui.draw_initial_box();
    }

    result
}

async fn input_from_rx(
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    prompt: &str,
) -> Option<String> {
    print!("{}: ", prompt.bold().cyan());
    let _ = io::stdout().flush();
    let mut buf = String::new();

    while let Some(key) = key_rx.recv().await {
        match key {
            AppKey::Char(c) => {
                buf.push(c);
                print!("{}", c);
                let _ = io::stdout().flush();
            }
            AppKey::Backspace => {
                if buf.pop().is_some() {
                    print!("\x08 \x08");
                    let _ = io::stdout().flush();
                }
            }
            AppKey::Enter => {
                print!("\r\x1b[2K");
                let _ = io::stdout().flush();
                let res = buf.trim().to_string();
                if res.is_empty() {
                    return None;
                }
                return Some(res);
            }
            AppKey::Escape | AppKey::CtrlC => {
                print!("\r\x1b[2K");
                let _ = io::stdout().flush();
                return None;
            }
            _ => {}
        }
    }
    None
}

async fn pick_provider_and_model(
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    current_provider: &str,
    current_model: &str,
    chain_next: bool,
) -> Result<Option<(String, String)>, HarnessError> {
    let catalog = get_provider_catalog();
    let loaded = cred_manager.get_loaded_providers().await;

    // Only list providers that have wired credentials (or currently active)
    let available_catalog: Vec<_> = {
        let configured: Vec<_> = catalog
            .into_iter()
            .filter(|p| {
                loaded.iter().any(|l| l.eq_ignore_ascii_case(&p.id))
                    || p.id.eq_ignore_ascii_case(current_provider)
            })
            .collect();
        if configured.is_empty() {
            get_provider_catalog()
        } else {
            configured
        }
    };

    let items: Vec<String> = available_catalog
        .iter()
        .map(|p| {
            let mut tags = Vec::new();
            if p.id.eq_ignore_ascii_case(current_provider) {
                tags.push("active");
            }
            if loaded.iter().any(|l| l.eq_ignore_ascii_case(&p.id)) {
                tags.push("configured");
            }
            let tag_str = if tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", tags.join(", "))
            };
            format!("{:<28} ({}){}", p.display_name, p.id, tag_str)
        })
        .collect();

    let default_idx = available_catalog
        .iter()
        .position(|p| p.id.eq_ignore_ascii_case(current_provider))
        .unwrap_or(0);

    let sel = if let Some(ref mut rx) = key_rx {
        while rx.try_recv().is_ok() {}
        match select_from_rx(rx, ui, "Select Provider", &items, default_idx, false, true).await {
            Some(i) => i,
            None => {
                return Ok(None);
            }
        }
    } else {
        default_idx
    };

    let chosen_provider = available_catalog[sel].id.clone();

    // Fetch models for chosen provider
    let mut models = match discovery.get_models(&chosen_provider, cred_manager, false).await {
        Ok(m) => m,
        Err(_) => Vec::new(),
    };

    let default_model = get_default_model(&chosen_provider).to_string();
    if !default_model.is_empty() && !models.contains(&default_model) {
        models.insert(0, default_model.clone());
    }

    let mut model_items = models.clone();
    model_items.push("[Custom model name...]".to_string());

    let default_model_idx = models
        .iter()
        .position(|m| m.eq_ignore_ascii_case(current_model))
        .unwrap_or(0);

    let model_sel = if let Some(ref mut rx) = key_rx {
        while rx.try_recv().is_ok() {}
        match select_from_rx(
            rx,
            ui,
            &format!("Select Model [{}]", chosen_provider),
            &model_items,
            default_model_idx,
            true,
            chain_next,
        )
        .await
        {
            Some(i) => i,
            None => {
                return Ok(None);
            }
        }
    } else {
        default_model_idx
    };

    let chosen_model = if model_sel < models.len() {
        model_items[model_sel].clone()
    } else if let Some(ref mut rx) = key_rx {
        while rx.try_recv().is_ok() {}
        if chain_next && ui.is_attended {
            print!("\r\x1b[10A");
            for _ in 0..12 {
                print!("\r\x1b[2K\x1b[1B");
            }
            print!("\r\x1b[12A");
            ui.draw_initial_box();
        }
        match input_from_rx(rx, "Enter custom model name").await {
            Some(name) => name,
            None => {
                return Ok(None);
            }
        }
    } else {
        default_model
    };

    if chosen_model.is_empty() {
        return Ok(None);
    }

    Ok(Some((chosen_provider, chosen_model)))
}

pub(crate) async fn handle_model_switch(
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    store: &SqliteSessionStore,
    session_id: &str,
    current_provider: &mut String,
    current_model: &mut String,
    compact_model: &Option<String>,
) -> Result<(), HarnessError> {
    let Some((chosen_provider, chosen_model)) = pick_provider_and_model(
        key_rx.as_deref_mut(),
        ui,
        cred_manager,
        discovery,
        current_provider,
        current_model,
        true,
    )
    .await?
    else {
        ui.clear_box_on_exit();
        ui.draw_initial_box();
        return Ok(());
    };

    // Reasoning effort selection
    let reasoning_options = [
        ("Keep current / Default", None),
        ("minimal", Some(ReasoningEffort::Minimal)),
        ("low", Some(ReasoningEffort::Low)),
        ("medium", Some(ReasoningEffort::Medium)),
        ("high", Some(ReasoningEffort::High)),
        ("xhigh", Some(ReasoningEffort::Xhigh)),
        ("max", Some(ReasoningEffort::Max)),
        ("off / disabled", None),
    ];

    let reasoning_items: Vec<String> = reasoning_options.iter().map(|(label, _)| label.to_string()).collect();
    let current_effort = agent.lock().await.reasoning_effort();
    let default_reasoning_idx = match current_effort {
        None => 0,
        Some(ReasoningEffort::Minimal) => 1,
        Some(ReasoningEffort::Low) => 2,
        Some(ReasoningEffort::Medium) => 3,
        Some(ReasoningEffort::High) => 4,
        Some(ReasoningEffort::Xhigh) => 5,
        Some(ReasoningEffort::Max) => 6,
    };

    let reasoning_sel = if let Some(ref mut rx) = key_rx {
        while rx.try_recv().is_ok() {}
        match select_from_rx(rx, ui, "Select Reasoning Effort", &reasoning_items, default_reasoning_idx, true, false).await {
            Some(i) => i,
            None => {
                ui.clear_box_on_exit();
                ui.draw_initial_box();
                return Ok(());
            }
        }
    } else {
        default_reasoning_idx
    };

    let chosen_effort = if reasoning_sel == 0 {
        current_effort
    } else {
        reasoning_options[reasoning_sel].1
    };

    let new_provider = Arc::new(GenericProviderClient::new(
        chosen_provider.clone(),
        chosen_model.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&chosen_provider),
    )?);

    {
        let mut ag = agent.lock().await;
        ag.set_provider(new_provider);
        ag.set_reasoning_effort(chosen_effort);
    }

    *current_provider = chosen_provider.clone();
    *current_model = chosen_model.clone();

    // The switch is session state, not just process state: stamping the row means a later
    // `luna --session <id>` resumes on this pair instead of the config default. A failure to
    // persist is surfaced as a warning — the in-memory switch already happened.
    if let Err(error) = store.set_model(session_id, &chosen_provider, &chosen_model).await {
        ui.emit_above(&format!(
            "{} Model switched in this session, but persisting it failed: {}",
            "⚠".yellow(),
            error
        ));
    }

    // If compaction model is not custom set, default it to the used model
    if compact_model.is_none() {
        if let Ok(comp_client) = GenericProviderClient::new(
            chosen_provider.clone(),
            chosen_model.clone(),
            cred_manager.clone(),
            discovery.clone(),
            config.providers.get(&chosen_provider),
        ) {
            agent.lock().await.set_compaction(Some(harness_core::CompactionSetup {
                provider: Arc::new(comp_client),
                model: chosen_model.clone(),
            }));
        }
    }

    // The chained pickers redraw the box as they tear down — with the pre-switch model. Erase
    // that stale box so the status bar is drawn once, showing the model now in use.
    let context_max = get_context_window(&chosen_provider, &chosen_model).unwrap_or(128_000);
    // Switching models does not touch the transcript, so keep the real usage rather than resetting
    // the bar to 0.
    let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
    ui.set_model(&chosen_model, context_used, context_max);
    ui.clear_box_on_exit();
    ui.draw_initial_box();

    Ok(())
}

pub(crate) async fn handle_compact_model_switch(
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    current_provider: &str,
    current_model: &str,
    compact_provider: &mut Option<String>,
    compact_model: &mut Option<String>,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    if let Some(a) = arg {
        if a.eq_ignore_ascii_case("reset") || a.eq_ignore_ascii_case("default") {
            *compact_provider = None;
            *compact_model = None;
            let comp_client = GenericProviderClient::new(
                current_provider.to_string(),
                current_model.to_string(),
                cred_manager.clone(),
                discovery.clone(),
                config.providers.get(current_provider),
            )?;
            agent.lock().await.set_compaction(Some(harness_core::CompactionSetup {
                provider: Arc::new(comp_client),
                model: current_model.to_string(),
            }));
            ui.clear_box_on_exit();
            ui.draw_initial_box();
            ui.emit_above(&format!(
                "{} Compaction model reset to active model: {} ({})",
                "✓".green(),
                current_model,
                current_provider
            ));
            return Ok(());
        }
    }

    let initial_p = compact_provider.as_deref().unwrap_or(current_provider);
    let initial_m = compact_model.as_deref().unwrap_or(current_model);

    let Some((chosen_provider, chosen_model)) = pick_provider_and_model(
        key_rx.as_deref_mut(),
        ui,
        cred_manager,
        discovery,
        initial_p,
        initial_m,
        false,
    )
    .await?
    else {
        return Ok(());
    };

    let comp_client = GenericProviderClient::new(
        chosen_provider.clone(),
        chosen_model.clone(),
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&chosen_provider),
    )?;

    agent.lock().await.set_compaction(Some(harness_core::CompactionSetup {
        provider: Arc::new(comp_client),
        model: chosen_model.clone(),
    }));

    *compact_provider = Some(chosen_provider.clone());
    *compact_model = Some(chosen_model.clone());

    ui.emit_above(&format!(
        "{} Compaction model set to {} ({})",
        "✓".green(),
        chosen_model,
        chosen_provider
    ));

    Ok(())
}

/// `/rewind [turn]`: list this session's file checkpoints, or restore one turn's files.
///
/// Restoring is about *files*. The transcript is untouched — a rewind puts the workspace back so
/// the conversation can continue from a known state, it does not un-say what was said.
pub(crate) async fn handle_rewind_command(
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &str,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let controller = {
        let agent = agent.lock().await;
        agent.checkpoints()
    };
    let Some(controller) = controller else {
        ui.emit_above(&format!(
            "{} Checkpoints are off — enable them with {}",
            "•".cyan(),
            "checkpoints.enabled = true".bold()
        ));
        return Ok(());
    };
    if !controller.enabled() {
        ui.emit_above(&format!(
            "{} Checkpoints are off — enable them with {}",
            "•".cyan(),
            "checkpoints.enabled = true".bold()
        ));
        return Ok(());
    }

    let store = controller.store();
    let checkpoints = store.list(session_id)?;

    let Some(seq) = arg.and_then(|raw| raw.parse::<u32>().ok()) else {
        if checkpoints.is_empty() {
            ui.emit_above(&format!(
                "{} No checkpoints yet — the next turn that writes a file creates one",
                "•".cyan()
            ));
            return Ok(());
        }
        ui.emit_above(&format!(
            "{} {} checkpoint(s) for this session (newest last):",
            "•".cyan(),
            checkpoints.len()
        ));
        let len = checkpoints.len();
        for (index, checkpoint) in checkpoints.iter().enumerate() {
            // The counts come from the *bytes*, so a turn that wrote a file back unchanged reads
            // as unchanged rather than as a scary modification.
            let stat = store
                .diff(session_id, checkpoint.seq)
                .map(|turn| turn.summary().stat_line())
                .unwrap_or_else(|_| format!("{} file(s)", checkpoint.files.len()));
            let marker = if index + 1 == len { " (latest)" } else { "" };
            let prompt = checkpoint
                .prompt
                .as_deref()
                .map(|prompt| {
                    let flat = prompt.replace('\n', " ");
                    let short: String = flat.chars().take(60).collect();
                    format!(" — {}", short.dimmed())
                })
                .unwrap_or_default();
            ui.emit_above(&format!(
                "    {}  {}{}{}",
                format!("turn {}", checkpoint.seq).bold(),
                stat.cyan(),
                marker.green(),
                prompt
            ));
        }
        ui.emit_above(&format!(
            "    Restore one with {}",
            format!("/rewind <turn>").bold()
        ));
        return Ok(());
    };

    let report = store.restore(session_id, seq)?;
    if report.is_empty() {
        ui.emit_above(&format!(
            "{} Turn {} changed nothing restorable",
            "•".cyan(),
            seq
        ));
    } else {
        ui.emit_above(&format!(
            "{} Rewound turn {}: {} file(s) restored, {} deleted",
            "✔".green().bold(),
            seq,
            report.restored.len(),
            report.deleted.len()
        ));
        for path in report.restored.iter().chain(report.deleted.iter()) {
            ui.emit_above(&format!("    {}", path.dimmed()));
        }
    }
    for skipped in &report.skipped {
        ui.emit_above(&format!("    {} {}", "skipped".yellow(), skipped.dimmed()));
    }
    ui.emit_above(&format!(
        "{} The transcript is unchanged — files only",
        "•".cyan()
    ));
    Ok(())
}

/// `/diff [turn|--all]`: what the workspace actually changed, read from the turn's checkpoint.
///
/// `/rewind` is a write. This is the read that belongs before it: the same snapshot, diffed against
/// the files as they are now, so a person can see what a turn did before deciding to undo it.
pub(crate) async fn handle_diff_command(
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &str,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let controller = {
        let agent = agent.lock().await;
        agent.checkpoints()
    };
    let Some(controller) = controller.filter(|controller| controller.enabled()) else {
        ui.emit_above(&format!(
            "{} Checkpoints are off — enable them with {}",
            "•".cyan(),
            "checkpoints.enabled = true".bold()
        ));
        return Ok(());
    };

    let store = controller.store();
    let checkpoints = store.list(session_id)?;
    if checkpoints.is_empty() {
        ui.emit_above(&format!(
            "{} No checkpoints yet — the next turn that writes a file creates one",
            "•".cyan()
        ));
        return Ok(());
    }

    let raw = arg.unwrap_or("").trim();
    let all = raw == "--all" || raw == "all";
    let seq = if all {
        None
    } else {
        match raw.split_whitespace().next().map(str::parse::<u32>) {
            Some(Ok(seq)) => Some(seq),
            _ => checkpoints.last().map(|checkpoint| checkpoint.seq),
        }
    };

    let turns = match seq {
        Some(seq) => vec![store.diff(session_id, seq)?],
        None => store.diff_all(session_id)?,
    };

    for turn in &turns {
        let summary = turn.summary();
        let prompt = turn
            .prompt
            .as_deref()
            .map(|prompt| prompt.replace('\n', " "))
            .map(|prompt| format!(" — {}", prompt.dimmed()))
            .unwrap_or_default();
        ui.emit_above(&format!(
            "{}  {}{}",
            format!("turn {}", turn.seq).bold(),
            summary.stat_line().cyan(),
            prompt
        ));

        if turn.is_empty() && turn.unrecorded.is_empty() {
            ui.emit_above(&format!(
                "    {}",
                "nothing changed (the workspace matches this turn's snapshot)".dimmed()
            ));
            continue;
        }

        for file in turn.changed() {
            ui.emit_above(&format!(
                "    {} {:<9} {} {}",
                file.kind.marker(),
                file.kind.as_str().yellow(),
                file.path.bold(),
                format!("+{} -{}", file.additions, file.deletions).dimmed()
            ));
            if let Some(note) = &file.note {
                ui.emit_above(&format!("        {}", note.dimmed()));
            }
            // Hunks are the point of a diff, but a chat pane is not a pager: cap them and say so
            // rather than letting one regenerated lockfile scroll the conversation away.
            let mut budget = DIFF_HUNK_BUDGET;
            for line in file.diff.trim_end().lines() {
                if budget == 0 {
                    ui.emit_above(&format!(
                        "        {}",
                        "… more hunks not shown in chat — pipe `luna diff` for all of them".dimmed()
                    ));
                    break;
                }
                budget -= 1;
                let painted = if line.starts_with("+++") || line.starts_with("---") || line.starts_with("@@") {
                    line.cyan()
                } else if line.starts_with('+') {
                    line.green()
                } else if line.starts_with('-') {
                    line.red()
                } else {
                    line.normal()
                };
                ui.emit_above(&format!("        {}", painted));
            }
        }
        if !turn.unrecorded.is_empty() {
            ui.emit_above(&format!(
                "    {}",
                format!(
                    "! {} file(s) changed by a shell command — no snapshot, /rewind cannot undo these:",
                    turn.unrecorded.len()
                )
                .yellow()
            ));
            for file in &turn.unrecorded {
                ui.emit_above(&format!(
                    "      {:<9} {}",
                    file.kind.as_str(),
                    file.path.dimmed()
                ));
            }
        }
        ui.emit_above("");
    }

    Ok(())
}

/// How many diff lines a chat reply shows per turned file.
const DIFF_HUNK_BUDGET: usize = 40;

/// `/name [name]`: set the current session's user-assigned name, or report it when no name is given.
pub(crate) async fn handle_name_command(
    ui: &ChatUi,
    store: &SqliteSessionStore,
    session_id: &str,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    if let Some(raw) = arg {
        store.set_name(session_id, raw).await?;
    }

    let name = store.get(session_id).await?.name;
    ui.set_session_name(&name);

    if name.is_empty() {
        if arg.is_some() {
            ui.emit_above(&format!("{} Session name cleared", "✔".green().bold()));
        } else {
            ui.emit_above(&format!(
                "{} This session has no name — set one with {}",
                "•".cyan(),
                "/name <name>".bold()
            ));
        }
    } else if arg.is_some() {
        ui.emit_above(&format!(
            "{} Session name set to '{}'",
            "✔".green().bold(),
            name.cyan()
        ));
    } else {
        ui.emit_above(&format!("{} Session name: {}", "•".cyan(), name.cyan()));
    }

    Ok(())
}

/// `/new [name]`: start a fresh chat session, rebinding the agent and clearing the screen.
pub(crate) async fn handle_new_command(
    ui: &ChatUi,
    store: &Arc<SqliteSessionStore>,
    config: &Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &mut String,
    current_provider: &str,
    current_model: &str,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let new_id = resolve_session(store.as_ref(), SessionSelection::New).await?;
    let name = arg.map(|s| s.trim()).unwrap_or("");
    if !name.is_empty() {
        store.set_name(&new_id, name).await?;
    }
    // The new session inherits the model currently in use, so the switch carried into it is
    // not lost the next time this session is resumed.
    store
        .set_model(&new_id, current_provider, current_model)
        .await?;

    *session_id = new_id.clone();
    agent.lock().await.set_session(SessionBinding {
        store: store.clone(),
        session_id: new_id.clone(),
        resume_max_messages: config.session.resume_max_messages,
    });

    let context_max = get_context_window(current_provider, current_model).unwrap_or(128_000);
    ui.set_status(current_model, 0, context_max);
    ui.set_session_name(name);

    if ui.is_attended {
        crate::commands::clear_screen();
        ui.draw_initial_box();
        if name.is_empty() {
            ui.emit_above(&format!(
                "{} Started new session {}",
                "✔".green().bold(),
                session_id.cyan()
            ));
        } else {
            ui.emit_above(&format!(
                "{} Started new session '{}' ({})",
                "✔".green().bold(),
                name.cyan(),
                session_id.dimmed()
            ));
        }
    } else {
        if name.is_empty() {
            println!(
                "{} Started new session {}",
                "✔".green().bold(),
                session_id.cyan()
            );
        } else {
            println!(
                "{} Started new session '{}' ({})",
                "✔".green().bold(),
                name.cyan(),
                session_id.dimmed()
            );
        }
    }

    Ok(())
}

/// `/delete`: permanently delete the current chat session and start a fresh one.
pub(crate) async fn handle_delete_command(
    ui: &ChatUi,
    store: &Arc<SqliteSessionStore>,
    config: &Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &mut String,
    current_provider: &str,
    current_model: &str,
) -> Result<(), HarnessError> {
    let old_id = session_id.clone();
    let old_name = store.get(&old_id).await.map(|s| s.name).unwrap_or_default();

    // Delete current session from SQLite store
    store.delete_session(&old_id).await?;

    // Start a fresh new session
    let new_id = resolve_session(store.as_ref(), SessionSelection::New).await?;
    store
        .set_model(&new_id, current_provider, current_model)
        .await?;

    *session_id = new_id.clone();
    agent.lock().await.set_session(SessionBinding {
        store: store.clone(),
        session_id: new_id.clone(),
        resume_max_messages: config.session.resume_max_messages,
    });

    let context_max = get_context_window(current_provider, current_model).unwrap_or(128_000);
    ui.set_status(current_model, 0, context_max);
    ui.set_session_name("");

    let desc = if old_name.is_empty() {
        old_id.cyan().to_string()
    } else {
        format!("'{}' ({})", old_name.cyan(), old_id.dimmed())
    };

    if ui.is_attended {
        crate::commands::clear_screen();
        ui.draw_initial_box();
        ui.emit_above(&format!(
            "{} Deleted session {} and started fresh session {}",
            "✔".green().bold(),
            desc,
            new_id.cyan()
        ));
    } else {
        println!(
            "{} Deleted session {} and started fresh session {}",
            "✔".green().bold(),
            desc,
            new_id.cyan()
        );
    }

    Ok(())
}

/// `/summarize [name]`, `/compact [name]`, or `/new -s [name]`:
/// Summarize the current session's history and start a new session seeded with the summary.
pub(crate) async fn handle_summarize_and_new_command(
    ui: &ChatUi,
    store: &Arc<SqliteSessionStore>,
    config: &Config,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    session_id: &mut String,
    current_provider: &str,
    current_model: &str,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let replayed = store.replay(session_id, usize::MAX).await?;
    let has_content = replayed
        .history
        .iter()
        .any(|s| !s.is_summary && s.message.role != Role::System);

    if !has_content {
        ui.draw_initial_box();
        ui.emit_above(&format!(
            "{} Nothing to summarize in this session",
            "•".cyan()
        ));
        return Ok(());
    }

    let old_id = session_id.clone();
    let name = arg.map(|s| s.trim()).unwrap_or("");

    ui.start_activity(
        "summarize session",
        "summarize",
        &serde_json::json!({ "session": old_id }),
    );

    let summary_res = {
        let ag = agent.lock().await;
        ag.summarize_current_session(current_model).await
    };

    ui.stop_activity();

    let summary = match summary_res {
        Ok(s) => s,
        Err(e) => {
            ui.draw_initial_box();
            ui.emit_above(&format!(
                "{} Failed to summarize session: {}",
                "✖".red().bold(),
                e
            ));
            return Ok(());
        }
    };

    let new_id = resolve_session(store.as_ref(), SessionSelection::New).await?;
    if !name.is_empty() {
        store.set_name(&new_id, name).await?;
    }
    // As with `/new`: the follow-up session inherits the model in use when it was created.
    store
        .set_model(&new_id, current_provider, current_model)
        .await?;

    let summary_header = if !name.is_empty() {
        format!("Context summary from session '{}' ({}):\n{}", name, old_id, summary)
    } else {
        format!("Context summary from session {}:\n{}", old_id, summary)
    };
    let summary_msg = Message::system(summary_header);
    store.append(&new_id, &summary_msg, true).await?;

    *session_id = new_id.clone();
    agent.lock().await.set_session(SessionBinding {
        store: store.clone(),
        session_id: new_id.clone(),
        resume_max_messages: config.session.resume_max_messages,
    });

    let context_max = get_context_window(current_provider, current_model).unwrap_or(128_000);
    let summary_tokens = harness_core::heuristic_tokens(&[summary_msg]);
    ui.set_status(current_model, summary_tokens, context_max);
    ui.set_session_name(name);

    if ui.is_attended {
        crate::commands::clear_screen();
        ui.draw_initial_box();
        if name.is_empty() {
            ui.emit_above(&format!(
                "{} Started new session {} with summary from {}",
                "✔".green().bold(),
                session_id.cyan(),
                old_id.dimmed()
            ));
        } else {
            ui.emit_above(&format!(
                "{} Started new session '{}' ({}) with summary from {}",
                "✔".green().bold(),
                name.cyan(),
                session_id.dimmed(),
                old_id.dimmed()
            ));
        }
        ui.emit_above(&format!(
            "\n{}\n{}\n",
            "Summary:".bold(),
            summary.dimmed()
        ));
    } else {
        if name.is_empty() {
            println!(
                "{} Started new session {} with summary from {}",
                "✔".green().bold(),
                session_id.cyan(),
                old_id.dimmed()
            );
        } else {
            println!(
                "{} Started new session '{}' ({}) with summary from {}",
                "✔".green().bold(),
                name.cyan(),
                session_id.dimmed(),
                old_id.dimmed()
            );
        }
        println!("\n{}\n{}\n", "Summary:".bold(), summary);
    }

    Ok(())
}

/// `/compact`: fold the current session's oldest turns into a summary in place — the session
/// keeps its id and name, only the context shrinks.
pub(crate) async fn handle_compact_command(
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    current_provider: &str,
    current_model: &str,
) -> Result<(), HarnessError> {
    ui.start_activity("compact context", "compact", &serde_json::json!({}));
    let result = {
        let ag = agent.lock().await;
        ag.compact_current_session().await
    };
    ui.stop_activity();

    match result {
        Ok(notice) => {
            ui.emit_above(&format!(
                "{} Compacted {} turns into 1 summary",
                "✔".green().bold(),
                notice.turns
            ));
            // The summary replaced the folded turns, so the gauge drops for real.
            let context_used = agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
            let context_max = get_context_window(current_provider, current_model).unwrap_or(128_000);
            ui.update_status(context_used, context_max);
        }
        Err(e) => {
            ui.emit_above(&format!("{} Failed to compact session: {}", "✖".red().bold(), e));
        }
    }

    Ok(())
}

pub(crate) async fn handle_reasoning_switch(
    mut key_rx: Option<&mut mpsc::UnboundedReceiver<AppKey>>,
    ui: &ChatUi,
    agent: &Arc<tokio::sync::Mutex<ChatAgent>>,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let chosen_effort = if let Some(s) = arg {
        if s.eq_ignore_ascii_case("off") || s.eq_ignore_ascii_case("none") {
            None
        } else {
            match ReasoningEffort::from_str_loose(s) {
                Some(e) => Some(e),
                None => {
                    ui.draw_initial_box();
                    ui.emit_above(&format!(
                        "{} Unknown reasoning effort '{}'. Available: minimal, low, medium, high, xhigh, max, off",
                        "✖".red().bold(),
                        s
                    ));
                    return Ok(());
                }
            }
        }
    } else {
        let reasoning_options = [
            ("off / default", None),
            ("minimal", Some(ReasoningEffort::Minimal)),
            ("low", Some(ReasoningEffort::Low)),
            ("medium", Some(ReasoningEffort::Medium)),
            ("high", Some(ReasoningEffort::High)),
            ("xhigh", Some(ReasoningEffort::Xhigh)),
            ("max", Some(ReasoningEffort::Max)),
        ];
        let reasoning_items: Vec<String> = reasoning_options.iter().map(|(label, _)| label.to_string()).collect();
        let current_effort = agent.lock().await.reasoning_effort();
        let default_idx = match current_effort {
            None => 0,
            Some(ReasoningEffort::Minimal) => 1,
            Some(ReasoningEffort::Low) => 2,
            Some(ReasoningEffort::Medium) => 3,
            Some(ReasoningEffort::High) => 4,
            Some(ReasoningEffort::Xhigh) => 5,
            Some(ReasoningEffort::Max) => 6,
        };

        if let Some(ref mut rx) = key_rx {
            while rx.try_recv().is_ok() {}
            match select_from_rx(rx, ui, "Select Reasoning Effort", &reasoning_items, default_idx, false, false).await {
                Some(i) => reasoning_options[i].1,
                None => {
                    return Ok(());
                }
            }
        } else {
            None
        }
    };

    agent.lock().await.set_reasoning_effort(chosen_effort);
    // Same as the model switch: the picker left a box behind, so erase it before redrawing.
    ui.clear_box_on_exit();
    ui.draw_initial_box();

    let effort_label = chosen_effort
        .map(|e| e.to_string())
        .unwrap_or_else(|| "off".to_string());

    ui.emit_above(&format!(
        "{} Reasoning effort set to '{}'",
        "✔".green().bold(),
        effort_label.yellow()
    ));

    Ok(())
}

