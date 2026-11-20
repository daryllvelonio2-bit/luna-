//! Full-screen `ratatui` renderer matching OpenCode's TUI layout and visual design.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use harness_core::ReasoningEffort;

use super::super::render::{
    format_elapsed_secs, reply_for_display, shorten_path, shorten_paths_in_text, truncate_to_cells,
    SPINNER_FRAMES,
};
use super::super::ui::{TuiLiveSnapshot, TuiPtyPane};
use super::markdown::{
    clean_terminal_text, parse_terminal_line, render_markdown, strip_ansi, wrap_plain_text,
    wrap_spans,
};
use crate::commands::chat::tui::state::{
    ActiveModal, ComposerHitBox, ImagePreview, LiveThinkingView, ModelPickerItem, PaletteAction,
    PaletteItem, ProviderPickerItem, SubagentUiEntry, TabHitBox, TabHitTarget,
    TranscriptClickTarget, TranscriptHitBox, TuiState, TurnMeta,
};
use crate::commands::chat::tui::theme::{
    accent_coral, bg_approval, bg_approval_selected, bg_canvas, bg_code, bg_elevated, bg_selected,
    bg_selection, bg_surface, border_subtle, cyan_link, friendly_model_name,
    friendly_provider_name, green_ok, humanize_tokens, logo_gray, mode_accent, note_accent,
    red_err, scanner_color, text_dim, text_muted, text_primary, yellow_warn, LUNA_LOGO_LEFT,
    LUNA_LOGO_RIGHT,
};
use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureLandingItem {
    pub enabled: bool,
    pub disabled_days_left: Option<u64>,
}

