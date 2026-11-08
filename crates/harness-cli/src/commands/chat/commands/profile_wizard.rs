//! `/profile` inside the chat, drawn with the same modal UI as approvals and the other popups.
//!
//! The standalone `luna profile create` uses `dialoguer` because it runs before (or outside) the
//! chat. In here the terminal is already in raw mode and the key reader owns stdin, so the wizard
//! reads the chat's own key channel instead: full-screen frames, the same rounded box, the same
//! `❯` focus marker and key hints. Nothing here touches stdin directly, which is also why the
//! wizard no longer needs to pause the reader.
//!
//! The filesystem work is shared with the standalone flow (`profile::create_profile_with`); this
//! module only asks the questions.

use colored::Colorize;
use tokio::sync::mpsc;

use harness_core::config::Config;
use harness_core::profiles::{self, ProfileText, DEFAULT_PROFILE};
use harness_core::HarnessError;

use super::super::keys::AppKey;
use super::super::modal::{paint_centred, ModalScreen};
use super::super::ui::{get_terminal_width, ChatUi};
use super::wrap_words;
use crate::commands::chat::keys;
use crate::commands::profile as profile_cmd;
use crate::commands::profile::ProfileCommand;

/// How many picker rows fit in one frame. The list scrolls rather than growing the box.
const PICKER_PAGE: usize = 7;

/// Run a `/profile` subcommand in the chat UI.
pub(crate) async fn handle_profile_command(
    command: ProfileCommand,
    config: &Config,
    global: &Config,
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
) -> Result<(), HarnessError> {
    match command {
        ProfileCommand::List => {
            ui.emit_above(&profile_cmd::list_text(&config.paths.profiles_dir));
            Ok(())
        }
        ProfileCommand::Show { name, full } => {
            ui.emit_above(&profile_cmd::show_text(
                name.as_deref().unwrap_or(DEFAULT_PROFILE),
                &config.paths.profiles_dir,
                full,
            )?);
            Ok(())
        }
        ProfileCommand::Path { name } => {
            ui.emit_above(&profile_cmd::path_text(
                name.as_deref().unwrap_or(DEFAULT_PROFILE),
                config,
            )?);
            Ok(())
        }
        ProfileCommand::Create { name } => create(name, global, key_rx, ui).await,
        ProfileCommand::Delete { name, force } => delete(name, force, config, key_rx, ui).await,
    }
}

async fn create(
    name: Option<String>,
    source: &Config,
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
) -> Result<(), HarnessError> {
    let profiles_dir = source.paths.profiles_dir.clone();
    std::fs::create_dir_all(&profiles_dir).map_err(|error| {
        HarnessError::Io(format!(
            "failed to create {}: {}",
            profiles_dir.display(),
            error
        ))
    })?;

    let name = match name {
        Some(name) => name,
        None => {
            let Some(name) = input_step(
                ui,
                key_rx,
                "Create Profile — Name",
                "Lowercase letters, digits and '-'. This is also the launcher name.",
                "",
                "",
                |value| {
                    profiles::validate_name(value)?;
                    Ok(value.to_string())
                },
            )
            .await
            else {
                return cancelled(ui);
            };
            name
        }
    };
    profiles::validate_name(&name)?;
    if profiles::profile_dir(&profiles_dir, &name).exists() {
        return Err(HarnessError::ConfigurationError(format!(
            "profile '{}' already exists at {}",
            name,
            profiles::profile_dir(&profiles_dir, &name).display()
        )));
    }

    let Some(summary) = input_step(
        ui,
        key_rx,
        "Create Profile — Summary",
        "One line describing what this profile is for.",
        "",
        "Custom profile",
        |value| Ok(value.to_string()),
    )
    .await
    else {
        return cancelled(ui);
    };
    let summary = if summary.trim().is_empty() {
        "Custom profile".to_string()
    } else {
        summary.trim().to_string()
    };

    let seed = profiles::default_profile()?;
    let text = ProfileText {
        summary: Some(summary.clone()),
        persona: Some(seed.persona.clone()),
        behavior: Some(seed.behavior.clone()),
    };

    // The editor is a real terminal program, not a chat frame: step out of raw mode, lend it
    // stdin for the duration, then hand both back. The prompt text is seeded, so an absent or
    // failed editor leaves the defaults in place.
    ui.emit_above(&format!(
        "{} Opening $EDITOR to edit the persona and behavior (seeded from the default profile).",
        "•".cyan()
    ));
    let edited = edit_prompts(&name, &text, key_rx);
    ui.redraw_box();
    let text = edited.unwrap_or(text);

    let persona = text.persona.unwrap_or(seed.persona.clone());
    let behavior = text.behavior.unwrap_or(seed.behavior.clone());
    let summary = text.summary.unwrap_or(summary);

    let Some(copy_providers) = choice_step(
        ui,
        key_rx,
        "Create Profile — Providers",
        "Use the same provider credentials as the default profile?",
        &["Yes, copy them", "No, start with none"],
        0,
    )
    .await
    else {
        return cancelled(ui);
    };
    let copy_providers = copy_providers == 0;

    let notes = match pick_notes(source, key_rx, ui).await {
        Some(notes) => notes,
        None => return cancelled(ui),
    };
    let memories = match pick_memories(source, key_rx, ui).await {
        Some(memories) => memories,
        None => return cancelled(ui),
    };

    let review = vec![
        review_row("name", &name),
        review_row("summary", &summary),
        review_row("providers", if copy_providers { "same" } else { "fresh" }),
        review_row("notes", &notes.len().to_string()),
        review_row("memories", &memories.len().to_string()),
    ];
    let Some(proceed) = choice_step_with_body(
        ui,
        key_rx,
        "Create Profile — Review",
        review,
        &["Create profile", "Cancel"],
        0,
    )
    .await
    else {
        return cancelled(ui);
    };
    if proceed != 0 {
        return cancelled(ui);
    }

    let dest = profile_cmd::create_profile_with(
        &profiles_dir,
        source,
        &name,
        summary,
        persona,
        behavior,
        copy_providers,
        &notes,
        &memories,
    )?;
    ui.emit_above(&format!(
        "{} Created {}",
        "✔".green().bold(),
        dest.display()
    ));
    ui.emit_above(&format!(
        "{} Open the new profile with `luna {}` or `{}`.",
        "✔".green().bold(),
        name,
        name
    ));
    Ok(())
}

