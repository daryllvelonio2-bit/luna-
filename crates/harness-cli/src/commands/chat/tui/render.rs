//! Full-screen `ratatui` renderer matching OpenCode's TUI layout and visual design.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use harness_core::ReasoningEffort;

use super::super::render::{format_elapsed_secs, reply_for_display, SPINNER_FRAMES};
use super::super::ui::TuiLiveSnapshot;
use super::markdown::{render_markdown, strip_ansi, wrap_plain_text};
use super::state::{
    ActiveModal, ModelPickerItem, PaletteAction, PaletteItem, ProviderPickerItem, TabHitBox,
    TabHitTarget, TuiState,
};
use super::theme::{
    friendly_model_name, friendly_provider_name, humanize_tokens, mode_accent, ACCENT_CORAL,
    BG_CANVAS, BG_ELEVATED, BG_SELECTED, BG_SURFACE, BORDER_SUBTLE, GREEN_OK, LOGO_GRAY, RED_ERR,
    TEXT_DIM, TEXT_MUTED, TEXT_PRIMARY, YELLOW_WARN, LUNA_LOGO_LEFT, LUNA_LOGO_RIGHT,
};

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

    // Estimate widths and drop earliest tabs if overflowing.
    let tab_display_width = |title: &str, is_active: bool| -> u16 {
        let truncated = truncate_str(title, 18);
        if is_active {
            (truncated.width() + 4) as u16
        } else {
            (truncated.width() + 3) as u16
        }
    };

    let mut start_idx = 0usize;
    loop {
        let total_w: u16 = visible_tabs[start_idx..]
            .iter()
            .map(|(_, t)| tab_display_width(&t.title, t.id == active_session_id))
            .sum();
        if cursor_x + total_w <= max_tabs_x || start_idx >= active_pos {
            break;
        }
        start_idx += 1;
    }

    let slice = &visible_tabs[start_idx..];
    let slice_len = slice.len();
    for (rel_idx, (_, tab)) in slice.iter().enumerate() {
        let is_active = tab.id == active_session_id;
        let truncated = truncate_str(&tab.title, 18);
        let label = if is_active {
            format!("  {truncated}  ")
        } else {
            format!(" {truncated}  ")
        };
        let w = label.width() as u16;
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

        st.hit_boxes.push(TabHitBox {
            x_start: cursor_x,
            x_end: cursor_x + w,
            target: TabHitTarget::Session(tab.id.clone()),
        });
        spans.push(Span::styled(label, style));
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

    // Build transcript lines
    let mut lines = build_transcript_lines(st, snap, info, content_w as usize);
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
    let visible_slice: Vec<Line<'static>> = if start < end {
        lines.drain(start..end).collect()
    } else {
        Vec::new()
    };

    frame.render_widget(Paragraph::new(visible_slice), transcript_rect);

    // Scroll indicator if scrolled up
    if st.scroll_from_bottom > 0 && transcript_rect.width > 16 {
        let badge = format!(" ↑ {} lines (End) ", st.scroll_from_bottom);
        let bw = badge.width() as u16;
        if transcript_rect.width > bw {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    badge,
                    Style::default().fg(TEXT_PRIMARY).bg(BG_ELEVATED),
                ))),
                Rect {
                    x: transcript_rect.x + transcript_rect.width - bw,
                    y: transcript_rect.y,
                    width: bw,
                    height: 1,
                },
            );
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
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("▎", Style::default().fg(accent).bg(BG_SURFACE)),
                    Span::styled(
                        format!("  {line_str}"),
                        Style::default().fg(TEXT_PRIMARY).bg(BG_SURFACE),
                    ),
                ])),
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
    context_label: Option<&str>,
    is_landing: bool,
) {
    let mut right_spans: Vec<Span<'static>> = Vec::new();
    if let Some(ctx) = context_label {
        right_spans.push(Span::styled(
            format!("{ctx}   "),
            Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
        ));
    }
    if st.active_turn.is_some() {
        right_spans.push(Span::styled(
            "esc",
            Style::default().fg(TEXT_PRIMARY).bg(BG_CANVAS),
        ));
        right_spans.push(Span::styled(
            " interrupt   ",
            Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
        ));
    } else if is_landing {
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
    let max_left = (rect.width as usize).saturating_sub(right_w + 2);
    let left_text = truncate_str(&st.cwd_label, max_left);
    let left_w = left_text.width();
    let spacer = (rect.width as usize).saturating_sub(left_w + right_w);

    let mut spans = vec![
        Span::styled(left_text, Style::default().fg(TEXT_MUTED).bg(BG_CANVAS)),
        Span::styled(" ".repeat(spacer), Style::default().bg(BG_CANVAS)),
    ];
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
) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let card_inner_w = width.saturating_sub(4).max(12);

    for (idx, turn) in st.turns.iter().enumerate() {
        // 1. User message card (`#1e1e20` + coral `▎` left bar)
        if !turn.user_query.trim().is_empty() {
            push_user_card(&mut out, &turn.user_query, width, card_inner_w);
            out.push(Line::default());
        }

        // 2. Reasoning (collapsible with Ctrl+O)
        if let Some(ref reasoning) = turn.reasoning {
            let trimmed = reasoning.trim();
            if !trimmed.is_empty() {
                if st.process_expanded {
                    for rline in render_markdown(trimmed, card_inner_w) {
                        let mut spans = vec![Span::styled(
                            "  │ ",
                            Style::default().fg(TEXT_DIM).bg(BG_CANVAS),
                        )];
                        for s in rline.spans {
                            spans.push(Span::styled(
                                s.content,
                                s.style.fg(TEXT_MUTED).add_modifier(Modifier::ITALIC),
                            ));
                        }
                        out.push(Line::from(spans));
                    }
                    out.push(Line::default());
                } else {
                    let line_count = trimmed.lines().count();
                    out.push(Line::from(Span::styled(
                        format!("  ▸ Thinking ({line_count} lines · ctrl+o to expand)"),
                        Style::default().fg(TEXT_DIM).bg(BG_CANVAS),
                    )));
                }
            }
        }

        // 3. Tool calls
        if !turn.tool_items.is_empty() {
            if st.process_expanded {
                for item in &turn.tool_items {
                    let call_clean = strip_ansi(&item.call_formatted);
                    out.push(Line::from(Span::styled(
                        format!("  ▸ {}", truncate_str(&call_clean, card_inner_w)),
                        Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
                    )));
                    if !item.result_formatted.is_empty() {
                        let res_clean = strip_ansi(&item.result_formatted);
                        let ok = !res_clean.contains('✖');
                        let color = if ok { GREEN_OK } else { RED_ERR };
                        out.push(Line::from(Span::styled(
                            format!("    {}", truncate_str(&res_clean, card_inner_w.saturating_sub(2))),
                            Style::default().fg(color).bg(BG_CANVAS),
                        )));
                    }
                    for diff in item.diff_lines.iter().take(12) {
                        let clean = strip_ansi(diff);
                        let color = if clean.trim_start().starts_with('+') {
                            GREEN_OK
                        } else if clean.trim_start().starts_with('-') {
                            RED_ERR
                        } else {
                            TEXT_DIM
                        };
                        out.push(Line::from(Span::styled(
                            format!("    {}", truncate_str(&clean, card_inner_w.saturating_sub(2))),
                            Style::default().fg(color).bg(BG_CANVAS),
                        )));
                    }
                    if !item.output.is_empty() {
                        for oline in item.output.lines().take(12) {
                            let clean = strip_ansi(oline);
                            out.push(Line::from(Span::styled(
                                format!("    │ {}", truncate_str(&clean, card_inner_w.saturating_sub(4))),
                                Style::default().fg(TEXT_DIM).bg(BG_CANVAS),
                            )));
                        }
                    }
                }
                out.push(Line::default());
            } else {
                let names: Vec<&str> = turn
                    .tool_items
                    .iter()
                    .map(|t| t.tool_name.as_str())
                    .collect();
                let preview = if names.len() <= 4 {
                    names.join(", ")
                } else {
                    format!("{}, +{} more", names[..3].join(", "), names.len() - 3)
                };
                out.push(Line::from(Span::styled(
                    format!("  ▸ {} tool(s): {preview} (ctrl+o to expand)", names.len()),
                    Style::default().fg(TEXT_DIM).bg(BG_CANVAS),
                )));
            }
        }

        // 4. Assistant reply (markdown)
        let display_reply = reply_for_display(&turn.reply);
        if !display_reply.trim().is_empty() {
            if !turn.reasoning.as_deref().unwrap_or("").is_empty() || !turn.tool_items.is_empty() {
                out.push(Line::default());
            }
            for mline in render_markdown(&display_reply, card_inner_w) {
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

        // Streaming reasoning tail
        if !snap.reasoning.trim().is_empty() {
            let rlines = render_markdown(snap.reasoning.trim(), card_inner_w);
            let skip = rlines.len().saturating_sub(8);
            for rline in rlines.into_iter().skip(skip) {
                let mut spans = vec![Span::styled(
                    "  │ ",
                    Style::default().fg(TEXT_DIM).bg(BG_CANVAS),
                )];
                for s in rline.spans {
                    spans.push(Span::styled(
                        s.content,
                        s.style.fg(TEXT_MUTED).add_modifier(Modifier::ITALIC),
                    ));
                }
                out.push(Line::from(spans));
            }
            out.push(Line::default());
        }

        // Completed tools so far in this turn + running tool spinner
        for item in info.in_flight_tools {
            let call_clean = strip_ansi(&item.call_formatted);
            if item.result_formatted.is_empty() {
                // Still running (handled with spinner below if active)
                if snap.activity_call.is_none() {
                    out.push(Line::from(Span::styled(
                        format!("  ▸ {}", truncate_str(&call_clean, card_inner_w)),
                        Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
                    )));
                }
            } else {
                let res_clean = strip_ansi(&item.result_formatted);
                let icon = if res_clean.contains('✖') { "✖" } else { "✔" };
                let color = if res_clean.contains('✖') {
                    RED_ERR
                } else {
                    GREEN_OK
                };
                out.push(Line::from(vec![
                    Span::styled(format!("  {icon} "), Style::default().fg(color).bg(BG_CANVAS)),
                    Span::styled(
                        truncate_str(&call_clean, card_inner_w.saturating_sub(2)),
                        Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
                    ),
                ]));
            }
        }

        if let Some(ref call) = snap.activity_call {
            let spinner = SPINNER_FRAMES[st.spinner_tick % SPINNER_FRAMES.len()];
            let secs = snap.activity_elapsed_secs.unwrap_or(0.0);
            let detail = snap
                .activity_detail
                .as_deref()
                .map(|d| format!(" — {d}"))
                .unwrap_or_default();
            out.push(Line::from(vec![
                Span::styled(
                    format!("  {spinner} "),
                    Style::default().fg(ACCENT_CORAL).bg(BG_CANVAS),
                ),
                Span::styled(
                    truncate_str(
                        &format!("{}{detail} ({secs:.1}s)", strip_ansi(call)),
                        card_inner_w.saturating_sub(4),
                    ),
                    Style::default().fg(TEXT_PRIMARY).bg(BG_CANVAS),
                ),
            ]));
            for tail in &snap.tool_output_tail {
                out.push(Line::from(Span::styled(
                    format!("    │ {}", truncate_str(&strip_ansi(tail), card_inner_w.saturating_sub(6))),
                    Style::default().fg(TEXT_DIM).bg(BG_CANVAS),
                )));
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
            let spinner = SPINNER_FRAMES[st.spinner_tick % SPINNER_FRAMES.len()];
            let secs = st.turn_start.elapsed().as_secs_f32();
            out.push(Line::from(vec![
                Span::styled(
                    format!("  {spinner} "),
                    Style::default().fg(ACCENT_CORAL).bg(BG_CANVAS),
                ),
                Span::styled(
                    format!("Thinking… ({secs:.1}s)"),
                    Style::default().fg(TEXT_MUTED).bg(BG_CANVAS),
                ),
            ]));
        }
        out.push(Line::default());
    }

    out
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
        out.push(Line::from(vec![
            Span::styled("▎", Style::default().fg(ACCENT_CORAL).bg(BG_SURFACE)),
            Span::styled("  ", Style::default().bg(BG_SURFACE)),
            Span::styled(line, Style::default().fg(TEXT_PRIMARY).bg(BG_SURFACE)),
            Span::styled(" ".repeat(pad), Style::default().bg(BG_SURFACE)),
        ]));
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
                            .fg(if is_sel { TEXT_PRIMARY } else { TEXT_PRIMARY })
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
