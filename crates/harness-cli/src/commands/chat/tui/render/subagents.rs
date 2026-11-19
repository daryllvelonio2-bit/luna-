//! subagents

use super::*;

pub(crate) fn render_subagents_modal(
    frame: &mut Frame,
    rect: Rect,
    items: &[SubagentUiEntry],
    selected: usize,
    active_only: bool,
    spinner_tick: usize,
) {
    let hint = "enter inspect · tab filter · x stop · esc back";
    let inner = draw_modal_frame(frame, rect, "Subagents", hint);
    if inner.height < 3 || inner.width < 10 {
        return;
    }

    let running_count = items.iter().filter(|i| i.status == "running").count();
    let completed_count = items.iter().filter(|i| i.status == "completed").count();

    let mut rows: Vec<Line<'static>> = Vec::new();

    // Top filter & status row
    let inner_w = inner.width as usize;
    let raw_filter = if active_only {
        " [Active] (Tab / Ctrl+A to show all) "
    } else {
        " [All] (Tab / Ctrl+A to show active) "
    };
    let count_label = format!("{running_count} running · {completed_count} completed");
    let count_disp = if inner_w < 38 {
        format!("{running_count}r/{completed_count}d")
    } else {
        count_label
    };
    let filter_label = truncate_str(raw_filter, inner_w.saturating_sub(count_disp.width() + 3));
    let pad = inner_w.saturating_sub(filter_label.width() + count_disp.width() + 2);
    rows.push(Line::from(vec![
        Span::styled(" ", Style::default().bg(bg_elevated())),
        Span::styled(
            filter_label,
            Style::default()
                .fg(if active_only {
                    cyan_link()
                } else {
                    text_primary()
                })
                .bg(bg_surface())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ".repeat(pad), Style::default().bg(bg_elevated())),
        Span::styled(
            count_disp,
            Style::default().fg(text_muted()).bg(bg_elevated()),
        ),
        Span::styled(" ", Style::default().bg(bg_elevated())),
    ]));
    rows.push(Line::default());

    let filtered: Vec<&SubagentUiEntry> = items
        .iter()
        .filter(|i| !active_only || i.status == "running")
        .collect();

    let list_h = (inner.height as usize).saturating_sub(2);
    if filtered.is_empty() {
        let msg = if active_only {
            "   No active subagents running. Press Tab to view all subagents."
        } else {
            "   No subagent tasks recorded."
        };
        rows.push(Line::from(Span::styled(
            msg,
            Style::default().fg(text_muted()).bg(bg_elevated()),
        )));
    } else {
        let sel = selected.min(filtered.len().saturating_sub(1));
        let scroll = sel.saturating_sub(list_h.saturating_sub(1));
        for (idx, item) in filtered.iter().enumerate().skip(scroll).take(list_h) {
            let is_sel = idx == sel;
            let bg = if is_sel { bg_selected() } else { bg_elevated() };
            let prefix = if is_sel { " ▸ " } else { "   " };

            let (status_icon, status_color) = if item.status == "running" {
                let spinner = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
                (spinner, cyan_link())
            } else if item.status == "completed" {
                ("●", green_ok())
            } else if item.status.starts_with("failed") {
                ("◍", red_err())
            } else {
                ("○", text_muted())
            };

            let elapsed_str = format!("{}s", item.elapsed_secs);
            let agent_badge = format!("[{}] ", item.agent);
            let avail_w = (inner.width as usize)
                .saturating_sub(prefix.len() + 2 + agent_badge.len() + elapsed_str.len() + 3);
            let title_trunc = truncate_str(&item.title, avail_w);

            let row_pad = (inner.width as usize).saturating_sub(
                prefix.len() + 2 + agent_badge.len() + title_trunc.width() + elapsed_str.len() + 2,
            );

            rows.push(Line::from(vec![
                Span::styled(
                    prefix,
                    Style::default()
                        .fg(if is_sel { accent_coral() } else { text_muted() })
                        .bg(bg),
                ),
                Span::styled(
                    format!("{status_icon} "),
                    Style::default().fg(status_color).bg(bg),
                ),
                Span::styled(
                    agent_badge,
                    Style::default()
                        .fg(if is_sel { accent_coral() } else { cyan_link() })
                        .add_modifier(Modifier::BOLD)
                        .bg(bg),
                ),
                Span::styled(title_trunc, Style::default().fg(text_primary()).bg(bg)),
                Span::styled(" ".repeat(row_pad), Style::default().bg(bg)),
                Span::styled(elapsed_str, Style::default().fg(text_muted()).bg(bg)),
                Span::styled(" ", Style::default().bg(bg)),
            ]));
        }
    }

    frame.render_widget(Paragraph::new(rows), inner);
}

pub(crate) fn render_subagent_inspector(
    frame: &mut Frame,
    rect: Rect,
    items: &[SubagentUiEntry],
    current_idx: usize,
    scroll: usize,
    spinner_tick: usize,
) {
    let Some(item) = items.get(current_idx) else {
        return;
    };

    let title = format!("Subagent: {}", item.agent);
    let hint = if items.len() > 1 {
        "tab next · esc back · ↑/↓ scroll · x stop"
    } else {
        "esc back · ↑/↓ scroll · x stop"
    };
    let inner = draw_modal_frame(frame, rect, &title, hint);
    if inner.height < 4 || inner.width < 10 {
        return;
    }

    let (status_icon, _) = if item.status == "running" {
        let spinner = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
        (spinner, cyan_link())
    } else if item.status == "completed" {
        ("●", green_ok())
    } else if item.status.starts_with("failed") {
        ("◍", red_err())
    } else {
        ("○", text_muted())
    };

    let cycle_badge = if items.len() > 1 {
        format!("Tab: {} of {} ›", current_idx + 1, items.len())
    } else {
        String::new()
    };

    let mut header_rows = Vec::new();
    let title_line = format!("{status_icon} {} — {}", item.agent, item.title);
    let avail_title = (inner.width as usize).saturating_sub(cycle_badge.len() + 3);
    let title_trunc = truncate_str(&title_line, avail_title);
    let hpad = (inner.width as usize).saturating_sub(title_trunc.width() + cycle_badge.len() + 2);

    header_rows.push(Line::from(vec![
        Span::styled(" ", Style::default().bg(bg_elevated())),
        Span::styled(
            title_trunc,
            Style::default()
                .fg(text_primary())
                .add_modifier(Modifier::BOLD)
                .bg(bg_elevated()),
        ),
        Span::styled(" ".repeat(hpad), Style::default().bg(bg_elevated())),
        Span::styled(
            cycle_badge,
            Style::default()
                .fg(accent_coral())
                .add_modifier(Modifier::BOLD)
                .bg(bg_elevated()),
        ),
        Span::styled(" ", Style::default().bg(bg_elevated())),
    ]));

    let meta_line = format!(
        " ID: {} · Status: {} · Duration: {}s · Activity: {}",
        item.id, item.status, item.elapsed_secs, item.activity
    );
    header_rows.push(Line::from(Span::styled(
        truncate_str(&meta_line, inner.width as usize),
        Style::default().fg(text_muted()).bg(bg_elevated()),
    )));
    header_rows.push(Line::from(Span::styled(
        "─".repeat(inner.width as usize),
        Style::default().fg(border_subtle()).bg(bg_elevated()),
    )));

    let header_h = header_rows.len() as u16;
    frame.render_widget(
        Paragraph::new(header_rows),
        Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: header_h,
        },
    );

    let body_rect = Rect {
        x: inner.x,
        y: inner.y + header_h,
        width: inner.width,
        height: inner.height.saturating_sub(header_h),
    };
    if body_rect.height == 0 {
        return;
    }

    let content_lines = build_subagent_inspector_lines(item, body_rect.width as usize);
    let visible_h = body_rect.height as usize;
    let max_scroll = content_lines.len().saturating_sub(visible_h);
    let clamped_scroll = scroll.min(max_scroll);

    let rows: Vec<Line<'static>> = content_lines
        .into_iter()
        .skip(clamped_scroll)
        .take(visible_h)
        .collect();

    frame.render_widget(Paragraph::new(rows), body_rect);
}