async fn delete(
    name: String,
    force: bool,
    config: &Config,
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
) -> Result<(), HarnessError> {
    if !force {
        let Some(choice) = choice_step(
            ui,
            key_rx,
            "Delete Profile",
            &format!("Delete profile '{}' and all of its data?", name),
            &["Yes, delete it", "No, keep it"],
            1,
        )
        .await
        else {
            return cancelled(ui);
        };
        if choice != 0 {
            return cancelled(ui);
        }
    }
    profile_cmd::delete_profile(&name, &config.paths.profiles_dir)?;
    ui.emit_above(&format!("{} Deleted {}", "✔".green().bold(), name));
    Ok(())
}

fn cancelled(ui: &ChatUi) -> Result<(), HarnessError> {
    ui.emit_above(&format!("{} Cancelled.", "•".dimmed()));
    Ok(())
}

/// Run `$EDITOR` over the seeded prompt text. The chat's reader is paused while it runs so the
/// editor receives every keystroke; `create` repaints the composer once this returns.
fn edit_prompts(
    name: &str,
    text: &ProfileText,
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
) -> Option<ProfileText> {
    // Keys queued for the wizard do not belong to the editor.
    while key_rx.try_recv().is_ok() {}
    let raw = crossterm::terminal::is_raw_mode_enabled().unwrap_or(false);
    if raw {
        let _ = crossterm::terminal::disable_raw_mode();
    }
    let pause = keys::pause_input();
    let result = profile_cmd::edit_prompts(name, text);
    drop(pause);
    if raw {
        let _ = crossterm::terminal::enable_raw_mode();
    }
    result.ok()
}

/// If the profile has notes and memories, ask which ones to carry over.
async fn pick_notes(
    source: &Config,
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
) -> Option<Vec<std::path::PathBuf>> {
    let choices = profile_cmd::notes_for_copy(source);
    if choices.is_empty() {
        ui.emit_above(&format!("{} No notes to copy.", "•".dimmed()));
        return Some(Vec::new());
    }
    let labels: Vec<String> = choices.iter().map(|(label, _)| label.clone()).collect();
    let picked = multi_step(
        ui,
        key_rx,
        "Create Profile — Notes",
        "Notes to copy into the new profile.",
        &labels,
    )
    .await?;
    Some(
        choices
            .into_iter()
            .zip(picked)
            .filter_map(|((_, path), picked)| picked.then_some(path))
            .collect(),
    )
}

