//! composer

use super::*;

/// Where one image thumbnail lands inside the composer's preview strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreviewPlacement {
    pub(crate) index: usize,
    pub(crate) x: u16,
    pub(crate) y: u16,
    pub(crate) width: u16,
    pub(crate) height: u16,
}

/// Lay thumbnails out left to right, wrapping when they run past the card.
pub(crate) fn layout_preview_sizes(
    sizes: &[(u16, u16)],
    max_width: u16,
) -> (Vec<PreviewPlacement>, u16) {
    let max_width = max_width.max(1);
    let mut placements = Vec::with_capacity(sizes.len());
    let mut x = 0u16;
    let mut y = 0u16;
    let mut row_h = 0u16;
    for (index, &(width, height)) in sizes.iter().enumerate() {
        let width = width.min(max_width);
        if x > 0 && x.saturating_add(width) > max_width {
            y = y.saturating_add(row_h).saturating_add(1);
            x = 0;
            row_h = 0;
        }
        placements.push(PreviewPlacement {
            index,
            x,
            y,
            width,
            height,
        });
        x = x.saturating_add(width).saturating_add(1);
        row_h = row_h.max(height);
    }
    let height = if placements.is_empty() {
        0
    } else {
        y.saturating_add(row_h)
    };
    (placements, height)
}

/// Fit a decoded preview into the card width, preserving its aspect ratio.
///
/// The preview is decoded at the largest useful size; painting it at the card's real width keeps
/// the composer from overflowing on a narrow terminal without cropping the image.
pub(crate) fn preview_display_size(preview: &ImagePreview, max_width: u16) -> (u16, u16) {
    let max_width = max_width.max(1);
    let width = preview.width.min(max_width);
    if width == preview.width || preview.width == 0 {
        return (width.max(1), preview.height);
    }
    let height = ((preview.height as u32 * width as u32) / preview.width as u32)
        .max(1)
        .min(preview.height as u32) as u16;
    (width.max(1), height)
}

/// Height of the composer preview strip, in terminal rows (zero when there are no images).
pub(crate) fn composer_preview_height(st: &TuiState, max_width: u16) -> u16 {
    let sizes: Vec<(u16, u16)> = st
        .composer
        .attachments
        .iter()
        .filter_map(|att| att.preview.as_ref())
        .map(|preview| preview_display_size(preview, max_width))
        .collect();
    layout_preview_sizes(&sizes, max_width).1
}

/// Whether the pointer sits inside a rectangle, used to gate hover-only controls.
pub(crate) fn pointer_inside(
    pointer: Option<(u16, u16)>,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
) -> bool {
    pointer.is_some_and(|(px, py)| {
        px >= x && px < x.saturating_add(width) && py >= y && py < y.saturating_add(height)
    })
}

/// Paint one thumbnail with half-block cells and arm its close button.
pub(crate) fn render_preview_cell(
    frame: &mut Frame,
    rect: Rect,
    placement: &PreviewPlacement,
    token: &str,
    preview: &ImagePreview,
    pointer: Option<(u16, u16)>,
    hit_boxes: &mut Vec<ComposerHitBox>,
) {
    let abs_x = rect.x.saturating_add(3).saturating_add(placement.x);
    let abs_y = rect.y.saturating_add(1).saturating_add(placement.y);
    let right_edge = rect.x.saturating_add(rect.width);
    let draw_w = placement.width.min(right_edge.saturating_sub(abs_x));

    for row in 0..placement.height {
        let mut spans = Vec::with_capacity(draw_w as usize);
        for x in 0..draw_w {
            let Some((top, bottom)) =
                preview.sampled_cell(x, row, placement.width, placement.height)
            else {
                continue;
            };
            spans.push(Span::styled("▀", Style::default().fg(top).bg(bottom)));
        }
        if spans.is_empty() {
            continue;
        }
        frame.render_widget(
            Paragraph::new(Line::from(spans)),
            Rect {
                x: abs_x,
                y: abs_y.saturating_add(row),
                width: draw_w,
                height: 1,
            },
        );
    }

    // The close button only shows while the pointer is over the thumbnail, so the image itself is
    // never covered at rest. It keeps the surface colour and a muted glyph: no red warning box.
    let close_x = abs_x.saturating_add(placement.width.saturating_sub(1));
    let over_preview = pointer_inside(pointer, abs_x, abs_y, placement.width, placement.height);
    if over_preview && close_x < right_edge {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "✕",
                Style::default().fg(text_muted()).bg(bg_surface()),
            )),
            Rect {
                x: close_x,
                y: abs_y,
                width: 1,
                height: 1,
            },
        );
        hit_boxes.push(ComposerHitBox {
            y: abs_y,
            x_start: close_x,
            x_end: close_x.saturating_add(1),
            token: token.to_string(),
        });
    }
}

