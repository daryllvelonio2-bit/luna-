//! Keyboard, mouse, and paste event handling for the TUI.

use std::io;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use harness_core::{ApprovalDecision, HarnessError, ReasoningEffort};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::interactive::state::{Env, Flow};
use super::actions::{
    apply_agent_mode, apply_model_target, apply_thinking_selection, create_new_session,
    interrupt_turn, open_models_for_target, open_provider_picker_for, open_session_picker,
    open_thinking_picker, run_palette_action, run_slash_command, set_all_process_expanded,
    submit_composer, switch_to_session, sync_slash_popup,
};
use super::render::{
    filtered_models, filtered_palette_items, filtered_providers, filtered_sessions,
};
use super::state::{
    ActiveModal, AgentMode, LiveThinkingView, ModelTarget, TabHitTarget, TextSelection,
    TranscriptClickTarget, TuiState,
};
use crate::commands::clarify::{ClarifyAnswer, ClarifyResponse};

/// Dispatch one terminal event (`Key`, `Mouse`, `Paste`, `Resize`).
pub async fn handle_event(
    env: &mut Env<'_>,
    st: &mut TuiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    event: Event,
) -> Result<Flow, HarnessError> {
    match event {
        Event::Key(key) => {
            // Ignore key-release events on terminals with enhanced keyboard reporting.
            if key.kind == KeyEventKind::Release {
                return Ok(Flow::Continue);
            }
            handle_key(env, st, terminal, key).await
        }
        Event::Mouse(mouse) => handle_mouse(env, st, mouse).await,
        Event::Paste(text) => {
            handle_paste(env, st, &text);
            Ok(Flow::Continue)
        }
        Event::Resize(_, _) => {
            // Ratatui autoresizes on draw. Ensure buffer geometry matches the new window
            // size without emitting an ANSI clear sequence that causes screen flicker.
            let _ = terminal.autoresize();
            Ok(Flow::Continue)
        }
        Event::FocusGained | Event::FocusLost => Ok(Flow::Continue),
    }
}

fn handle_paste(env: &Env<'_>, st: &mut TuiState, text: &str) {
    let cleaned = text.replace("\r\n", "\n").replace('\r', "\n");
    match &mut st.modal {
        ActiveModal::None => {
            if cleaned.is_empty() {
                if let Ok(Some(img)) = crate::commands::clipboard_image::read_clipboard_image() {
                    st.composer
                        .attach_image(img.width, img.height, img.data_url());
                    sync_slash_popup(env, st);
                }
                return;
            }
            st.composer.paste_text(&cleaned);
            sync_slash_popup(env, st);
        }
        ActiveModal::CommandPalette { query, selected } => {
            query.push_str(cleaned.trim());
            *selected = 0;
        }
        ActiveModal::SessionPicker {
            query, selected, ..
        } => {
            query.push_str(cleaned.trim());
            *selected = 0;
        }
        ActiveModal::ProviderPicker {
            query, selected, ..
        } => {
            query.push_str(cleaned.trim());
            *selected = 0;
        }
        ActiveModal::ModelPicker {
            query, selected, ..
        } => {
            query.push_str(cleaned.trim());
            *selected = 0;
        }
        ActiveModal::Clarify {
            custom,
            editing_custom,
            ..
        } if *editing_custom => {
            custom.push_str(cleaned.trim());
        }
        ActiveModal::Sudo { input, .. } => {
            input.push_str(cleaned.trim());
        }
        ActiveModal::InputModal { value, .. } => {
            value.push_str(cleaned.trim());
        }
        _ => {}
    }
}

async fn handle_mouse(
    env: &mut Env<'_>,
    st: &mut TuiState,
    mouse: MouseEvent,
) -> Result<Flow, HarnessError> {
    // Hover state drives the preview close buttons, so it has to track the pointer even while a
    // modal owns the keyboard.
    st.pointer = Some((mouse.column, mouse.row));
    if !matches!(st.modal, ActiveModal::None) {
        match mouse.kind {
            MouseEventKind::ScrollUp => match &mut st.modal {
                ActiveModal::CommandPalette { selected, .. }
                | ActiveModal::SessionPicker { selected, .. }
                | ActiveModal::ProviderPicker { selected, .. }
                | ActiveModal::ModelPicker { selected, .. }
                | ActiveModal::ThinkingPicker { selected, .. } => {
                    *selected = selected.saturating_sub(1);
                }
                ActiveModal::TextModal { scroll, .. } => {
                    *scroll = scroll.saturating_sub(3);
                }
                _ => {}
            },
            MouseEventKind::ScrollDown => match &mut st.modal {
                ActiveModal::CommandPalette { query, selected } => {
                    let count = filtered_palette_items(query).len();
                    if count > 0 {
                        *selected = (*selected + 1).min(count.saturating_sub(1));
                    }
                }
                ActiveModal::SessionPicker {
                    query,
                    sessions,
                    selected,
                } => {
                    let count = filtered_sessions(sessions, query).len();
                    if count > 0 {
                        *selected = (*selected + 1).min(count.saturating_sub(1));
                    }
                }
                ActiveModal::ProviderPicker {
                    query,
                    items,
                    selected,
                    ..
                } => {
                    let count = filtered_providers(items, query).len();
                    if count > 0 {
                        *selected = (*selected + 1).min(count.saturating_sub(1));
                    }
                }
                ActiveModal::ModelPicker {
                    query,
                    items,
                    selected,
                    ..
                } => {
                    let count = filtered_models(items, query).len();
                    if count > 0 {
                        *selected = (*selected + 1).min(count.saturating_sub(1));
                    }
                }
                ActiveModal::ThinkingPicker { selected, .. } => {
                    *selected = (*selected + 1).min(6);
                }
                ActiveModal::TextModal { lines, scroll, .. } => {
                    let max_s = lines.len().saturating_sub(1);
                    *scroll = (*scroll + 3).min(max_s);
                }
                _ => {}
            },
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(r) = st.modal_rect {
                    if mouse.column < r.x
                        || mouse.column >= r.x + r.width
                        || mouse.row < r.y
                        || mouse.row >= r.y + r.height
                    {
                        close_modal_with_cancel(st);
                    }
                }
            }
            _ => {}
        }
        return Ok(Flow::Continue);
    }
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            st.scroll_from_bottom = (st.scroll_from_bottom + 3).min(st.max_scroll);
            clear_selection(st);
        }
        MouseEventKind::ScrollDown => {
            st.scroll_from_bottom = st.scroll_from_bottom.saturating_sub(3);
            clear_selection(st);
        }
        MouseEventKind::Down(MouseButton::Left) if mouse.row == 0 => {
            clear_selection(st);
            let hit = st
                .hit_boxes
                .iter()
                .find(|hb| mouse.column >= hb.x_start && mouse.column < hb.x_end)
                .map(|hb| hb.target.clone());
            if let Some(target) = hit {
                match target {
                    TabHitTarget::OverflowButton => {
                        open_session_picker(env, st).await;
                    }
                    TabHitTarget::Session(id) => {
                        if let Err(e) = switch_to_session(env, st, &id).await {
                            env.ui
                                .set_transient_error(Some(format!("Session switch failed: {e}")));
                        }
                    }
                    TabHitTarget::NewSession => {
                        if let Err(e) = create_new_session(env, st).await {
                            env.ui
                                .set_transient_error(Some(format!("New session failed: {e}")));
                        }
                    }
                }
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            let attachment = st
                .composer_hit_boxes
                .iter()
                .find(|hit| {
                    mouse.row == hit.y && mouse.column >= hit.x_start && mouse.column < hit.x_end
                })
                .map(|hit| hit.token.clone());
            if let Some(token) = attachment {
                st.composer.remove_attachment(&token);
                clear_selection(st);
            } else {
                begin_left_down(env, st, mouse);
            }
        }
        MouseEventKind::Drag(MouseButton::Left) => {
            extend_selection(st, mouse);
        }
        MouseEventKind::Up(MouseButton::Left) => {
            finish_left_up(env, st).await;
        }
        MouseEventKind::Down(MouseButton::Middle) => {
            let (cx, cy, cw, ch) = st.composer_input_area;
            if cw > 0
                && ch > 0
                && mouse.column >= cx
                && mouse.column < cx + cw
                && mouse.row >= cy
                && mouse.row < cy + ch
            {
                if let Some(text) = crate::commands::clipboard_image::read_primary_text()
                    .or_else(crate::commands::clipboard_image::read_clipboard_text)
                {
                    st.composer.paste_text(&text);
                    sync_slash_popup(env, st);
                }
            }
        }
        _ => {}
    }
    Ok(Flow::Continue)
}