async fn pick_memories(
    source: &Config,
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    ui: &ChatUi,
) -> Option<Vec<String>> {
    let choices = profile_cmd::memories_for_copy(source);
    if choices.is_empty() {
        ui.emit_above(&format!("{} No memories to copy.", "•".dimmed()));
        return Some(Vec::new());
    }
    let labels: Vec<String> = choices.iter().map(|(label, _)| label.clone()).collect();
    let picked = multi_step(
        ui,
        key_rx,
        "Create Profile — Memories",
        "Memories to copy into the new profile.",
        &labels,
    )
    .await?;
    Some(
        choices
            .into_iter()
            .zip(picked)
            .filter_map(|((_, id), picked)| picked.then_some(id))
            .collect(),
    )
}

// ---------------------------------------------------------------- steps

async fn input_step<F>(
    ui: &ChatUi,
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    title: &str,
    question: &str,
    initial: &str,
    placeholder: &str,
    validate: F,
) -> Option<String>
where
    F: Fn(&str) -> Result<String, HarnessError>,
{
    // Keys typed before the frame opened belong to the composer, not to this answer.
    while key_rx.try_recv().is_ok() {}

    let mut input = initial.to_string();
    let mut error: Option<String> = None;

    ui.set_modal_active(true);
    ui.wait_for_paints().await;
    let screen = ModalScreen::enter();
    let width = get_terminal_width().saturating_sub(2);
    paint_centred(&render_input_box(
        title,
        question,
        &input,
        placeholder,
        error.as_deref(),
        width,
    ));

    let answer = loop {
        let key = match key_rx.recv().await {
            Some(key) => key,
            None => break None,
        };
        match key {
            AppKey::Enter => {
                let candidate = if input.trim().is_empty() {
                    placeholder.trim().to_string()
                } else {
                    input.trim().to_string()
                };
                match validate(&candidate) {
                    Ok(value) => break Some(value),
                    Err(message) => error = Some(message.to_string()),
                }
            }
            AppKey::Backspace => {
                input.pop();
                error = None;
            }
            AppKey::Escape | AppKey::CtrlC | AppKey::Ctrl('c') => break None,
            AppKey::Char(c) if !c.is_control() => {
                input.push(c);
                error = None;
            }
            _ => continue,
        }
        paint_centred(&render_input_box(
            title,
            question,
            &input,
            placeholder,
            error.as_deref(),
            width,
        ));
    };

    drop(screen);
    ui.set_modal_active(false);
    ui.redraw_box();
    answer
}

async fn choice_step(
    ui: &ChatUi,
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    title: &str,
    question: &str,
    options: &[&str],
    default_idx: usize,
) -> Option<usize> {
    let body = wrap_words(question, get_terminal_width().saturating_sub(8))
        .into_iter()
        .map(|line| line.to_string())
        .collect();
    choice_step_with_body(ui, key_rx, title, body, options, default_idx).await
}

async fn choice_step_with_body(
    ui: &ChatUi,
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    title: &str,
    body: Vec<String>,
    options: &[&str],
    default_idx: usize,
) -> Option<usize> {
    if options.is_empty() {
        return None;
    }
    while key_rx.try_recv().is_ok() {}

    let mut selected = default_idx.min(options.len() - 1);

    ui.set_modal_active(true);
    ui.wait_for_paints().await;
    let screen = ModalScreen::enter();
    let width = get_terminal_width().saturating_sub(2);
    paint_centred(&render_choice_box(title, &body, options, selected, width));

    let answer = loop {
        let key = match key_rx.recv().await {
            Some(key) => key,
            None => break None,
        };
        let chosen = match key {
            AppKey::Up | AppKey::Left | AppKey::BackTab => {
                selected = (selected + options.len() - 1) % options.len();
                None
            }
            AppKey::Down | AppKey::Right | AppKey::Tab => {
                selected = (selected + 1) % options.len();
                None
            }
            AppKey::Char('y') | AppKey::Char('Y') if options.len() == 2 => Some(0),
            AppKey::Char('n') | AppKey::Char('N') if options.len() == 2 => Some(1),
            AppKey::Enter | AppKey::Char(' ') => Some(selected),
            AppKey::Escape | AppKey::CtrlC | AppKey::Ctrl('c') => break None,
            _ => continue,
        };
        if let Some(index) = chosen {
            break Some(index);
        }
        paint_centred(&render_choice_box(title, &body, options, selected, width));
    };

    drop(screen);
    ui.set_modal_active(false);
    ui.redraw_box();
    answer
}