pub(crate) fn has_antigravity_footer(snap: &TuiLiveSnapshot) -> bool {
    snap.account_name
        .as_deref()
        .is_some_and(|a| !a.trim().is_empty())
        || !snap.quota.is_empty()
}

pub(crate) fn quota_circle_glyph(remaining_fraction: f64) -> &'static str {
    let f = remaining_fraction.clamp(0.0, 1.0);
    if f >= 0.875 {
        "●"
    } else if f >= 0.625 {
        "◕"
    } else if f >= 0.375 {
        "◑"
    } else if f >= 0.125 {
        "◔"
    } else {
        "○"
    }
}

pub(crate) fn quota_circle_color(remaining_fraction: f64) -> Color {
    let f = remaining_fraction.clamp(0.0, 1.0);
    if f > 0.50 {
        green_ok()
    } else if f > 0.20 {
        yellow_warn()
    } else {
        red_err()
    }
}

pub(crate) fn build_antigravity_footer_line(
    snap: &TuiLiveSnapshot,
    accent: Color,
    card_width: usize,
) -> Line<'static> {
    let now = chrono::Utc::now();
    let mut quota_spans: Vec<Span<'static>> = Vec::new();
    let mut quota_w = 0usize;

    for (idx, line) in snap.quota.iter().enumerate() {
        if idx > 0 {
            let sep = " · ";
            quota_spans.push(Span::styled(
                sep,
                Style::default().fg(text_dim()).bg(bg_surface()),
            ));
            quota_w += sep.width();
        }
        let glyph = quota_circle_glyph(line.remaining_fraction);
        let color = quota_circle_color(line.remaining_fraction);
        let window_label = match line.window {
            harness_providers::QuotaWindow::FiveHour => "5h",
            harness_providers::QuotaWindow::Weekly => "w",
        };
        let pct_val = (line.remaining_fraction.clamp(0.0, 1.0) * 100.0).round();
        let pct_str = format!(" {pct_val:.0}%");
        let pct_color = if line.remaining_fraction <= 0.20 {
            red_err()
        } else {
            text_primary()
        };
        let reset = line
            .reset_time
            .as_deref()
            .map(|r| crate::commands::chat::status::format_reset_time_remaining(r, now))
            .filter(|r| !r.is_empty());

        quota_spans.push(Span::styled(
            glyph,
            Style::default().fg(color).bg(bg_surface()),
        ));
        quota_spans.push(Span::styled(
            format!(" {window_label}"),
            Style::default().fg(text_muted()).bg(bg_surface()),
        ));
        quota_spans.push(Span::styled(
            pct_str.clone(),
            Style::default()
                .fg(pct_color)
                .bg(bg_surface())
                .add_modifier(Modifier::BOLD),
        ));
        quota_w += glyph.width() + 1 + window_label.width() + pct_str.width();

        if let Some(ref rem) = reset {
            let rem_str = format!(" {rem}");
            quota_w += rem_str.width();
            quota_spans.push(Span::styled(
                rem_str,
                Style::default().fg(text_muted()).bg(bg_surface()),
            ));
        }
    }

    let avail = card_width.saturating_sub(4); // "▎  " (3) + right margin (1)
    let account_raw = snap
        .account_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let mut spans = vec![
        Span::styled("▎", Style::default().fg(accent).bg(bg_surface())),
        Span::styled("  ", Style::default().bg(bg_surface())),
    ];

    match (account_raw, quota_w > 0) {
        (Some(acct), true) => {
            if quota_w >= avail {
                let text = quota_spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>();
                spans.push(Span::styled(
                    truncate_str(&text, avail),
                    Style::default().fg(text_muted()).bg(bg_surface()),
                ));
            } else {
                let max_acct = avail.saturating_sub(quota_w + 2);
                if max_acct >= 6 {
                    let acct_disp = truncate_str(acct, max_acct);
                    let gap = avail.saturating_sub(acct_disp.width() + quota_w);
                    spans.push(Span::styled(
                        acct_disp,
                        Style::default().fg(cyan_link()).bg(bg_surface()),
                    ));
                    spans.push(Span::styled(
                        " ".repeat(gap),
                        Style::default().bg(bg_surface()),
                    ));
                }
                spans.extend(quota_spans);
            }
        }
        (Some(acct), false) => {
            spans.push(Span::styled(
                truncate_str(acct, avail),
                Style::default().fg(cyan_link()).bg(bg_surface()),
            ));
        }
        (None, true) => {
            if quota_w <= avail {
                spans.extend(quota_spans);
            } else {
                let text = quota_spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>();
                spans.push(Span::styled(
                    truncate_str(&text, avail),
                    Style::default().fg(text_muted()).bg(bg_surface()),
                ));
            }
        }
        (None, false) => {}
    }

    Line::from(spans)
}