pub(crate) fn push_subagent_log_lines(
    lines: &mut Vec<Line<'static>>,
    log_tail: &[String],
    card_inner_w: usize,
) {
    for raw in log_tail {
        let clean = clean_terminal_text(raw);
        let trimmed = clean.trim_start();
        let color = if trimmed.starts_with("▶ Tool Call:") {
            cyan_link()
        } else if trimmed.starts_with("✔ Tool Result") && trimmed.contains("FAILED") {
            red_err()
        } else if trimmed.starts_with("✔ Tool Result") {
            green_ok()
        } else if trimmed.starts_with("===") || trimmed.starts_with("───") {
            text_muted()
        } else {
            text_primary()
        };
        let mut style = Style::default().fg(color);
        if trimmed.starts_with("▶ Tool Call:") || trimmed.starts_with("✔ Tool Result") {
            style = style.add_modifier(Modifier::BOLD);
        }
        if clean.trim().is_empty() {
            lines.push(Line::default());
        } else {
            for wrapped in wrap_plain_text(&clean, card_inner_w) {
                lines.push(Line::from(Span::styled(format!("  {wrapped}"), style)));
            }
        }
    }
}

pub(crate) fn build_subagent_inspector_lines(
    item: &SubagentUiEntry,
    width: usize,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let card_inner_w = width.saturating_sub(4).max(10);

    if item.turns.is_empty() {
        if !item.title.trim().is_empty() {
            lines.push(Line::from(vec![
                Span::styled("▎ ", Style::default().fg(accent_coral()).bg(bg_surface())),
                Span::styled(
                    "Task Goal: ",
                    Style::default()
                        .fg(accent_coral())
                        .add_modifier(Modifier::BOLD)
                        .bg(bg_surface()),
                ),
            ]));
            for qline in wrap_plain_text(&item.title, card_inner_w) {
                let pad = card_inner_w.saturating_sub(qline.width());
                lines.push(Line::from(vec![
                    Span::styled("▎ ", Style::default().fg(accent_coral()).bg(bg_surface())),
                    Span::styled(qline, Style::default().fg(text_primary()).bg(bg_surface())),
                    Span::styled(" ".repeat(pad), Style::default().bg(bg_surface())),
                ]));
            }
            lines.push(Line::default());
        }

        if !item.log_tail.is_empty() {
            push_subagent_log_lines(&mut lines, &item.log_tail, card_inner_w);
            lines.push(Line::default());
        } else if item.status == "running" {
            lines.push(Line::from(Span::styled(
                "  Tools and responses will appear here live.",
                Style::default().fg(text_muted()),
            )));
        } else {
            lines.push(Line::from(Span::styled(
                format!("  No transcript recorded. (Status: {})", item.status),
                Style::default().fg(text_muted()),
            )));
        }

        if item.status == "running" {
            lines.push(Line::from(Span::styled(
                format!("  ⠋ Subagent is running ({})", item.activity),
                Style::default()
                    .fg(cyan_link())
                    .add_modifier(Modifier::BOLD),
            )));
        }
        return lines;
    }

    for (turn_idx, turn) in item.turns.iter().enumerate() {
        if !turn.user_query.trim().is_empty() {
            let label = if turn_idx == 0 {
                "Task Goal"
            } else {
                "Follow-up"
            };
            lines.push(Line::from(vec![
                Span::styled("▎ ", Style::default().fg(accent_coral()).bg(bg_surface())),
                Span::styled(
                    format!("{label}: "),
                    Style::default()
                        .fg(accent_coral())
                        .add_modifier(Modifier::BOLD)
                        .bg(bg_surface()),
                ),
            ]));
            for qline in wrap_plain_text(&turn.user_query, card_inner_w) {
                let pad = card_inner_w.saturating_sub(qline.width());
                lines.push(Line::from(vec![
                    Span::styled("▎ ", Style::default().fg(accent_coral()).bg(bg_surface())),
                    Span::styled(qline, Style::default().fg(text_primary()).bg(bg_surface())),
                    Span::styled(" ".repeat(pad), Style::default().bg(bg_surface())),
                ]));
            }
            lines.push(Line::default());
        }

        lines.push(Line::from(vec![
            Span::styled("● ", Style::default().fg(cyan_link())),
            Span::styled(
                format!("Subagent [{}]", item.agent),
                Style::default()
                    .fg(cyan_link())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" · {:.1}s", turn.duration.as_secs_f64()),
                Style::default().fg(text_muted()),
            ),
        ]));

        if let Some(ref reasoning) = turn.reasoning {
            lines.push(Line::from(Span::styled(
                "  ◈ Thinking",
                Style::default()
                    .fg(text_muted())
                    .add_modifier(Modifier::ITALIC),
            )));
            for rline in wrap_plain_text(reasoning, card_inner_w) {
                lines.push(Line::from(Span::styled(
                    format!("    {rline}"),
                    Style::default().fg(text_muted()),
                )));
            }
            lines.push(Line::default());
        }

        if !turn.tool_items.is_empty() {
            for t_item in &turn.tool_items {
                let glyph = if t_item.is_error { "✖ " } else { "✓ " };
                let glyph_color = if t_item.is_error {
                    red_err()
                } else {
                    green_ok()
                };
                let desc = if !t_item.call_formatted.is_empty() {
                    &t_item.call_formatted
                } else {
                    &t_item.tool_name
                };
                lines.push(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        glyph,
                        Style::default()
                            .fg(glyph_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        truncate_str(desc, card_inner_w.saturating_sub(4)),
                        Style::default().fg(text_primary()),
                    ),
                ]));
                for diff in t_item.diff_lines.iter().take(6) {
                    let clean = clean_terminal_text(diff);
                    let c = if clean.trim_start().starts_with('+') {
                        green_ok()
                    } else if clean.trim_start().starts_with('-') {
                        red_err()
                    } else {
                        text_muted()
                    };
                    lines.push(Line::from(Span::styled(
                        format!("      {}", truncate_str(&clean, card_inner_w)),
                        Style::default().fg(c),
                    )));
                }
            }
            lines.push(Line::default());
        } else if !turn.tools.is_empty() {
            for tool in &turn.tools {
                let glyph = if tool.is_error { "✖ " } else { "✓ " };
                let glyph_color = if tool.is_error { red_err() } else { green_ok() };
                lines.push(Line::from(vec![
                    Span::styled("  ", Style::default()),
                    Span::styled(
                        glyph,
                        Style::default()
                            .fg(glyph_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        truncate_str(&tool.name, 24),
                        Style::default()
                            .fg(text_primary())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(
                            " {}",
                            truncate_str(&tool.output, card_inner_w.saturating_sub(28))
                        ),
                        Style::default().fg(text_muted()),
                    ),
                ]));
            }
            lines.push(Line::default());
        }

        if !turn.reply.is_empty() {
            let rendered = render_markdown(&turn.reply, card_inner_w);
            for rline in rendered {
                lines.push(rline);
            }
            lines.push(Line::default());
        }
    }

    if item.status == "running" {
        if !item.log_tail.is_empty() {
            lines.push(Line::from(Span::styled(
                "  ── Live Activity ──",
                Style::default().fg(text_muted()),
            )));
            push_subagent_log_lines(&mut lines, &item.log_tail, card_inner_w);
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled(
            format!("  ⠋ Subagent is running ({})", item.activity),
            Style::default()
                .fg(cyan_link())
                .add_modifier(Modifier::BOLD),
        )));
    }

    lines
}