async fn multi_step(
    ui: &ChatUi,
    key_rx: &mut mpsc::UnboundedReceiver<AppKey>,
    title: &str,
    question: &str,
    items: &[String],
) -> Option<Vec<bool>> {
    if items.is_empty() {
        return Some(Vec::new());
    }
    while key_rx.try_recv().is_ok() {}

    let mut selected = 0usize;
    let mut checked = vec![false; items.len()];

    ui.set_modal_active(true);
    ui.wait_for_paints().await;
    let screen = ModalScreen::enter();
    let width = get_terminal_width().saturating_sub(2);
    paint_centred(&render_picker_box(
        title, question, items, &checked, selected, width,
    ));

    let answer = loop {
        let key = match key_rx.recv().await {
            Some(key) => key,
            None => break None,
        };
        match key {
            AppKey::Up | AppKey::Left | AppKey::BackTab => {
                selected = (selected + items.len() - 1) % items.len();
            }
            AppKey::Down | AppKey::Right | AppKey::Tab => {
                selected = (selected + 1) % items.len();
            }
            AppKey::Home => selected = 0,
            AppKey::End => selected = items.len() - 1,
            AppKey::PageUp => selected = selected.saturating_sub(PICKER_PAGE),
            AppKey::PageDown => selected = (selected + PICKER_PAGE).min(items.len() - 1),
            AppKey::Char(' ') => checked[selected] = !checked[selected],
            AppKey::Char('a') | AppKey::Char('A') => {
                let all = checked.iter().all(|value| *value);
                checked.iter_mut().for_each(|value| *value = !all);
            }
            AppKey::Enter => break Some(checked.clone()),
            AppKey::Escape | AppKey::CtrlC | AppKey::Ctrl('c') => break None,
            _ => continue,
        }
        paint_centred(&render_picker_box(
            title, question, items, &checked, selected, width,
        ));
    };

    drop(screen);
    ui.set_modal_active(false);
    ui.redraw_box();
    answer
}

// ---------------------------------------------------------------- rendering

/// Strip ANSI and cut to `room` cells, marking the cut with an ellipsis.
fn cut(text: &str, room: usize) -> String {
    if room == 0 {
        return String::new();
    }
    let plain = dialoguer::console::strip_ansi_codes(text);
    if plain.chars().count() > room {
        format!("{}…", plain.chars().take(room - 1).collect::<String>())
    } else {
        plain.to_string()
    }
}

fn visible_len(text: &str) -> usize {
    dialoguer::console::strip_ansi_codes(text).chars().count()
}

/// One full-width row inside the box's side borders.
fn row(content: &str, total: usize) -> String {
    let max_content = total.saturating_sub(4);
    let truncated = cut(content, max_content);
    let pad = max_content.saturating_sub(visible_len(&truncated));
    format!("│ {}{} │", truncated, " ".repeat(pad))
}

fn boxed(title: &str, body: &[String], hint: &str, width: usize) -> String {
    let total = width.clamp(32, 78);
    let blank = row("", total);
    let mut lines = Vec::new();
    let dashes = total.saturating_sub(title.chars().count() + 5);
    lines.push(format!("╭─ {} {}╮", title.bold(), "─".repeat(dashes)));
    lines.push(blank.clone());
    lines.extend(body.iter().cloned());
    lines.push(blank.clone());
    lines.push(row(&format!(" {}", hint.dimmed()), total));
    lines.push(format!("╰{}╯", "─".repeat(total.saturating_sub(2))));
    lines.join("\n") + "\n"
}

fn render_input_box(
    title: &str,
    question: &str,
    input: &str,
    placeholder: &str,
    error: Option<&str>,
    width: usize,
) -> String {
    let total = width.clamp(32, 78);
    let mut body = Vec::new();
    for line in wrap_words(question, total.saturating_sub(6)) {
        body.push(row(&format!(" {}", line), total));
    }
    body.push(row("", total));

    let label = "Value";
    let dashes = total.saturating_sub(label.len() + 11);
    body.push(row(
        &format!(" ┌─ {} {}┐ ", label.cyan(), "─".repeat(dashes)),
        total,
    ));
    let room = total.saturating_sub(10);
    let shown = if input.is_empty() {
        cut(placeholder, room).dimmed().to_string()
    } else {
        cut(input, room).bold().to_string()
    };
    body.push(row(
        &format!(" │ {} {} │ ", "❯".cyan().bold(), shown),
        total,
    ));
    body.push(row(
        &format!(" └{}┘ ", "─".repeat(total.saturating_sub(8))),
        total,
    ));

    body.push(row("", total));
    if let Some(message) = error {
        body.push(row(
            &format!(
                " {} {}",
                "✖".red().bold(),
                cut(message, total.saturating_sub(8)).red()
            ),
            total,
        ));
    } else {
        // Kept as an empty row rather than dropped: the frame's height must not change with state.
        body.push(row("", total));
    }

    boxed(title, &body, "Enter continues • Esc cancels", total)
}

