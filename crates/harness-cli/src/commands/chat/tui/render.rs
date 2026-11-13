//! Full-screen `ratatui` renderer matching OpenCode's TUI layout and visual design.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use harness_core::ReasoningEffort;

use super::super::render::{format_elapsed_secs, reply_for_display, shorten_path, shorten_paths_in_text, SPINNER_FRAMES};
use super::super::ui::TuiLiveSnapshot;
use super::markdown::{render_markdown, strip_ansi, wrap_plain_text};
use super::state::{
    ActiveModal, LiveThinkingView, ModelPickerItem, PaletteAction, PaletteItem, ProviderPickerItem,
    TabHitBox, TabHitTarget, TranscriptClickTarget, TranscriptHitBox, TuiState,
};
use super::theme::{
    friendly_model_name, friendly_provider_name, humanize_tokens, mode_accent, ACCENT_CORAL,
    BG_CANVAS, BG_ELEVATED, BG_SELECTED, BG_SURFACE, BORDER_SUBTLE, GREEN_OK, LOGO_GRAY, RED_ERR,
    TEXT_DIM, TEXT_MUTED, TEXT_PRIMARY, YELLOW_WARN, LUNA_LOGO_LEFT, LUNA_LOGO_RIGHT,
};
use ratatui::style::Color;

pub struct FooterInfo<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub session_id: &'a str,
    pub in_flight_tools: &'a [super::super::render::TurnProcessItem],
}

/// Draw one complete TUI frame.
pub fn draw(
    frame: &mut Frame,
    st: &mut TuiState,
    snap: &TuiLiveSnapshot,
    info: &FooterInfo<'_>,
) {
    let area = frame.area();
    st.last_rendered_rect = (area.width, area.height);

    // Paint the entire frame canvas `#0e0e0f`.
    frame.render_widget(
        Block::default().style(Style::default().bg(BG_CANVAS)),
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

    let show_landing = st.turns.is_empty()
        && st.active_turn.is_none()
        && st.system_notices.is_empty()
        && snap.transient_error.is_none();

    let composer_rect = if show_landing {
        st.transcript_hit_boxes.clear();
        render_landing(frame, body_area, st, info)
    } else {
        render_conversation(frame, body_area, st, snap, info)
    };

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
}

fn render_tab_bar(frame: &mut Frame, area: Rect, st: &mut TuiState, active_session_id: &str) {
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
        Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
    ));
    cursor_x += count_w;

    // Leave 2 spaces before tabs.
    spans.push(Span::styled("  ", Style::default().bg(BG_CANVAS)));
    cursor_x += 2;

    // Determine how many trailing tabs fit within `area.width - 6`.
    let right_reserve: u16 = 4; // room for ` + `
    let max_tabs_x = area.x + area.width.saturating_sub(right_reserve);

    // Keep the active tab visible by taking a trailing window of tabs that includes it.
    let visible_tabs: Vec<(usize, &super::state::SessionTab)> =
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
    let tab_display_width = |tab: &super::state::SessionTab, is_active: bool| -> u16 {
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
                .fg(TEXT_PRIMARY)
                .bg(BG_SURFACE)
                .add_modifier(Modifier::BOLD)
        } else if rel_idx == 0 && slice_len > 3 {
            Style::default().fg(TEXT_DIM).bg(BG_CANVAS)
        } else {
            Style::default().fg(TEXT_MUTED).bg(BG_CANVAS)
        };
        let bg = if is_active { BG_SURFACE } else { BG_CANVAS };

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
                Style::default().fg(ACCENT_CORAL).bg(bg),
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
            Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_landing(
    frame: &mut Frame,
    area: Rect,
    st: &TuiState,
    info: &FooterInfo<'_>,
) -> Rect {
    let box_width = area.width.saturating_sub(12).clamp(40, 84).min(area.width.saturating_sub(2));
    let box_x = area.x + (area.width.saturating_sub(box_width)) / 2;

    let inner_text_w = box_width.saturating_sub(5) as usize;
    let wrapped_input = wrap_composer_lines(&st.composer.text, st.composer.cursor, inner_text_w);
    let input_rows = wrapped_input.lines.len().clamp(1, 5) as u16;
    // Card height: 1 top pad + input_rows + 1 blank spacer + 1 footer row = input_rows + 3
    let card_h = input_rows + 3;

    // Total vertical lockup: logo (3) + gap (2) + optional update (2) + card + hint (1)
    let update_rows: u16 = if st.update_notice.is_some() { 2 } else { 0 };
    let total_h = 3 + 2 + update_rows + card_h + 1;
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
            Span::styled(*left, Style::default().fg(LOGO_GRAY).bg(BG_CANVAS)),
            Span::styled(
                *right,
                Style::default()
                    .fg(TEXT_PRIMARY)
                    .bg(BG_CANVAS)
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
            Span::styled("/update", Style::default().fg(ACCENT_CORAL).bg(BG_CANVAS)),
            Span::styled(
                format!(" to install {version_label}"),
                Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
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

    // 3. Centered composer card
    let composer_rect = Rect {
        x: box_x,
        y: cur_y,
        width: box_width,
        height: card_h.min(area.y + area.height.saturating_sub(cur_y + 2)),
    };
    render_composer_card(
        frame,
        composer_rect,
        st,
        info,
        &wrapped_input,
        true,
    );

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
            None,
            None,
            true,
        );
    }

    // 5. Bottom-right version tag (`0.1.8`)
    let ver_y = area.y + area.height.saturating_sub(1);
    let ver_w = (st.version.width() + 2) as u16;
    if ver_y > hint_y && area.width > ver_w {
        let ver_x = area.x + area.width - ver_w;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{} ", st.version),
                Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
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

fn render_conversation(
    frame: &mut Frame,
    area: Rect,
    st: &mut TuiState,
    snap: &TuiLiveSnapshot,
    info: &FooterInfo<'_>,
) -> Rect {
    let margin = if area.width >= 48 { 2 } else { 1 };
    let content_x = area.x + margin;
    let content_w = area.width.saturating_sub(margin * 2).max(16);

    // Compute composer height
    let inner_text_w = content_w.saturating_sub(5) as usize;
    let wrapped_input = wrap_composer_lines(&st.composer.text, st.composer.cursor, inner_text_w);
    let input_rows = wrapped_input.lines.len().clamp(1, 6) as u16;
    let card_h = input_rows + 3;

    // Optional chrome strips above composer (todo, delegate, memory notices, queue, compaction, error)
    let chrome_lines = build_chrome_lines(st, snap, content_w as usize);
    let chrome_h = chrome_lines.len().min(6) as u16;

    let bottom_total = chrome_h + card_h + 1; // +1 for bottom status hint row
    let transcript_h = area.height.saturating_sub(bottom_total + 1).max(3);

    let transcript_rect = Rect {
        x: content_x,
        y: area.y + 1,
        width: content_w,
        height: transcript_h,
    };

    // Build transcript lines + clickable line hit targets
    let (mut lines, line_targets) = build_transcript_lines(st, snap, info, content_w as usize);
    let total_lines = lines.len();
    let view_h = transcript_rect.height as usize;
    st.max_scroll = total_lines.saturating_sub(view_h);
    if st.scroll_from_bottom > st.max_scroll {
        st.scroll_from_bottom = st.max_scroll;
    }
    let start = total_lines
        .saturating_sub(view_h)
        .saturating_sub(st.scroll_from_bottom);
    let end = (start + view_h).min(total_lines);

    st.transcript_hit_boxes.clear();
    for (line_idx, target) in line_targets {
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

    let visible_slice: Vec<Line<'static>> = if start < end {
        lines.drain(start..end).collect()
    } else {
        Vec::new()
    };

    frame.render_widget(Paragraph::new(visible_slice), transcript_rect);

    // Scroll-to-latest badge right-aligned above composer when scrolled up
    if st.scroll_from_bottom > 0 && transcript_rect.width > 18 && transcript_rect.height > 0 {
        let badge = " Jump to latest ↓ ";
        let bw = badge.width() as u16;
        if transcript_rect.width > bw {
            let badge_x = transcript_rect.x + transcript_rect.width - bw;
            let badge_y = transcript_rect.y + transcript_rect.height - 1;
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    badge,
                    Style::default().fg(TEXT_PRIMARY).bg(BG_CANVAS),
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
        frame.render_widget(
            Paragraph::new(chrome_lines[take_from..].to_vec()),
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
    render_composer_card(
        frame,
        composer_rect,
        st,
        info,
        &wrapped_input,
        false,
    );

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
            Some(&ctx_label),
            false,
        );
    }

    composer_rect
}

fn render_composer_card(
    frame: &mut Frame,
    rect: Rect,
    st: &TuiState,
    info: &FooterInfo<'_>,
    wrapped: &WrappedComposer,
    is_landing: bool,
) {
    if rect.height == 0 || rect.width < 6 {
        return;
    }
    let accent = mode_accent(st.mode.label());
    let inner_w = rect.width.saturating_sub(1) as usize;

    // Fill all card rows with `▎` accent bar + `BG_SURFACE` background.
    for row in 0..rect.height {
        let y = rect.y + row;
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("▎", Style::default().fg(accent).bg(BG_SURFACE)),
                Span::styled(" ".repeat(inner_w), Style::default().bg(BG_SURFACE)),
            ])),
            Rect {
                x: rect.x,
                y,
                width: rect.width,
                height: 1,
            },
        );
    }

    // Input rows start at `rect.y + 1` (leaving 1 top padding row inside the card).
    let max_input_rows = rect.height.saturating_sub(3).max(1) as usize;
    let scroll_off = wrapped
        .cursor_line
        .saturating_sub(max_input_rows.saturating_sub(1));

    if st.composer.text.is_empty() && rect.height >= 2 {
        let placeholder = if st.active_turn.is_some() {
            "Type to queue or steer next message..."
        } else if is_landing {
            "Ask anything... \"Fix broken tests\""
        } else {
            ""
        };
        let text_area_w = inner_w.saturating_sub(2);
        let clipped = truncate_str(placeholder, text_area_w);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("▎", Style::default().fg(accent).bg(BG_SURFACE)),
                Span::styled(
                    format!("  {clipped}"),
                    Style::default().fg(TEXT_MUTED).bg(BG_SURFACE),
                ),
            ])),
            Rect {
                x: rect.x,
                y: rect.y + 1,
                width: rect.width,
                height: 1,
            },
        );
    } else {
        for (vis_idx, line_str) in wrapped
            .lines
            .iter()
            .skip(scroll_off)
            .take(max_input_rows)
            .enumerate()
        {
            let y = rect.y + 1 + vis_idx as u16;
            let mut spans = vec![
                Span::styled("▎", Style::default().fg(accent).bg(BG_SURFACE)),
                Span::styled("  ", Style::default().bg(BG_SURFACE)),
            ];
            spans.extend(styled_composer_line_spans(line_str));
            frame.render_widget(
                Paragraph::new(Line::from(spans)),
                Rect {
                    x: rect.x,
                    y,
                    width: rect.width,
                    height: 1,
                },
            );
        }
    }

    // Place terminal cursor when no modal is open.
    if matches!(st.modal, ActiveModal::None) && rect.height >= 2 {
        let vis_cursor_y = wrapped.cursor_line.saturating_sub(scroll_off);
        let cur_x = (rect.x + 3 + wrapped.cursor_col as u16).min(rect.x + rect.width - 1);
        let cur_y = (rect.y + 1 + vis_cursor_y as u16).min(rect.y + rect.height - 1);
        frame.set_cursor_position((cur_x, cur_y));
    }

    // Footer metadata row at bottom of composer card (`Build auto · Model Provider · high`).
    if rect.height >= 3 {
        let footer_y = rect.y + rect.height - 1;
        let model_display = friendly_model_name(info.model);
        let provider_display = friendly_provider_name(info.provider);
        let thinking_label = info
            .reasoning_effort
            .map(|e| e.to_string())
            .unwrap_or_else(|| "off".to_string());
        let thinking_color = if info.reasoning_effort.is_some() {
            ACCENT_CORAL
        } else {
            TEXT_MUTED
        };

        let footer = Line::from(vec![
            Span::styled("▎", Style::default().fg(accent).bg(BG_SURFACE)),
            Span::styled("  ", Style::default().bg(BG_SURFACE)),
            Span::styled(
                st.mode.label(),
                Style::default()
                    .fg(accent)
                    .bg(BG_SURFACE)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {} · ", st.mode.sub_label()),
                Style::default().fg(TEXT_MUTED).bg(BG_SURFACE),
            ),
            Span::styled(
                model_display,
                Style::default()
                    .fg(TEXT_PRIMARY)
                    .bg(BG_SURFACE)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {provider_display} · "),
                Style::default().fg(TEXT_MUTED).bg(BG_SURFACE),
            ),
            Span::styled(
                thinking_label,
                Style::default().fg(thinking_color).bg(BG_SURFACE),
            ),
        ]);
        frame.render_widget(
            Paragraph::new(footer),
            Rect {
                x: rect.x,
                y: footer_y,
                width: rect.width,
                height: 1,
            },
        );
    }
}