fn clear_selection(st: &mut TuiState) {
    st.selection = None;
    st.pending_transcript_click = None;
}

/// Arm a selection at the pointer and remember any click target under it.
///
/// The click target is not fired yet: a press that turns into a drag is a text selection, not a
/// toggle, so clicks are committed on button release.
fn begin_left_down(env: &Env<'_>, st: &mut TuiState, mouse: MouseEvent) {
    let now = std::time::Instant::now();
    let is_multi = if let Some((last_time, last_c, last_r)) = st.last_click {
        now.duration_since(last_time).as_millis() <= 400
            && (mouse.column as i32 - last_c as i32).abs() <= 1
            && mouse.row == last_r
    } else {
        false
    };
    if is_multi {
        st.click_count = (st.click_count % 3) + 1;
    } else {
        st.click_count = 1;
    }
    st.last_click = Some((now, mouse.column, mouse.row));

    // Clicking inside the composer text area places the editing cursor.
    let (cx, cy, cw, ch) = st.composer_input_area;
    if cw > 0
        && ch > 0
        && mouse.column >= cx
        && mouse.column < cx + cw
        && mouse.row >= cy
        && mouse.row < cy + ch
    {
        let target_line = (mouse.row - cy) as usize + st.composer_scroll_off;
        let target_col = (mouse.column - cx) as usize;
        let max_w = (st.composer_area.2 as usize).saturating_sub(4).max(12);
        st.composer.cursor =
            cursor_byte_from_coords(&st.composer.text, target_line, target_col, max_w);
        clear_selection(st);
        return;
    }

    let (tx, ty, tw, th) = st.transcript_area;
    let in_transcript = tw > 0
        && th > 0
        && mouse.column >= tx
        && mouse.column < tx.saturating_add(tw)
        && mouse.row >= ty
        && mouse.row < ty.saturating_add(th);

    let hit = st
        .transcript_hit_boxes
        .iter()
        .find(|hb| hb.y == mouse.row && mouse.column >= hb.x_start && mouse.column < hb.x_end)
        .map(|hb| hb.target.clone());

    if in_transcript {
        let line = st.transcript_start + (mouse.row - ty) as usize;
        let column = (mouse.column - tx) as usize;
        let line_text = st
            .transcript_visible_text
            .get((mouse.row - ty) as usize)
            .cloned()
            .unwrap_or_default();

        if st.click_count == 2 {
            let (start_col, end_col) = find_word_bounds(&line_text, column);
            st.selection = Some(TextSelection {
                anchor: (line, start_col),
                cursor: (line, end_col.saturating_sub(1)),
                dragging: false,
            });
            st.pending_transcript_click = None;
            copy_selection_to_clipboard(env, st);
            return;
        } else if st.click_count == 3 {
            let line_w = line_text.width().max(1);
            st.selection = Some(TextSelection {
                anchor: (line, 0),
                cursor: (line, line_w.saturating_sub(1)),
                dragging: false,
            });
            st.pending_transcript_click = None;
            copy_selection_to_clipboard(env, st);
            return;
        }

        st.selection = Some(TextSelection {
            anchor: (line, column),
            cursor: (line, column),
            dragging: true,
        });
        st.pending_transcript_click = hit;
    } else {
        st.selection = None;
        // Badge clicks (and any future hit box outside the text area) still work.
        st.pending_transcript_click = hit;
        if st.pending_transcript_click.is_none() {
            clear_selection(st);
        }
    }
}

fn extend_selection(st: &mut TuiState, mouse: MouseEvent) {
    let (tx, ty, tw, th) = st.transcript_area;
    if tw == 0 || th == 0 {
        return;
    }
    let Some(selection) = st.selection.as_mut() else {
        return;
    };
    if !selection.dragging {
        return;
    }

    // Auto-scroll when pointer drags beyond the transcript's top or bottom edge.
    if mouse.row < ty && st.scroll_from_bottom < st.max_scroll {
        st.scroll_from_bottom = (st.scroll_from_bottom + 1).min(st.max_scroll);
        st.transcript_start = st.transcript_start.saturating_sub(1);
    } else if mouse.row >= ty.saturating_add(th) && st.scroll_from_bottom > 0 {
        st.scroll_from_bottom = st.scroll_from_bottom.saturating_sub(1);
        st.transcript_start = st.transcript_start.saturating_add(1);
    }

    let row = mouse.row.clamp(ty, ty.saturating_add(th).saturating_sub(1));
    let column = mouse
        .column
        .clamp(tx, tx.saturating_add(tw).saturating_sub(1));
    selection.cursor = (
        st.transcript_start + (row - ty) as usize,
        (column - tx) as usize,
    );
    if selection.is_range() {
        st.pending_transcript_click = None;
    }
}