impl Default for FeatureLandingItem {
    fn default() -> Self {
        Self {
            enabled: true,
            disabled_days_left: None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LandingFeaturesStatus {
    pub mcp: FeatureLandingItem,
    pub skills: FeatureLandingItem,
    pub memory: FeatureLandingItem,
    pub notes: FeatureLandingItem,
    pub memory_mode: harness_core::MemoryMode,
}

pub struct FooterInfo<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub session_id: &'a str,
    pub in_flight_tools: &'a [crate::commands::chat::render::TurnProcessItem],
    pub features: LandingFeaturesStatus,
}

/// Minimum transcript size required before the live pty mirror is placed.
///
/// The pane is an overlay in the transcript's bottom-right corner, so it hides text rather than
/// shrinking the conversation. Below this size it would hide most of the visible lines, and the
/// small screen is not worth the cost.
const PTY_PANE_MIN_TRANSCRIPT_WIDTH: u16 = 56;
const PTY_PANE_MIN_TRANSCRIPT_HEIGHT: u16 = 16;
/// The pane stays a monitor, not a workspace: one third of the transcript, within these bounds.
const PTY_PANE_MIN_WIDTH: u16 = 30;
const PTY_PANE_MAX_WIDTH: u16 = 38;
/// Rows the pty pane occupies, borders included.
const PTY_PANE_ROWS: u16 = 12;

/// Where the live pty mirror sits: the bottom-right corner of the visible transcript.
///
/// `None` when the transcript is too small to spare the corner, which keeps a narrow window's
/// conversation readable instead of covering it with a terminal.
pub(crate) fn pty_pane_rect(transcript: Rect) -> Option<Rect> {
    if transcript.width < PTY_PANE_MIN_TRANSCRIPT_WIDTH
        || transcript.height < PTY_PANE_MIN_TRANSCRIPT_HEIGHT
    {
        return None;
    }
    let width = (transcript.width / 3)
        .clamp(PTY_PANE_MIN_WIDTH, PTY_PANE_MAX_WIDTH)
        .min(transcript.width.saturating_sub(2));
    let height = PTY_PANE_ROWS.min(transcript.height.saturating_sub(2));
    if width < 20 || height < 5 {
        return None;
    }
    Some(Rect {
        x: transcript.x + transcript.width - width,
        y: transcript.y + transcript.height - height,
        width,
        height,
    })
}

/// Draw one complete TUI frame.
pub fn draw(frame: &mut Frame, st: &mut TuiState, snap: &TuiLiveSnapshot, info: &FooterInfo<'_>) {
    let area = frame.area();
    st.last_rendered_rect = (area.width, area.height);

    // Reset every cell first. `Block` only applies a style, so without this a shorter line leaves
    // glyphs from the previous frame behind and the transcript looks like two frames are
    // overlaid (the classic "stable system" ghosting).
    frame.render_widget(Clear, area);
    // Paint the entire frame canvas `#0e0e0f`.
    frame.render_widget(
        Block::default().style(Style::default().bg(bg_canvas())),
        area,
    );

    if area.width < 20 || area.height < 8 {
        return;
    }

    // Row 0: OpenCode-style top session tab bar.
    let tab_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width,
        height: 1,
    };
    render_tab_bar(frame, tab_area, st, info.session_id);

    let body_area = Rect {
        x: area.x,
        y: area.y + 1,
        width: area.width,
        height: area.height.saturating_sub(1),
    };

    let now = std::time::Instant::now();
    let queue_empty = st.queue.is_empty();
    let turn_active = st.active_turn.is_some();
    let has_visible_notices = st
        .system_notices
        .iter()
        .any(|n| !n.is_stale(now, queue_empty, turn_active));

    let show_landing = st.turns.is_empty()
        && st.active_turn.is_none()
        && !has_visible_notices
        && snap.transient_error.is_none();

    let pty_pane_open = snap.pty_pane.is_some();

    let composer_rect = if show_landing {
        st.transcript_hit_boxes.clear();
        st.transcript_area = (0, 0, 0, 0);
        st.transcript_start = 0;
        st.transcript_visible_text.clear();
        st.selection = None;
        st.pending_transcript_click = None;
        render_landing(frame, body_area, st, info, snap)
    } else {
        render_conversation(frame, body_area, st, snap, info, pty_pane_open)
    };

    if let Some(pane) = &snap.pty_pane {
        let transcript = Rect {
            x: st.transcript_area.0,
            y: st.transcript_area.1,
            width: st.transcript_area.2,
            height: st.transcript_area.3,
        };
        if let Some(rect) = pty_pane_rect(transcript) {
            // The pane covers this corner: a click there must not toggle the text underneath.
            st.transcript_hit_boxes
                .retain(|hit| hit.y < rect.y || hit.y >= rect.y + rect.height);
            render_pty_pane(frame, rect, pane);
        }
    }

    // Slash autocomplete popup sits directly above the composer when typing `/...`.
    if matches!(st.modal, ActiveModal::None)
        && !st.slash_popup.dismissed
        && !st.slash_popup.matches.is_empty()
        && st.composer.text.starts_with('/')
        && !st.composer.text.contains('\n')
        && !st.composer.text[1..].contains(' ')
    {
        render_slash_popup(frame, area, composer_rect, st);
    }

    // Active modal overlay renders on top of everything.
    render_modal(frame, area, st, info);

    // Safety net: Sanitize all buffer cells before crossterm renders them.
    // Any unexpanded tab or control character written to a terminal emulator
    // causes cursor jumps or unexpected line wrapping that desyncs the TUI viewport.
    let buf = frame.buffer_mut();
    for cell in buf.content.iter_mut() {
        if cell.symbol().contains('\t') || cell.symbol().chars().any(|c| c.is_control()) {
            cell.set_symbol(" ");
        }
    }
}

pub(crate) fn strip_terminal_indent(lines: &[String]) -> Vec<String> {
    let non_empty: Vec<&str> = lines
        .iter()
        .map(|s| s.as_str())
        .filter(|s| !s.trim().is_empty())
        .collect();
    if non_empty.is_empty() {
        return lines.to_vec();
    }
    let mut indents: Vec<usize> = non_empty
        .iter()
        .map(|s| s.len().saturating_sub(s.trim_start().len()))
        .collect();
    let min_indent = indents.iter().copied().min().unwrap_or(0);
    if min_indent >= 2 {
        return lines
            .iter()
            .map(|s| {
                if s.len() >= min_indent {
                    s[min_indent..].to_string()
                } else {
                    s.trim_start().to_string()
                }
            })
            .collect();
    }
    indents.sort_unstable();
    let cutoff = indents[indents.len() / 3];
    if cutoff >= 4 {
        return lines
            .iter()
            .map(|s| {
                let lead = s.len().saturating_sub(s.trim_start().len());
                if lead >= cutoff {
                    s[cutoff..].to_string()
                } else {
                    s.clone()
                }
            })
            .collect();
    }
    lines.to_vec()
}

/// The live pty mirror: the last screen the agent's pty session reported.
///
/// The session survives the call that opened it, so this pane is drawn from its own snapshot, not
/// from the running tool. Escape sequences and `\r` overwrites are already resolved by the pty
/// bridge, which is what makes a TUI read like a screen instead of raw control bytes.
pub(crate) fn render_pty_pane(frame: &mut Frame, rect: Rect, pane: &TuiPtyPane) {
    frame.render_widget(Clear, rect);

    let title = format!(
        " pty · {} ",
        truncate_to_cells(&pane.title, rect.width.saturating_sub(12) as usize)
    );
    let status = if pane.running {
        " running ".to_string()
    } else if pane.idle_secs >= 60 {
        format!(" idle {} ", format_elapsed_secs(pane.idle_secs))
    } else {
        " idle ".to_string()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_subtle()).bg(bg_surface()))
        .style(Style::default().bg(bg_surface()))
        .title(Span::styled(
            title,
            Style::default()
                .fg(accent_coral())
                .bg(bg_surface())
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(
            Line::from(Span::styled(
                status,
                Style::default().fg(text_dim()).bg(bg_surface()),
            ))
            .right_aligned(),
        );
    let inner = block.inner(rect);
    frame.render_widget(block, rect);

    let width = inner.width as usize;
    let avail = inner.height as usize;
    if width == 0 || avail == 0 {
        return;
    }

    let lines = &pane.lines;
    let mut rows: Vec<Line<'static>> = Vec::with_capacity(avail);
    if lines.is_empty() {
        rows.push(parse_terminal_line(
            if pane.running {
                "waiting for screen…"
            } else {
                "no screen captured"
            },
            width,
        ));
    } else {
        let slice = if lines.len() > avail {
            let shown = avail.saturating_sub(1).max(1);
            let start = lines.len().saturating_sub(shown);
            rows.push(parse_terminal_line(
                &format!("… {start} earlier lines"),
                width,
            ));
            &lines[start..]
        } else {
            for _ in 0..avail.saturating_sub(lines.len()) {
                rows.push(parse_terminal_line("", width));
            }
            lines.as_slice()
        };
        for line in strip_terminal_indent(slice) {
            rows.push(parse_terminal_line(&line, width));
        }
    }

    frame.render_widget(
        Paragraph::new(rows).style(Style::default().bg(bg_surface())),
        inner,
    );
}

pub(crate) fn render_tab_bar(
    frame: &mut Frame,
    area: Rect,
    st: &mut TuiState,
    active_session_id: &str,
) {
    st.hit_boxes.clear();

    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut cursor_x = area.x;

    // `<N` session count indicator on far left.
    let count_label = format!(" <{} ", st.total_sessions_count.max(st.tabs.len()));
    let count_w = count_label.width() as u16;
    st.hit_boxes.push(TabHitBox {
        x_start: cursor_x,
        x_end: cursor_x + count_w,
        target: TabHitTarget::OverflowButton,
    });
    spans.push(Span::styled(
        count_label,
        Style::default().fg(text_muted()).bg(bg_canvas()),
    ));
    cursor_x += count_w;

    // Leave 2 spaces before tabs.
    spans.push(Span::styled("  ", Style::default().bg(bg_canvas())));
    cursor_x += 2;

    // Determine how many trailing tabs fit within `area.width - 6`.
    let right_reserve: u16 = 4; // room for ` + `
    let max_tabs_x = area.x + area.width.saturating_sub(right_reserve);

    // Keep the active tab visible by taking a trailing window of tabs that includes it.
    let visible_tabs: Vec<(usize, &crate::commands::chat::tui::state::SessionTab)> =
        st.tabs.iter().enumerate().collect();
    let active_pos = visible_tabs
        .iter()
        .position(|(_, t)| t.id == active_session_id)
        .unwrap_or(visible_tabs.len().saturating_sub(1));

    let tab_is_busy = |tab_id: &str| -> bool {
        (tab_id == active_session_id && st.active_turn.is_some())
            || st
                .background_sessions
                .get(tab_id)
                .is_some_and(|s| s.active_turn.is_some())
    };

    // Estimate widths and drop earliest tabs if overflowing.
    let tab_display_width =
        |tab: &crate::commands::chat::tui::state::SessionTab, is_active: bool| -> u16 {
            let truncated = truncate_str(&tab.title, 18);
            let busy_w = if tab_is_busy(&tab.id) { 2 } else { 0 };
            if is_active {
                (truncated.width() + busy_w + 4) as u16
            } else {
                (truncated.width() + busy_w + 3) as u16
            }
        };

    let mut start_idx = 0usize;
    loop {
        let total_w: u16 = visible_tabs[start_idx..]
            .iter()
            .map(|(_, t)| tab_display_width(t, t.id == active_session_id))
            .sum();
        if cursor_x + total_w <= max_tabs_x || start_idx >= active_pos {
            break;
        }
        start_idx += 1;
    }

    let spinner = SPINNER_FRAMES[st.spinner_tick % SPINNER_FRAMES.len()];
    let slice = &visible_tabs[start_idx..];
    let slice_len = slice.len();
    for (rel_idx, (_, tab)) in slice.iter().enumerate() {
        let is_active = tab.id == active_session_id;
        let busy = tab_is_busy(&tab.id);
        let truncated = truncate_str(&tab.title, 18);
        let w = tab_display_width(tab, is_active);
        if cursor_x + w > max_tabs_x {
            break;
        }

        let style = if is_active {
            Style::default()
                .fg(text_primary())
                .bg(bg_surface())
                .add_modifier(Modifier::BOLD)
        } else if rel_idx == 0 && slice_len > 3 {
            Style::default().fg(text_dim()).bg(bg_canvas())
        } else {
            Style::default().fg(text_muted()).bg(bg_canvas())
        };
        let bg = if is_active { bg_surface() } else { bg_canvas() };

        st.hit_boxes.push(TabHitBox {
            x_start: cursor_x,
            x_end: cursor_x + w,
            target: TabHitTarget::Session(tab.id.clone()),
        });
        let lead = if is_active { "  " } else { " " };
        spans.push(Span::styled(lead, style));
        if busy {
            spans.push(Span::styled(
                format!("{spinner} "),
                Style::default().fg(accent_coral()).bg(bg),
            ));
        }
        spans.push(Span::styled(format!("{truncated}  "), style));
        cursor_x += w;
    }

    // `+` button right after the tabs (just like Screenshot 2).
    if cursor_x + 3 <= area.x + area.width {
        let plus_label = " + ";
        let pw = plus_label.width() as u16;
        st.hit_boxes.push(TabHitBox {
            x_start: cursor_x,
            x_end: cursor_x + pw,
            target: TabHitTarget::NewSession,
        });
        spans.push(Span::styled(
            plus_label,
            Style::default().fg(text_muted()).bg(bg_canvas()),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub(crate) fn render_landing(
    frame: &mut Frame,
    area: Rect,
    st: &mut TuiState,
    info: &FooterInfo<'_>,
    snap: &TuiLiveSnapshot,
) -> Rect {
    let box_width = area
        .width
        .saturating_sub(12)
        .clamp(40, 84)
        .min(area.width.saturating_sub(2));
    let box_x = area.x + (area.width.saturating_sub(box_width)) / 2;

    let inner_text_w = box_width.saturating_sub(5) as usize;
    let wrapped_input = wrap_composer_lines(&st.composer.text, st.composer.cursor, inner_text_w);
    let input_rows = wrapped_input.lines.len().clamp(1, 5) as u16;
    let preview_h = composer_preview_height(st, box_width.saturating_sub(3));
    let extra_footer = u16::from(has_antigravity_footer(snap));
    // Card height: 1 top pad + preview strip + 1 gap + input_rows + 1 blank spacer + footer (+ optional antigravity row).
    let card_h = input_rows + 3 + preview_h + u16::from(preview_h > 0) + extra_footer;

    // Optional chrome strips above composer
    let chrome_lines = build_chrome_lines(st, snap, box_width as usize);
    let chrome_h = chrome_lines.len().min(4) as u16;

    // Total vertical lockup: logo (3) + gap (2) + optional update (2) + chrome + card + hint (1) + gap (1) + status (2)
    let status_rows: u16 = if area.width < 58 { 3 } else { 2 };
    let update_rows: u16 = if st.update_notice.is_some() { 2 } else { 0 };
    let total_h = 3 + 2 + update_rows + chrome_h + card_h + 1 + 1 + status_rows;
    let top_y = area.y + area.height.saturating_sub(total_h + 1) / 2;

    // 1. Centered `luna` block logo
    let logo_w = (LUNA_LOGO_LEFT[0].width() + LUNA_LOGO_RIGHT[0].width()) as u16;
    let logo_x = area.x + area.width.saturating_sub(logo_w) / 2;
    for (row_idx, (left, right)) in LUNA_LOGO_LEFT
        .iter()
        .zip(LUNA_LOGO_RIGHT.iter())
        .enumerate()
    {
        let y = top_y + row_idx as u16;
        if y >= area.y + area.height {
            break;
        }
        let line = Line::from(vec![
            Span::styled(*left, Style::default().fg(logo_gray()).bg(bg_canvas())),
            Span::styled(
                *right,
                Style::default()
                    .fg(text_primary())
                    .bg(bg_canvas())
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(line),
            Rect {
                x: logo_x,
                y,
                width: logo_w.min(area.width.saturating_sub(logo_x - area.x)),
                height: 1,
            },
        );
    }

    let mut cur_y = top_y + 4;

    // 2. Optional update notice (`/update to install vX.Y.Z`)
    if let Some(ref version_label) = st.update_notice {
        let msg = Line::from(vec![
            Span::styled(
                "/update",
                Style::default().fg(accent_coral()).bg(bg_canvas()),
            ),
            Span::styled(
                format!(" to install {version_label}"),
                Style::default().fg(text_muted()).bg(bg_canvas()),
            ),
        ]);
        let msg_w = (7 + 12 + version_label.width()) as u16;
        let msg_x = area.x + area.width.saturating_sub(msg_w) / 2;
        if cur_y < area.y + area.height {
            frame.render_widget(
                Paragraph::new(msg),
                Rect {
                    x: msg_x,
                    y: cur_y,
                    width: msg_w.min(area.width),
                    height: 1,
                },
            );
        }
        cur_y += 2;
    } else {
        cur_y += 1;
    }

    if chrome_h > 0 {
        let chrome_rect = Rect {
            x: box_x,
            y: cur_y,
            width: box_width,
            height: chrome_h,
        };
        let take_from = chrome_lines.len().saturating_sub(chrome_h as usize);
        let visible_chrome = &chrome_lines[take_from..];
        for (i, line) in visible_chrome.iter().enumerate() {
            if snap.delegate_count > 0 && line.spans.iter().any(|s| s.content.contains("Subagent"))
            {
                st.transcript_hit_boxes.push(TranscriptHitBox {
                    y: cur_y + i as u16,
                    x_start: box_x,
                    x_end: box_x + box_width,
                    target: TranscriptClickTarget::OpenSubagents,
                });
            }
        }
        frame.render_widget(Paragraph::new(visible_chrome.to_vec()), chrome_rect);
        cur_y += chrome_h;
    }

    // 3. Centered composer card
    let composer_rect = Rect {
        x: box_x,
        y: cur_y,
        width: box_width,
        height: card_h.min(area.y + area.height.saturating_sub(cur_y + 2)),
    };
    render_composer_card(frame, composer_rect, st, info, snap, &wrapped_input, true);

    // 4. Hint row right below composer card
    let hint_y = composer_rect.y + composer_rect.height;
    if hint_y < area.y + area.height {
        render_composer_hint_row(
            frame,
            Rect {
                x: box_x,
                y: hint_y,
                width: box_width,
                height: 1,
            },
            st,
            Some(snap),
            info.in_flight_tools,
            None,
            true,
        );
    }

    // 5. Feature & memory-saver status strip below hint row
    let ver_y = area.y + area.height.saturating_sub(1);
    let status_lines = build_landing_status_lines(&info.features, area.width as usize);
    for (idx, (line, line_w)) in status_lines.into_iter().enumerate() {
        let sy = hint_y + 2 + idx as u16;
        if sy >= ver_y {
            break;
        }
        let draw_w = (line_w as u16).min(area.width);
        let sx = area.x + area.width.saturating_sub(draw_w) / 2;
        frame.render_widget(
            Paragraph::new(line),
            Rect {
                x: sx,
                y: sy,
                width: draw_w,
                height: 1,
            },
        );
    }

    // 6. Bottom-right version tag (`0.1.8`)
    let ver_w = (st.version.width() + 2) as u16;
    if ver_y > hint_y && area.width > ver_w {
        let ver_x = area.x + area.width - ver_w;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{} ", st.version),
                Style::default().fg(text_muted()).bg(bg_canvas()),
            ))),
            Rect {
                x: ver_x,
                y: ver_y,
                width: ver_w,
                height: 1,
            },
        );
    }

    composer_rect
}

pub(crate) fn feature_status_spans(
    name: &str,
    item: FeatureLandingItem,
) -> (Vec<Span<'static>>, usize) {
    if item.enabled {
        let text_len = 2 + name.width() + 3;
        (
            vec![
                Span::styled("● ", Style::default().fg(green_ok()).bg(bg_canvas())),
                Span::styled(
                    name.to_string(),
                    Style::default().fg(text_primary()).bg(bg_canvas()),
                ),
                Span::styled(" on", Style::default().fg(text_muted()).bg(bg_canvas())),
            ],
            text_len,
        )
    } else {
        let suffix = match item.disabled_days_left {
            Some(days) => format!(" off ({days}d)"),
            None => " off".to_string(),
        };
        let text_len = 2 + name.width() + suffix.width();
        (
            vec![
                Span::styled("○ ", Style::default().fg(text_dim()).bg(bg_canvas())),
                Span::styled(
                    name.to_string(),
                    Style::default().fg(text_dim()).bg(bg_canvas()),
                ),
                Span::styled(suffix, Style::default().fg(text_dim()).bg(bg_canvas())),
            ],
            text_len,
        )
    }
}

pub(crate) fn join_feature_spans(items: &[(&str, FeatureLandingItem)]) -> (Line<'static>, usize) {
    let mut spans = Vec::new();
    let mut width = 0usize;
    for (idx, (name, item)) in items.iter().enumerate() {
        if idx > 0 {
            spans.push(Span::styled(
                " · ",
                Style::default().fg(text_dim()).bg(bg_canvas()),
            ));
            width += 3;
        }
        let (part, part_w) = feature_status_spans(name, *item);
        spans.extend(part);
        width += part_w;
    }
    (Line::from(spans), width)
}

pub(crate) fn build_landing_status_lines(
    features: &LandingFeaturesStatus,
    avail_width: usize,
) -> Vec<(Line<'static>, usize)> {
    let entries = [
        ("mcp", features.mcp),
        ("skills", features.skills),
        ("memory", features.memory),
        ("notes", features.notes),
    ];
    let mut lines = Vec::new();
    if avail_width < 58 {
        lines.push(join_feature_spans(&entries[..2]));
        lines.push(join_feature_spans(&entries[2..]));
    } else {
        lines.push(join_feature_spans(&entries));
    }

    let (saver_label, saver_color) = match features.memory_mode {
        harness_core::MemoryMode::Session => ("session", text_primary()),
        harness_core::MemoryMode::Background => ("background saver", cyan_link()),
    };
    let saver_w = "saver: ".width() + saver_label.width();
    lines.push((
        Line::from(vec![
            Span::styled("saver: ", Style::default().fg(text_muted()).bg(bg_canvas())),
            Span::styled(
                saver_label,
                Style::default().fg(saver_color).bg(bg_canvas()),
            ),
        ]),
        saver_w,
    ));
    lines
}

pub(crate) fn render_conversation(
    frame: &mut Frame,
    area: Rect,
    st: &mut TuiState,
    snap: &TuiLiveSnapshot,
    info: &FooterInfo<'_>,
    pty_pane_open: bool,
) -> Rect {
    let margin = if area.width >= 48 { 2 } else { 1 };
    let content_x = area.x + margin;
    let content_w = area.width.saturating_sub(margin * 2).max(16);

    // Compute composer height
    let inner_text_w = content_w.saturating_sub(5) as usize;
    let wrapped_input = wrap_composer_lines(&st.composer.text, st.composer.cursor, inner_text_w);
    let input_rows = wrapped_input.lines.len().clamp(1, 6) as u16;
    let preview_h = composer_preview_height(st, content_w.saturating_sub(3));
    let extra_footer = u16::from(has_antigravity_footer(snap));
    let card_h = input_rows + 3 + preview_h + u16::from(preview_h > 0) + extra_footer;

    // Optional chrome strips above composer (todo, delegate, memory notices, queue, compaction, error)
    let chrome_lines = build_chrome_lines(st, snap, content_w as usize);
    let chrome_h = chrome_lines.len().min(8) as u16;

    let bottom_total = chrome_h + card_h + 1; // +1 for bottom status hint row
    let transcript_h = area.height.saturating_sub(bottom_total + 1).max(3);

    let transcript_rect = Rect {
        x: content_x,
        y: area.y + 1,
        width: content_w,
        height: transcript_h,
    };

    let card_inner_w = (content_w as usize).saturating_sub(4).max(12);

    // Build/update settled transcript cache and active turn lines
    let (notice_lines, active_lines, active_hit_targets) = build_active_and_notice_lines(
        st,
        snap,
        info,
        content_w as usize,
        card_inner_w,
        pty_pane_open,
    );
    ensure_settled_transcript(st, info, content_w as usize, card_inner_w);

    let settled_len = st.settled_cache.lines.len();
    let notice_len = notice_lines.len();
    let active_len = active_lines.len();
    let total_lines = settled_len + notice_len + active_len;

    // While scrolled up, pin the top visible transcript line (`scroll_anchor_top`) so streaming
    // output, tool updates, chrome height changes, or turn completion at the bottom never push
    // what the user is reading out of view.
    let full_view_h = transcript_rect.height as usize;
    let badge_room = full_view_h > 1 && transcript_rect.width > 18;
    let bottom_max_scroll = total_lines.saturating_sub(full_view_h);
    let badge_view_h = if badge_room {
        full_view_h - 1
    } else {
        full_view_h
    };
    let badge_max_scroll = total_lines.saturating_sub(badge_view_h);

    if st.scroll_anchor_top.is_none() && st.scroll_from_bottom > 0 {
        st.scroll_anchor_top = Some(badge_max_scroll.saturating_sub(st.scroll_from_bottom));
    }

    let (view_h, start, show_badge) = if let Some(anchor_top) = st.scroll_anchor_top {
        if bottom_max_scroll == 0 || anchor_top >= bottom_max_scroll {
            st.jump_to_latest();
            st.max_scroll = bottom_max_scroll;
            (full_view_h, bottom_max_scroll, false)
        } else {
            let start = anchor_top.min(badge_max_scroll);
            st.scroll_anchor_top = Some(start);
            st.max_scroll = badge_max_scroll;
            st.scroll_from_bottom = badge_max_scroll.saturating_sub(start).max(1);
            (badge_view_h, start, badge_room)
        }
    } else {
        st.max_scroll = bottom_max_scroll;
        st.scroll_from_bottom = 0;
        (full_view_h, bottom_max_scroll, false)
    };
    let end = (start + view_h).min(total_lines);

    st.transcript_area = (
        transcript_rect.x,
        transcript_rect.y,
        transcript_rect.width,
        view_h as u16,
    );
    st.transcript_start = start;

    st.transcript_hit_boxes.clear();
    for (line_idx, target) in &st.settled_cache.hit_targets {
        if *line_idx >= start && *line_idx < end {
            let screen_y = transcript_rect.y + (*line_idx - start) as u16;
            st.transcript_hit_boxes.push(TranscriptHitBox {
                y: screen_y,
                x_start: transcript_rect.x,
                x_end: transcript_rect.x + transcript_rect.width,
                target: target.clone(),
            });
        }
    }
    let active_base = settled_len + notice_len;
    for (rel_idx, target) in active_hit_targets {
        let line_idx = active_base + rel_idx;
        if line_idx >= start && line_idx < end {
            let screen_y = transcript_rect.y + (line_idx - start) as u16;
            st.transcript_hit_boxes.push(TranscriptHitBox {
                y: screen_y,
                x_start: transcript_rect.x,
                x_end: transcript_rect.x + transcript_rect.width,
                target,
            });
        }
    }

    let selection_bounds = st.selection.map(|sel| sel.bounds());
    let selected = |line: usize, col: usize| {
        selection_bounds.is_some_and(|(start, end)| (line, col) >= start && (line, col) <= end)
    };
    let mut visible_slice: Vec<Line<'static>> = Vec::with_capacity(end.saturating_sub(start));
    let mut visible_text: Vec<String> = Vec::with_capacity(end.saturating_sub(start));
    for i in start..end {
        let source = if i < settled_len {
            &st.settled_cache.lines[i]
        } else if i < settled_len + notice_len {
            &notice_lines[i - settled_len]
        } else {
            &active_lines[i - settled_len - notice_len]
        };
        visible_text.push(line_plain_text(source));
        let mut line = source.clone();
        if let Some(((sel_start_line, sel_start_col), (sel_end_line, sel_end_col))) =
            selection_bounds
        {
            if i >= sel_start_line && i <= sel_end_line {
                let from = if i == sel_start_line {
                    sel_start_col
                } else {
                    0
                };
                let to = if i == sel_end_line {
                    sel_end_col.saturating_add(1)
                } else {
                    usize::MAX
                };
                line = highlight_line_selection(line, from, to);
            }
        }
        visible_slice.push(line);
    }
    st.transcript_visible_text = visible_text;

    // Fill the blank cells after a selected line so a block drag visibly reaches the pointer's
    // row instead of highlighting only the text up to that line's right margin.
    if selection_bounds.is_some() {
        for i in start..end {
            let row = transcript_rect.y + (i - start) as u16;
            let line_w = visible_slice[i - start]
                .spans
                .iter()
                .map(|s| s.content.width())
                .sum::<usize>()
                .min(transcript_rect.width as usize);
            if !selected(i, line_w) {
                continue;
            }
            for col in line_w..transcript_rect.width as usize {
                if !selected(i, col) {
                    break;
                }
                let Some(cell) = frame
                    .buffer_mut()
                    .cell_mut((transcript_rect.x + col as u16, row))
                else {
                    continue;
                };
                cell.set_style(Style::default().bg(bg_selection()));
            }
        }
    }

    frame.render_widget(Paragraph::new(visible_slice), transcript_rect);

    // Scroll-to-latest badge, on its own row just below the transcript when scrolled up.
    if show_badge {
        let badge = " Jump to latest ↓ ";
        let bw = badge.width() as u16;
        if transcript_rect.width > bw {
            let badge_x = transcript_rect.x + transcript_rect.width - bw;
            let badge_y = transcript_rect.y + view_h as u16;
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    badge,
                    Style::default().fg(text_primary()).bg(bg_canvas()),
                ))),
                Rect {
                    x: badge_x,
                    y: badge_y,
                    width: bw,
                    height: 1,
                },
            );
            st.transcript_hit_boxes.push(TranscriptHitBox {
                y: badge_y,
                x_start: badge_x,
                x_end: badge_x + bw,
                target: TranscriptClickTarget::JumpToLatest,
            });
        }
    }

