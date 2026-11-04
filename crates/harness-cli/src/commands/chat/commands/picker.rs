//! The pickers: the interactive menus the chat hangs off the composer.
//!
//! They read the same key channel the composer does ([`super::keys::AppKey`]) rather than driving
//! `dialoguer` themselves, so a picker on the alternate path (the `/connect` flow, the model
//! switch) looks and behaves exactly like the slash-command menu.
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use colored::Colorize;
use tokio::sync::mpsc;

use super::CommandEntry;
use crate::commands::chat::keys::AppKey;
use crate::commands::chat::render::PROMPT;
use crate::commands::chat::ui::{get_terminal_width, ChatUi};
use harness_core::HarnessError;
use harness_providers::{
    get_default_model, get_provider_catalog, CredentialManager, ModelDiscovery,
};

pub(crate) async fn select_slash_command(
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
    entries: &[CommandEntry],
    // The menu owns the keyboard while it is open, so Ctrl+V has to be handled here as well;
    // otherwise pasting a screenshot after typing `/worker` silently does nothing.
    pending_images: &Arc<Mutex<Vec<String>>>,
) -> Option<String> {
    if !ui.is_attended {
        return None;
    }

    const PAGE_SIZE: usize = 5;
    const BOX_ROWS: usize = PAGE_SIZE + 2;

    let mut selected: usize = 0;

    let get_filtered = |filter: &str| -> Vec<(usize, &CommandEntry)> {
        let raw = filter.strip_prefix('/').unwrap_or(filter);
        // Once the user has typed past the command name, the menu has done its job. Keeping it
        // open while they write the argument covered the very lines they were typing into.
        if raw.contains(char::is_whitespace) {
            return Vec::new();
        }
        let f = raw.trim().to_lowercase();
        if f.is_empty() {
            return entries.iter().enumerate().collect();
        }
        let f_cmd = f.split_whitespace().next().unwrap_or(&f);
        let mut matches: Vec<(usize, &CommandEntry)> = entries
            .iter()
            .enumerate()
            .filter(|(_, cmd)| {
                let name = cmd.name.to_lowercase();
                let details = cmd.details.to_lowercase();
                name.contains(&f)
                    || details.contains(&f)
                    || (!f_cmd.is_empty() && name.eq_ignore_ascii_case(&format!("/{}", f_cmd)))
            })
            .collect();
        // Prioritize prefix matches (commands starting with the filter) over substring matches.
        // Then, within each group, the exact name match comes first.
        matches.sort_by(|a, b| {
            let a_name = a.1.name.to_lowercase();
            let b_name = b.1.name.to_lowercase();
            let a_prefix = a_name.starts_with(&format!("/{}", f));
            let b_prefix = b_name.starts_with(&format!("/{}", f));
            match (a_prefix, b_prefix) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => {
                    let a_exact = a_name == format!("/{}", f);
                    let b_exact = b_name == format!("/{}", f);
                    match (a_exact, b_exact) {
                        (true, false) => std::cmp::Ordering::Less,
                        (false, true) => std::cmp::Ordering::Greater,
                        _ => std::cmp::Ordering::Equal,
                    }
                }
            }
        });
        matches
    };

    let render_box = |sel: usize, filt: &str| -> Vec<String> {
        let width = get_terminal_width().clamp(44, 76);
        let inner_width = width.saturating_sub(4);
        let filtered = get_filtered(filt);

        let mut lines = Vec::with_capacity(BOX_ROWS);
        lines.push(
            format!("┌{}┐", "─".repeat(width.saturating_sub(2)))
                .dimmed()
                .to_string(),
        );

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

            for (idx, &(_, cmd)) in filtered.iter().enumerate().take(end).skip(start) {
                let is_sel = idx == sel;
                let pfx = if is_sel {
                    "❯ ".cyan().bold().to_string()
                } else {
                    "  ".to_string()
                };
                let cmd_styled = if is_sel {
                    cmd.name.bold().to_string()
                } else {
                    cmd.name.cyan().to_string()
                };

                let pfx_len = 2;
                let name_len = cmd.name.chars().count();
                let gap_len = 2;
                let remaining_width = inner_width.saturating_sub(pfx_len + name_len + gap_len);

                let details_disp = if cmd.details.chars().count() > remaining_width {
                    format!(
                        "{}…",
                        cmd.details
                            .chars()
                            .take(remaining_width.saturating_sub(1))
                            .collect::<String>()
                    )
                } else {
                    cmd.details.to_string()
                };
                let details_len = details_disp.chars().count();
                let pad_len =
                    inner_width.saturating_sub(pfx_len + name_len + gap_len + details_len);

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

        lines.push(
            format!("└{}┘", "─".repeat(width.saturating_sub(2)))
                .dimmed()
                .to_string(),
        );
        // Centred on the terminal like every other popup. At column 0 the menu reads as a stray
        // block of output rather than a box over the composer. Every row is exactly `width` cells.
        let left = get_terminal_width().saturating_sub(width) / 2;
        if left > 0 {
            for line in &mut lines {
                line.insert_str(0, &" ".repeat(left));
            }
        }
        lines
    };

    // Ensure space below: scroll terminal if needed to provide canvas
    let buf = ui
        .input_buf
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    print!("\x1b[2B\r");
    for _ in 0..BOX_ROWS {
        println!();
    }
    print!("\x1b[{}A\r{}", BOX_ROWS + 2, ui.prompt_row(&buf));
    let _ = io::stdout().flush();

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

    let draw_menu = |sel: usize, filt: &str, buf_str: &str| {
        // The argument starts after the first space: hide the menu instead of replacing it with
        // an empty "(no matching commands)" box that still covers the transcript.
        let raw = filt.strip_prefix('/').unwrap_or(filt);
        if raw.contains(char::is_whitespace) {
            erase_menu(buf_str);
            return;
        }
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

    let cur_filter = {
        let b = ui.input_buf.lock().unwrap_or_else(|e| e.into_inner());
        b.clone()
    };
    draw_menu(selected, &cur_filter, &cur_filter);

    let mut result = None;

    while let Some(key) = key_rx.recv().await {
        let filter_str = ui
            .input_buf
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
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
                    let cmd_name = &filtered[selected].1.name;
                    let trimmed = filter_str.trim();
                    if let Some(rest) = trimmed.strip_prefix(cmd_name.as_str()) {
                        let rest = rest.trim();
                        if !rest.is_empty() {
                            result = Some(format!("{} {}", cmd_name, rest));
                        } else {
                            result = Some(cmd_name.to_string());
                        }
                    } else if let Some((_, rest)) = trimmed.split_once(' ') {
                        let rest = rest.trim();
                        if !rest.is_empty() {
                            result = Some(format!("{} {}", cmd_name, rest));
                        } else {
                            result = Some(cmd_name.to_string());
                        }
                    } else {
                        result = Some(cmd_name.to_string());
                    }
                } else if filter_str.starts_with('/') {
                    // Enter on a filter with no menu match falls through to the typed command
                    // itself, so typing "/name luna" straight through still executes.
                    result = Some(filter_str.clone());
                }
                break;
            }
            AppKey::Tab => {
                if !filtered.is_empty() && selected < filtered.len() {
                    let cmd = &filtered[selected].1.name;
                    let mut b = ui.input_buf.lock().unwrap_or_else(|e| e.into_inner());
                    b.clear();
                    b.push_str(cmd.as_str());
                    b.push(' ');
                    drop(b);
                    break;
                }
            }
            AppKey::Escape | AppKey::CtrlC | AppKey::Ctrl('c') => {
                ui.input_buf
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear();
                // Cancel means cancel: drop anything Ctrl+V attached while the menu was open.
                pending_images
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear();
                break;
            }
            AppKey::Backspace => {
                let mut b = ui.input_buf.lock().unwrap_or_else(|e| e.into_inner());
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
            AppKey::Ctrl('v') => {
                // Mirror the composer: the marker is the visible attachment, and Esc removes it
                // along with the pending image. Nothing is printed into the transcript, so there
                // is no stale "attached" line left behind after cancelling.
                match crate::commands::clipboard_image::read_clipboard_image() {
                    Ok(Some(img)) => {
                        let data_url = img.data_url();
                        let marker = format!("[image: {}x{}]", img.width, img.height);
                        {
                            let mut b = ui.input_buf.lock().unwrap_or_else(|e| e.into_inner());
                            if !b.is_empty() && !b.ends_with(' ') {
                                b.push(' ');
                            }
                            b.push_str(&marker);
                            b.push(' ');
                        }
                        pending_images
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .push(data_url);
                        ui.redraw_input_line();
                        let new_buf = ui
                            .input_buf
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .clone();
                        draw_menu(selected, &new_buf, &new_buf);
                    }
                    Ok(None) => {
                        ui.emit_above(&format!(
                            "{} Clipboard holds no image (nothing to paste)",
                            "•".dimmed()
                        ));
                        ui.redraw_input_line();
                        draw_menu(selected, &filter_str, &filter_str);
                    }
                    Err(e) => {
                        ui.emit_above(&format!(
                            "{} Screenshot paste failed: {}",
                            "✖".red().bold(),
                            e
                        ));
                        ui.redraw_input_line();
                        draw_menu(selected, &filter_str, &filter_str);
                    }
                }
            }
            AppKey::Char(c) if c != '\n' && c != '\r' && !c.is_control() => {
                let mut b = ui.input_buf.lock().unwrap_or_else(|e| e.into_inner());
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

    let final_buf = ui
        .input_buf
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    erase_menu(&final_buf);
    result
}

pub(crate) async fn select_from_rx(
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
            filtered.len().div_ceil(PAGE_SIZE)
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

            for (idx, (_, item)) in filtered.iter().enumerate().take(end).skip(start) {
                let item_width = term_width.saturating_sub(6);
                let display_item = if item.len() > item_width {
                    format!("{}…", &item[..item_width.saturating_sub(1)])
                } else {
                    item.to_string()
                };

                if idx == selected {
                    lines.push(format!(
                        "\r\x1b[2K  {} {}",
                        "❯".cyan().bold(),
                        display_item.bold()
                    ));
                } else {
                    lines.push(format!("\r\x1b[2K    {}", display_item.dimmed()));
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
            AppKey::Escape | AppKey::CtrlC | AppKey::Ctrl('c') => {
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

pub(crate) async fn input_from_rx(
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
            AppKey::Escape | AppKey::CtrlC | AppKey::Ctrl('c') => {
                print!("\r\x1b[2K");
                let _ = io::stdout().flush();
                return None;
            }
            _ => {}
        }
    }
    None
}

pub(super) async fn pick_provider_and_model(
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
    let has_opencode_daemon = harness_providers::read_opencode_service_info().is_some();
    let has_opencode_cli = harness_providers::find_opencode_binary().is_ok();

    // Only list providers that have wired credentials (or currently active, or daemon/cli detected)
    let available_catalog: Vec<_> = {
        let configured: Vec<_> = catalog
            .into_iter()
            .filter(|p| {
                loaded.iter().any(|l| l.eq_ignore_ascii_case(&p.id))
                    || p.id.eq_ignore_ascii_case(current_provider)
                    || (p.id == "opencode" && has_opencode_daemon)
                    || (p.id == "opencode-cli" && has_opencode_cli)
            })
            .collect();
        if configured.is_empty() {
            get_provider_catalog()
        } else {
            configured
        }
    };

    let mut items: Vec<String> = Vec::new();
    for p in &available_catalog {
        let mut tags = Vec::new();
        if p.id.eq_ignore_ascii_case(current_provider) {
            tags.push("active".to_string());
        }
        if loaded.iter().any(|l| l.eq_ignore_ascii_case(&p.id)) {
            tags.push("configured".to_string());
        }
        if p.id.eq_ignore_ascii_case("opencode") {
            if has_opencode_daemon {
                tags.push("daemon".to_string());
            }
            let oc_creds = cred_manager.get_credentials("opencode").await;
            if let Some(primary) = oc_creds
                .iter()
                .find(|c| c.is_priority && !c.is_temporarily_disabled)
                .or_else(|| oc_creds.iter().find(|c| !c.is_temporarily_disabled))
            {
                if !primary.label.is_empty() {
                    tags.push(primary.label.clone());
                }
            }
        } else if p.id.eq_ignore_ascii_case("opencode-cli") {
            if has_opencode_cli {
                tags.push("cli".to_string());
            }
            let cli_creds = cred_manager.get_credentials("opencode-cli").await;
            if let Some(primary) = cli_creds
                .iter()
                .find(|c| c.is_priority && !c.is_temporarily_disabled)
                .or_else(|| cli_creds.iter().find(|c| !c.is_temporarily_disabled))
            {
                if !primary.label.is_empty() {
                    tags.push(primary.label.clone());
                }
            }
        } else if p.id.eq_ignore_ascii_case("antigravity") || p.id.eq_ignore_ascii_case("agy") {
            let agy_creds = cred_manager.get_credentials("antigravity").await;
            if let Some(primary) = agy_creds
                .iter()
                .find(|c| c.is_priority && !c.is_temporarily_disabled)
                .or_else(|| agy_creds.iter().find(|c| !c.is_temporarily_disabled))
            {
                let acc = if !primary.label.is_empty() {
                    primary.label.clone()
                } else if let Ok(val) = serde_json::from_str::<serde_json::Value>(&primary.api_key)
                {
                    val.get("email")
                        .and_then(|e| e.as_str())
                        .unwrap_or("")
                        .to_string()
                } else {
                    String::new()
                };
                if !acc.is_empty() {
                    tags.push(acc);
                }
            }
        }
        let tag_str = if tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", tags.join(", "))
        };
        items.push(format!("{:<28} ({}){}", p.display_name, p.id, tag_str));
    }

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

    // If the chosen provider has multiple credentials in the pool, allow choosing which one to prioritize
    let provider_creds = cred_manager.get_credentials(&chosen_provider).await;
    if provider_creds.len() > 1 {
        let mut cred_items = Vec::new();
        let mut default_cred_idx = 0;
        for (i, c) in provider_creds.iter().enumerate() {
            let pri_str = if c.is_priority { " (priority)" } else { "" };
            cred_items.push(format!("{:<22}{}", c.label, pri_str));
            if c.is_priority {
                default_cred_idx = i;
            }
        }
        cred_items.push("[Keep current rotation]".to_string());

        if let Some(ref mut rx) = key_rx {
            while rx.try_recv().is_ok() {}
            if let Some(cred_sel) = select_from_rx(
                rx,
                ui,
                &format!("Select Credential [{}]", chosen_provider),
                &cred_items,
                default_cred_idx,
                true,
                true,
            )
            .await
            {
                if cred_sel < provider_creds.len() {
                    let target = &provider_creds[cred_sel];
                    let _ = cred_manager
                        .set_priority_credential(&chosen_provider, &target.id)
                        .await;
                }
            }
        }
    }

    // Fetch models for chosen provider
    let mut models = discovery
        .get_models(&chosen_provider, cred_manager, false)
        .await
        .unwrap_or_default();

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

/// Interactive workspace directory picker that navigates directories with fuzzy search and parent navigation.
pub(crate) async fn pick_workspace_directory(
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
    initial_dir: &std::path::Path,
) -> Result<Option<std::path::PathBuf>, HarnessError> {
    if !ui.is_attended {
        return Ok(None);
    }

    let mut current = if initial_dir.is_dir() {
        harness_core::fsutil::canonicalize(initial_dir)
            .unwrap_or_else(|_| initial_dir.to_path_buf())
    } else {
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    };

    let mut is_chained = false;
    loop {
        let mut subdirs = Vec::new();
        if let Ok(entries) = std::fs::read_dir(&current) {
            for entry in entries.flatten() {
                if let Ok(file_type) = entry.file_type() {
                    if file_type.is_dir() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if name == ".git" {
                            continue;
                        }
                        subdirs.push(name);
                    }
                }
            }
        }
        subdirs.sort_by_key(|a| a.to_lowercase());

        let mut items = Vec::new();
        // Item 0: Select current directory
        items.push(format!("✔ [Select this directory] ({})", current.display()));
        // Item 1: Parent directory
        if current.parent().is_some() {
            items.push(".. (parent directory)".to_string());
        }
        // Subdirectories
        for d in &subdirs {
            items.push(format!("{}/", d));
        }
        // Custom path entry
        items.push("[Enter custom path...]".to_string());

        let prompt = format!("Workspace: {}", current.display());
        while key_rx.try_recv().is_ok() {}

        let sel = select_from_rx(key_rx, ui, &prompt, &items, 0, is_chained, true).await;

        let Some(idx) = sel else {
            ui.clear_box_on_exit();
            ui.draw_initial_box();
            return Ok(None);
        };

        if idx == 0 {
            ui.clear_box_on_exit();
            ui.draw_initial_box();
            return Ok(Some(current));
        }

        let has_parent = current.parent().is_some();
        if has_parent && idx == 1 {
            if let Some(parent) = current.parent() {
                current = parent.to_path_buf();
            }
            is_chained = true;
            continue;
        }

        let custom_idx = items.len() - 1;
        if idx == custom_idx {
            ui.clear_box_on_exit();
            ui.draw_initial_box();
            while key_rx.try_recv().is_ok() {}
            if let Some(path_str) = input_from_rx(key_rx, "Enter directory path").await {
                let expanded = harness_core::config::expand_tilde(&path_str);
                let p = std::path::PathBuf::from(expanded);
                if p.is_dir() {
                    let canon = harness_core::fsutil::canonicalize(&p).unwrap_or(p);
                    return Ok(Some(canon));
                } else {
                    ui.emit_above(&format!(
                        "{} Directory not found: {}",
                        "✖".red().bold(),
                        path_str
                    ));
                    return Ok(None);
                }
            } else {
                return Ok(None);
            }
        }

        let sub_idx = if has_parent { idx - 2 } else { idx - 1 };
        if sub_idx < subdirs.len() {
            current = current.join(&subdirs[sub_idx]);
            if let Ok(canon) = harness_core::fsutil::canonicalize(&current) {
                current = canon;
            }
            is_chained = true;
            continue;
        }

        ui.clear_box_on_exit();
        ui.draw_initial_box();
        return Ok(Some(current));
    }
}
