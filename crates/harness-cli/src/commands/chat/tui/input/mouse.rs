//! Mouse event handling, clicks, drag selection, and auto-scrolling for the TUI.

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use harness_core::HarnessError;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::super::super::interactive::state::{Env, Flow};
use super::super::actions::{
    create_new_session, open_session_picker, switch_to_session, sync_slash_popup,
};
use super::super::render::{
    filtered_models, filtered_palette_items, filtered_providers, filtered_sessions,
};
use super::super::state::{
    ActiveModal, LiveThinkingView, TabHitTarget, TextSelection, TranscriptClickTarget, TuiState,
};
use super::modals::close_modal_with_cancel;

pub async fn handle_mouse(
    env: &mut Env<'_>,
    st: &mut TuiState,
    mouse: MouseEvent,
) -> Result<Flow, HarnessError> {
    st.pointer = Some((mouse.column, mouse.row));
    if !matches!(st.modal, ActiveModal::None) {
        match mouse.kind {
            MouseEventKind::ScrollUp => match &mut st.modal {
                ActiveModal::CommandPalette { selected, .. }
                | ActiveModal::SessionPicker { selected, .. }
                | ActiveModal::ProviderPicker { selected, .. }
                | ActiveModal::ModelPicker { selected, .. }
                | ActiveModal::ThinkingPicker { selected, .. }
                | ActiveModal::SubagentsModal { selected, .. } => {
                    *selected = selected.saturating_sub(1);
                }
                ActiveModal::TextModal { scroll, .. }
                | ActiveModal::SubagentInspector { scroll, .. } => {
                    *scroll = scroll.saturating_sub(3);
                }
                ActiveModal::Clarify {
                    scroll,
                    follow_selection,
                    ..
                } => {
                    *scroll = scroll.saturating_sub(3);
                    *follow_selection = false;
                }
                ActiveModal::Approval {
                    reply,
                    reply_scroll,
                    diff_scroll,
                    ..
                } => {
                    if reply.is_some() {
                        *reply_scroll = reply_scroll.saturating_sub(3);
                    } else {
                        *diff_scroll = diff_scroll.saturating_sub(3);
                    }
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
                ActiveModal::SubagentsModal {
                    items, selected, ..
                } => {
                    *selected = (*selected + 1).min(items.len().saturating_sub(1));
                }
                ActiveModal::TextModal { lines, scroll, .. } => {
                    let max_s = lines.len().saturating_sub(1);
                    *scroll = (*scroll + 3).min(max_s);
                }
                ActiveModal::SubagentInspector { scroll, .. } => {
                    *scroll += 3;
                }
                ActiveModal::Clarify {
                    scroll,
                    follow_selection,
                    ..
                } => {
                    *scroll = scroll.saturating_add(3);
                    *follow_selection = false;
                }
                ActiveModal::Approval {
                    reply,
                    reply_scroll,
                    diff_scroll,
                    ..
                } => {
                    if reply.is_some() {
                        *reply_scroll += 3;
                    } else {
                        *diff_scroll += 3;
                    }
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
            st.scroll_transcript_up(3);
            clear_selection(st);
        }
        MouseEventKind::ScrollDown => {
            st.scroll_transcript_down(3);
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

pub fn clear_selection(st: &mut TuiState) {
    st.selection = None;
    st.pending_transcript_click = None;
}

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
                dragging: true,
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

        st.selection = None;
        st.pending_transcript_click = hit;
    } else {
        st.selection = None;
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
    let Some(mut selection) = st.selection else {
        return;
    };
    if !selection.dragging {
        return;
    }

    if mouse.row < ty && st.scroll_from_bottom < st.max_scroll {
        st.scroll_transcript_up(1);
        st.transcript_start = st.transcript_start.saturating_sub(1);
    } else if mouse.row >= ty.saturating_add(th) && st.scroll_from_bottom > 0 {
        st.scroll_transcript_down(1);
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
    st.selection = Some(selection);
}

pub async fn finish_left_up(env: &mut Env<'_>, st: &mut TuiState) {
    let Some(selection) = st.selection else {
        if let Some(target) = st.pending_transcript_click.take() {
            apply_transcript_click(env, st, target).await;
        }
        return;
    };

    if !selection.dragging {
        return;
    }

    if selection.is_range() || st.click_count >= 2 {
        copy_selection_to_clipboard(env, st);
        st.selection = Some(TextSelection {
            dragging: false,
            ..selection
        });
        st.pending_transcript_click = None;
    } else {
        st.selection = None;
        if let Some(target) = st.pending_transcript_click.take() {
            apply_transcript_click(env, st, target).await;
        }
    }
}

pub fn copy_selection_to_clipboard(env: &Env<'_>, st: &mut TuiState) {
    let Some(selection) = st.selection else {
        return;
    };
    if selection.is_range() || st.click_count >= 2 {
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

pub async fn apply_transcript_click(
    env: &Env<'_>,
    st: &mut TuiState,
    target: TranscriptClickTarget,
) {
    match target {
        TranscriptClickTarget::OpenSubagents => {
            super::super::actions::open_subagents_list(env, st).await;
        }
        TranscriptClickTarget::OpenSubagent(id) => {
            super::super::actions::open_subagent_inspector(env, st, &id).await;
        }
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
            st.jump_to_latest();
            clear_selection(st);
        }
        TranscriptClickTarget::OpenCredentialPriority(provider) => {
            super::super::actions::open_connect_priority_credential(env, st, provider).await;
        }
    }
}

pub fn selection_text(st: &TuiState, selection: TextSelection) -> String {
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
                            Some(super::super::render::line_plain_text(
                                &st.settled_cache.lines[line_idx],
                            ))
                        } else {
                            None
                        }
                    })
            } else if line_idx < st.settled_cache.lines.len() {
                Some(super::super::render::line_plain_text(
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

pub fn find_word_bounds(text: &str, target_col: usize) -> (usize, usize) {
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

pub fn cursor_byte_from_coords(
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
            byte_pos += 1;
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
pub fn selection_text_from(
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

pub fn slice_cells(text: &str, from_col: usize, to_col: usize) -> String {
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