async fn finish_left_up(env: &mut Env<'_>, st: &mut TuiState) {
    let Some(selection) = st.selection else {
        if let Some(target) = st.pending_transcript_click.take() {
            apply_transcript_click(st, target);
        }
        return;
    };

    if !selection.dragging {
        return;
    }

    if selection.is_range() {
        copy_selection_to_clipboard(env, st);
        st.selection = Some(TextSelection {
            dragging: false,
            ..selection
        });
        st.pending_transcript_click = None;
    } else {
        // A plain click: drop the one-cell selection and run the click target, if any.
        st.selection = None;
        if let Some(target) = st.pending_transcript_click.take() {
            apply_transcript_click(st, target);
        }
    }
}

fn copy_selection_to_clipboard(env: &Env<'_>, st: &mut TuiState) {
    let Some(selection) = st.selection else {
        return;
    };
    if selection.is_range() {
        let text = selection_text(st, selection);
        if !text.is_empty() {
            match crate::commands::clipboard_image::write_clipboard_text(&text) {
                Ok(()) => {
                    let chars = text.chars().count();
                    let noun = if chars == 1 {
                        "character"
                    } else {
                        "characters"
                    };
                    st.selection_notice = Some((
                        format!("Copied {chars} {noun} to clipboard"),
                        std::time::Instant::now(),
                    ));
                }
                Err(e) => env
                    .ui
                    .set_transient_error(Some(format!("Copy failed: {e}"))),
            }
        }
    }
}

fn apply_transcript_click(st: &mut TuiState, target: TranscriptClickTarget) {
    match target {
        TranscriptClickTarget::ToggleTurnThinking(idx, step_idx) => {
            if !st.expanded_thinking.remove(&(idx, step_idx)) {
                st.expanded_thinking.insert((idx, step_idx));
            }
        }
        TranscriptClickTarget::ToggleTurnTools(idx, group_idx) => {
            if !st.expanded_tools.remove(&(idx, group_idx)) {
                st.expanded_tools.insert((idx, group_idx));
            }
        }
        TranscriptClickTarget::ToggleLiveThinkingStep(step_idx) => {
            if !st.expanded_live_thinking.remove(&step_idx) {
                st.expanded_live_thinking.insert(step_idx);
            }
        }
        TranscriptClickTarget::ToggleLiveToolStep(step_idx) => {
            if !st.expanded_live_tools.remove(&step_idx) {
                st.expanded_live_tools.insert(step_idx);
            }
        }
        TranscriptClickTarget::ToggleLiveThinking => {
            st.live_thinking_view = match st.live_thinking_view {
                LiveThinkingView::Tail => LiveThinkingView::Expanded,
                LiveThinkingView::Expanded => LiveThinkingView::Collapsed,
                LiveThinkingView::Collapsed => LiveThinkingView::Expanded,
            };
        }
        TranscriptClickTarget::JumpToLatest => {
            st.scroll_from_bottom = 0;
            clear_selection(st);
        }
    }
}

/// Plain text covered by a selection. If lines belong to earlier turns in history that scrolled off,
/// they are read directly from the settled transcript cache.
fn selection_text(st: &TuiState, selection: TextSelection) -> String {
    let ((start_line, start_col), (end_line, end_col)) = selection.bounds();
    let width = st.transcript_area.2 as usize;
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut first = true;

    for line_idx in start_line..=end_line {
        let line_text: Option<String> =
            if let Some(vis_idx) = line_idx.checked_sub(st.transcript_start) {
                st.transcript_visible_text
                    .get(vis_idx)
                    .cloned()
                    .or_else(|| {
                        if line_idx < st.settled_cache.lines.len() {
                            Some(super::render::line_plain_text(
                                &st.settled_cache.lines[line_idx],
                            ))
                        } else {
                            None
                        }
                    })
            } else if line_idx < st.settled_cache.lines.len() {
                Some(super::render::line_plain_text(
                    &st.settled_cache.lines[line_idx],
                ))
            } else {
                None
            };
        let Some(line) = line_text else {
            continue;
        };
        let from = if line_idx == start_line { start_col } else { 0 };
        let to = if line_idx == end_line {
            end_col.saturating_add(1)
        } else {
            width
        }
        .min(width);
        if !first {
            out.push('\n');
        }
        first = false;
        out.push_str(&slice_cells(&line, from, to));
    }
    out
}

fn find_word_bounds(text: &str, target_col: usize) -> (usize, usize) {
    if text.is_empty() {
        return (0, 0);
    }
    let mut chars_with_cols = Vec::new();
    let mut col = 0usize;
    for ch in text.chars() {
        let w = ch.width().unwrap_or(0);
        let start = col;
        col += w;
        chars_with_cols.push((ch, start, col));
    }
    if chars_with_cols.is_empty() {
        return (0, 0);
    }
    let target_idx = chars_with_cols
        .iter()
        .position(|&(_, start, end)| target_col >= start && target_col < end)
        .unwrap_or(chars_with_cols.len().saturating_sub(1));

    let (target_char, _, _) = chars_with_cols[target_idx];
    let is_word_char =
        |c: char| c.is_alphanumeric() || c == '_' || c == '-' || c == '/' || c == '.' || c == ':';
    let target_is_word = is_word_char(target_char);

    let mut left = target_idx;
    while left > 0 {
        let (c, _, _) = chars_with_cols[left - 1];
        if is_word_char(c) == target_is_word && !c.is_whitespace() {
            left -= 1;
        } else {
            break;
        }
    }

    let mut right = target_idx;
    while right + 1 < chars_with_cols.len() {
        let (c, _, _) = chars_with_cols[right + 1];
        if is_word_char(c) == target_is_word && !c.is_whitespace() {
            right += 1;
        } else {
            break;
        }
    }

    (chars_with_cols[left].1, chars_with_cols[right].2)
}

fn cursor_byte_from_coords(
    text: &str,
    target_line: usize,
    target_col: usize,
    max_width: usize,
) -> usize {
    let width = max_width.max(8);
    let mut current_line = 0usize;
    let mut byte_pos = 0usize;

    for (raw_idx, raw_line) in text.split('\n').enumerate() {
        if raw_idx > 0 {
            byte_pos += 1; // `\n`
        }
        if raw_line.is_empty() {
            if current_line == target_line {
                return byte_pos;
            }
            current_line += 1;
            continue;
        }
        let mut cur_w = 0usize;
        for ch in raw_line.chars() {
            let ch_len = ch.len_utf8();
            let cw = ch.width().unwrap_or(0);
            if cur_w + cw > width && cur_w > 0 {
                if current_line == target_line {
                    return byte_pos;
                }
                current_line += 1;
                cur_w = 0;
            }
            if current_line == target_line && cur_w + cw > target_col {
                return byte_pos;
            }
            cur_w += cw;
            byte_pos += ch_len;
        }
        if current_line == target_line {
            return byte_pos;
        }
        current_line += 1;
    }
    byte_pos
}