pub(crate) fn voice_waveform_bars(tick: usize, level: f32) -> String {
    const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let amp = level.clamp(0.15, 1.0);
    let mut out = String::with_capacity(15);
    for i in 0..5usize {
        let phase = ((tick + i * 2) % 8) as f32 / 7.0;
        let wave = 0.45 + 0.55 * (1.0 - (phase - 0.5).abs() * 2.0);
        let idx = ((amp * wave) * 7.0).round().clamp(0.0, 7.0) as usize;
        out.push(BARS[idx]);
    }
    out
}

pub(crate) fn render_composer_card(
    frame: &mut Frame,
    rect: Rect,
    st: &mut TuiState,
    info: &FooterInfo<'_>,
    snap: &TuiLiveSnapshot,
    wrapped: &WrappedComposer,
    is_landing: bool,
) {
    if rect.height == 0 || rect.width < 6 {
        return;
    }
    let accent = if st.voice_speaking_detected {
        if st.spinner_tick % 4 < 2 {
            green_ok()
        } else {
            cyan_link()
        }
    } else if st.voice_transcribing {
        cyan_link()
    } else {
        mode_accent(st.mode.label())
    };
    let inner_w = rect.width.saturating_sub(1) as usize;

    // Fill all card rows with `▎` accent bar + `bg_surface()` background.
    for row in 0..rect.height {
        let y = rect.y + row;
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("▎", Style::default().fg(accent).bg(bg_surface())),
                Span::styled(" ".repeat(inner_w), Style::default().bg(bg_surface())),
            ])),
            Rect {
                x: rect.x,
                y,
                width: rect.width,
                height: 1,
            },
        );
    }

    // Image previews sit in a strip at the top of the card, above the input line.
    let previews: Vec<(String, ImagePreview)> = st
        .composer
        .attachments
        .iter()
        .filter_map(|att| {
            att.preview
                .clone()
                .map(|preview| (att.token.clone(), preview))
        })
        .collect();
    let sizes: Vec<(u16, u16)> = previews
        .iter()
        .map(|(_, preview)| preview_display_size(preview, rect.width.saturating_sub(3).max(1)))
        .collect();
    let (placements, preview_h) = layout_preview_sizes(&sizes, rect.width.saturating_sub(3).max(1));
    let pointer = st.pointer;
    st.composer_hit_boxes.clear();
    for placement in &placements {
        let (token, preview) = &previews[placement.index];
        render_preview_cell(
            frame,
            rect,
            placement,
            token,
            preview,
            pointer,
            &mut st.composer_hit_boxes,
        );
    }

    // Input rows start below the preview strip (plus one gap row when a preview is shown).
    let show_ag_footer = has_antigravity_footer(snap) && rect.height >= 4;
    let footer_rows = 1u16 + u16::from(show_ag_footer);
    let input_top = rect.y + 1 + preview_h + u16::from(preview_h > 0);
    let max_input_rows = rect
        .height
        .saturating_sub((input_top - rect.y) + 1 + footer_rows)
        .max(1) as usize;
    let scroll_off = wrapped
        .cursor_line
        .saturating_sub(max_input_rows.saturating_sub(1));

    st.composer_area = (rect.x, rect.y, rect.width, rect.height);
    st.composer_input_area = (rect.x + 3, input_top, inner_w as u16, max_input_rows as u16);
    st.composer_scroll_off = scroll_off;

    if st.composer.text.is_empty() && rect.height >= 2 {
        let (placeholder, placeholder_color) = if st.active_turn.is_some() {
            (
                "Type to queue or steer next message...".to_string(),
                text_muted(),
            )
        } else if st.voice_speaking_detected {
            let bars = voice_waveform_bars(st.spinner_tick, st.voice_audio_level);
            (format!("{bars}  Listening to your voice..."), green_ok())
        } else if st.voice_transcribing {
            let spinner = SPINNER_FRAMES[st.spinner_tick % SPINNER_FRAMES.len()];
            (format!("{spinner}  Transcribing voice..."), cyan_link())
        } else if st.voice_active {
            let hint = if st.voice_auto_send {
                "Type or talk anytime... (say \"Luna\" or type /voice off)"
            } else {
                "Type or talk anytime... (verify before send · /voice off)"
            };
            (hint.to_string(), text_muted())
        } else if is_landing {
            (
                "Ask anything... \"Fix broken tests\"".to_string(),
                text_muted(),
            )
        } else {
            (String::new(), text_muted())
        };
        let text_area_w = inner_w.saturating_sub(2);
        let clipped = truncate_str(&placeholder, text_area_w);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled("▎", Style::default().fg(accent).bg(bg_surface())),
                Span::styled(
                    format!("  {clipped}"),
                    Style::default().fg(placeholder_color).bg(bg_surface()),
                ),
            ])),
            Rect {
                x: rect.x,
                y: input_top,
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
            let y = input_top + vis_idx as u16;
            let mut spans = vec![
                Span::styled("▎", Style::default().fg(accent).bg(bg_surface())),
                Span::styled("  ", Style::default().bg(bg_surface())),
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
        let cur_y = (input_top + vis_cursor_y as u16).min(rect.y + rect.height - 1);
        frame.set_cursor_position((cur_x, cur_y));
    }

    // Footer metadata row at bottom of composer card (`Build auto · Model Provider · high`).
    if rect.height >= 3 {
        let footer_y = rect.y + rect.height - footer_rows;
        let model_display = friendly_model_name(info.model);
        let provider_display = friendly_provider_name(info.provider);
        let thinking_label = info
            .reasoning_effort
            .map(|e| e.to_string())
            .unwrap_or_else(|| "off".to_string());
        let thinking_color = if info.reasoning_effort.is_some() {
            accent_coral()
        } else {
            text_muted()
        };

        let mut footer_spans = vec![
            Span::styled("▎", Style::default().fg(accent).bg(bg_surface())),
            Span::styled("  ", Style::default().bg(bg_surface())),
            Span::styled(
                st.mode.label(),
                Style::default()
                    .fg(accent)
                    .bg(bg_surface())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" · ", Style::default().fg(text_muted()).bg(bg_surface())),
            Span::styled(
                model_display,
                Style::default()
                    .fg(text_primary())
                    .bg(bg_surface())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {provider_display} · "),
                Style::default().fg(text_muted()).bg(bg_surface()),
            ),
            Span::styled(
                thinking_label,
                Style::default().fg(thinking_color).bg(bg_surface()),
            ),
        ];
        if st.voice_active {
            footer_spans.push(Span::styled(
                " · ",
                Style::default().fg(text_muted()).bg(bg_surface()),
            ));
            let (voice_badge, voice_color) = if st.voice_speaking_detected {
                let bars = voice_waveform_bars(st.spinner_tick, st.voice_audio_level);
                (format!("🎙 {bars} Listening"), green_ok())
            } else if st.voice_transcribing {
                let spinner = SPINNER_FRAMES[st.spinner_tick % SPINNER_FRAMES.len()];
                (format!("🎙 {spinner} Transcribing"), cyan_link())
            } else if st
                .voice_is_playing
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                let bars = voice_waveform_bars(st.spinner_tick, 0.55);
                (format!("🎙 {bars} Speaking"), cyan_link())
            } else if !st.voice_auto_send {
                ("🎙 Voice ON · verify".to_string(), green_ok())
            } else {
                ("🎙 Voice ON".to_string(), green_ok())
            };
            footer_spans.push(Span::styled(
                voice_badge,
                Style::default()
                    .fg(voice_color)
                    .bg(bg_surface())
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let footer = Line::from(footer_spans);
        frame.render_widget(
            Paragraph::new(footer),
            Rect {
                x: rect.x,
                y: footer_y,
                width: rect.width,
                height: 1,
            },
        );
        if show_ag_footer {
            let ag_y = rect.y + rect.height - 1;
            let ag_line = build_antigravity_footer_line(snap, accent, rect.width as usize);
            frame.render_widget(
                Paragraph::new(ag_line),
                Rect {
                    x: rect.x,
                    y: ag_y,
                    width: rect.width,
                    height: 1,
                },
            );
            st.transcript_hit_boxes.push(TranscriptHitBox {
                y: ag_y,
                x_start: rect.x,
                x_end: rect.x.saturating_add(rect.width),
                target: TranscriptClickTarget::OpenCredentialPriority("antigravity".to_string()),
            });
        }
    }
}

pub(crate) fn render_composer_hint_row(
    frame: &mut Frame,
    rect: Rect,
    st: &TuiState,
    snap: Option<&TuiLiveSnapshot>,
    in_flight_tools: &[crate::commands::chat::render::TurnProcessItem],
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
            let color = scanner_color(dist);
            left_spans.push(Span::styled(
                "▪",
                Style::default().fg(color).bg(bg_canvas()),
            ));
        }
        left_spans.push(Span::styled(
            "  esc ",
            Style::default().fg(text_primary()).bg(bg_canvas()),
        ));
        left_spans.push(Span::styled(
            "interrupt",
            Style::default().fg(text_muted()).bg(bg_canvas()),
        ));
    }

    if let Some(ref project) = st.memory_project_filter {
        right_spans.push(Span::styled(
            format!("memory: {project} · "),
            Style::default().fg(cyan_link()).bg(bg_canvas()),
        ));
    }

    if let Some(s) = snap {
        if s.memory_distilling.is_some() {
            right_spans.push(Span::styled(
                "🧠 memory · ",
                Style::default().fg(accent_coral()).bg(bg_canvas()),
            ));
        }
        if s.delegate_count > 0 {
            let label = if s.delegate_count == 1 {
                "● 1 subagent · ".to_string()
            } else {
                format!("● {} subagents · ", s.delegate_count)
            };
            right_spans.push(Span::styled(
                label,
                Style::default().fg(cyan_link()).bg(bg_canvas()),
            ));
        }
    }

    let (total_added, total_deleted) = st.total_code_changes(Some(in_flight_tools));
    right_spans.push(Span::styled(
        format!("+{total_added}"),
        Style::default().fg(green_ok()).bg(bg_canvas()),
    ));
    right_spans.push(Span::styled(" ", Style::default().bg(bg_canvas())));
    right_spans.push(Span::styled(
        format!("-{total_deleted}"),
        Style::default().fg(red_err()).bg(bg_canvas()),
    ));
    right_spans.push(Span::styled(
        " · ",
        Style::default().fg(text_muted()).bg(bg_canvas()),
    ));

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
                    Style::default().fg(text_muted()).bg(bg_canvas()),
                ));
            }
        }
    }

    if let Some(ctx) = context_label {
        right_spans.push(Span::styled(
            format!("{ctx}   "),
            Style::default().fg(text_muted()).bg(bg_canvas()),
        ));
    }
    if is_landing && st.active_turn.is_none() {
        right_spans.push(Span::styled(
            "shift+tab",
            Style::default().fg(text_primary()).bg(bg_canvas()),
        ));
        right_spans.push(Span::styled(
            " agents   ",
            Style::default().fg(text_muted()).bg(bg_canvas()),
        ));
    }
    right_spans.push(Span::styled(
        "ctrl+p",
        Style::default().fg(text_primary()).bg(bg_canvas()),
    ));
    right_spans.push(Span::styled(
        " commands",
        Style::default().fg(text_muted()).bg(bg_canvas()),
    ));

    let right_w: usize = right_spans.iter().map(|s| s.content.width()).sum();
    if st.active_turn.is_none() {
        let max_left = (rect.width as usize).saturating_sub(right_w + 2);
        let left_text = truncate_str(&st.cwd_label, max_left);
        left_spans.push(Span::styled(
            left_text,
            Style::default().fg(text_muted()).bg(bg_canvas()),
        ));
    }

    let left_w: usize = left_spans.iter().map(|s| s.content.width()).sum();
    let spacer = (rect.width as usize).saturating_sub(left_w + right_w);

    let mut spans = left_spans;
    spans.push(Span::styled(
        " ".repeat(spacer),
        Style::default().bg(bg_canvas()),
    ));
    spans.extend(right_spans);
    frame.render_widget(Paragraph::new(Line::from(spans)), rect);
}