fn render_choice_box(
    title: &str,
    body: &[String],
    options: &[&str],
    selected: usize,
    width: usize,
) -> String {
    let total = width.clamp(32, 78);
    let mut rows: Vec<String> = body
        .iter()
        .map(|line| row(&format!(" {}", line), total))
        .collect();
    rows.push(row("", total));
    for (index, option) in options.iter().enumerate() {
        let label = cut(option, total.saturating_sub(8));
        if index == selected {
            rows.push(row(
                &format!(" {} {}", "❯".cyan().bold(), label.bold()),
                total,
            ));
        } else {
            rows.push(row(&format!("   {}", label.dimmed()), total));
        }
    }
    boxed(
        title,
        &rows,
        "↑/↓ or y/n selects • Enter confirms • Esc cancels",
        total,
    )
}

fn render_picker_box(
    title: &str,
    question: &str,
    items: &[String],
    checked: &[bool],
    selected: usize,
    width: usize,
) -> String {
    let total = width.clamp(32, 78);
    let mut body = Vec::new();
    for line in wrap_words(question, total.saturating_sub(6)) {
        body.push(row(&format!(" {}", line), total));
    }
    body.push(row("", total));

    // Keep the focused row visible without growing the frame past one page.
    let start = if selected >= PICKER_PAGE {
        selected + 1 - PICKER_PAGE
    } else {
        0
    };
    let end = (start + PICKER_PAGE).min(items.len());
    for index in start..end {
        let mark = if checked.get(index).copied().unwrap_or(false) {
            "[x]".green().bold().to_string()
        } else {
            "[ ]".dimmed().to_string()
        };
        let label = cut(&items[index], total.saturating_sub(10));
        if index == selected {
            body.push(row(
                &format!(" {} {} {}", "❯".cyan().bold(), mark, label.bold()),
                total,
            ));
        } else {
            body.push(row(&format!("   {} {}", mark, label.dimmed()), total));
        }
    }
    // A list longer than one page keeps the frame height fixed so paging never makes it jump.
    // A shorter list stays compact: empty rows under the last item are just noise.
    if items.len() > PICKER_PAGE {
        for _ in (end - start)..PICKER_PAGE {
            body.push(row("", total));
        }
    }

    body.push(row(
        &format!(" {} {} of {}", "·".dimmed(), selected + 1, items.len()),
        total,
    ));

    boxed(
        title,
        &body,
        "Space toggles • ↑/↓ moves • a all • Enter saves • Esc cancels",
        total,
    )
}

fn review_row(label: &str, value: &str) -> String {
    format!("{} {}", format!("{}:", label).dimmed(), value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn width_of(text: &str) -> Vec<usize> {
        text.lines()
            .map(|line| dialoguer::console::strip_ansi_codes(line).chars().count())
            .collect()
    }

    #[test]
    fn picker_frames_hold_one_width_in_every_state() {
        let items: Vec<String> = (0..12).map(|index| format!("note {index}")).collect();
        for width in [20, 32, 44, 78, 120] {
            for selected in [0, 5, 11] {
                let mut checked = vec![false; items.len()];
                checked[selected] = true;
                let frame = render_picker_box(
                    "Create Profile — Notes",
                    "Notes to copy into the new profile.",
                    &items,
                    &checked,
                    selected,
                    width,
                );
                let widths = width_of(&frame);
                assert!(
                    widths.iter().all(|value| *value == widths[0]),
                    "widths at width={width}, selected={selected}: {widths:?}"
                );
                assert!(widths[0] <= width.clamp(32, 78));
            }
        }
    }

    #[test]
    fn checkboxes_show_the_state_of_each_row() {
        let items = vec!["first".to_string(), "second".to_string()];
        let checked = vec![false, true];
        let frame = render_picker_box("Notes", "Pick one.", &items, &checked, 1, 60);
        assert!(frame.contains("[ ] first"));
        assert!(frame.contains("[x] second"));
    }
}