#[cfg(test)]
fn selection_text_from(
    visible_text: &[String],
    transcript_start: usize,
    width: usize,
    selection: TextSelection,
) -> String {
    let ((start_line, start_col), (end_line, end_col)) = selection.bounds();
    if width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut first = true;

    for line_idx in start_line..=end_line {
        let Some(visible_idx) = line_idx.checked_sub(transcript_start) else {
            continue;
        };
        let Some(line) = visible_text.get(visible_idx) else {
            continue;
        };
        let from = if line_idx == start_line { start_col } else { 0 };
        let to = if line_idx == end_line {
            end_col.saturating_add(1)
        } else {
            width
        }
        .min(width);
        if !first {
            out.push('\n');
        }
        first = false;
        out.push_str(&slice_cells(line, from, to));
    }
    out
}

fn slice_cells(text: &str, from_col: usize, to_col: usize) -> String {
    if to_col <= from_col {
        return String::new();
    }
    let mut out = String::new();
    let mut col = 0usize;
    for ch in text.chars() {
        let width = ch.width().unwrap_or(0);
        if width == 0 {
            if col >= from_col && col <= to_col {
                out.push(ch);
            }
            continue;
        }
        if col >= to_col {
            break;
        }
        if col + width > from_col && col < to_col {
            out.push(ch);
        }
        col += width;
    }
    out
}

async fn handle_key(
    env: &mut Env<'_>,
    st: &mut TuiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    key: KeyEvent,
) -> Result<Flow, HarnessError> {
    // Ctrl+C always works (interrupt active turn, or double-tap to exit).
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        if st.active_turn.is_some() {
            interrupt_turn(env, st).await;
            st.ctrl_c_armed = None;
            return Ok(Flow::Continue);
        }
        if !matches!(st.modal, ActiveModal::None) {
            close_modal_with_cancel(st);
            return Ok(Flow::Continue);
        }
        if let Some(armed) = st.ctrl_c_armed {
            if armed.elapsed().as_secs() < 2 {
                return Ok(Flow::Break);
            }
        }
        st.ctrl_c_armed = Some(std::time::Instant::now());
        return Ok(Flow::Continue);
    }

    // Disarm Ctrl+C exit prompt on any other key.
    st.ctrl_c_armed = None;

    // If a modal is active, it owns all keyboard input.
    if !matches!(st.modal, ActiveModal::None) {
        return handle_modal_key(env, st, terminal, key).await;
    }

    // Ctrl+P opens the Command Palette.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('p') {
        st.modal = ActiveModal::CommandPalette {
            query: String::new(),
            selected: 0,
        };
        return Ok(Flow::Continue);
    }

    // Shift+Tab cycles Agent Mode (`Build` -> `Auto` -> `Plan`).
    if key.code == KeyCode::BackTab {
        apply_agent_mode(env, st, st.mode.cycle());
        return Ok(Flow::Continue);
    }

    // Ctrl+N opens a new session tab.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('n') {
        if let Err(e) = create_new_session(env, st).await {
            env.ui
                .set_transient_error(Some(format!("New session failed: {e}")));
        }
        return Ok(Flow::Continue);
    }

    // Ctrl+O toggles thinking/tool expansion (or live tool output tail while running).
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('o') {
        if env.ui.has_live_activity() {
            env.ui.toggle_live_expanded();
        } else {
            set_all_process_expanded(st, !st.process_expanded);
        }
        return Ok(Flow::Continue);
    }

    // Ctrl+T toggles todo panel collapse.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('t') {
        env.ui.toggle_todo_collapsed();
        return Ok(Flow::Continue);
    }

    // Ctrl+V pastes clipboard image (if present) or clipboard text.
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('v') {
        handle_ctrl_v(env, st);
        return Ok(Flow::Continue);
    }

    // Slash autocomplete popup intercepts navigation & Tab/Enter/Esc.
    let slash_active = !st.slash_popup.dismissed
        && !st.slash_popup.matches.is_empty()
        && st.composer.text.starts_with('/')
        && !st.composer.text.contains('\n')
        && !st.composer.text[1..].contains(' ');

    if slash_active {
        match key.code {
            KeyCode::Up => {
                if st.slash_popup.selected > 0 {
                    st.slash_popup.selected -= 1;
                } else {
                    st.slash_popup.selected = st.slash_popup.matches.len().saturating_sub(1);
                }
                return Ok(Flow::Continue);
            }
            KeyCode::Down => {
                st.slash_popup.selected =
                    (st.slash_popup.selected + 1) % st.slash_popup.matches.len();
                return Ok(Flow::Continue);
            }
            KeyCode::Tab => {
                if let Some(entry) = st.slash_popup.matches.get(st.slash_popup.selected) {
                    st.composer.set_text(format!("{} ", entry.name));
                    sync_slash_popup(env, st);
                }
                return Ok(Flow::Continue);
            }
            KeyCode::Esc => {
                st.slash_popup.dismissed = true;
                return Ok(Flow::Continue);
            }
            KeyCode::Enter
                if !key.modifiers.contains(KeyModifiers::SHIFT)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(entry) = st.slash_popup.matches.get(st.slash_popup.selected).cloned() {
                    // Commands that expect an argument complete into composer if user hasn't typed exact match.
                    let needs_arg = matches!(entry.name.as_str(), "/name" | "/goal" | "/fork");
                    if needs_arg && st.composer.text != entry.name {
                        st.composer.set_text(format!("{} ", entry.name));
                        sync_slash_popup(env, st);
                        return Ok(Flow::Continue);
                    }
                    st.composer.clear();
                    sync_slash_popup(env, st);
                    return run_slash_command(env, st, terminal, &entry.name, &entry.name).await;
                }
            }
            _ => {}
        }
    }

    // Escape clears a text selection first, then interrupts or clears the transient error.
    if key.code == KeyCode::Esc {
        if st.selection.is_some() {
            clear_selection(st);
            return Ok(Flow::Continue);
        }
        if st.active_turn.is_some() {
            interrupt_turn(env, st).await;
        } else if env.ui.transient_error().is_some() {
            env.ui.clear_transient_error();
        }
        return Ok(Flow::Continue);
    }

    // Transcript scrolling keys
    match key.code {
        KeyCode::PageUp => {
            let step = (st.last_rendered_rect.1 as usize / 2).max(4);
            st.scroll_from_bottom = (st.scroll_from_bottom + step).min(st.max_scroll);
            clear_selection(st);
            return Ok(Flow::Continue);
        }
        KeyCode::PageDown => {
            let step = (st.last_rendered_rect.1 as usize / 2).max(4);
            st.scroll_from_bottom = st.scroll_from_bottom.saturating_sub(step);
            clear_selection(st);
            return Ok(Flow::Continue);
        }
        _ => {}
    }

    // Composer editing & submission
    match key.code {
        KeyCode::Enter => {
            if key.modifiers.contains(KeyModifiers::SHIFT)
                || key.modifiers.contains(KeyModifiers::ALT)
            {
                st.composer.insert_char('\n');
                sync_slash_popup(env, st);
            } else {
                return submit_composer(env, st, terminal).await;
            }
        }
        KeyCode::Backspace => {
            if key.modifiers.contains(KeyModifiers::CONTROL)
                || key.modifiers.contains(KeyModifiers::ALT)
            {
                st.composer.delete_word_back();
            } else {
                st.composer.backspace();
            }
            sync_slash_popup(env, st);
        }
        KeyCode::Delete => {
            st.composer.delete();
            sync_slash_popup(env, st);
        }
        KeyCode::Left => st.composer.move_left(),
        KeyCode::Right => st.composer.move_right(),
        KeyCode::Home => st.composer.move_home(),
        KeyCode::End => st.composer.move_end(),
        KeyCode::Up => {
            if st.composer.is_on_first_line() {
                st.composer.history_prev();
                sync_slash_popup(env, st);
            } else {
                st.composer.move_up_line();
            }
        }
        KeyCode::Down => {
            if st.composer.is_on_last_line() {
                st.composer.history_next();
                sync_slash_popup(env, st);
            } else {
                st.composer.move_down_line();
            }
        }
        KeyCode::Char(c) => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                match c {
                    'a' => st.composer.move_home(),
                    'e' => st.composer.move_end(),
                    'w' => {
                        st.composer.delete_word_back();
                        sync_slash_popup(env, st);
                    }
                    'u' => {
                        st.composer.clear();
                        sync_slash_popup(env, st);
                    }
                    _ => {}
                }
            } else if !key.modifiers.contains(KeyModifiers::ALT) {
                st.composer.insert_char(c);
                sync_slash_popup(env, st);
            }
        }
        _ => {}
    }

    Ok(Flow::Continue)
}