fn render_composer_hint_row(
    frame: &mut Frame,
    rect: Rect,
    st: &TuiState,
    snap: Option<&TuiLiveSnapshot>,
    context_label: Option<&str>,
    is_landing: bool,
) {
    let mut left_spans: Vec<Span<'static>> = Vec::new();
    let mut right_spans: Vec<Span<'static>> = Vec::new();

    if st.active_turn.is_some() {
        // Animated OpenCode-style coral scanner bar (`▪▪▪▪▪▪▪▪ esc interrupt`)
        const SCANNER_LEN: usize = 8;
        let cycle = (SCANNER_LEN - 1) * 2;
        let pos_step = st.spinner_tick % cycle;
        let head = if pos_step < SCANNER_LEN {
            pos_step
        } else {
            cycle - pos_step
        };
        for i in 0..SCANNER_LEN {
            let dist = head.abs_diff(i);
            let color = match dist {
                0 => ACCENT_CORAL,
                1 => Color::Rgb(168, 84, 60),
                2 => Color::Rgb(96, 52, 42),
                _ => Color::Rgb(54, 34, 30),
            };
            left_spans.push(Span::styled("▪", Style::default().fg(color).bg(BG_CANVAS)));
        }
        left_spans.push(Span::styled(
            "  esc ",
            Style::default().fg(TEXT_PRIMARY).bg(BG_CANVAS),
        ));
        left_spans.push(Span::styled(
            "interrupt",
            Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
        ));
    }

    if let Some(s) = snap {
        if st.active_turn.is_some() {
            if let Some(ref tool_name) = s.activity_tool_name {
                let kind_label = if matches!(tool_name.as_str(), "terminal" | "shell" | "bash") {
                    "shell"
                } else {
                    "tool"
                };
                right_spans.push(Span::styled(
                    format!("↓ 1 {kind_label} · "),
                    Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
                ));
            }
        }
    }

    if let Some(ctx) = context_label {
        right_spans.push(Span::styled(
            format!("{ctx}   "),
            Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
        ));
    }
    if is_landing && st.active_turn.is_none() {
        right_spans.push(Span::styled(
            "shift+tab",
            Style::default().fg(TEXT_PRIMARY).bg(BG_CANVAS),
        ));
        right_spans.push(Span::styled(
            " agents   ",
            Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
        ));
    }
    right_spans.push(Span::styled(
        "ctrl+p",
        Style::default().fg(TEXT_PRIMARY).bg(BG_CANVAS),
    ));
    right_spans.push(Span::styled(
        " commands",
        Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
    ));

    let right_w: usize = right_spans.iter().map(|s| s.content.width()).sum();
    if st.active_turn.is_none() {
        let max_left = (rect.width as usize).saturating_sub(right_w + 2);
        let left_text = truncate_str(&st.cwd_label, max_left);
        left_spans.push(Span::styled(
            left_text,
            Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
        ));
    }

    let left_w: usize = left_spans.iter().map(|s| s.content.width()).sum();
    let spacer = (rect.width as usize).saturating_sub(left_w + right_w);

    let mut spans = left_spans;
    spans.push(Span::styled(
        " ".repeat(spacer),
        Style::default().bg(BG_CANVAS),
    ));
    spans.extend(right_spans);
    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

fn build_chrome_lines(
    st: &TuiState,
    snap: &TuiLiveSnapshot,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(ref err) = snap.transient_error {
        lines.push(Line::from(vec![
            Span::styled("✖ ", Style::default().fg(RED_ERR).bg(BG_CANVAS)),
            Span::styled(
                truncate_str(&strip_ansi(err), width.saturating_sub(3)),
                Style::default().fg(RED_ERR).bg(BG_CANVAS),
            ),
        ]));
    }
    if let Some((forced, secs)) = snap.compacting {
        let kind = if forced { "Compacting" } else { "Auto-compacting" };
        let spinner = SPINNER_FRAMES[st.spinner_tick % SPINNER_FRAMES.len()];
        lines.push(Line::from(vec![
            Span::styled(
                format!("{spinner} {kind} context… ({secs:.1}s)"),
                Style::default().fg(YELLOW_WARN).bg(BG_CANVAS),
            ),
        ]));
    }
    if let Some(ref todos) = snap.todo_snapshot {
        let total = todos.items.len();
        let done = todos
            .items
            .iter()
            .filter(|i| i.status == harness_core::tools::todo::TodoStatus::Done)
            .count();
        if total > 0 {
            let active_task = todos
                .items
                .iter()
                .find(|i| i.status == harness_core::tools::todo::TodoStatus::InProgress)
                .map(|i| i.text.as_str())
                .unwrap_or("");
            let summary = if snap.todo_collapsed || active_task.is_empty() {
                format!("☑ Todos: {done}/{total} done (ctrl+t toggle)")
            } else {
                format!(
                    "☑ Todos: {done}/{total} · {}",
                    truncate_str(active_task, width.saturating_sub(24))
                )
            };
            lines.push(Line::from(Span::styled(
                summary,
                Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
            )));
        }
    }
    if snap.delegate_count > 0 {
        if let Some((ref goal, ref act)) = snap.delegate_last {
            let spinner = SPINNER_FRAMES[st.spinner_tick % SPINNER_FRAMES.len()];
            let more = if snap.delegate_count > 1 {
                format!(" (+{})", snap.delegate_count - 1)
            } else {
                String::new()
            };
            lines.push(Line::from(Span::styled(
                truncate_str(
                    &format!("{spinner} Subagent{more}: {goal} — {act}"),
                    width,
                ),
                Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
            )));
        }
    }
    if snap.queue_depth > 0 {
        lines.push(Line::from(Span::styled(
            format!("⏸ {} queued — will run next", snap.queue_depth),
            Style::default().fg(YELLOW_WARN).bg(BG_CANVAS),
        )));
    }
    if let Some(armed_at) = st.ctrl_c_armed {
        if armed_at.elapsed().as_secs() < 2 {
            lines.push(Line::from(Span::styled(
                "Press Ctrl+C again to exit Luna",
                Style::default().fg(YELLOW_WARN).bg(BG_CANVAS),
            )));
        }
    }
    lines
}

fn build_transcript_lines(
    st: &TuiState,
    snap: &TuiLiveSnapshot,
    info: &FooterInfo<'_>,
    width: usize,
) -> (Vec<Line<'static>>, Vec<(usize, TranscriptClickTarget)>) {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut hit_targets: Vec<(usize, TranscriptClickTarget)> = Vec::new();
    let card_inner_w = width.saturating_sub(4).max(12);

    for (idx, turn) in st.turns.iter().enumerate() {
        // 1. User message card (`#1e1e20` + coral `▎` left bar)
        if !turn.user_query.trim().is_empty() {
            push_user_card(&mut out, &turn.user_query, width, card_inner_w);
            out.push(Line::default());
        }

        // 2 & 3. Chronological reasoning, prose, & OpenCode tool cards (clickable, or Ctrl+O to toggle all)
        render_completed_turn_process(
            &mut out,
            &mut hit_targets,
            idx,
            turn,
            st.process_expanded,
            &st.expanded_thinking,
            &st.expanded_tools,
            width,
            card_inner_w,
        );

        // 4. Assistant reply (markdown)
        let display_reply = reply_for_display(&turn.reply);
        if !display_reply.trim().is_empty() {
            if !turn.reasoning.as_deref().unwrap_or("").is_empty() || !turn.tool_items.is_empty() {
                out.push(Line::default());
            }
            for mline in render_markdown(display_reply, card_inner_w) {
                let mut spans = vec![Span::styled("  ", Style::default().bg(BG_CANVAS))];
                spans.extend(mline.spans);
                out.push(Line::from(spans));
            }
        }

        // 5. Turn footer (`Build · Model Name · 2.2s · 5.6 tok/s`)
        let meta = st.turn_meta.get(idx);
        let mode_str = meta.map(|m| m.mode.as_str()).unwrap_or(st.mode.label());
        let model_raw = meta.map(|m| m.model.as_str()).unwrap_or(info.model);
        let model_disp = friendly_model_name(model_raw);
        let dur_secs = turn.duration.as_secs_f64();
        let dur_label = if dur_secs < 60.0 {
            format!("{dur_secs:.1}s")
        } else {
            format_elapsed_secs(turn.duration.as_secs())
        };

        let mut footer_spans = vec![
            Span::styled("  ", Style::default().bg(BG_CANVAS)),
            Span::styled(
                mode_str.to_string(),
                Style::default().fg(mode_accent(mode_str)).bg(BG_CANVAS),
            ),
            Span::styled(
                format!(" · {model_disp} · {dur_label}"),
                Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
            ),
        ];
        if let Some(toks) = meta.and_then(|m| m.output_tokens) {
            if dur_secs > 0.05 && toks > 0 {
                let tps = toks as f64 / dur_secs;
                footer_spans.push(Span::styled(
                    format!(" · {tps:.1} tok/s"),
                    Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
                ));
            }
        }
        out.push(Line::default());
        out.push(Line::from(footer_spans));
        out.push(Line::default());
    }

    // Inline system notices (e.g. from commands)
    for notice in &st.system_notices {
        for line in wrap_plain_text(&strip_ansi(notice), card_inner_w) {
            out.push(Line::from(Span::styled(
                format!("  {line}"),
                Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
            )));
        }
    }
    if !st.system_notices.is_empty() {
        out.push(Line::default());
    }

    // Active streaming turn
    if st.active_turn.is_some() {
        if !st.current_query.trim().is_empty() {
            push_user_card(&mut out, &st.current_query, width, card_inner_w);
            out.push(Line::default());
        }

        render_active_turn_process(
            &mut out,
            &mut hit_targets,
            snap,
            info.in_flight_tools,
            st.process_expanded,
            &st.expanded_live_thinking,
            &st.expanded_live_tools,
            st.live_thinking_view,
            st.spinner_tick,
            st.turn_start.elapsed().as_secs_f32(),
            width,
            card_inner_w,
        );
        out.push(Line::default());
    }

    (out, hit_targets)
}

#[allow(clippy::too_many_arguments)]
fn render_completed_turn_process(
    out: &mut Vec<Line<'static>>,
    hit_targets: &mut Vec<(usize, TranscriptClickTarget)>,
    turn_idx: usize,
    turn: &super::super::render::TurnRecord,
    process_expanded: bool,
    collapsed_thinking: &std::collections::HashSet<(usize, usize)>,
    expanded_tools: &std::collections::HashSet<(usize, usize)>,
    width: usize,
    card_inner_w: usize,
) {
    let mut thinking_idx = 0usize;
    let mut tool_group_idx = 0usize;
    if let Some(ref reasoning) = turn.reasoning {
        let trimmed = reasoning.trim();
        if !trimmed.is_empty() {
            let open = process_expanded || !collapsed_thinking.contains(&(turn_idx, thinking_idx));
            push_settled_thinking_block(
                out,
                hit_targets,
                trimmed,
                None,
                open,
                TranscriptClickTarget::ToggleTurnThinking(turn_idx, thinking_idx),
                card_inner_w,
            );
            thinking_idx += 1;
        }
    }

    let mut pending_explored: Vec<&super::super::render::TurnProcessItem> = Vec::new();
    for item in &turn.tool_items {
        if item.is_thinking() {
            if !pending_explored.is_empty() {
                let open =
                    process_expanded || expanded_tools.contains(&(turn_idx, tool_group_idx));
                push_explored_group(
                    out,
                    hit_targets,
                    &pending_explored,
                    open,
                    TranscriptClickTarget::ToggleTurnTools(turn_idx, tool_group_idx),
                    width,
                    card_inner_w,
                );
                tool_group_idx += 1;
                pending_explored.clear();
            }
            let trimmed = item.output.trim();
            if !trimmed.is_empty() {
                let open =
                    process_expanded || !collapsed_thinking.contains(&(turn_idx, thinking_idx));
                push_settled_thinking_block(
                    out,
                    hit_targets,
                    trimmed,
                    item.duration_secs,
                    open,
                    TranscriptClickTarget::ToggleTurnThinking(turn_idx, thinking_idx),
                    card_inner_w,
                );
                thinking_idx += 1;
            }
        } else if item.is_assistant_text() {
            if !pending_explored.is_empty() {
                let open =
                    process_expanded || expanded_tools.contains(&(turn_idx, tool_group_idx));
                push_explored_group(
                    out,
                    hit_targets,
                    &pending_explored,
                    open,
                    TranscriptClickTarget::ToggleTurnTools(turn_idx, tool_group_idx),
                    width,
                    card_inner_w,
                );
                tool_group_idx += 1;
                pending_explored.clear();
            }
            let trimmed = item.output.trim();
            if !trimmed.is_empty() {
                for mline in render_markdown(trimmed, card_inner_w) {
                    let mut spans = vec![Span::styled("  ", Style::default().bg(BG_CANVAS))];
                    spans.extend(mline.spans);
                    out.push(Line::from(spans));
                }
                out.push(Line::default());
            }
        } else if is_collapsible_exploration(item) {
            pending_explored.push(item);
        } else {
            if !pending_explored.is_empty() {
                let open =
                    process_expanded || expanded_tools.contains(&(turn_idx, tool_group_idx));
                push_explored_group(
                    out,
                    hit_targets,
                    &pending_explored,
                    open,
                    TranscriptClickTarget::ToggleTurnTools(turn_idx, tool_group_idx),
                    width,
                    card_inner_w,
                );
                tool_group_idx += 1;
                pending_explored.clear();
            }
            let open = process_expanded || expanded_tools.contains(&(turn_idx, tool_group_idx));
            push_completed_tool_card(
                out,
                hit_targets,
                item,
                open,
                TranscriptClickTarget::ToggleTurnTools(turn_idx, tool_group_idx),
                width,
                card_inner_w,
            );
            tool_group_idx += 1;
        }
    }
    if !pending_explored.is_empty() {
        let open = process_expanded || expanded_tools.contains(&(turn_idx, tool_group_idx));
        push_explored_group(
            out,
            hit_targets,
            &pending_explored,
            open,
            TranscriptClickTarget::ToggleTurnTools(turn_idx, tool_group_idx),
            width,
            card_inner_w,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_active_turn_process(
    out: &mut Vec<Line<'static>>,
    hit_targets: &mut Vec<(usize, TranscriptClickTarget)>,
    snap: &TuiLiveSnapshot,
    in_flight_tools: &[super::super::render::TurnProcessItem],
    process_expanded: bool,
    collapsed_live_thinking: &std::collections::HashSet<usize>,
    expanded_live_tools: &std::collections::HashSet<usize>,
    live_thinking_view: LiveThinkingView,
    spinner_tick: usize,
    turn_secs: f32,
    width: usize,
    card_inner_w: usize,
) {
    // Chronological settled thinking blocks, prose, and tool cards executed so far in this turn
    let mut pending_explored: Vec<(usize, &super::super::render::TurnProcessItem)> = Vec::new();
    for (step_idx, item) in in_flight_tools.iter().enumerate() {
        if item.is_thinking() {
            if !pending_explored.is_empty() {
                let first_idx = pending_explored[0].0;
                let open = process_expanded || expanded_live_tools.contains(&first_idx);
                let items: Vec<&super::super::render::TurnProcessItem> =
                    pending_explored.iter().map(|(_, it)| *it).collect();
                push_explored_group(
                    out,
                    hit_targets,
                    &items,
                    open,
                    TranscriptClickTarget::ToggleLiveToolStep(first_idx),
                    width,
                    card_inner_w,
                );
                pending_explored.clear();
            }
            let trimmed = item.output.trim();
            if !trimmed.is_empty() {
                let open = process_expanded || !collapsed_live_thinking.contains(&step_idx);
                push_settled_thinking_block(
                    out,
                    hit_targets,
                    trimmed,
                    item.duration_secs,
                    open,
                    TranscriptClickTarget::ToggleLiveThinkingStep(step_idx),
                    card_inner_w,
                );
            }
            continue;
        }
        if item.is_assistant_text() {
            if !pending_explored.is_empty() {
                let first_idx = pending_explored[0].0;
                let open = process_expanded || expanded_live_tools.contains(&first_idx);
                let items: Vec<&super::super::render::TurnProcessItem> =
                    pending_explored.iter().map(|(_, it)| *it).collect();
                push_explored_group(
                    out,
                    hit_targets,
                    &items,
                    open,
                    TranscriptClickTarget::ToggleLiveToolStep(first_idx),
                    width,
                    card_inner_w,
                );
                pending_explored.clear();
            }
            let trimmed = item.output.trim();
            if !trimmed.is_empty() {
                for mline in render_markdown(trimmed, card_inner_w) {
                    let mut spans = vec![Span::styled("  ", Style::default().bg(BG_CANVAS))];
                    spans.extend(mline.spans);
                    out.push(Line::from(spans));
                }
                out.push(Line::default());
            }
            continue;
        }
        if item.result_formatted.is_empty() {
            // Still running: flush pending explored group and draw running card below if active
            if !pending_explored.is_empty() {
                let first_idx = pending_explored[0].0;
                let open = process_expanded || expanded_live_tools.contains(&first_idx);
                let items: Vec<&super::super::render::TurnProcessItem> =
                    pending_explored.iter().map(|(_, it)| *it).collect();
                push_explored_group(
                    out,
                    hit_targets,
                    &items,
                    open,
                    TranscriptClickTarget::ToggleLiveToolStep(first_idx),
                    width,
                    card_inner_w,
                );
                pending_explored.clear();
            }
            if snap.activity_call.is_none() {
                push_running_tool_card(
                    out,
                    &item.tool_name,
                    item.arguments.as_ref(),
                    Some(&item.call_formatted),
                    None,
                    0.0,
                    0,
                    &[],
                    spinner_tick,
                    width,
                    card_inner_w,
                );
            }
            continue;
        }
        if is_collapsible_exploration(item) {
            pending_explored.push((step_idx, item));
        } else {
            if !pending_explored.is_empty() {
                let first_idx = pending_explored[0].0;
                let open = process_expanded || expanded_live_tools.contains(&first_idx);
                let items: Vec<&super::super::render::TurnProcessItem> =
                    pending_explored.iter().map(|(_, it)| *it).collect();
                push_explored_group(
                    out,
                    hit_targets,
                    &items,
                    open,
                    TranscriptClickTarget::ToggleLiveToolStep(first_idx),
                    width,
                    card_inner_w,
                );
                pending_explored.clear();
            }
            let open = process_expanded || expanded_live_tools.contains(&step_idx);
            push_completed_tool_card(
                out,
                hit_targets,
                item,
                open,
                TranscriptClickTarget::ToggleLiveToolStep(step_idx),
                width,
                card_inner_w,
            );
        }
    }
    if !pending_explored.is_empty() {
        let first_idx = pending_explored[0].0;
        let open = process_expanded || expanded_live_tools.contains(&first_idx);
        let items: Vec<&super::super::render::TurnProcessItem> =
            pending_explored.iter().map(|(_, it)| *it).collect();
        push_explored_group(
            out,
            hit_targets,
            &items,
            open,
            TranscriptClickTarget::ToggleLiveToolStep(first_idx),
            width,
            card_inner_w,
        );
    }

    if let Some(ref call) = snap.activity_call {
        let tool_name = snap.activity_tool_name.as_deref().unwrap_or("tool");
        let secs = snap.activity_elapsed_secs.unwrap_or(0.0);
        push_running_tool_card(
            out,
            tool_name,
            snap.activity_args.as_ref(),
            Some(call),
            snap.activity_detail.as_deref(),
            secs,
            snap.activity_tool_output_hidden,
            &snap.activity_tool_output_raw,
            spinner_tick,
            width,
            card_inner_w,
        );
    }

    // Current round's live reasoning block (positioned after all earlier tools/thinking)
    if !snap.reasoning.trim().is_empty() {
        let trimmed = snap.reasoning.trim();
        let secs = snap.reasoning_secs.unwrap_or(turn_secs);
        let view_mode = if process_expanded {
            LiveThinkingView::Expanded
        } else {
            live_thinking_view
        };
        let actively_thinking = snap.content.trim().is_empty() && snap.activity_call.is_none();
        if actively_thinking {
            let spinner = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
            let header_idx = out.len();
            out.push(Line::from(vec![
                Span::styled("  │ ", Style::default().fg(BORDER_SUBTLE).bg(BG_CANVAS)),
                Span::styled(
                    format!("{spinner} Thought: {secs:.1}s"),
                    Style::default().fg(ACCENT_CORAL).bg(BG_CANVAS),
                ),
            ]));
            hit_targets.push((header_idx, TranscriptClickTarget::ToggleLiveThinking));

            if view_mode != LiveThinkingView::Collapsed {
                out.push(Line::from(Span::styled(
                    "  │",
                    Style::default().fg(BORDER_SUBTLE).bg(BG_CANVAS),
                )));
                let rlines = render_markdown(trimmed, card_inner_w.saturating_sub(2));
                let skip = if view_mode == LiveThinkingView::Expanded {
                    0
                } else {
                    rlines.len().saturating_sub(10)
                };
                for rline in rlines.into_iter().skip(skip) {
                    let mut spans = vec![Span::styled(
                        "  │ ",
                        Style::default().fg(BORDER_SUBTLE).bg(BG_CANVAS),
                    )];
                    for s in rline.spans {
                        spans.push(Span::styled(
                            s.content,
                            s.style.fg(TEXT_MUTED).bg(BG_CANVAS),
                        ));
                    }
                    let row_idx = out.len();
                    out.push(Line::from(spans));
                    hit_targets.push((row_idx, TranscriptClickTarget::ToggleLiveThinking));
                }
            }
            out.push(Line::default());
        } else {
            let open = view_mode != LiveThinkingView::Collapsed;
            push_settled_thinking_block(
                out,
                hit_targets,
                trimmed,
                snap.reasoning_secs,
                open,
                TranscriptClickTarget::ToggleLiveThinking,
                card_inner_w,
            );
        }
    }

    // Live streaming reply text
    if !snap.content.trim().is_empty() {
        for mline in render_markdown(&snap.content, card_inner_w) {
            let mut spans = vec![Span::styled("  ", Style::default().bg(BG_CANVAS))];
            spans.extend(mline.spans);
            out.push(Line::from(spans));
        }
    } else if snap.reasoning.trim().is_empty() && snap.activity_call.is_none() {
        let spinner = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
        out.push(Line::from(vec![
            Span::styled("  │ ", Style::default().fg(BORDER_SUBTLE).bg(BG_CANVAS)),
            Span::styled(
                format!("{spinner} Thinking… {turn_secs:.1}s"),
                Style::default().fg(ACCENT_CORAL).bg(BG_CANVAS),
            ),
        ]));
    }
}

fn push_settled_thinking_block(
    out: &mut Vec<Line<'static>>,
    hit_targets: &mut Vec<(usize, TranscriptClickTarget)>,
    trimmed: &str,
    duration_secs: Option<f32>,
    open: bool,
    click_target: TranscriptClickTarget,
    card_inner_w: usize,
) {
    let label = match duration_secs {
        Some(secs) => format!("Thought: {secs:.1}s"),
        None => "Thought".to_string(),
    };
    if open {
        let header_idx = out.len();
        out.push(Line::from(vec![
            Span::styled("  │ ", Style::default().fg(BORDER_SUBTLE).bg(BG_CANVAS)),
            Span::styled(label, Style::default().fg(ACCENT_CORAL).bg(BG_CANVAS)),
        ]));
        hit_targets.push((header_idx, click_target.clone()));
        let spacer_idx = out.len();
        out.push(Line::from(Span::styled(
            "  │",
            Style::default().fg(BORDER_SUBTLE).bg(BG_CANVAS),
        )));
        hit_targets.push((spacer_idx, click_target.clone()));
        for rline in render_markdown(trimmed, card_inner_w.saturating_sub(2)) {
            let mut spans = vec![Span::styled(
                "  │ ",
                Style::default().fg(BORDER_SUBTLE).bg(BG_CANVAS),
            )];
            for s in rline.spans {
                spans.push(Span::styled(
                    s.content,
                    s.style.fg(TEXT_MUTED).bg(BG_CANVAS),
                ));
            }
            let row_idx = out.len();
            out.push(Line::from(spans));
            hit_targets.push((row_idx, click_target.clone()));
        }
        out.push(Line::default());
    } else {
        let line_count = trimmed.lines().count().max(1);
        let unit = if line_count == 1 { "line" } else { "lines" };
        let row_idx = out.len();
        out.push(Line::from(vec![
            Span::styled("  │ ", Style::default().fg(BORDER_SUBTLE).bg(BG_CANVAS)),
            Span::styled(
                format!("▸ {label} ({line_count} {unit})"),
                Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
            ),
        ]));
        hit_targets.push((row_idx, click_target));
        out.push(Line::default());
    }
}

fn is_collapsible_exploration(item: &super::super::render::TurnProcessItem) -> bool {
    if item.is_error {
        return false;
    }
    match item.tool_name.as_str() {
        "read_file" | "read" | "list_files" | "ls" => true,
        "search_files" | "grep" | "find" => item.output.lines().count() <= 1 && item.output_dropped == 0,
        _ => false,
    }
}

fn push_explored_group(
    out: &mut Vec<Line<'static>>,
    hit_targets: &mut Vec<(usize, TranscriptClickTarget)>,
    items: &[&super::super::render::TurnProcessItem],
    open: bool,
    click_target: TranscriptClickTarget,
    width: usize,
    card_inner_w: usize,
) {
    if open {
        for item in items {
            push_completed_tool_card(
                out,
                hit_targets,
                item,
                true,
                click_target.clone(),
                width,
                card_inner_w,
            );
        }
        return;
    }

    let mut reads = 0usize;
    let mut searches = 0usize;
    let mut lists = 0usize;
    for item in items {
        match item.tool_name.as_str() {
            "read_file" | "read" => reads += 1,
            "list_files" | "ls" => lists += 1,
            "search_files" | "grep" | "find" => searches += 1,
            _ => {}
        }
    }
    let mut parts = Vec::new();
    if searches > 0 {
        parts.push(if searches == 1 {
            "1 search".to_string()
        } else {
            format!("{searches} searches")
        });
    }
    if reads > 0 {
        parts.push(if reads == 1 {
            "1 read".to_string()
        } else {
            format!("{reads} reads")
        });
    }
    if lists > 0 {
        parts.push(if lists == 1 {
            "1 list".to_string()
        } else {
            format!("{lists} lists")
        });
    }
    let summary = parts.join(", ");
    let row_idx = out.len();
    out.push(Line::from(Span::styled(
        format!("  → Explored: {summary}"),
        Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
    )));
    hit_targets.push((row_idx, click_target));
    out.push(Line::default());
}

fn format_opencode_tool_header(
    tool_name: &str,
    arguments: Option<&serde_json::Value>,
    fallback_call: Option<&str>,
) -> (&'static str, String) {
    let arg_str = |key: &str| -> Option<&str> {
        arguments
            .and_then(|a| a.get(key))
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
    };
    let arg_num = |key: &str| -> Option<u64> { arguments.and_then(|a| a.get(key)).and_then(|v| v.as_u64()) };

    match tool_name {
        "terminal" | "shell" | "bash" => {
            if let Some(cmd) = arg_str("command") {
                let first = cmd.lines().next().unwrap_or(cmd).trim();
                let mut header = shorten_paths_in_text(first);
                if cmd.lines().count() > 1 {
                    header.push_str(" …");
                }
                if let Some(wd) = arg_str("workdir") {
                    header = format!("{header} (in {})", shorten_path(wd));
                }
                return ("$ ", header);
            }
        }
        "search_files" | "grep" | "find" => {
            if let Some(pat) = arg_str("pattern").or_else(|| arg_str("query")) {
                let path = arg_str("path").unwrap_or(".");
                let mut desc = format!("{pat} in {}", shorten_path(path));
                if let Some(glob) = arg_str("file_glob").filter(|g| *g != "*") {
                    desc.push_str(&format!(" ({glob})"));
                }
                return ("⋮ ", desc);
            }
        }
        "read_file" | "read" => {
            if let Some(p) = arg_str("path")
                .or_else(|| arg_str("file"))
                .or_else(|| arg_str("filepath"))
            {
                let short = shorten_path(p);
                let range = match (arg_num("offset"), arg_num("limit")) {
                    (Some(off), Some(lim)) => format!(" L{}-{}", off, off + lim.saturating_sub(1)),
                    (Some(off), None) => format!(" L{off}+"),
                    (None, Some(lim)) => format!(" L1-{lim}"),
                    _ => String::new(),
                };
                return ("→ ", format!("Read {short}{range}"));
            }
        }
        "list_files" | "ls" => {
            let short = shorten_path(arg_str("path").unwrap_or("."));
            return ("→ ", format!("List {short}"));
        }
        "patch" | "edit" | "apply_patch" => {
            if let Some(p) = arg_str("path")
                .or_else(|| arg_str("filePath"))
                .or_else(|| arg_str("filepath"))
                .or_else(|| arg_str("file"))
            {
                return ("← ", format!("Edit {}", shorten_path(p)));
            }
            return ("← ", "Edit patch".to_string());
        }
        "write_file" | "write" => {
            if let Some(p) = arg_str("path")
                .or_else(|| arg_str("file"))
                .or_else(|| arg_str("filepath"))
            {
                return ("← ", format!("Write {}", shorten_path(p)));
            }
        }
        "delegate_task" => {
            if let Some(tasks) = arguments.and_then(|a| a.get("tasks")).and_then(|v| v.as_array()) {
                if let Some(goal) = tasks
                    .first()
                    .and_then(|t| t.get("goal"))
                    .and_then(|g| g.as_str())
                {
                    let extra = if tasks.len() > 1 {
                        format!(" (+{} more)", tasks.len() - 1)
                    } else {
                        String::new()
                    };
                    return ("◈ ", format!("Subagent: {goal}{extra}"));
                }
            }
        }
        _ => {}
    }

    let cleaned = fallback_call
        .map(strip_ansi)
        .unwrap_or_else(|| tool_name.to_string());
    let cleaned = cleaned
        .trim()
        .trim_end_matches("(success)")
        .trim_end_matches("(failed)")
        .trim();
    if cleaned.is_empty() {
        ("⋮ ", tool_name.to_string())
    } else {
        ("⋮ ", cleaned.to_string())
    }
}

fn push_card_row(
    out: &mut Vec<Line<'static>>,
    hit_targets: Option<&mut Vec<(usize, TranscriptClickTarget)>>,
    click_target: Option<&TranscriptClickTarget>,
    border_color: Color,
    width: usize,
    content_spans: Vec<Span<'static>>,
) {
    let content_w: usize = content_spans.iter().map(|s| s.content.width()).sum();
    let pad = width.saturating_sub(3 + content_w);
    let mut spans = vec![
        Span::styled("│", Style::default().fg(border_color).bg(BG_SURFACE)),
        Span::styled("  ", Style::default().bg(BG_SURFACE)),
    ];
    spans.extend(content_spans);
    spans.push(Span::styled(" ".repeat(pad), Style::default().bg(BG_SURFACE)));
    let row_idx = out.len();
    out.push(Line::from(spans));
    if let (Some(targets), Some(target)) = (hit_targets, click_target) {
        targets.push((row_idx, target.clone()));
    }
}

fn push_card_blank_row(
    out: &mut Vec<Line<'static>>,
    hit_targets: Option<&mut Vec<(usize, TranscriptClickTarget)>>,
    click_target: Option<&TranscriptClickTarget>,
    border_color: Color,
    width: usize,
) {
    let blank_fill = " ".repeat(width.saturating_sub(1));
    let row_idx = out.len();
    out.push(Line::from(vec![
        Span::styled("│", Style::default().fg(border_color).bg(BG_SURFACE)),
        Span::styled(blank_fill, Style::default().bg(BG_SURFACE)),
    ]));
    if let (Some(targets), Some(target)) = (hit_targets, click_target) {
        targets.push((row_idx, target.clone()));
    }
}

fn push_completed_tool_card(
    out: &mut Vec<Line<'static>>,
    hit_targets: &mut Vec<(usize, TranscriptClickTarget)>,
    item: &super::super::render::TurnProcessItem,
    open: bool,
    click_target: TranscriptClickTarget,
    width: usize,
    card_inner_w: usize,
) {
    if item.tool_name == "steer" || item.tool_name == "approval" {
        let clean = strip_ansi(&item.call_formatted);
        if !clean.trim().is_empty() {
            out.push(Line::from(Span::styled(
                format!("  {}", truncate_str(clean.trim(), card_inner_w)),
                Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
            )));
            out.push(Line::default());
        }
        return;
    }

    let border_color = if item.is_error { RED_ERR } else { BORDER_SUBTLE };
    let (prefix, header_body) = format_opencode_tool_header(
        &item.tool_name,
        item.arguments.as_ref(),
        Some(&item.call_formatted),
    );
    let prefix_w = prefix.width();
    let wrap_w = card_inner_w.saturating_sub(prefix_w).max(8);
    let header_lines = wrap_plain_text(&header_body, wrap_w);

    push_card_blank_row(out, Some(hit_targets), Some(&click_target), border_color, width);

    for (i, hline) in header_lines.into_iter().enumerate() {
        let lead = if i == 0 {
            prefix.to_string()
        } else {
            " ".repeat(prefix_w)
        };
        push_card_row(
            out,
            Some(hit_targets),
            Some(&click_target),
            border_color,
            width,
            vec![
                Span::styled(
                    lead,
                    Style::default()
                        .fg(if item.is_error { RED_ERR } else { TEXT_PRIMARY })
                        .bg(BG_SURFACE)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    hline,
                    Style::default()
                        .fg(TEXT_PRIMARY)
                        .bg(BG_SURFACE)
                        .add_modifier(Modifier::BOLD),
                ),
            ],
        );
    }

    let has_diff = !item.diff_lines.is_empty() && !item.is_error;
    let raw_lines: Vec<String> = item
        .output
        .lines()
        .map(strip_ansi)
        .filter(|l| !l.trim().is_empty())
        .collect();
    let tail_cap = if open { 40 } else { 7 };

    if has_diff {
        push_card_blank_row(out, Some(hit_targets), Some(&click_target), border_color, width);
        let max_diff = if open { item.diff_lines.len() } else { 10 };
        for diff in item.diff_lines.iter().take(max_diff) {
            let clean = strip_ansi(diff);
            let color = if clean.trim_start().starts_with('+') {
                GREEN_OK
            } else if clean.trim_start().starts_with('-') {
                RED_ERR
            } else {
                TEXT_MUTED
            };
            push_card_row(
                out,
                Some(hit_targets),
                Some(&click_target),
                border_color,
                width,
                vec![Span::styled(
                    truncate_str(&clean, card_inner_w),
                    Style::default().fg(color).bg(BG_SURFACE),
                )],
            );
        }
        if item.diff_lines.len() > max_diff {
            push_card_row(
                out,
                Some(hit_targets),
                Some(&click_target),
                border_color,
                width,
                vec![Span::styled(
                    format!("({} more diff lines)", item.diff_lines.len() - max_diff),
                    Style::default().fg(TEXT_DIM).bg(BG_SURFACE),
                )],
            );
        }
    } else if !raw_lines.is_empty() {
        push_card_blank_row(out, Some(hit_targets), Some(&click_target), border_color, width);
        let start = raw_lines.len().saturating_sub(tail_cap);
        let hidden = item.output_dropped + start;
        if hidden > 0 {
            push_card_row(
                out,
                Some(hit_targets),
                Some(&click_target),
                border_color,
                width,
                vec![Span::styled(
                    format!("({hidden} earlier lines)"),
                    Style::default().fg(TEXT_MUTED).bg(BG_SURFACE),
                )],
            );
        }
        let body_color = if item.is_error && raw_lines.len() <= 6 {
            RED_ERR
        } else {
            TEXT_MUTED
        };
        for oline in &raw_lines[start..] {
            for wrapped in wrap_plain_text(oline, card_inner_w) {
                push_card_row(
                    out,
                    Some(hit_targets),
                    Some(&click_target),
                    border_color,
                    width,
                    vec![Span::styled(
                        wrapped,
                        Style::default().fg(body_color).bg(BG_SURFACE),
                    )],
                );
            }
        }
    } else if item.is_error && !item.result_formatted.is_empty() {
        push_card_blank_row(out, Some(hit_targets), Some(&click_target), border_color, width);
        let res_clean = strip_ansi(&item.result_formatted);
        push_card_row(
            out,
            Some(hit_targets),
            Some(&click_target),
            border_color,
            width,
            vec![Span::styled(
                truncate_str(res_clean.trim(), card_inner_w),
                Style::default().fg(RED_ERR).bg(BG_SURFACE),
            )],
        );
    }

    push_card_blank_row(out, Some(hit_targets), Some(&click_target), border_color, width);
    out.push(Line::default());
}

#[allow(clippy::too_many_arguments)]
fn push_running_tool_card(
    out: &mut Vec<Line<'static>>,
    tool_name: &str,
    arguments: Option<&serde_json::Value>,
    fallback_call: Option<&str>,
    detail: Option<&str>,
    secs: f32,
    hidden_lines: usize,
    raw_tail: &[String],
    spinner_tick: usize,
    width: usize,
    card_inner_w: usize,
) {
    let spinner = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
    let (prefix, header_body) = format_opencode_tool_header(tool_name, arguments, fallback_call);
    let suffix = match detail {
        Some(d) => format!(" {d} ({secs:.1}s)"),
        None => format!(" ({secs:.1}s)"),
    };
    let full_header = format!("{header_body}{suffix}");
    let prefix_w = format!("{spinner} {prefix}").width();
    let wrap_w = card_inner_w.saturating_sub(prefix_w).max(8);
    let header_lines = wrap_plain_text(&full_header, wrap_w);

    push_card_blank_row(out, None, None, ACCENT_CORAL, width);
    for (i, hline) in header_lines.into_iter().enumerate() {
        if i == 0 {
            push_card_row(
                out,
                None,
                None,
                ACCENT_CORAL,
                width,
                vec![
                    Span::styled(
                        format!("{spinner} "),
                        Style::default().fg(ACCENT_CORAL).bg(BG_SURFACE),
                    ),
                    Span::styled(
                        prefix.to_string(),
                        Style::default()
                            .fg(TEXT_PRIMARY)
                            .bg(BG_SURFACE)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        hline,
                        Style::default()
                            .fg(TEXT_PRIMARY)
                            .bg(BG_SURFACE)
                            .add_modifier(Modifier::BOLD),
                    ),
                ],
            );
        } else {
            push_card_row(
                out,
                None,
                None,
                ACCENT_CORAL,
                width,
                vec![
                    Span::styled(" ".repeat(prefix_w), Style::default().bg(BG_SURFACE)),
                    Span::styled(
                        hline,
                        Style::default()
                            .fg(TEXT_PRIMARY)
                            .bg(BG_SURFACE)
                            .add_modifier(Modifier::BOLD),
                    ),
                ],
            );
        }
    }

    if !raw_tail.is_empty() || hidden_lines > 0 {
        push_card_blank_row(out, None, None, ACCENT_CORAL, width);
        if hidden_lines > 0 {
            push_card_row(
                out,
                None,
                None,
                ACCENT_CORAL,
                width,
                vec![Span::styled(
                    format!("({hidden_lines} earlier lines)"),
                    Style::default().fg(TEXT_MUTED).bg(BG_SURFACE),
                )],
            );
        }
        for line in raw_tail {
            for wrapped in wrap_plain_text(&strip_ansi(line), card_inner_w) {
                push_card_row(
                    out,
                    None,
                    None,
                    ACCENT_CORAL,
                    width,
                    vec![Span::styled(
                        wrapped,
                        Style::default().fg(TEXT_MUTED).bg(BG_SURFACE),
                    )],
                );
            }
        }
    }

    push_card_blank_row(out, None, None, ACCENT_CORAL, width);
    out.push(Line::default());
}

fn push_user_card(
    out: &mut Vec<Line<'static>>,
    query: &str,
    width: usize,
    inner_w: usize,
) {
    let blank_fill = " ".repeat(width.saturating_sub(1));
    out.push(Line::from(vec![
        Span::styled("▎", Style::default().fg(ACCENT_CORAL).bg(BG_SURFACE)),
        Span::styled(blank_fill.clone(), Style::default().bg(BG_SURFACE)),
    ]));

    for line in wrap_plain_text(query.trim(), inner_w) {
        let pad = width.saturating_sub(3 + line.width());
        let mut spans = vec![
            Span::styled("▎", Style::default().fg(ACCENT_CORAL).bg(BG_SURFACE)),
            Span::styled("  ", Style::default().bg(BG_SURFACE)),
        ];
        spans.extend(styled_composer_line_spans(&line));
        spans.push(Span::styled(" ".repeat(pad), Style::default().bg(BG_SURFACE)));
        out.push(Line::from(spans));
    }

    out.push(Line::from(vec![
        Span::styled("▎", Style::default().fg(ACCENT_CORAL).bg(BG_SURFACE)),
        Span::styled(blank_fill, Style::default().bg(BG_SURFACE)),
    ]));
}

fn render_slash_popup(
    frame: &mut Frame,
    screen: Rect,
    composer_rect: Rect,
    st: &TuiState,
) {
    let max_items = st.slash_popup.matches.len().min(8);
    if max_items == 0 {
        return;
    }
    let popup_h = (max_items + 2) as u16;
    if composer_rect.y < screen.y + popup_h + 1 {
        return;
    }
    let popup_w = composer_rect.width.clamp(36, 72);
    let popup_rect = Rect {
        x: composer_rect.x,
        y: composer_rect.y - popup_h,
        width: popup_w,
        height: popup_h,
    };

    frame.render_widget(Clear, popup_rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER_SUBTLE).bg(BG_ELEVATED))
        .style(Style::default().bg(BG_ELEVATED))
        .title(Span::styled(
            " Commands ",
            Style::default().fg(TEXT_MUTED).bg(BG_ELEVATED),
        ));
    let inner = block.inner(popup_rect);
    frame.render_widget(block, popup_rect);

    let scroll = st
        .slash_popup
        .selected
        .saturating_sub(max_items.saturating_sub(1));
    let rows: Vec<Line<'static>> = st
        .slash_popup
        .matches
        .iter()
        .enumerate()
        .skip(scroll)
        .take(max_items)
        .map(|(idx, entry)| {
            let sel = idx == st.slash_popup.selected;
            let bg = if sel { BG_SELECTED } else { BG_ELEVATED };
            let name_fg = if sel { ACCENT_CORAL } else { TEXT_PRIMARY };
            let desc_w = (inner.width as usize).saturating_sub(entry.name.width() + 4);
            let desc = truncate_str(&entry.details, desc_w);
            let pad = (inner.width as usize)
                .saturating_sub(entry.name.width() + desc.width() + 3);
            Line::from(vec![
                Span::styled(
                    format!(" {} ", entry.name),
                    Style::default()
                        .fg(name_fg)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" ", Style::default().bg(bg)),
                Span::styled(desc, Style::default().fg(TEXT_MUTED).bg(bg)),
                Span::styled(" ".repeat(pad), Style::default().bg(bg)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), inner);
}

fn render_modal(frame: &mut Frame, screen: Rect, st: &TuiState, info: &FooterInfo<'_>) {
    match &st.modal {
        ActiveModal::None => {}
        ActiveModal::CommandPalette { query, selected } => {
            let items = filtered_palette_items(query);
            let w = screen.width.saturating_sub(6).clamp(44, 68);
            let h = screen.height.saturating_sub(4).clamp(10, 20);
            let rect = centered_rect(screen, w, h);
            let inner = draw_modal_frame(frame, rect, "Commands", "esc");

            let mut lines = Vec::new();
            lines.push(search_input_line(query, inner.width as usize));
            lines.push(Line::from(Span::styled(
                "─".repeat(inner.width as usize),
                Style::default().fg(BORDER_SUBTLE).bg(BG_ELEVATED),
            )));

            let list_h = (inner.height as usize).saturating_sub(2).max(1);
            let sel = (*selected).min(items.len().saturating_sub(1));
            let scroll = sel.saturating_sub(list_h.saturating_sub(1));

            for (idx, item) in items.iter().enumerate().skip(scroll).take(list_h) {
                let is_sel = idx == sel;
                let bg = if is_sel { BG_SELECTED } else { BG_ELEVATED };
                let left = format!(" {} ", item.title);
                let right = if item.shortcut.is_empty() {
                    format!(" {} ", item.category)
                } else {
                    format!(" {} ", item.shortcut)
                };
                let pad = (inner.width as usize).saturating_sub(left.width() + right.width());
                lines.push(Line::from(vec![
                    Span::styled(
                        left,
                        Style::default()
                            .fg(TEXT_PRIMARY)
                            .bg(bg)
                            .add_modifier(if is_sel {
                                Modifier::BOLD
                            } else {
                                Modifier::empty()
                            }),
                    ),
                    Span::styled(" ".repeat(pad), Style::default().bg(bg)),
                    Span::styled(
                        right,
                        Style::default()
                            .fg(if is_sel { ACCENT_CORAL } else { TEXT_MUTED })
                            .bg(bg),
                    ),
                ]));
            }
            if items.is_empty() {
                lines.push(Line::from(Span::styled(
                    " No matching commands",
                    Style::default().fg(TEXT_MUTED).bg(BG_ELEVATED),
                )));
            }
            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::SessionPicker {
            query,
            sessions,
            selected,
        } => {
            let filtered = filtered_sessions(sessions, query);
            let w = screen.width.saturating_sub(6).clamp(48, 76);
            let h = screen.height.saturating_sub(4).clamp(10, 20);
            let rect = centered_rect(screen, w, h);
            let inner = draw_modal_frame(frame, rect, "Sessions", "enter switch · esc");

            let mut lines = Vec::new();
            lines.push(search_input_line(query, inner.width as usize));
            lines.push(Line::from(Span::styled(
                "─".repeat(inner.width as usize),
                Style::default().fg(BORDER_SUBTLE).bg(BG_ELEVATED),
            )));

            let list_h = (inner.height as usize).saturating_sub(2).max(1);
            let sel = (*selected).min(filtered.len().saturating_sub(1));
            let scroll = sel.saturating_sub(list_h.saturating_sub(1));

            for (idx, sess) in filtered.iter().enumerate().skip(scroll).take(list_h) {
                let is_sel = idx == sel;
                let bg = if is_sel { BG_SELECTED } else { BG_ELEVATED };
                let is_cur = sess.id == info.session_id;
                let marker = if is_cur { "●" } else { " " };
                let label = if !sess.name.is_empty() {
                    sess.name.clone()
                } else if !sess.title.is_empty() {
                    sess.title.clone()
                } else {
                    "New session".to_string()
                };
                let right = format!(" {} msgs · {} ", sess.message_count, &sess.id[..sess.id.len().min(8)]);
                let max_name = (inner.width as usize).saturating_sub(right.width() + 4);
                let left = format!(" {marker} {}", truncate_str(&label, max_name));
                let pad = (inner.width as usize).saturating_sub(left.width() + right.width());
                lines.push(Line::from(vec![
                    Span::styled(
                        left,
                        Style::default()
                            .fg(if is_cur { ACCENT_CORAL } else { TEXT_PRIMARY })
                            .bg(bg),
                    ),
                    Span::styled(" ".repeat(pad), Style::default().bg(bg)),
                    Span::styled(right, Style::default().fg(TEXT_MUTED).bg(bg)),
                ]));
            }
            if filtered.is_empty() {
                lines.push(Line::from(Span::styled(
                    " No sessions match",
                    Style::default().fg(TEXT_MUTED).bg(BG_ELEVATED),
                )));
            }
            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::ProviderPicker {
            query,
            items,
            selected,
        } => {
            let filtered = filtered_providers(items, query);
            let w = screen.width.saturating_sub(6).clamp(50, 78);
            let h = screen.height.saturating_sub(4).clamp(10, 22);
            let rect = centered_rect(screen, w, h);
            let inner = draw_modal_frame(frame, rect, "Select Provider", "enter models · esc");

            let mut lines = Vec::new();
            lines.push(search_input_line(query, inner.width as usize));
            lines.push(Line::from(Span::styled(
                "─".repeat(inner.width as usize),
                Style::default().fg(BORDER_SUBTLE).bg(BG_ELEVATED),
            )));

            let list_h = (inner.height as usize).saturating_sub(2).max(1);
            let sel = (*selected).min(filtered.len().saturating_sub(1));
            let scroll = sel.saturating_sub(list_h.saturating_sub(1));

            for (idx, item) in filtered.iter().enumerate().skip(scroll).take(list_h) {
                let is_sel = idx == sel;
                let bg = if is_sel { BG_SELECTED } else { BG_ELEVATED };
                let marker = if item.is_current { "●" } else { " " };
                let right = if item.tag.is_empty() {
                    format!(" {} ", item.id)
                } else {
                    format!(" {} · {} ", item.id, item.tag)
                };
                let max_right = (inner.width as usize) / 2;
                let right_disp = truncate_str(&right, max_right.max(12));
                let max_name = (inner.width as usize).saturating_sub(right_disp.width() + 4);
                let left = format!(" {marker} {}", truncate_str(&item.display_name, max_name));
                let pad = (inner.width as usize).saturating_sub(left.width() + right_disp.width());
                lines.push(Line::from(vec![
                    Span::styled(
                        left,
                        Style::default()
                            .fg(if item.is_current {
                                ACCENT_CORAL
                            } else {
                                TEXT_PRIMARY
                            })
                            .bg(bg),
                    ),
                    Span::styled(" ".repeat(pad), Style::default().bg(bg)),
                    Span::styled(right_disp, Style::default().fg(TEXT_MUTED).bg(bg)),
                ]));
            }
            if filtered.is_empty() {
                lines.push(Line::from(Span::styled(
                    " No providers match",
                    Style::default().fg(TEXT_MUTED).bg(BG_ELEVATED),
                )));
            }
            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::ModelPicker {
            provider_display,
            query,
            items,
            selected,
            ..
        } => {
            let filtered = filtered_models(items, query);
            let w = screen.width.saturating_sub(6).clamp(50, 78);
            let h = screen.height.saturating_sub(4).clamp(10, 22);
            let rect = centered_rect(screen, w, h);
            let title = format!("Models · {provider_display}");
            let inner = draw_modal_frame(
                frame,
                rect,
                &title,
                "enter select · ← providers · esc",
            );

            let mut lines = Vec::new();
            lines.push(search_input_line(query, inner.width as usize));
            lines.push(Line::from(Span::styled(
                "─".repeat(inner.width as usize),
                Style::default().fg(BORDER_SUBTLE).bg(BG_ELEVATED),
            )));

            let list_h = (inner.height as usize).saturating_sub(2).max(1);
            let sel = (*selected).min(filtered.len().saturating_sub(1));
            let scroll = sel.saturating_sub(list_h.saturating_sub(1));

            for (idx, item) in filtered.iter().enumerate().skip(scroll).take(list_h) {
                let is_sel = idx == sel;
                let bg = if is_sel { BG_SELECTED } else { BG_ELEVATED };
                let marker = if item.is_current { "●" } else { " " };
                let right = if item.model != item.display_name {
                    format!(" {} ", item.model)
                } else {
                    String::new()
                };
                let max_right = (inner.width as usize) / 2;
                let right_disp = if right.is_empty() {
                    String::new()
                } else {
                    truncate_str(&right, max_right.max(12))
                };
                let max_name = (inner.width as usize).saturating_sub(right_disp.width() + 4);
                let left = format!(" {marker} {}", truncate_str(&item.display_name, max_name));
                let pad = (inner.width as usize).saturating_sub(left.width() + right_disp.width());
                lines.push(Line::from(vec![
                    Span::styled(
                        left,
                        Style::default()
                            .fg(if item.is_current {
                                ACCENT_CORAL
                            } else {
                                TEXT_PRIMARY
                            })
                            .bg(bg),
                    ),
                    Span::styled(" ".repeat(pad), Style::default().bg(bg)),
                    Span::styled(right_disp, Style::default().fg(TEXT_MUTED).bg(bg)),
                ]));
            }
            if filtered.is_empty() {
                lines.push(Line::from(Span::styled(
                    " No models match",
                    Style::default().fg(TEXT_MUTED).bg(BG_ELEVATED),
                )));
            }
            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::ThinkingPicker { current, selected } => {
            let options = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
            let rect = centered_rect(screen, 42, 11);
            let inner = draw_modal_frame(frame, rect, "Thinking Level", "enter · esc");
            let cur_str = current
                .map(|e| e.to_string())
                .unwrap_or_else(|| "off".to_string());
            let lines: Vec<Line<'static>> = options
                .iter()
                .enumerate()
                .map(|(idx, opt)| {
                    let is_sel = idx == *selected;
                    let is_cur = *opt == cur_str;
                    let bg = if is_sel { BG_SELECTED } else { BG_ELEVATED };
                    let marker = if is_cur { "●" } else { " " };
                    let text = format!(" {marker} {opt}");
                    let pad = (inner.width as usize).saturating_sub(text.width());
                    Line::from(vec![
                        Span::styled(
                            text,
                            Style::default()
                                .fg(if is_cur { ACCENT_CORAL } else { TEXT_PRIMARY })
                                .bg(bg),
                        ),
                        Span::styled(" ".repeat(pad), Style::default().bg(bg)),
                    ])
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::Approval { req, selected, .. } => {
            let w = screen.width.saturating_sub(6).clamp(50, 82);
            let h = screen.height.saturating_sub(4).clamp(12, 22);
            let rect = centered_rect(screen, w, h);
            let inner = draw_modal_frame(frame, rect, "Approve Tool Call", "↑/↓ · enter");

            let mut lines = Vec::new();
            lines.push(Line::from(vec![
                Span::styled(" Tool: ", Style::default().fg(TEXT_MUTED).bg(BG_ELEVATED)),
                Span::styled(
                    req.tool.clone(),
                    Style::default()
                        .fg(ACCENT_CORAL)
                        .bg(BG_ELEVATED)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            if let Some(ref target) = req.target {
                lines.push(Line::from(Span::styled(
                    format!(" Target: {}", truncate_str(target, (inner.width as usize).saturating_sub(10))),
                    Style::default().fg(TEXT_PRIMARY).bg(BG_ELEVATED),
                )));
            }
            lines.push(Line::default());
            if let Some(ref diff) = req.diff {
                let preview_budget = (inner.height as usize).saturating_sub(7);
                for dline in diff.lines().take(preview_budget) {
                    let color = if dline.starts_with('+') {
                        GREEN_OK
                    } else if dline.starts_with('-') {
                        RED_ERR
                    } else {
                        TEXT_MUTED
                    };
                    lines.push(Line::from(Span::styled(
                        format!(" {}", truncate_str(dline, (inner.width as usize).saturating_sub(2))),
                        Style::default().fg(color).bg(BG_ELEVATED),
                    )));
                }
                lines.push(Line::default());
            }

            let choices = ["Allow once", "Allow for this session (Auto mode)", "Deny"];
            for (idx, ch) in choices.iter().enumerate() {
                let is_sel = idx == *selected;
                let bg = if is_sel { BG_SELECTED } else { BG_ELEVATED };
                let prefix = if is_sel { " ▸ " } else { "   " };
                let line = format!("{prefix}{ch}");
                let pad = (inner.width as usize).saturating_sub(line.width());
                lines.push(Line::from(vec![
                    Span::styled(
                        line,
                        Style::default()
                            .fg(if idx == 2 && is_sel {
                                RED_ERR
                            } else if is_sel {
                                ACCENT_CORAL
                            } else {
                                TEXT_PRIMARY
                            })
                            .bg(bg),
                    ),
                    Span::styled(" ".repeat(pad), Style::default().bg(bg)),
                ]));
            }
            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::Clarify {
            req,
            question_idx,
            selected,
            custom,
            editing_custom,
            ..
        } => {
            let w = screen.width.saturating_sub(6).clamp(48, 76);
            let h = screen.height.saturating_sub(4).clamp(12, 20);
            let rect = centered_rect(screen, w, h);
            let title = format!("Question {}/{}", question_idx + 1, req.questions.len().max(1));
            let inner = draw_modal_frame(frame, rect, &title, "enter · esc");

            let mut lines = Vec::new();
            if let Some(q) = req.questions.get(*question_idx) {
                for wline in wrap_plain_text(&q.question, (inner.width as usize).saturating_sub(2)) {
                    lines.push(Line::from(Span::styled(
                        format!(" {wline}"),
                        Style::default()
                            .fg(TEXT_PRIMARY)
                            .bg(BG_ELEVATED)
                            .add_modifier(Modifier::BOLD),
                    )));
                }
                lines.push(Line::default());
                for (idx, choice) in q.choices.iter().enumerate() {
                    let is_sel = !*editing_custom && idx == *selected;
                    let bg = if is_sel { BG_SELECTED } else { BG_ELEVATED };
                    let text = format!(
                        " {} {}",
                        if is_sel { "▸" } else { " " },
                        truncate_str(choice, (inner.width as usize).saturating_sub(4))
                    );
                    let pad = (inner.width as usize).saturating_sub(text.width());
                    lines.push(Line::from(vec![
                        Span::styled(
                            text,
                            Style::default()
                                .fg(if is_sel { ACCENT_CORAL } else { TEXT_PRIMARY })
                                .bg(bg),
                        ),
                        Span::styled(" ".repeat(pad), Style::default().bg(bg)),
                    ]));
                }
                let custom_idx = q.choices.len();
                let is_sel = *editing_custom || *selected == custom_idx;
                let bg = if is_sel { BG_SELECTED } else { BG_ELEVATED };
                let label = if *editing_custom {
                    format!(" ▸ Answer: {custom}_")
                } else {
                    " ▸ Type your answer...".to_string()
                };
                let pad = (inner.width as usize).saturating_sub(label.width());
                lines.push(Line::from(vec![
                    Span::styled(
                        label,
                        Style::default()
                            .fg(if is_sel { ACCENT_CORAL } else { TEXT_MUTED })
                            .bg(bg),
                    ),
                    Span::styled(" ".repeat(pad), Style::default().bg(bg)),
                ]));
            }
            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::Sudo { prompt, input, .. } => {
            let w = screen.width.saturating_sub(6).clamp(44, 68);
            let rect = centered_rect(screen, w, 7);
            let inner = draw_modal_frame(frame, rect, "Authentication Required", "enter · esc");
            let masked = "*".repeat(input.chars().count());
            let lines = vec![
                Line::from(Span::styled(
                    format!(" {}", truncate_str(prompt, (inner.width as usize).saturating_sub(2))),
                    Style::default().fg(TEXT_PRIMARY).bg(BG_ELEVATED),
                )),
                Line::default(),
                Line::from(Span::styled(
                    format!(" Password: {masked}_"),
                    Style::default().fg(ACCENT_CORAL).bg(BG_ELEVATED),
                )),
            ];
            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::Help { scroll } => {
            let w = screen.width.saturating_sub(6).clamp(48, 72);
            let h = screen.height.saturating_sub(4).clamp(14, 24);
            let rect = centered_rect(screen, w, h);
            let inner = draw_modal_frame(frame, rect, "Keyboard & Commands", "esc");
            let help_rows = [
                ("ctrl+p", "Open command palette"),
                ("shift+tab", "Cycle agent mode (Build / Auto / Plan)"),
                ("ctrl+n", "Open a new session tab"),
                ("ctrl+o", "Expand/collapse reasoning & tool details"),
                ("ctrl+t", "Toggle todo panel"),
                ("ctrl+v", "Paste clipboard image or text"),
                ("shift+enter / alt+enter", "Insert newline in composer"),
                ("pageup / pagedown / scroll", "Scroll transcript"),
                ("esc", "Interrupt running turn / close dialog"),
                ("ctrl+c (x2)", "Exit Luna"),
                ("", ""),
                ("/model [name]", "Switch provider and model"),
                ("/thinking [level]", "Set reasoning effort (off..max)"),
                ("/sessions", "Switch between saved sessions"),
                ("/new", "Start a new session"),
                ("/compact", "Compact conversation context"),
                ("/diff", "Show turn file diffs"),
                ("/usage", "Show token usage summary"),
                ("/jobs", "View background tasks and subagents"),
            ];
            let visible_h = inner.height as usize;
            let lines: Vec<Line<'static>> = help_rows
                .iter()
                .skip(*scroll)
                .take(visible_h)
                .map(|(k, v)| {
                    if k.is_empty() {
                        return Line::default();
                    }
                    Line::from(vec![
                        Span::styled(
                            format!(" {:<28}", k),
                            Style::default().fg(ACCENT_CORAL).bg(BG_ELEVATED),
                        ),
                        Span::styled(*v, Style::default().fg(TEXT_PRIMARY).bg(BG_ELEVATED)),
                    ])
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), inner);
        }
    }
}

fn draw_modal_frame(frame: &mut Frame, rect: Rect, title: &str, hint: &str) -> Rect {
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(BORDER_SUBTLE).bg(BG_ELEVATED))
        .style(Style::default().bg(BG_ELEVATED))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(TEXT_PRIMARY)
                .bg(BG_ELEVATED)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(
            Line::from(Span::styled(
                format!(" {hint} "),
                Style::default().fg(TEXT_MUTED).bg(BG_ELEVATED),
            ))
            .right_aligned(),
        );
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    inner
}

fn search_input_line(query: &str, width: usize) -> Line<'static> {
    let prefix = " > ";
    let shown = if query.is_empty() {
        "Search..."
    } else {
        query
    };
    let color = if query.is_empty() {
        TEXT_MUTED
    } else {
        TEXT_PRIMARY
    };
    let pad = width.saturating_sub(prefix.width() + shown.width());
    Line::from(vec![
        Span::styled(prefix, Style::default().fg(ACCENT_CORAL).bg(BG_ELEVATED)),
        Span::styled(shown.to_string(), Style::default().fg(color).bg(BG_ELEVATED)),
        Span::styled(" ".repeat(pad), Style::default().bg(BG_ELEVATED)),
    ])
}

fn centered_rect(area: Rect, w: u16, h: u16) -> Rect {
    let width = w.min(area.width);
    let height = h.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}

pub fn all_palette_items() -> Vec<PaletteItem> {
    vec![
        PaletteItem {
            title: "New session".into(),
            shortcut: "ctrl+n".into(),
            category: "Session".into(),
            action: PaletteAction::NewSession,
        },
        PaletteItem {
            title: "Switch session".into(),
            shortcut: "/sessions".into(),
            category: "Session".into(),
            action: PaletteAction::SwitchSession,
        },
        PaletteItem {
            title: "Switch model".into(),
            shortcut: "/model".into(),
            category: "Model".into(),
            action: PaletteAction::SwitchModel,
        },
        PaletteItem {
            title: "Cycle agent mode".into(),
            shortcut: "shift+tab".into(),
            category: "Agent".into(),
            action: PaletteAction::CycleAgentMode,
        },
        PaletteItem {
            title: "Set thinking level".into(),
            shortcut: "/thinking".into(),
            category: "Model".into(),
            action: PaletteAction::SelectThinking,
        },
        PaletteItem {
            title: "Toggle thinking & tool details".into(),
            shortcut: "ctrl+o".into(),
            category: "View".into(),
            action: PaletteAction::ToggleProcessDetails,
        },
        PaletteItem {
            title: "Toggle todo panel".into(),
            shortcut: "ctrl+t".into(),
            category: "View".into(),
            action: PaletteAction::ToggleTodoPanel,
        },
        PaletteItem {
            title: "Compact conversation context".into(),
            shortcut: "/compact".into(),
            category: "Session".into(),
            action: PaletteAction::CompactContext,
        },
        PaletteItem {
            title: "Show session file diffs".into(),
            shortcut: "/diff".into(),
            category: "Tools".into(),
            action: PaletteAction::ShowDiff,
        },
        PaletteItem {
            title: "Show token usage".into(),
            shortcut: "/usage".into(),
            category: "Tools".into(),
            action: PaletteAction::ShowUsage,
        },
        PaletteItem {
            title: "Background jobs & subagents".into(),
            shortcut: "/jobs".into(),
            category: "Tools".into(),
            action: PaletteAction::ShowBackgroundJobs,
        },
        PaletteItem {
            title: "Workspace directory".into(),
            shortcut: "/dir".into(),
            category: "Config".into(),
            action: PaletteAction::SlashCommand("/dir".into()),
        },
        PaletteItem {
            title: "Settings".into(),
            shortcut: "/settings".into(),
            category: "Config".into(),
            action: PaletteAction::SlashCommand("/settings".into()),
        },
        PaletteItem {
            title: "Keyboard shortcuts & help".into(),
            shortcut: "/help".into(),
            category: "Help".into(),
            action: PaletteAction::ShowHelp,
        },
        PaletteItem {
            title: "Quit Luna".into(),
            shortcut: "/exit".into(),
            category: "App".into(),
            action: PaletteAction::Quit,
        },
    ]
}

pub fn filtered_palette_items(query: &str) -> Vec<PaletteItem> {
    let q = query.trim().to_ascii_lowercase();
    all_palette_items()
        .into_iter()
        .filter(|item| {
            q.is_empty()
                || item.title.to_ascii_lowercase().contains(&q)
                || item.category.to_ascii_lowercase().contains(&q)
                || item.shortcut.to_ascii_lowercase().contains(&q)
        })
        .collect()
}

pub fn filtered_sessions<'a>(
    sessions: &'a [harness_core::SessionRecord],
    query: &str,
) -> Vec<&'a harness_core::SessionRecord> {
    let q = query.trim().to_ascii_lowercase();
    sessions
        .iter()
        .filter(|s| {
            q.is_empty()
                || s.name.to_ascii_lowercase().contains(&q)
                || s.title.to_ascii_lowercase().contains(&q)
                || s.id.to_ascii_lowercase().contains(&q)
        })
        .collect()
}

pub fn filtered_providers<'a>(
    items: &'a [ProviderPickerItem],
    query: &str,
) -> Vec<&'a ProviderPickerItem> {
    let q = query.trim().to_ascii_lowercase();
    items
        .iter()
        .filter(|p| {
            q.is_empty()
                || p.id.to_ascii_lowercase().contains(&q)
                || p.display_name.to_ascii_lowercase().contains(&q)
                || p.tag.to_ascii_lowercase().contains(&q)
        })
        .collect()
}

pub fn filtered_models<'a>(items: &'a [ModelPickerItem], query: &str) -> Vec<&'a ModelPickerItem> {
    let q = query.trim().to_ascii_lowercase();
    items
        .iter()
        .filter(|m| {
            q.is_empty()
                || m.model.to_ascii_lowercase().contains(&q)
                || m.provider.to_ascii_lowercase().contains(&q)
                || m.display_name.to_ascii_lowercase().contains(&q)
        })
        .collect()
}

struct WrappedComposer {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
}

fn styled_composer_line_spans(line: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = line;
    while !rest.is_empty() {
        let img_pos = rest.find("[Image ").filter(|&p| rest[p..].contains(']'));
        let paste_pos = rest.find("[Pasted ~").filter(|&p| rest[p..].contains(']'));
        let next = match (img_pos, paste_pos) {
            (Some(a), Some(b)) => Some((a.min(b), a <= b)),
            (Some(a), None) => Some((a, true)),
            (None, Some(b)) => Some((b, false)),
            (None, None) => None,
        };
        let Some((pos, is_image)) = next else {
            spans.push(Span::styled(
                rest.to_string(),
                Style::default().fg(TEXT_PRIMARY).bg(BG_SURFACE),
            ));
            break;
        };
        if pos > 0 {
            spans.push(Span::styled(
                rest[..pos].to_string(),
                Style::default().fg(TEXT_PRIMARY).bg(BG_SURFACE),
            ));
        }
        let after = &rest[pos..];
        if let Some(end) = after.find(']') {
            let token = &after[..=end];
            let fg = if is_image { ACCENT_CORAL } else { YELLOW_WARN };
            spans.push(Span::styled(
                token.to_string(),
                Style::default()
                    .fg(fg)
                    .bg(BG_ELEVATED)
                    .add_modifier(Modifier::BOLD),
            ));
            rest = &after[end + 1..];
        } else {
            spans.push(Span::styled(
                after.to_string(),
                Style::default().fg(TEXT_PRIMARY).bg(BG_SURFACE),
            ));
            break;
        }
    }
    spans
}

fn wrap_composer_lines(text: &str, cursor_byte: usize, max_width: usize) -> WrappedComposer {
    let width = max_width.max(8);
    let mut lines = Vec::new();
    let mut cursor_line = 0usize;
    let mut cursor_col = 0usize;
    let mut byte_pos = 0usize;
    let mut found_cursor = false;

    for (raw_idx, raw_line) in text.split('\n').enumerate() {
        if raw_idx > 0 {
            byte_pos += 1; // `\n`
        }
        if raw_line.is_empty() {
            if !found_cursor && cursor_byte <= byte_pos {
                cursor_line = lines.len();
                cursor_col = 0;
                found_cursor = true;
            }
            lines.push(String::new());
            continue;
        }
        let mut cur = String::new();
        let mut cur_w = 0usize;
        for ch in raw_line.chars() {
            let ch_len = ch.len_utf8();
            let cw = ch.width().unwrap_or(0);
            if cur_w + cw > width && !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
                cur_w = 0;
            }
            if !found_cursor && cursor_byte == byte_pos {
                cursor_line = lines.len();
                cursor_col = cur_w;
                found_cursor = true;
            }
            cur.push(ch);
            cur_w += cw;
            byte_pos += ch_len;
        }
        if !found_cursor && cursor_byte == byte_pos {
            cursor_line = lines.len();
            cursor_col = cur_w;
            found_cursor = true;
        }
        lines.push(cur);
    }

    if lines.is_empty() {
        lines.push(String::new());
    }
    if !found_cursor {
        cursor_line = lines.len().saturating_sub(1);
        cursor_col = lines.last().map(|l| l.width()).unwrap_or(0);
    }
    WrappedComposer {
        lines,
        cursor_line,
        cursor_col,
    }
}

fn truncate_str(s: &str, max_cells: usize) -> String {
    if s.width() <= max_cells {
        return s.to_string();
    }
    if max_cells == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw + 1 > max_cells {
            out.push('…');
            break;
        }
        out.push(ch);
        w += cw;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::time::Duration;

    use super::super::super::render::{TurnProcessItem, TurnRecord};

    fn line_text(line: &Line<'_>) -> String {
        line.spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>()
    }

    #[test]
    fn active_turn_renders_new_thinking_after_completed_tools() {
        let in_flight = vec![
            TurnProcessItem::thinking_with_duration("round 1 reasoning", Some(1.4)),
            TurnProcessItem {
                tool_name: "list_files".to_string(),
                call_formatted: "list_files(path: \"\")".to_string(),
                result_formatted: "✔ listed 5 entries".to_string(),
                diff_lines: Vec::new(),
                output: String::new(),
                output_dropped: 0,
                arguments: Some(serde_json::json!({ "path": "." })),
                duration_secs: Some(0.2),
                is_error: false,
            },
            TurnProcessItem::thinking_with_duration("round 2 reasoning", Some(2.0)),
            TurnProcessItem {
                tool_name: "terminal".to_string(),
                call_formatted: "run ls -la".to_string(),
                result_formatted: "✔ exit 0".to_string(),
                diff_lines: Vec::new(),
                output: "line1\nline2\nline3\nline4\nline5\nline6\nline7\nline8".to_string(),
                output_dropped: 185,
                arguments: Some(serde_json::json!({ "command": "ls -la /home/janelle" })),
                duration_secs: Some(0.4),
                is_error: false,
            },
        ];
        let snap = TuiLiveSnapshot {
            reasoning: "round 3 active reasoning".to_string(),
            reasoning_secs: Some(0.8),
            ..Default::default()
        };
        let mut out = Vec::new();
        let mut hits = Vec::new();
        let mut collapsed_steps = HashSet::new();
        collapsed_steps.insert(0);
        let expanded_tools = HashSet::new();

        render_active_turn_process(
            &mut out,
            &mut hits,
            &snap,
            &in_flight,
            false,
            &collapsed_steps,
            &expanded_tools,
            LiveThinkingView::Tail,
            0,
            4.2,
            84,
            80,
        );

        let rendered: Vec<String> = out.iter().map(line_text).collect();
        let joined = rendered.join("\n");

        let pos_t1 = joined.find("▸ Thought: 1.4s (1 line)").unwrap();
        let pos_tool1 = joined.find("→ Explored: 1 list").unwrap();
        let pos_t2 = joined.find("Thought: 2.0s").unwrap();
        let pos_t2_body = joined.find("round 2 reasoning").unwrap();
        let pos_tool2_cmd = joined.find("$ ls -la /home/janelle").unwrap();
        let pos_tool2_hidden = joined.find("(186 earlier lines)").unwrap();
        let pos_t3_hdr = joined.find("Thought: 0.8s").unwrap();
        let pos_t3_body = joined.find("round 3 active reasoning").unwrap();

        assert!(
            pos_t1 < pos_tool1
                && pos_tool1 < pos_t2
                && pos_t2 < pos_t2_body
                && pos_t2_body < pos_tool2_cmd
                && pos_tool2_cmd < pos_tool2_hidden
                && pos_tool2_hidden < pos_t3_hdr
                && pos_t3_hdr < pos_t3_body,
            "chronological order violated:\n{joined}"
        );
        // Verify shell card uses BG_SURFACE
        let card_line = out
            .iter()
            .find(|l| line_text(l).contains("$ ls -la /home/janelle"))
            .unwrap();
        assert!(card_line.spans.iter().all(|s| s.style.bg == Some(BG_SURFACE)));
        assert!(hits
            .iter()
            .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleLiveThinkingStep(0))));
        assert!(hits
            .iter()
            .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleLiveThinkingStep(2))));
        assert!(hits
            .iter()
            .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleLiveToolStep(3))));
        assert!(hits
            .iter()
            .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleLiveThinking)));
    }

    #[test]
    fn completed_turn_renders_interleaved_thinking_prose_and_tool_cards() {
        let turn = TurnRecord {
            user_query: "what is this repo about".to_string(),
            reasoning: None,
            tools: Vec::new(),
            tool_items: vec![
                TurnProcessItem::thinking_with_duration("first thought", Some(1.1)),
                TurnProcessItem {
                    tool_name: "list_files".to_string(),
                    call_formatted: "list_files(path: \"\")".to_string(),
                    result_formatted: "✔ ok".to_string(),
                    diff_lines: Vec::new(),
                    output: String::new(),
                    output_dropped: 0,
                    arguments: Some(serde_json::json!({ "path": "." })),
                    duration_secs: None,
                    is_error: false,
                },
                TurnProcessItem::thinking_with_duration("second thought", Some(2.0)),
                TurnProcessItem::assistant_text("Found it. Let me inspect."),
                TurnProcessItem {
                    tool_name: "read_file".to_string(),
                    call_formatted: "read_file(path: \"README.md\")".to_string(),
                    result_formatted: "✔ ok".to_string(),
                    diff_lines: Vec::new(),
                    output: String::new(),
                    output_dropped: 0,
                    arguments: Some(serde_json::json!({ "path": "README.md" })),
                    duration_secs: None,
                    is_error: false,
                },
                TurnProcessItem::thinking_with_duration("final thought", Some(0.5)),
            ],
            duration: Duration::from_secs(3),
            reply: "Done".to_string(),
        };

        let mut out = Vec::new();
        let mut hits = Vec::new();
        let mut collapsed_thinking = HashSet::new();
        collapsed_thinking.insert((0, 0));
        let expanded_tools = HashSet::new();

        render_completed_turn_process(
            &mut out,
            &mut hits,
            0,
            &turn,
            false,
            &collapsed_thinking,
            &expanded_tools,
            84,
            80,
        );

        let joined = out.iter().map(line_text).collect::<Vec<_>>().join("\n");
        let p_think0 = joined.find("▸ Thought: 1.1s (1 line)").unwrap();
        let p_tools0 = joined.find("→ Explored: 1 list").unwrap();
        let p_think1 = joined.find("second thought").unwrap();
        let p_prose = joined.find("Found it. Let me inspect.").unwrap();
        let p_tools1 = joined.find("→ Explored: 1 read").unwrap();
        let p_think2 = joined[p_tools1..]
            .find("final thought")
            .map(|rel| p_tools1 + rel)
            .unwrap();

        assert!(
            p_think0 < p_tools0
                && p_tools0 < p_think1
                && p_think1 < p_prose
                && p_prose < p_tools1
                && p_tools1 < p_think2,
            "completed turn chronological order violated:\n{joined}"
        );
        assert!(hits
            .iter()
            .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleTurnThinking(0, 0))));
        assert!(hits
            .iter()
            .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleTurnTools(0, 0))));
        assert!(hits
            .iter()
            .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleTurnThinking(0, 1))));
        assert!(hits
            .iter()
            .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleTurnTools(0, 1))));
        assert!(hits
            .iter()
            .any(|(_, t)| matches!(t, TranscriptClickTarget::ToggleTurnThinking(0, 2))));
    }

    #[test]
    fn composer_line_highlights_image_and_pasted_tokens() {
        let spans = styled_composer_line_spans("fix [Image 1] and [Pasted ~12 lines] now");
        assert_eq!(spans.len(), 5);
        assert_eq!(spans[0].content, "fix ");
        assert_eq!(spans[1].content, "[Image 1]");
        assert_eq!(spans[1].style.fg, Some(ACCENT_CORAL));
        assert_eq!(spans[2].content, " and ");
        assert_eq!(spans[3].content, "[Pasted ~12 lines]");
        assert_eq!(spans[3].style.fg, Some(YELLOW_WARN));
        assert_eq!(spans[4].content, " now");
    }
}