pub(crate) struct WrappedComposer {
    pub(crate) lines: Vec<String>,
    pub(crate) cursor_line: usize,
    pub(crate) cursor_col: usize,
}

pub(crate) fn styled_composer_line_spans(line: &str) -> Vec<Span<'static>> {
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
                Style::default().fg(text_primary()).bg(bg_surface()),
            ));
            break;
        };
        if pos > 0 {
            spans.push(Span::styled(
                rest[..pos].to_string(),
                Style::default().fg(text_primary()).bg(bg_surface()),
            ));
        }
        let after = &rest[pos..];
        if let Some(end) = after.find(']') {
            let token = &after[..=end];
            let fg = if is_image {
                accent_coral()
            } else {
                yellow_warn()
            };
            spans.push(Span::styled(
                token.to_string(),
                Style::default()
                    .fg(fg)
                    .bg(bg_elevated())
                    .add_modifier(Modifier::BOLD),
            ));
            rest = &after[end + 1..];
        } else {
            spans.push(Span::styled(
                after.to_string(),
                Style::default().fg(text_primary()).bg(bg_surface()),
            ));
            break;
        }
    }
    spans
}

pub(crate) fn wrap_composer_lines(
    text: &str,
    cursor_byte: usize,
    max_width: usize,
) -> WrappedComposer {
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