fn handle_ctrl_v(env: &Env<'_>, st: &mut TuiState) {
    if let Ok(Some(img)) = crate::commands::clipboard_image::read_clipboard_image() {
        st.composer
            .attach_image(img.width, img.height, img.data_url());
        sync_slash_popup(env, st);
        return;
    }
    if let Some(text) = crate::commands::clipboard_image::read_clipboard_text() {
        st.composer.paste_text(&text);
        sync_slash_popup(env, st);
    }
}

async fn handle_modal_key(
    env: &mut Env<'_>,
    st: &mut TuiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    key: KeyEvent,
) -> Result<Flow, HarnessError> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('v') {
        if let Some(text) = crate::commands::clipboard_image::read_clipboard_text() {
            handle_paste(env, st, &text);
        }
        return Ok(Flow::Continue);
    }

    let modal = std::mem::replace(&mut st.modal, ActiveModal::None);
    match modal {
        ActiveModal::None => {}
        ActiveModal::CommandPalette {
            mut query,
            mut selected,
        } => {
            if key.code == KeyCode::Esc
                || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('p'))
            {
                return Ok(Flow::Continue);
            }
            let items = filtered_palette_items(&query);
            match key.code {
                KeyCode::Up => {
                    if !items.is_empty() {
                        selected = if selected == 0 {
                            items.len() - 1
                        } else {
                            selected - 1
                        };
                    }
                    st.modal = ActiveModal::CommandPalette { query, selected };
                }
                KeyCode::Down => {
                    if !items.is_empty() {
                        selected = (selected + 1) % items.len();
                    }
                    st.modal = ActiveModal::CommandPalette { query, selected };
                }
                KeyCode::Enter => {
                    if let Some(item) = items.get(selected.min(items.len().saturating_sub(1))) {
                        return run_palette_action(env, st, terminal, item.action.clone()).await;
                    }
                }
                KeyCode::Backspace => {
                    query.pop();
                    st.modal = ActiveModal::CommandPalette { query, selected: 0 };
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    query.push(c);
                    st.modal = ActiveModal::CommandPalette { query, selected: 0 };
                }
                _ => {
                    st.modal = ActiveModal::CommandPalette { query, selected };
                }
            }
        }
        ActiveModal::SessionPicker {
            mut query,
            sessions,
            mut selected,
        } => {
            if key.code == KeyCode::Esc {
                return Ok(Flow::Continue);
            }
            let filtered_ids: Vec<String> = filtered_sessions(&sessions, &query)
                .into_iter()
                .map(|s| s.id.clone())
                .collect();
            match key.code {
                KeyCode::Up => {
                    if !filtered_ids.is_empty() {
                        selected = if selected == 0 {
                            filtered_ids.len() - 1
                        } else {
                            selected - 1
                        };
                    }
                    st.modal = ActiveModal::SessionPicker {
                        query,
                        sessions,
                        selected,
                    };
                }
                KeyCode::Down => {
                    if !filtered_ids.is_empty() {
                        selected = (selected + 1) % filtered_ids.len();
                    }
                    st.modal = ActiveModal::SessionPicker {
                        query,
                        sessions,
                        selected,
                    };
                }
                KeyCode::Enter => {
                    if let Some(id) =
                        filtered_ids.get(selected.min(filtered_ids.len().saturating_sub(1)))
                    {
                        if let Err(e) = switch_to_session(env, st, id).await {
                            env.ui
                                .set_transient_error(Some(format!("Session switch failed: {e}")));
                        }
                    }
                }
                KeyCode::Backspace => {
                    query.pop();
                    st.modal = ActiveModal::SessionPicker {
                        query,
                        sessions,
                        selected: 0,
                    };
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    query.push(c);
                    st.modal = ActiveModal::SessionPicker {
                        query,
                        sessions,
                        selected: 0,
                    };
                }
                _ => {
                    st.modal = ActiveModal::SessionPicker {
                        query,
                        sessions,
                        selected,
                    };
                }
            }
        }
        ActiveModal::ProviderPicker {
            target,
            mut query,
            items,
            mut selected,
        } => {
            if key.code == KeyCode::Esc {
                return Ok(Flow::Continue);
            }
            let filtered_ids: Vec<String> = filtered_providers(&items, &query)
                .into_iter()
                .map(|p| p.id.clone())
                .collect();
            match key.code {
                KeyCode::Up => {
                    if !filtered_ids.is_empty() {
                        selected = if selected == 0 {
                            filtered_ids.len() - 1
                        } else {
                            selected - 1
                        };
                    }
                    st.modal = ActiveModal::ProviderPicker {
                        target,
                        query,
                        items,
                        selected,
                    };
                }
                KeyCode::Down => {
                    if !filtered_ids.is_empty() {
                        selected = (selected + 1) % filtered_ids.len();
                    }
                    st.modal = ActiveModal::ProviderPicker {
                        target,
                        query,
                        items,
                        selected,
                    };
                }
                KeyCode::Enter | KeyCode::Right => {
                    if let Some(provider_id) =
                        filtered_ids.get(selected.min(filtered_ids.len().saturating_sub(1)))
                    {
                        open_models_for_target(env, st, target.clone(), provider_id, String::new())
                            .await;
                    }
                }
                KeyCode::Backspace => {
                    query.pop();
                    st.modal = ActiveModal::ProviderPicker {
                        target,
                        query,
                        items,
                        selected: 0,
                    };
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    query.push(c);
                    st.modal = ActiveModal::ProviderPicker {
                        target,
                        query,
                        items,
                        selected: 0,
                    };
                }
                _ => {
                    st.modal = ActiveModal::ProviderPicker {
                        target,
                        query,
                        items,
                        selected,
                    };
                }
            }
        }
        ActiveModal::ModelPicker {
            target,
            provider,
            provider_display,
            mut query,
            items,
            mut selected,
        } => {
            if key.code == KeyCode::Esc {
                return Ok(Flow::Continue);
            }
            let filtered_pairs: Vec<(String, String)> = filtered_models(&items, &query)
                .into_iter()
                .map(|m| (m.provider.clone(), m.model.clone()))
                .collect();
            match key.code {
                KeyCode::Up => {
                    if !filtered_pairs.is_empty() {
                        selected = if selected == 0 {
                            filtered_pairs.len() - 1
                        } else {
                            selected - 1
                        };
                    }
                    st.modal = ActiveModal::ModelPicker {
                        target,
                        provider,
                        provider_display,
                        query,
                        items,
                        selected,
                    };
                }
                KeyCode::Down => {
                    if !filtered_pairs.is_empty() {
                        selected = (selected + 1) % filtered_pairs.len();
                    }
                    st.modal = ActiveModal::ModelPicker {
                        target,
                        provider,
                        provider_display,
                        query,
                        items,
                        selected,
                    };
                }
                KeyCode::Left => {
                    open_provider_picker_for(env, st, target.clone()).await;
                }
                KeyCode::Enter => {
                    if let Some((prov, model)) =
                        filtered_pairs.get(selected.min(filtered_pairs.len().saturating_sub(1)))
                    {
                        if matches!(target, ModelTarget::Session) {
                            // `/models` chains into the thinking-level picker: remember the
                            // chosen pair and apply both once the effort is confirmed.
                            st.pending_model_choice =
                                Some((target.clone(), prov.clone(), model.clone()));
                            open_thinking_picker(env, st).await;
                        } else if let Err(e) =
                            apply_model_target(env, st, target.clone(), prov, model).await
                        {
                            env.ui
                                .set_transient_error(Some(format!("Model switch failed: {e}")));
                        }
                    }
                }
                KeyCode::Backspace => {
                    if query.is_empty() {
                        open_provider_picker_for(env, st, target.clone()).await;
                    } else {
                        query.pop();
                        st.modal = ActiveModal::ModelPicker {
                            target,
                            provider,
                            provider_display,
                            query,
                            items,
                            selected: 0,
                        };
                    }
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    query.push(c);
                    st.modal = ActiveModal::ModelPicker {
                        target,
                        provider,
                        provider_display,
                        query,
                        items,
                        selected: 0,
                    };
                }
                _ => {
                    st.modal = ActiveModal::ModelPicker {
                        target,
                        provider,
                        provider_display,
                        query,
                        items,
                        selected,
                    };
                }
            }
        }
        ActiveModal::SelectModal {
            title,
            items,
            mut selected,
            action,
        } => {
            if key.code == KeyCode::Esc {
                return Ok(Flow::Continue);
            }
            match key.code {
                KeyCode::Up => {
                    if !items.is_empty() {
                        selected = if selected == 0 {
                            items.len() - 1
                        } else {
                            selected - 1
                        };
                    }
                    st.modal = ActiveModal::SelectModal {
                        title,
                        items,
                        selected,
                        action,
                    };
                }
                KeyCode::Down => {
                    if !items.is_empty() {
                        selected = (selected + 1) % items.len();
                    }
                    st.modal = ActiveModal::SelectModal {
                        title,
                        items,
                        selected,
                        action,
                    };
                }
                KeyCode::Enter => {
                    let choice = selected.min(items.len().saturating_sub(1));
                    super::actions::run_select_action(env, st, action, choice).await;
                }
                _ => {
                    st.modal = ActiveModal::SelectModal {
                        title,
                        items,
                        selected,
                        action,
                    };
                }
            }
        }
        ActiveModal::InputModal {
            title,
            prompt,
            mut value,
            secret,
            action,
        } => match key.code {
            KeyCode::Esc => {}
            KeyCode::Enter => {
                super::actions::run_input_action(env, st, action, value).await;
            }
            KeyCode::Backspace => {
                value.pop();
                st.modal = ActiveModal::InputModal {
                    title,
                    prompt,
                    value,
                    secret,
                    action,
                };
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                value.push(c);
                st.modal = ActiveModal::InputModal {
                    title,
                    prompt,
                    value,
                    secret,
                    action,
                };
            }
            _ => {
                st.modal = ActiveModal::InputModal {
                    title,
                    prompt,
                    value,
                    secret,
                    action,
                };
            }
        },
        ActiveModal::TextModal {
            title,
            lines,
            mut scroll,
        } => match key.code {
            KeyCode::Esc => {}
            KeyCode::Up => {
                scroll = scroll.saturating_sub(1);
                st.modal = ActiveModal::TextModal {
                    title,
                    lines,
                    scroll,
                };
            }
            KeyCode::Down => {
                scroll = (scroll + 1).min(lines.len().saturating_sub(1));
                st.modal = ActiveModal::TextModal {
                    title,
                    lines,
                    scroll,
                };
            }
            KeyCode::PageUp => {
                scroll = scroll.saturating_sub(10);
                st.modal = ActiveModal::TextModal {
                    title,
                    lines,
                    scroll,
                };
            }
            KeyCode::PageDown => {
                scroll = (scroll + 10).min(lines.len().saturating_sub(1));
                st.modal = ActiveModal::TextModal {
                    title,
                    lines,
                    scroll,
                };
            }
            KeyCode::Home => {
                st.modal = ActiveModal::TextModal {
                    title,
                    lines,
                    scroll: 0,
                };
            }
            KeyCode::End => {
                let last = lines.len().saturating_sub(1);
                st.modal = ActiveModal::TextModal {
                    title,
                    lines,
                    scroll: last,
                };
            }
            _ => {
                st.modal = ActiveModal::TextModal {
                    title,
                    lines,
                    scroll,
                };
            }
        },
        ActiveModal::ThinkingPicker {
            current,
            mut selected,
        } => {
            let options: [Option<ReasoningEffort>; 7] = [
                None,
                Some(ReasoningEffort::Minimal),
                Some(ReasoningEffort::Low),
                Some(ReasoningEffort::Medium),
                Some(ReasoningEffort::High),
                Some(ReasoningEffort::Xhigh),
                Some(ReasoningEffort::Max),
            ];
            match key.code {
                KeyCode::Esc => {
                    // Cancelling the chained picker cancels the model switch too.
                    st.pending_model_choice = None;
                }
                KeyCode::Up => {
                    selected = if selected == 0 { 6 } else { selected - 1 };
                    st.modal = ActiveModal::ThinkingPicker { current, selected };
                }
                KeyCode::Down => {
                    selected = (selected + 1) % 7;
                    st.modal = ActiveModal::ThinkingPicker { current, selected };
                }
                KeyCode::Enter => {
                    apply_thinking_selection(env, st, options[selected]).await;
                    if let Some((target, provider, model)) = st.pending_model_choice.take() {
                        if let Err(e) = apply_model_target(env, st, target, &provider, &model).await
                        {
                            env.ui
                                .set_transient_error(Some(format!("Model switch failed: {e}")));
                        }
                    }
                }
                _ => {
                    st.modal = ActiveModal::ThinkingPicker { current, selected };
                }
            }
        }
        ActiveModal::Approval {
            req,
            resp_tx,
            mut selected,
        } => match key.code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                let _ = resp_tx.send(ApprovalDecision::Deny);
            }
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Char('1') => {
                let _ = resp_tx.send(ApprovalDecision::Run);
            }
            KeyCode::Char('a') | KeyCode::Char('A') | KeyCode::Char('2') => {
                apply_agent_mode(env, st, AgentMode::Auto);
                let _ = resp_tx.send(ApprovalDecision::Run);
            }
            KeyCode::Char('3') => {
                let _ = resp_tx.send(ApprovalDecision::Deny);
            }
            KeyCode::Up => {
                selected = if selected == 0 { 2 } else { selected - 1 };
                st.modal = ActiveModal::Approval {
                    req,
                    resp_tx,
                    selected,
                };
            }
            KeyCode::Down => {
                selected = (selected + 1) % 3;
                st.modal = ActiveModal::Approval {
                    req,
                    resp_tx,
                    selected,
                };
            }
            KeyCode::Enter => match selected {
                0 => {
                    let _ = resp_tx.send(ApprovalDecision::Run);
                }
                1 => {
                    apply_agent_mode(env, st, AgentMode::Auto);
                    let _ = resp_tx.send(ApprovalDecision::Run);
                }
                _ => {
                    let _ = resp_tx.send(ApprovalDecision::Deny);
                }
            },
            _ => {
                st.modal = ActiveModal::Approval {
                    req,
                    resp_tx,
                    selected,
                };
            }
        },
        ActiveModal::Clarify {
            req,
            resp_tx,
            mut question_idx,
            mut answers,
            mut selected,
            mut custom,
            editing_custom,
        } => {
            if key.code == KeyCode::Esc {
                if editing_custom {
                    st.modal = ActiveModal::Clarify {
                        req,
                        resp_tx,
                        question_idx,
                        answers,
                        selected,
                        custom,
                        editing_custom: false,
                    };
                } else {
                    let _ = resp_tx.send(ClarifyResponse {
                        answer: None,
                        choice_index: None,
                        answers,
                    });
                }
                return Ok(Flow::Continue);
            }

            let choices_len = req
                .questions
                .get(question_idx)
                .map(|q| q.choices.len())
                .unwrap_or(0);
            let total_opts = choices_len + 1;

            if editing_custom {
                match key.code {
                    KeyCode::Backspace => {
                        custom.pop();
                        st.modal = ActiveModal::Clarify {
                            req,
                            resp_tx,
                            question_idx,
                            answers,
                            selected,
                            custom,
                            editing_custom: true,
                        };
                    }
                    KeyCode::Enter => {
                        let q_text = req.questions[question_idx].question.clone();
                        let ans = std::mem::take(&mut custom);
                        answers.push(ClarifyAnswer {
                            question: q_text,
                            answer: ans,
                            choice_index: None,
                        });
                        question_idx += 1;
                        if question_idx >= req.questions.len() {
                            let first_ans = answers.first().map(|a| a.answer.clone());
                            let first_idx = answers.first().and_then(|a| a.choice_index);
                            let _ = resp_tx.send(ClarifyResponse {
                                answer: first_ans,
                                choice_index: first_idx,
                                answers,
                            });
                        } else {
                            st.modal = ActiveModal::Clarify {
                                req,
                                resp_tx,
                                question_idx,
                                answers,
                                selected: 0,
                                custom: String::new(),
                                editing_custom: false,
                            };
                        }
                    }
                    KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                        custom.push(c);
                        st.modal = ActiveModal::Clarify {
                            req,
                            resp_tx,
                            question_idx,
                            answers,
                            selected,
                            custom,
                            editing_custom: true,
                        };
                    }
                    _ => {
                        st.modal = ActiveModal::Clarify {
                            req,
                            resp_tx,
                            question_idx,
                            answers,
                            selected,
                            custom,
                            editing_custom: true,
                        };
                    }
                }
            } else {
                match key.code {
                    KeyCode::Up => {
                        selected = if selected == 0 {
                            total_opts.saturating_sub(1)
                        } else {
                            selected - 1
                        };
                        st.modal = ActiveModal::Clarify {
                            req,
                            resp_tx,
                            question_idx,
                            answers,
                            selected,
                            custom,
                            editing_custom,
                        };
                    }
                    KeyCode::Down => {
                        selected = (selected + 1) % total_opts.max(1);
                        st.modal = ActiveModal::Clarify {
                            req,
                            resp_tx,
                            question_idx,
                            answers,
                            selected,
                            custom,
                            editing_custom,
                        };
                    }
                    KeyCode::Enter => {
                        if selected >= choices_len {
                            st.modal = ActiveModal::Clarify {
                                req,
                                resp_tx,
                                question_idx,
                                answers,
                                selected,
                                custom,
                                editing_custom: true,
                            };
                        } else {
                            let q = &req.questions[question_idx];
                            let chosen = q.choices[selected].clone();
                            answers.push(ClarifyAnswer {
                                question: q.question.clone(),
                                answer: chosen,
                                choice_index: Some(selected),
                            });
                            question_idx += 1;
                            if question_idx >= req.questions.len() {
                                let first_ans = answers.first().map(|a| a.answer.clone());
                                let first_idx = answers.first().and_then(|a| a.choice_index);
                                let _ = resp_tx.send(ClarifyResponse {
                                    answer: first_ans,
                                    choice_index: first_idx,
                                    answers,
                                });
                            } else {
                                st.modal = ActiveModal::Clarify {
                                    req,
                                    resp_tx,
                                    question_idx,
                                    answers,
                                    selected: 0,
                                    custom: String::new(),
                                    editing_custom: false,
                                };
                            }
                        }
                    }
                    _ => {
                        st.modal = ActiveModal::Clarify {
                            req,
                            resp_tx,
                            question_idx,
                            answers,
                            selected,
                            custom,
                            editing_custom,
                        };
                    }
                }
            }
        }
        ActiveModal::Sudo {
            prompt,
            resp_tx,
            mut input,
        } => match key.code {
            KeyCode::Esc => {
                let _ = resp_tx.send(None);
            }
            KeyCode::Enter => {
                let _ = resp_tx.send(Some(input));
            }
            KeyCode::Backspace => {
                input.pop();
                st.modal = ActiveModal::Sudo {
                    prompt,
                    resp_tx,
                    input,
                };
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                input.push(c);
                st.modal = ActiveModal::Sudo {
                    prompt,
                    resp_tx,
                    input,
                };
            }
            _ => {
                st.modal = ActiveModal::Sudo {
                    prompt,
                    resp_tx,
                    input,
                };
            }
        },
        ActiveModal::Help { mut scroll } => match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => {}
            KeyCode::Up => {
                scroll = scroll.saturating_sub(1);
                st.modal = ActiveModal::Help { scroll };
            }
            KeyCode::Down => {
                scroll = scroll.saturating_add(1).min(12);
                st.modal = ActiveModal::Help { scroll };
            }
            _ => {
                st.modal = ActiveModal::Help { scroll };
            }
        },
    }
    Ok(Flow::Continue)
}