    let mut cur_y = transcript_rect.y + transcript_rect.height;

    if chrome_h > 0 {
        let take_from = chrome_lines.len().saturating_sub(chrome_h as usize);
        let visible_chrome = &chrome_lines[take_from..];
        for (i, line) in visible_chrome.iter().enumerate() {
            let row_y = cur_y + i as u16;
            if snap.delegate_count > 0 && line.spans.iter().any(|s| s.content.contains("Subagent"))
            {
                st.transcript_hit_boxes.push(TranscriptHitBox {
                    y: row_y,
                    x_start: content_x,
                    x_end: content_x + content_w,
                    target: TranscriptClickTarget::OpenSubagents,
                });
            }
        }
        frame.render_widget(
            Paragraph::new(visible_chrome.to_vec()),
            Rect {
                x: content_x,
                y: cur_y,
                width: content_w,
                height: chrome_h,
            },
        );
        cur_y += chrome_h;
    }

    let composer_rect = Rect {
        x: content_x,
        y: cur_y,
        width: content_w,
        height: card_h.min(area.y + area.height.saturating_sub(cur_y + 1)),
    };
    render_composer_card(frame, composer_rect, st, info, snap, &wrapped_input, false);

    let hint_y = composer_rect.y + composer_rect.height;
    if hint_y < area.y + area.height {
        let ctx_pct = if snap.context_max > 0 {
            ((snap.context_used as f64 / snap.context_max as f64) * 100.0).round() as u32
        } else {
            0
        };
        let ctx_label = format!("{} ({ctx_pct}%)", humanize_tokens(snap.context_used));
        render_composer_hint_row(
            frame,
            Rect {
                x: content_x,
                y: hint_y,
                width: content_w,
                height: 1,
            },
            st,
            Some(snap),
            info.in_flight_tools,
            Some(&ctx_label),
            false,
        );
    }

    composer_rect
}

pub mod cards;
pub mod composer;
pub mod modals;
pub mod palette;
pub mod subagents;

#[cfg(test)]
mod tests;

pub(crate) use cards::*;
pub(crate) use composer::*;
pub(crate) use modals::*;
pub use palette::*;
pub(crate) use subagents::*;