fn close_modal_with_cancel(st: &mut TuiState) {
    let modal = std::mem::replace(&mut st.modal, ActiveModal::None);
    match modal {
        ActiveModal::Approval { resp_tx, .. } => {
            let _ = resp_tx.send(ApprovalDecision::Deny);
        }
        ActiveModal::Clarify {
            resp_tx, answers, ..
        } => {
            let _ = resp_tx.send(ClarifyResponse {
                answer: None,
                choice_index: None,
                answers,
            });
        }
        ActiveModal::Sudo { resp_tx, .. } => {
            let _ = resp_tx.send(None);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::{selection_text_from, slice_cells, TextSelection};

    #[test]
    fn slice_cells_uses_terminal_columns() {
        assert_eq!(slice_cells("abcdef", 1, 4), "bcd");
        assert_eq!(slice_cells("abcdef", 0, usize::MAX), "abcdef");
        assert_eq!(slice_cells("abcdef", 4, 4), "");
    }

    #[test]
    fn slice_cells_keeps_wide_glyphs_whole() {
        // `中` occupies cells 1-2, so selecting only its first cell still copies the glyph.
        assert_eq!(slice_cells("a中b", 1, 2), "中");
        assert_eq!(slice_cells("a中b", 2, 3), "中");
        assert_eq!(slice_cells("a中b", 3, 4), "b");
    }

    fn selection(anchor: (usize, usize), cursor: (usize, usize)) -> TextSelection {
        TextSelection {
            anchor,
            cursor,
            dragging: true,
        }
    }

    #[test]
    fn selection_text_includes_the_cell_under_the_cursor() {
        let lines = vec!["hello world".to_string()];
        assert_eq!(
            selection_text_from(&lines, 0, 20, selection((0, 0), (0, 4))),
            "hello"
        );
    }

    #[test]
    fn selection_text_normalizes_a_backwards_drag() {
        let lines = vec!["hello world".to_string()];
        assert_eq!(
            selection_text_from(&lines, 0, 20, selection((0, 4), (0, 0))),
            "hello"
        );
    }

    #[test]
    fn selection_text_joins_multiple_visible_lines() {
        let lines = vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()];
        // Lines 10..12 are visible; start mid-line 11 and end mid-line 12.
        assert_eq!(
            selection_text_from(&lines, 10, 20, selection((11, 1), (12, 2))),
            "eta\ngam"
        );
    }

    #[test]
    fn selection_text_skips_lines_outside_the_visible_snapshot() {
        let lines = vec!["only".to_string()];
        assert_eq!(
            selection_text_from(&lines, 5, 20, selection((4, 0), (5, 3))),
            "only"
        );
        assert_eq!(
            selection_text_from(&lines, 5, 0, selection((5, 0), (5, 3))),
            ""
        );
    }

    #[test]
    fn find_word_bounds_identifies_words_and_flags() {
        use super::find_word_bounds;
        let line = "cargo test --release /path/to/file.rs";
        assert_eq!(find_word_bounds(line, 7), (6, 10)); // "test"
        assert_eq!(find_word_bounds(line, 13), (11, 20)); // "--release"
        assert_eq!(find_word_bounds(line, 25), (21, 37)); // "/path/to/file.rs"
        assert_eq!(find_word_bounds(line, 5), (5, 6)); // whitespace
    }

    #[test]
    fn cursor_byte_from_coords_maps_lines_and_columns() {
        use super::cursor_byte_from_coords;
        let text = "hello\nworld";
        assert_eq!(cursor_byte_from_coords(text, 0, 0, 40), 0);
        assert_eq!(cursor_byte_from_coords(text, 0, 3, 40), 3);
        assert_eq!(cursor_byte_from_coords(text, 0, 10, 40), 5);
        assert_eq!(cursor_byte_from_coords(text, 1, 0, 40), 6);
        assert_eq!(cursor_byte_from_coords(text, 1, 2, 40), 8);
    }
}
