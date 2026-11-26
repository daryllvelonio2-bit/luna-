//! modals

use super::*;

pub(crate) fn render_modal(
    frame: &mut Frame,
    screen: Rect,
    st: &mut TuiState,
    info: &FooterInfo<'_>,
) {
    st.modal_rect = None;
    let spinner_tick = st.spinner_tick;
    match &mut st.modal {
        ActiveModal::None => {}
        ActiveModal::CommandPalette { query, selected } => {
            let items = filtered_palette_items(query);
            let (w, h) = responsive_modal_size(screen, 6, 4, 44, 72, 10, 20);
            let rect = centered_rect(screen, w, h);
            st.modal_rect = Some(rect);
            let inner = draw_modal_frame(frame, rect, "Commands", "esc");

            let inner_w = inner.width as usize;
            let mut lines = Vec::new();
            lines.push(search_input_line(query, inner_w));
            let cur_x =
                (inner.x + 3 + query.width() as u16).min(inner.x + inner.width.saturating_sub(1));
            if inner.height > 0 {
                frame.set_cursor_position((cur_x, inner.y));
            }
            lines.push(Line::from(Span::styled(
                "─".repeat(inner_w),
                Style::default().fg(border_subtle()).bg(bg_elevated()),
            )));

            let list_h = (inner.height as usize).saturating_sub(2).max(1);
            let sel = (*selected).min(items.len().saturating_sub(1));
            let scroll = sel.saturating_sub(list_h.saturating_sub(1));

            for (idx, item) in items.iter().enumerate().skip(scroll).take(list_h) {
                let is_sel = idx == sel;
                let bg = if is_sel { bg_selected() } else { bg_elevated() };
                let raw_right = if item.shortcut.is_empty() {
                    format!(" {} ", item.category)
                } else {
                    format!(" {} ", item.shortcut)
                };
                let max_right = (inner_w / 2).min(20);
                let right = if inner_w < 18 {
                    String::new()
                } else {
                    truncate_str(&raw_right, max_right)
                };
                let max_title = inner_w.saturating_sub(right.width() + 2);
                let left = format!(" {} ", truncate_str(&item.title, max_title));
                let pad = inner_w.saturating_sub(left.width() + right.width());
                lines.push(Line::from(vec![
                    Span::styled(
                        left,
                        Style::default()
                            .fg(text_primary())
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
                            .fg(if is_sel { accent_coral() } else { text_muted() })
                            .bg(bg),
                    ),
                ]));
            }
            if items.is_empty() {
                lines.push(Line::from(Span::styled(
                    truncate_str(" No matching commands", inner_w),
                    Style::default().fg(text_muted()).bg(bg_elevated()),
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
            let (w, h) = responsive_modal_size(screen, 6, 4, 48, 76, 10, 20);
            let rect = centered_rect(screen, w, h);
            st.modal_rect = Some(rect);
            let inner = draw_modal_frame(frame, rect, "Sessions", "enter switch · esc");

            let inner_w = inner.width as usize;
            let mut lines = Vec::new();
            lines.push(search_input_line(query, inner_w));
            let cur_x =
                (inner.x + 3 + query.width() as u16).min(inner.x + inner.width.saturating_sub(1));
            if inner.height > 0 {
                frame.set_cursor_position((cur_x, inner.y));
            }
            lines.push(Line::from(Span::styled(
                "─".repeat(inner_w),
                Style::default().fg(border_subtle()).bg(bg_elevated()),
            )));

            let list_h = (inner.height as usize).saturating_sub(2).max(1);
            let sel = (*selected).min(filtered.len().saturating_sub(1));
            let scroll = sel.saturating_sub(list_h.saturating_sub(1));

            for (idx, sess) in filtered.iter().enumerate().skip(scroll).take(list_h) {
                let is_sel = idx == sel;
                let bg = if is_sel { bg_selected() } else { bg_elevated() };
                let is_cur = sess.id == info.session_id;
                let marker = if is_cur { "●" } else { " " };
                let label = if !sess.name.is_empty() {
                    sess.name.clone()
                } else if !sess.title.is_empty() {
                    sess.title.clone()
                } else {
                    "New session".to_string()
                };
                let raw_right = format!(
                    " {} msgs · {} ",
                    sess.message_count,
                    &sess.id[..sess.id.len().min(8)]
                );
                let right = if inner_w < 26 {
                    String::new()
                } else {
                    truncate_str(&raw_right, (inner_w / 2).min(24))
                };
                let max_name = inner_w.saturating_sub(right.width() + 4);
                let left = format!(" {marker} {}", truncate_str(&label, max_name));
                let pad = inner_w.saturating_sub(left.width() + right.width());
                lines.push(Line::from(vec![
                    Span::styled(
                        left,
                        Style::default()
                            .fg(if is_cur {
                                accent_coral()
                            } else {
                                text_primary()
                            })
                            .bg(bg),
                    ),
                    Span::styled(" ".repeat(pad), Style::default().bg(bg)),
                    Span::styled(right, Style::default().fg(text_muted()).bg(bg)),
                ]));
            }
            if filtered.is_empty() {
                lines.push(Line::from(Span::styled(
                    truncate_str(" No sessions match", inner_w),
                    Style::default().fg(text_muted()).bg(bg_elevated()),
                )));
            }
            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::ProviderPicker {
            query,
            items,
            selected,
            ..
        } => {
            let filtered = filtered_providers(items, query);
            let (w, h) = responsive_modal_size(screen, 6, 4, 50, 78, 10, 22);
            let rect = centered_rect(screen, w, h);
            st.modal_rect = Some(rect);
            let inner = draw_modal_frame(frame, rect, "Select Provider", "enter models · esc");

            let inner_w = inner.width as usize;
            let mut lines = Vec::new();
            lines.push(search_input_line(query, inner_w));
            let cur_x =
                (inner.x + 3 + query.width() as u16).min(inner.x + inner.width.saturating_sub(1));
            if inner.height > 0 {
                frame.set_cursor_position((cur_x, inner.y));
            }
            lines.push(Line::from(Span::styled(
                "─".repeat(inner_w),
                Style::default().fg(border_subtle()).bg(bg_elevated()),
            )));

            let list_h = (inner.height as usize).saturating_sub(2).max(1);
            let sel = (*selected).min(filtered.len().saturating_sub(1));
            let scroll = sel.saturating_sub(list_h.saturating_sub(1));

            for (idx, item) in filtered.iter().enumerate().skip(scroll).take(list_h) {
                let is_sel = idx == sel;
                let bg = if is_sel { bg_selected() } else { bg_elevated() };
                let marker = if item.is_current { "●" } else { " " };
                let right = if item.tag.is_empty() {
                    format!(" {} ", item.id)
                } else {
                    format!(" {} · {} ", item.id, item.tag)
                };
                let right_disp = if inner_w < 24 {
                    String::new()
                } else {
                    truncate_str(&right, (inner_w / 2).min(24))
                };
                let max_name = inner_w.saturating_sub(right_disp.width() + 4);
                let left = format!(" {marker} {}", truncate_str(&item.display_name, max_name));
                let pad = inner_w.saturating_sub(left.width() + right_disp.width());
                lines.push(Line::from(vec![
                    Span::styled(
                        left,
                        Style::default()
                            .fg(if item.is_current {
                                accent_coral()
                            } else {
                                text_primary()
                            })
                            .bg(bg),
                    ),
                    Span::styled(" ".repeat(pad), Style::default().bg(bg)),
                    Span::styled(right_disp, Style::default().fg(text_muted()).bg(bg)),
                ]));
            }
            if filtered.is_empty() {
                lines.push(Line::from(Span::styled(
                    truncate_str(" No providers match", inner_w),
                    Style::default().fg(text_muted()).bg(bg_elevated()),
                )));
            }
            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::ModelPicker {
            provider,
            provider_display,
            query,
            items,
            selected,
            ..
        } => {
            let filtered = filtered_models(items, query);
            let (w, h) = responsive_modal_size(screen, 6, 4, 50, 78, 10, 22);
            let rect = centered_rect(screen, w, h);
            st.modal_rect = Some(rect);
            let title = format!("Models · {provider_display}");
            let inner = draw_modal_frame(frame, rect, &title, "enter select · ← providers · esc");

            let inner_w = inner.width as usize;
            let mut lines = Vec::new();
            lines.push(search_input_line(query, inner_w));
            let cur_x =
                (inner.x + 3 + query.width() as u16).min(inner.x + inner.width.saturating_sub(1));
            if inner.height > 0 {
                frame.set_cursor_position((cur_x, inner.y));
            }
            lines.push(Line::from(Span::styled(
                "─".repeat(inner_w),
                Style::default().fg(border_subtle()).bg(bg_elevated()),
            )));

            let list_h = (inner.height as usize).saturating_sub(2).max(1);
            let sel = (*selected).min(filtered.len().saturating_sub(1));
            let scroll = sel.saturating_sub(list_h.saturating_sub(1));

            for (idx, item) in filtered.iter().enumerate().skip(scroll).take(list_h) {
                let is_sel = idx == sel;
                let bg = if is_sel { bg_selected() } else { bg_elevated() };
                let marker = if item.is_current { "●" } else { " " };
                let right = if item.model != item.display_name {
                    format!(" {} ", item.model)
                } else {
                    String::new()
                };
                let right_disp = if right.is_empty() || inner_w < 26 {
                    String::new()
                } else {
                    truncate_str(&right, (inner_w / 2).min(28))
                };
                let max_name = inner_w.saturating_sub(right_disp.width() + 4);
                let left = format!(" {marker} {}", truncate_str(&item.display_name, max_name));
                let pad = inner_w.saturating_sub(left.width() + right_disp.width());
                lines.push(Line::from(vec![
                    Span::styled(
                        left,
                        Style::default()
                            .fg(if item.is_current {
                                accent_coral()
                            } else {
                                text_primary()
                            })
                            .bg(bg),
                    ),
                    Span::styled(" ".repeat(pad), Style::default().bg(bg)),
                    Span::styled(right_disp, Style::default().fg(text_muted()).bg(bg)),
                ]));
            }
            if filtered.is_empty() {
                let (msg, fg) = if st
                    .picker_loading
                    .as_deref()
                    .is_some_and(|p| p.eq_ignore_ascii_case(provider))
                {
                    (" Loading models…", accent_coral())
                } else {
                    (" No models match", text_muted())
                };
                lines.push(Line::from(Span::styled(
                    truncate_str(msg, inner_w),
                    Style::default().fg(fg).bg(bg_elevated()),
                )));
            }
            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::SelectModal {
            title,
            items,
            selected,
            action,
        } => {
            let desired_h = (items.len() as u16).saturating_add(2).clamp(6, 20);
            let (w, h) = responsive_modal_size(screen, 6, 4, 46, 76, 6, desired_h);
            let rect = centered_rect(screen, w, h);
            st.modal_rect = Some(rect);
            let hint = if matches!(
                action,
                crate::commands::chat::tui::state::SelectAction::SettingsMain
            ) {
                "↑/↓ · space toggle · enter · esc"
            } else if matches!(
                action,
                crate::commands::chat::tui::state::SelectAction::SettingsVoice
                    | crate::commands::chat::tui::state::SelectAction::SettingsVoiceList
            ) {
                "↑/↓ · enter · backspace back · esc"
            } else {
                "↑/↓ · enter · esc"
            };
            let inner = draw_modal_frame(frame, rect, title, hint);
            let list_h = (inner.height as usize).max(1);
            let sel = (*selected).min(items.len().saturating_sub(1));
            let scroll = sel.saturating_sub(list_h.saturating_sub(1));
            let lines: Vec<Line<'static>> = items
                .iter()
                .enumerate()
                .skip(scroll)
                .take(list_h)
                .map(|(idx, item)| {
                    let is_sel = idx == sel;
                    let bg = if is_sel { bg_selected() } else { bg_elevated() };
                    let prefix = if is_sel { " ▸ " } else { "   " };
                    let text = format!(
                        "{prefix}{}",
                        truncate_str(item, (inner.width as usize).saturating_sub(3))
                    );
                    let pad = (inner.width as usize).saturating_sub(text.width());
                    Line::from(vec![
                        Span::styled(
                            text,
                            Style::default()
                                .fg(if is_sel {
                                    accent_coral()
                                } else {
                                    text_primary()
                                })
                                .bg(bg),
                        ),
                        Span::styled(" ".repeat(pad), Style::default().bg(bg)),
                    ])
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::InputModal {
            title,
            prompt,
            value,
            secret,
            ..
        } => {
            let (w, max_h) = responsive_modal_size(screen, 6, 4, 46, 76, 7, 14);
            let inner_w = w.saturating_sub(2) as usize;
            let prompt_lines = wrap_plain_text(prompt, inner_w.saturating_sub(2).max(8));
            let desired_h = (prompt_lines.len() as u16)
                .saturating_add(4)
                .clamp(7, max_h);
            let rect = centered_rect(screen, w, desired_h.min(max_h));
            st.modal_rect = Some(rect);
            let inner = draw_modal_frame(frame, rect, title, "enter · esc");
            let actual_w = inner.width as usize;
            let actual_h = inner.height as usize;
            let masked = if *secret {
                "*".repeat(value.chars().count())
            } else {
                value.clone()
            };
            let avail_input = actual_w.saturating_sub(3);
            let mut shown = masked;
            if shown.width() > avail_input {
                let mut chars: Vec<char> = shown.chars().collect();
                while chars.iter().collect::<String>().width() > avail_input && !chars.is_empty() {
                    chars.remove(0);
                }
                shown = chars.into_iter().collect();
            }
            let max_prompt_rows = actual_h.saturating_sub(2).max(1);
            let mut lines: Vec<Line<'static>> = prompt_lines
                .into_iter()
                .take(max_prompt_rows)
                .map(|pl| {
                    Line::from(Span::styled(
                        format!(" {}", truncate_str(&pl, actual_w.saturating_sub(2))),
                        Style::default().fg(text_muted()).bg(bg_elevated()),
                    ))
                })
                .collect();
            if lines.len() + 2 <= actual_h {
                lines.push(Line::default());
            }
            lines.push(Line::from(Span::styled(
                format!(" {}_", shown),
                Style::default().fg(accent_coral()).bg(bg_elevated()),
            )));
            if actual_h > 0 {
                let cur_x = (inner.x + 1 + shown.width() as u16)
                    .min(inner.x + inner.width.saturating_sub(1));
                let cur_y = (inner.y + lines.len().saturating_sub(1) as u16)
                    .min(inner.y + inner.height.saturating_sub(1));
                frame.set_cursor_position((cur_x, cur_y));
            }
            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::TextModal {
            title,
            lines,
            scroll,
        } => {
            let (w, h) = responsive_modal_size(screen, 6, 4, 50, 82, 12, 24);
            let rect = centered_rect(screen, w, h);
            st.modal_rect = Some(rect);
            let inner = draw_modal_frame(frame, rect, title, "↑/↓ · pgup/pgdn · esc");
            let visible = (inner.height as usize).max(1);
            let max_scroll = lines.len().saturating_sub(visible);
            let rows: Vec<Line<'static>> = lines
                .iter()
                .skip((*scroll).min(max_scroll))
                .take(visible)
                .map(|line| {
                    Line::from(Span::styled(
                        truncate_str(line, (inner.width as usize).saturating_sub(1)),
                        Style::default().fg(text_primary()).bg(bg_elevated()),
                    ))
                })
                .collect();
            frame.render_widget(Paragraph::new(rows), inner);
        }
        ActiveModal::ThinkingPicker { current, selected } => {
            let options = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
            let (w, h) = responsive_modal_size(screen, 6, 4, 34, 44, 9, 11);
            let rect = centered_rect(screen, w, h);
            st.modal_rect = Some(rect);
            let inner = draw_modal_frame(frame, rect, "Thinking Level", "enter · esc");
            let cur_str = current
                .map(|e| e.to_string())
                .unwrap_or_else(|| "off".to_string());
            let list_h = (inner.height as usize).max(1);
            let sel = (*selected).min(options.len().saturating_sub(1));
            let scroll = sel.saturating_sub(list_h.saturating_sub(1));
            let lines: Vec<Line<'static>> = options
                .iter()
                .enumerate()
                .skip(scroll)
                .take(list_h)
                .map(|(idx, opt)| {
                    let is_sel = idx == sel;
                    let is_cur = *opt == cur_str;
                    let bg = if is_sel { bg_selected() } else { bg_elevated() };
                    let marker = if is_cur { "●" } else { " " };
                    let text = truncate_str(&format!(" {marker} {opt}"), inner.width as usize);
                    let pad = (inner.width as usize).saturating_sub(text.width());
                    Line::from(vec![
                        Span::styled(
                            text,
                            Style::default()
                                .fg(if is_cur {
                                    accent_coral()
                                } else {
                                    text_primary()
                                })
                                .bg(bg),
                        ),
                        Span::styled(" ".repeat(pad), Style::default().bg(bg)),
                    ])
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::OrchestratorEditor {
            name,
            draft,
            selected,
            ..
        } => {
            use crate::commands::chat::tui::actions::orchestrator::{
                effort_stop_index, EFFORT_STOPS,
            };

            let add_row = draft.subagents.len() + 1;
            let save_row = draft.subagents.len() + 2;
            let desired_h = (draft.subagents.len() as u16 + 13).clamp(14, 26);
            let (w, h) = responsive_modal_size(screen, 6, 4, 58, 80, desired_h, 28);
            let rect = centered_rect(screen, w, h);
            st.modal_rect = Some(rect);
            let title = format!("Orchestrator · {name}");
            let inner = draw_modal_frame(
                frame,
                rect,
                &title,
                "↑/↓ select · ←/→ thinking · enter pick · del remove · ctrl+s save · esc",
            );

            let inner_w = inner.width as usize;
            let sel = (*selected).min(save_row);
            let mut lines: Vec<Line<'static>> = Vec::new();

            let push_row = |lines: &mut Vec<Line<'static>>,
                            label: String,
                            right: String,
                            is_sel: bool,
                            label_color: Color| {
                let bg = if is_sel { bg_selected() } else { bg_elevated() };
                let marker = if is_sel { "❯" } else { " " };
                let right_disp = if right.is_empty() || inner_w < 28 {
                    String::new()
                } else {
                    truncate_str(&format!(" {right} "), (inner_w / 2).min(30))
                };
                let max_left = inner_w.saturating_sub(right_disp.width() + 4);
                let left = format!(" {marker} {}", truncate_str(&label, max_left));
                let pad = inner_w.saturating_sub(left.width() + right_disp.width());
                lines.push(Line::from(vec![
                    Span::styled(left, Style::default().fg(label_color).bg(bg)),
                    Span::styled(" ".repeat(pad), Style::default().bg(bg)),
                    Span::styled(right_disp, Style::default().fg(text_muted()).bg(bg)),
                ]));
            };

            lines.push(Line::from(Span::styled(
                truncate_str(" Orchestrator", inner_w),
                Style::default()
                    .fg(text_muted())
                    .bg(bg_elevated())
                    .add_modifier(Modifier::BOLD),
            )));
            let orch_model = if draft.orchestrator.model.is_empty() {
                "(not set — press Enter)".to_string()
            } else {
                format!(
                    "{} / {}",
                    friendly_provider_name(&draft.orchestrator.provider),
                    draft.orchestrator.model
                )
            };
            let orch_effort = draft
                .orchestrator
                .thinking
                .map(|e| e.as_str().to_string())
                .unwrap_or_else(|| "off".to_string());
            push_row(
                &mut lines,
                format!("Main: {orch_model}"),
                format!("thinking: {orch_effort}"),
                sel == 0,
                if sel == 0 {
                    accent_coral()
                } else {
                    text_primary()
                },
            );

            lines.push(Line::from(Span::styled(
                " ".repeat(inner_w),
                Style::default().bg(bg_elevated()),
            )));
            lines.push(Line::from(Span::styled(
                truncate_str(" Subagents", inner_w),
                Style::default()
                    .fg(text_muted())
                    .bg(bg_elevated())
                    .add_modifier(Modifier::BOLD),
            )));

            for (idx, sub) in draft.subagents.iter().enumerate() {
                let row_idx = idx + 1;
                let sub_effort = sub
                    .thinking
                    .map(|e| e.as_str().to_string())
                    .unwrap_or_else(|| "off".to_string());
                let label = format!(
                    "Subagent #{}: {} / {}",
                    idx + 1,
                    friendly_provider_name(&sub.provider),
                    sub.model
                );
                push_row(
                    &mut lines,
                    label,
                    format!("thinking: {sub_effort}"),
                    sel == row_idx,
                    if sel == row_idx {
                        accent_coral()
                    } else {
                        text_primary()
                    },
                );
            }

            push_row(
                &mut lines,
                "+ Add subagent".to_string(),
                String::new(),
                sel == add_row,
                if sel == add_row {
                    accent_auto()
                } else {
                    text_muted()
                },
            );

            lines.push(Line::from(Span::styled(
                "─".repeat(inner_w),
                Style::default().fg(border_subtle()).bg(bg_elevated()),
            )));

            // Effort/Thinking slider for the highlighted model (Orchestrator or a Subagent).
            let highlighted_effort = if sel == 0 {
                Some(draft.orchestrator.thinking)
            } else if sel >= 1 && sel <= draft.subagents.len() {
                Some(draft.subagents[sel - 1].thinking)
            } else {
                None
            };

            if let Some(effort) = highlighted_effort {
                let active_idx = effort_stop_index(effort);
                let stop_count = EFFORT_STOPS.len();
                let seg_w = ((inner_w.saturating_sub(10)) / (stop_count - 1)).clamp(4, 12);

                let mut track_spans = vec![Span::styled(
                    "  ",
                    Style::default().bg(bg_elevated()),
                )];
                for i in 0..stop_count {
                    let dot = if i <= active_idx { "●" } else { "○" };
                    let dot_fg = if i <= active_idx {
                        accent_auto()
                    } else {
                        text_muted()
                    };
                    track_spans.push(Span::styled(
                        dot,
                        Style::default().fg(dot_fg).bg(bg_elevated()),
                    ));
                    if i + 1 < stop_count {
                        let line_fg = if i < active_idx {
                            accent_auto()
                        } else {
                            text_muted()
                        };
                        track_spans.push(Span::styled(
                            "─".repeat(seg_w),
                            Style::default().fg(line_fg).bg(bg_elevated()),
                        ));
                    }
                }
                track_spans.push(Span::styled(
                    "  ◀ ▶",
                    Style::default().fg(text_muted()).bg(bg_elevated()),
                ));
                lines.push(Line::from(track_spans));

                let mut label_spans = vec![Span::styled(
                    "  ",
                    Style::default().bg(bg_elevated()),
                )];
                for (i, (_, lbl)) in EFFORT_STOPS.iter().enumerate() {
                    let is_cur = i == active_idx;
                    let fg = if is_cur { accent_auto() } else { text_muted() };
                    let mut st_lbl = Style::default().fg(fg).bg(bg_elevated());
                    if is_cur {
                        st_lbl = st_lbl.add_modifier(Modifier::BOLD);
                    }
                    let cell_w = if i + 1 < stop_count {
                        seg_w + 1
                    } else {
                        lbl.len()
                    };
                    let padded = format!("{:<width$}", lbl, width = cell_w);
                    label_spans.push(Span::styled(padded, st_lbl));
                }
                lines.push(Line::from(label_spans));
            } else {
                lines.push(Line::from(Span::styled(
                    truncate_str("  Highlight a model row to adjust thinking (←/→)", inner_w),
                    Style::default().fg(text_muted()).bg(bg_elevated()),
                )));
            }

            lines.push(Line::from(Span::styled(
                " ".repeat(inner_w),
                Style::default().bg(bg_elevated()),
            )));
            push_row(
                &mut lines,
                "[ Save Orchestrator ]".to_string(),
                "ctrl+s".to_string(),
                sel == save_row,
                if sel == save_row {
                    green_ok()
                } else {
                    text_primary()
                },
            );

            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::Approval {
            req,
            selected,
            diff_scroll,
            editing_input,
            input,
            asking,
            reply,
            reply_scroll,
            error,
            ..
        } => {
            let (max_w, max_h) = responsive_modal_size(screen, 4, 2, 58, 100, 10, 34);
            let hint = if *editing_input {
                "enter ask · ctrl+s send · esc back"
            } else if reply.is_some() {
                "↑/↓ · enter · q ask · pgup/pgdn · [/] diff · ctrl+s send"
            } else if !input.trim().is_empty() {
                "↑/↓ · enter · q ask · [/] diff · ctrl+s send"
            } else {
                "↑/↓ · enter · y/a/n/q · [/] diff"
            };
            let box_w_est = max_w.saturating_sub(4) as usize;
            let box_inner_w_est = box_w_est.saturating_sub(3).max(16);

            // Build data preview lines first so the popup prioritizes the actual payload.
            let preview_lines: Vec<String> = if let Some(ref diff) = req.diff {
                let mut raw: Vec<&str> = diff.lines().collect();
                if raw.first().is_some_and(|l| l.starts_with("--- "))
                    && raw.get(1).is_some_and(|l| l.starts_with("+++ "))
                {
                    raw.drain(..2);
                }
                if raw.len() > 1 && raw.first().is_some_and(|l| l.starts_with("@@ -0,0 ")) {
                    raw.remove(0);
                }
                raw.into_iter().map(str::to_string).collect()
            } else if req.tool == "terminal" {
                let mut rows = Vec::new();
                if let Some(cmd) = req.arguments.get("command").and_then(|c| c.as_str()) {
                    for (i, line) in cmd.lines().enumerate() {
                        let prefixed = if i == 0 {
                            format!("$ {line}")
                        } else {
                            format!("  {line}")
                        };
                        rows.extend(wrap_plain_text(&prefixed, box_inner_w_est));
                    }
                }
                if let Some(wd) = req
                    .arguments
                    .get("workdir")
                    .and_then(|w| w.as_str())
                    .filter(|w| !w.trim().is_empty())
                {
                    rows.push(format!("in {}", shorten_path(wd)));
                }
                rows
            } else if let Some(content) = req.arguments.get("content").and_then(|c| c.as_str()) {
                content.lines().map(|l| format!("+{l}")).collect()
            } else if !req.arguments.is_null()
                && !req.arguments.as_object().is_some_and(|obj| obj.is_empty())
            {
                let pretty = serde_json::to_string_pretty(&req.arguments)
                    .unwrap_or_else(|_| req.arguments.to_string());
                pretty
                    .lines()
                    .flat_map(|l| wrap_plain_text(l, box_inner_w_est))
                    .collect()
            } else {
                Vec::new()
            };
            let is_diff_preview = req.diff.is_some()
                || (req.tool != "terminal"
                    && req
                        .arguments
                        .get("content")
                        .and_then(|c| c.as_str())
                        .is_some());

            let reply_rendered: Vec<Line<'static>> = reply
                .as_deref()
                .map(|text| {
                    let mut md = render_markdown(text, box_inner_w_est.max(8));
                    while md.last().is_some_and(|l| l.spans.is_empty()) {
                        md.pop();
                    }
                    md
                })
                .unwrap_or_default();

            let has_input_row = *editing_input || !input.trim().is_empty();
            let choices_rows = 4usize;
            let header_rows = 2usize; // 1 header line + 1 spacer
            let input_rows = if has_input_row { 2usize } else { 0usize };
            let status_rows = if *asking || error.is_some() {
                2usize
            } else {
                0usize
            };
            let desired_preview = if preview_lines.is_empty() {
                0
            } else {
                preview_lines.len() + 1
            };
            let desired_reply = if reply_rendered.is_empty() {
                0
            } else {
                reply_rendered.len().min(6) + 1
            };
            let desired_inner_h = header_rows
                + desired_preview
                + desired_reply
                + status_rows
                + choices_rows
                + input_rows;
            let desired_h = (desired_inner_h as u16).saturating_add(2);
            let h = desired_h.clamp(10.min(max_h), max_h);
            let rect = centered_rect(screen, max_w, h);
            st.modal_rect = Some(rect);
            let inner = draw_approval_frame(frame, rect, hint);

            let inner_w = inner.width as usize;
            let inner_h = inner.height as usize;

            let fixed_rows = 1 + choices_rows + input_rows + status_rows;
            let body_budget = inner_h.saturating_sub(fixed_rows);
            let (preview_budget, reply_budget) =
                if !preview_lines.is_empty() && !reply_rendered.is_empty() {
                    let want_p = preview_lines.len();
                    let want_r = reply_rendered.len().min(6);
                    let avail = body_budget.saturating_sub(3); // spacers
                    if want_p + want_r <= avail {
                        (want_p, want_r)
                    } else {
                        let min_p = (avail / 2).max(2).min(want_p);
                        let r = want_r.min(avail.saturating_sub(min_p)).max(2.min(want_r));
                        let p = avail.saturating_sub(r).min(want_p).max(1);
                        (p, r)
                    }
                } else if !preview_lines.is_empty() {
                    (
                        preview_lines
                            .len()
                            .min(body_budget.saturating_sub(2).max(1)),
                        0,
                    )
                } else if !reply_rendered.is_empty() {
                    (
                        0,
                        reply_rendered
                            .len()
                            .min(body_budget.saturating_sub(2).max(1)),
                    )
                } else {
                    (0, 0)
                };

            let mut lines = Vec::new();
            let total_preview = preview_lines.len();
            let max_dscroll = total_preview.saturating_sub(preview_budget);
            let dscroll = (*diff_scroll).min(max_dscroll);
            let range_badge = if preview_budget > 0 && total_preview > preview_budget {
                format!(
                    " {}-{} / {} ",
                    dscroll + 1,
                    dscroll + preview_budget,
                    total_preview
                )
            } else {
                String::new()
            };
            let target_part = req
                .target
                .as_deref()
                .filter(|t| !(req.tool == "terminal" && !preview_lines.is_empty() && t.len() > 40))
                .map(shorten_path)
                .unwrap_or_default();
            let tool_badge = format!(" {} ", truncate_str(&req.tool, inner_w.saturating_sub(4)));
            let left_prefix_w = 1 + tool_badge.width() + if target_part.is_empty() { 0 } else { 2 };
            let target_avail = inner_w.saturating_sub(left_prefix_w + range_badge.width() + 1);
            let mut hdr_spans = vec![
                Span::styled(" ", Style::default().bg(bg_approval())),
                Span::styled(
                    tool_badge.clone(),
                    Style::default()
                        .fg(bg_canvas())
                        .bg(accent_coral())
                        .add_modifier(Modifier::BOLD),
                ),
            ];
            let mut hdr_used = 1 + tool_badge.width();
            if !target_part.is_empty() && target_avail > 4 {
                let target_disp = truncate_str(&target_part, target_avail);
                hdr_used += 2 + target_disp.width();
                hdr_spans.push(Span::styled("  ", Style::default().bg(bg_approval())));
                hdr_spans.push(Span::styled(
                    target_disp,
                    Style::default()
                        .fg(text_primary())
                        .bg(bg_approval())
                        .add_modifier(Modifier::BOLD),
                ));
            }
            if !range_badge.is_empty() && hdr_used + range_badge.width() <= inner_w {
                let pad = inner_w.saturating_sub(hdr_used + range_badge.width());
                hdr_spans.push(Span::styled(
                    " ".repeat(pad),
                    Style::default().bg(bg_approval()),
                ));
                hdr_spans.push(Span::styled(
                    range_badge,
                    Style::default()
                        .fg(yellow_warn())
                        .bg(bg_approval())
                        .add_modifier(Modifier::BOLD),
                ));
            }
            lines.push(Line::from(hdr_spans));
            if inner_h >= 9 {
                lines.push(Line::default());
            }

            if preview_budget > 0 && !preview_lines.is_empty() {
                let box_bg = bg_code();
                let box_w = inner_w.saturating_sub(2);
                let box_inner_w = box_w.saturating_sub(3);
                for dline in preview_lines.iter().skip(dscroll).take(preview_budget) {
                    let trimmed_start = dline.trim_start();
                    let color = if dline.starts_with("@@") {
                        cyan_link()
                    } else if is_diff_preview && trimmed_start.starts_with('+') {
                        green_ok()
                    } else if is_diff_preview && trimmed_start.starts_with('-') {
                        red_err()
                    } else if req.tool == "terminal" && dline.starts_with("$ ") {
                        cyan_link()
                    } else if is_diff_preview {
                        text_muted()
                    } else {
                        text_primary()
                    };
                    let shown = truncate_str(dline, box_inner_w);
                    let pad = box_w.saturating_sub(2 + shown.width());
                    lines.push(Line::from(vec![
                        Span::styled(" ", Style::default().bg(bg_approval())),
                        Span::styled("┃ ", Style::default().fg(yellow_warn()).bg(box_bg)),
                        Span::styled(shown, Style::default().fg(color).bg(box_bg)),
                        Span::styled(" ".repeat(pad), Style::default().bg(box_bg)),
                        Span::styled(" ", Style::default().bg(bg_approval())),
                    ]));
                }
                lines.push(Line::default());
            }

            if *asking && status_rows > 0 {
                let spinner = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
                lines.push(Line::from(Span::styled(
                    format!(" {spinner} Asking…"),
                    Style::default().fg(accent_coral()).bg(bg_approval()),
                )));
                lines.push(Line::default());
            } else if let Some(ref err) = error {
                if status_rows > 0 {
                    lines.push(Line::from(Span::styled(
                        format!(" ⚠ {}", truncate_str(err, inner_w.saturating_sub(4))),
                        Style::default().fg(red_err()).bg(bg_approval()),
                    )));
                    lines.push(Line::default());
                }
            } else if reply_budget > 0 && !reply_rendered.is_empty() {
                let box_bg = bg_surface();
                let box_w = inner_w.saturating_sub(2);
                let box_inner_w = box_w.saturating_sub(3);
                let max_scroll = reply_rendered.len().saturating_sub(reply_budget);
                let scroll = (*reply_scroll).min(max_scroll);
                for rline in reply_rendered.iter().skip(scroll).take(reply_budget) {
                    let mut spans = vec![
                        Span::styled(" ", Style::default().bg(bg_approval())),
                        Span::styled("│ ", Style::default().fg(accent_coral()).bg(box_bg)),
                    ];
                    let mut used = 0usize;
                    for sp in &rline.spans {
                        let w = sp.content.width();
                        if used + w > box_inner_w {
                            let avail = box_inner_w.saturating_sub(used);
                            let cut = truncate_str(&sp.content, avail);
                            used += cut.width();
                            spans.push(Span::styled(
                                cut,
                                sp.style
                                    .bg(box_bg)
                                    .fg(sp.style.fg.unwrap_or_else(text_primary)),
                            ));
                            break;
                        }
                        used += w;
                        spans.push(Span::styled(
                            sp.content.clone(),
                            sp.style
                                .bg(box_bg)
                                .fg(sp.style.fg.unwrap_or_else(text_primary)),
                        ));
                    }
                    let pad = box_w.saturating_sub(2 + used);
                    spans.push(Span::styled(" ".repeat(pad), Style::default().bg(box_bg)));
                    spans.push(Span::styled(" ", Style::default().bg(bg_approval())));
                    lines.push(Line::from(spans));
                }
                lines.push(Line::default());
            }

            let choices = [
                ("[y]", " Allow once", green_ok()),
                ("[a]", " Allow for this session (Auto mode)", yellow_warn()),
                ("[n]", " Deny", red_err()),
                ("[q]", " Ask a question / add a suggestion", cyan_link()),
            ];
            for (idx, (key, label, base_fg)) in choices.iter().enumerate() {
                let is_sel = !*editing_input && idx == *selected;
                let bg = if is_sel {
                    bg_approval_selected()
                } else {
                    bg_approval()
                };
                let prefix = if is_sel { " ▸ " } else { "   " };
                let full_len = prefix.width() + key.width() + label.width();
                let label_avail = inner_w.saturating_sub(prefix.width() + key.width());
                let shown_label = truncate_str(label, label_avail);
                let pad = inner_w.saturating_sub(full_len.min(inner_w));
                let mut key_style = Style::default()
                    .fg(*base_fg)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD);
                let mut label_style = Style::default()
                    .fg(if is_sel { text_primary() } else { *base_fg })
                    .bg(bg);
                if is_sel {
                    label_style = label_style.add_modifier(Modifier::BOLD);
                    key_style = key_style.add_modifier(Modifier::BOLD);
                }
                lines.push(Line::from(vec![
                    Span::styled(
                        prefix,
                        Style::default()
                            .fg(if is_sel { yellow_warn() } else { text_dim() })
                            .bg(bg)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(*key, key_style),
                    Span::styled(shown_label, label_style),
                    Span::styled(" ".repeat(pad), Style::default().bg(bg)),
                ]));
            }

            if *editing_input {
                if lines.len() + 2 <= inner_h {
                    lines.push(Line::default());
                }
                let prompt_prefix = if inner_w < 32 {
                    " ▸ Ask: "
                } else {
                    " ▸ Ask / suggest: "
                };
                let avail = inner_w.saturating_sub(prompt_prefix.width() + 1);
                let mut shown = input.clone();
                if shown.width() > avail {
                    let mut chars: Vec<char> = shown.chars().collect();
                    while chars.iter().collect::<String>().width() > avail && !chars.is_empty() {
                        chars.remove(0);
                    }
                    shown = chars.into_iter().collect();
                }
                let row_text = format!("{prompt_prefix}{shown}");
                let pad = inner_w.saturating_sub(row_text.width());
                lines.push(Line::from(vec![
                    Span::styled(
                        prompt_prefix,
                        Style::default()
                            .fg(yellow_warn())
                            .bg(bg_approval_selected())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        shown.clone(),
                        Style::default()
                            .fg(text_primary())
                            .bg(bg_approval_selected()),
                    ),
                    Span::styled(" ".repeat(pad), Style::default().bg(bg_approval_selected())),
                ]));
                if inner.height > 0 {
                    let cur_x = (inner.x + prompt_prefix.width() as u16 + shown.width() as u16)
                        .min(inner.x + inner.width.saturating_sub(1));
                    let cur_y = (inner.y + (lines.len().saturating_sub(1)) as u16)
                        .min(inner.y + inner.height.saturating_sub(1));
                    frame.set_cursor_position((cur_x, cur_y));
                }
            } else if !input.trim().is_empty() {
                if lines.len() + 2 <= inner_h {
                    lines.push(Line::default());
                }
                let note_line = format!(
                    " Draft: \"{}\" · Ctrl+S send",
                    truncate_str(input.trim(), inner_w.saturating_sub(26))
                );
                lines.push(Line::from(Span::styled(
                    truncate_str(&note_line, inner_w),
                    Style::default().fg(text_muted()).bg(bg_approval()),
                )));
            }

            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::Clarify {
            req,
            question_idx,
            selected,
            custom,
            editing_custom,
            scroll,
            follow_selection,
            ..
        } => {
            let (max_w, max_h) = responsive_modal_size(screen, 6, 4, 48, 96, 10, 32);
            let min_w = 54.min(max_w);
            let title = format!(
                "Question {}/{}",
                *question_idx + 1,
                req.questions.len().max(1)
            );
            let q_opt = req.questions.get(*question_idx);
            let longest_content = q_opt
                .map(|q| {
                    let q_max = q
                        .question
                        .lines()
                        .map(UnicodeWidthStr::width)
                        .max()
                        .unwrap_or(0)
                        + 2;
                    let c_max = q.choices.iter().map(|c| c.width() + 5).max().unwrap_or(0);
                    let custom_max = custom.width() + 14;
                    q_max.max(c_max).max(custom_max).max(32)
                })
                .unwrap_or(48) as u16;
            let w = longest_content.saturating_add(4).clamp(min_w, max_w);
            let inner_w = w.saturating_sub(2) as usize;
            let wrap_w = inner_w.saturating_sub(5).max(8);

            let mut lines: Vec<Line<'static>> = Vec::new();
            let mut option_ranges: Vec<(usize, usize)> = Vec::new();
            let mut cursor_pos: Option<(usize, u16)> = None;

            if let Some(q) = q_opt {
                for para in q.question.lines() {
                    if para.trim().is_empty() {
                        lines.push(Line::default());
                    } else {
                        for wline in wrap_plain_text(para, inner_w.saturating_sub(2).max(8)) {
                            lines.push(Line::from(Span::styled(
                                format!(" {wline}"),
                                Style::default()
                                    .fg(text_primary())
                                    .bg(bg_elevated())
                                    .add_modifier(Modifier::BOLD),
                            )));
                        }
                    }
                }
                lines.push(Line::from(Span::styled(
                    format!(" {}", "─".repeat(inner_w.saturating_sub(2))),
                    Style::default().fg(border_subtle()).bg(bg_elevated()),
                )));

                // Wrap every choice upfront so nothing is truncated with `...`.
                let mut wrapped_choices: Vec<Vec<Line<'static>>> =
                    Vec::with_capacity(q.choices.len());
                for (idx, choice) in q.choices.iter().enumerate() {
                    let is_sel = !*editing_custom && idx == *selected;
                    let bg = if is_sel { bg_selected() } else { bg_elevated() };
                    let title_fg = if is_sel {
                        accent_coral()
                    } else {
                        text_primary()
                    };
                    let body_fg = if is_sel { text_primary() } else { text_muted() };

                    let split_opt = choice
                        .split_once(" \u{2013} ")
                        .map(|(h, b)| (h, " \u{2013} ", b))
                        .or_else(|| {
                            choice
                                .split_once(" \u{2014} ")
                                .map(|(h, b)| (h, " \u{2014} ", b))
                        })
                        .or_else(|| choice.split_once(" - ").map(|(h, b)| (h, " - ", b)));

                    let spans = if let Some((head, sep, body)) = split_opt {
                        vec![
                            Span::styled(
                                head.to_string(),
                                Style::default()
                                    .fg(title_fg)
                                    .bg(bg)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                format!("{sep}{body}"),
                                Style::default().fg(body_fg).bg(bg),
                            ),
                        ]
                    } else {
                        vec![Span::styled(
                            choice.clone(),
                            Style::default().fg(title_fg).bg(bg),
                        )]
                    };
                    wrapped_choices.push(wrap_spans(&spans, wrap_w));
                }

                let any_choice_wrapped = wrapped_choices.iter().any(|wc| wc.len() > 1);
                for (idx, wlines) in wrapped_choices.into_iter().enumerate() {
                    let is_sel = !*editing_custom && idx == *selected;
                    let bg = if is_sel { bg_selected() } else { bg_elevated() };
                    let start_row = lines.len();
                    for (w_idx, mut wline) in wlines.into_iter().enumerate() {
                        let prefix = if w_idx == 0 && is_sel { " ▸ " } else { "   " };
                        let row_w = 3 + wline.width();
                        let pad = inner_w.saturating_sub(row_w);
                        let mut spans = Vec::with_capacity(wline.spans.len() + 2);
                        spans.push(Span::styled(
                            prefix,
                            Style::default()
                                .fg(if is_sel { accent_coral() } else { text_muted() })
                                .bg(bg),
                        ));
                        spans.append(&mut wline.spans);
                        spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
                        lines.push(Line::from(spans));
                    }
                    let end_row = lines.len().saturating_sub(1);
                    option_ranges.push((start_row, end_row));
                    if any_choice_wrapped && idx + 1 < q.choices.len() {
                        lines.push(Line::default());
                    }
                }

                if any_choice_wrapped && !q.choices.is_empty() {
                    lines.push(Line::default());
                }

                let custom_idx = q.choices.len();
                let is_sel = *editing_custom || *selected == custom_idx;
                let bg = if is_sel { bg_selected() } else { bg_elevated() };
                let start_row = lines.len();
                if *editing_custom || !custom.trim().is_empty() {
                    let custom_wrap_w = inner_w.saturating_sub(12).max(12);
                    let chunks = wrap_plain_text(custom, custom_wrap_w);
                    let chunk_count = chunks.len();
                    for (w_idx, chunk) in chunks.into_iter().enumerate() {
                        let prefix = if w_idx == 0 {
                            " ▸ Answer: "
                        } else {
                            "           "
                        };
                        let prefix_w = prefix.width();
                        let row_w = prefix_w + chunk.width();
                        let pad = inner_w.saturating_sub(row_w);
                        if *editing_custom && w_idx + 1 == chunk_count {
                            cursor_pos = Some((lines.len(), (prefix_w + chunk.width()) as u16));
                        }
                        lines.push(Line::from(vec![
                            Span::styled(
                                prefix,
                                Style::default()
                                    .fg(if is_sel { accent_coral() } else { text_muted() })
                                    .bg(bg),
                            ),
                            Span::styled(chunk, Style::default().fg(text_primary()).bg(bg)),
                            Span::styled(" ".repeat(pad), Style::default().bg(bg)),
                        ]));
                    }
                } else {
                    let label = if is_sel {
                        " ▸ Type your answer..."
                    } else {
                        "   Type your answer..."
                    };
                    let pad = inner_w.saturating_sub(label.width());
                    lines.push(Line::from(vec![
                        Span::styled(
                            label,
                            Style::default()
                                .fg(if is_sel { accent_coral() } else { text_muted() })
                                .bg(bg),
                        ),
                        Span::styled(" ".repeat(pad), Style::default().bg(bg)),
                    ]));
                }
                let end_row = lines.len().saturating_sub(1);
                option_ranges.push((start_row, end_row));
            }

            let desired_h = (lines.len() as u16).saturating_add(2);
            let h = desired_h.clamp(8.min(max_h), max_h);
            let rect = centered_rect(screen, w, h);
            st.modal_rect = Some(rect);

            let inner_h = rect.height.saturating_sub(2) as usize;
            let max_scroll = lines.len().saturating_sub(inner_h);
            if *follow_selection {
                if let Some(q) = q_opt {
                    let active_opt = if *editing_custom {
                        q.choices.len()
                    } else {
                        (*selected).min(option_ranges.len().saturating_sub(1))
                    };
                    if let Some(&(sel_start, sel_end)) = option_ranges.get(active_opt) {
                        if sel_end >= *scroll + inner_h {
                            *scroll = sel_end + 1 - inner_h;
                        }
                        if sel_start < *scroll {
                            *scroll = sel_start;
                        }
                        if *selected == 0 && !*editing_custom && sel_end < inner_h {
                            *scroll = 0;
                        }
                    }
                }
            }
            *scroll = (*scroll).min(max_scroll);

            let hint = if *editing_custom {
                "enter submit · esc back"
            } else if max_scroll > 0 {
                "↑/↓ · pgup/pgdn · enter · esc"
            } else {
                "↑/↓ · enter · esc"
            };
            let inner = draw_modal_frame(frame, rect, &title, hint);

            let visible_lines: Vec<Line<'static>> = lines
                .into_iter()
                .skip(*scroll)
                .take(inner.height as usize)
                .collect();
            frame.render_widget(Paragraph::new(visible_lines), inner);

            if let Some((cline, ccol)) = cursor_pos {
                if cline >= *scroll && cline < *scroll + inner.height as usize {
                    let cur_x = (inner.x + ccol).min(inner.x + inner.width.saturating_sub(1));
                    let cur_y = inner.y + (cline - *scroll) as u16;
                    frame.set_cursor_position((cur_x, cur_y));
                }
            }
        }
        ActiveModal::Sudo { prompt, input, .. } => {
            let (w, max_h) = responsive_modal_size(screen, 6, 4, 44, 68, 7, 12);
            let inner_w = w.saturating_sub(2) as usize;
            let prompt_lines = wrap_plain_text(prompt, inner_w.saturating_sub(2).max(8));
            let desired_h = (prompt_lines.len() as u16)
                .saturating_add(4)
                .clamp(7, max_h);
            let rect = centered_rect(screen, w, desired_h.min(max_h));
            st.modal_rect = Some(rect);
            let inner = draw_modal_frame(frame, rect, "Authentication Required", "enter · esc");
            let actual_w = inner.width as usize;
            let actual_h = inner.height as usize;
            let prefix = " Password: ";
            let avail = actual_w.saturating_sub(prefix.width() + 1);
            let masked_len = input.chars().count().min(avail);
            let masked = "*".repeat(masked_len);
            let max_prompt_rows = actual_h.saturating_sub(2).max(1);
            let mut lines: Vec<Line<'static>> = prompt_lines
                .into_iter()
                .take(max_prompt_rows)
                .map(|pl| {
                    Line::from(Span::styled(
                        format!(" {}", truncate_str(&pl, actual_w.saturating_sub(2))),
                        Style::default().fg(text_primary()).bg(bg_elevated()),
                    ))
                })
                .collect();
            if lines.len() + 2 <= actual_h {
                lines.push(Line::default());
            }
            lines.push(Line::from(Span::styled(
                format!("{prefix}{masked}_"),
                Style::default().fg(accent_coral()).bg(bg_elevated()),
            )));
            if actual_h > 0 {
                let cur_x = (inner.x + prefix.width() as u16 + masked.width() as u16)
                    .min(inner.x + inner.width.saturating_sub(1));
                let cur_y = (inner.y + lines.len().saturating_sub(1) as u16)
                    .min(inner.y + inner.height.saturating_sub(1));
                frame.set_cursor_position((cur_x, cur_y));
            }
            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::Help { scroll } => {
            let (w, h) = responsive_modal_size(screen, 6, 4, 48, 76, 10, 24);
            let rect = centered_rect(screen, w, h);
            st.modal_rect = Some(rect);
            let inner = draw_modal_frame(frame, rect, "Keyboard & Commands", "↑/↓ · esc");
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
                ("ctrl+b", "Inspect subagents and tasks"),
            ];
            let visible_h = (inner.height as usize).max(1);
            let max_scroll = help_rows.len().saturating_sub(visible_h);
            let inner_w = inner.width as usize;
            let key_col = (inner_w / 3).clamp(8, 28);
            let desc_max = inner_w.saturating_sub(key_col + 2);
            let lines: Vec<Line<'static>> = help_rows
                .iter()
                .skip((*scroll).min(max_scroll))
                .take(visible_h)
                .map(|(k, v)| {
                    if k.is_empty() {
                        return Line::default();
                    }
                    let key_disp = truncate_str(k, key_col.saturating_sub(1));
                    let pad = key_col.saturating_sub(key_disp.width());
                    Line::from(vec![
                        Span::styled(
                            format!(" {key_disp}{}", " ".repeat(pad)),
                            Style::default().fg(accent_coral()).bg(bg_elevated()),
                        ),
                        Span::styled(
                            truncate_str(v, desc_max),
                            Style::default().fg(text_primary()).bg(bg_elevated()),
                        ),
                    ])
                })
                .collect();
            frame.render_widget(Paragraph::new(lines), inner);
        }
        ActiveModal::SubagentsModal {
            items,
            selected,
            active_only,
        } => {
            let (w, h) = responsive_modal_size(screen, 6, 4, 54, 88, 12, 24);
            let rect = centered_rect(screen, w, h);
            st.modal_rect = Some(rect);
            render_subagents_modal(frame, rect, items, *selected, *active_only, spinner_tick);
        }
        ActiveModal::SubagentInspector {
            items,
            current_idx,
            scroll,
        } => {
            let (w, h) = responsive_modal_size(screen, 4, 2, 60, 110, 14, 38);
            let rect = centered_rect(screen, w, h);
            st.modal_rect = Some(rect);
            render_subagent_inspector(frame, rect, items, *current_idx, *scroll, spinner_tick);
        }
    }
}

pub(crate) fn draw_modal_frame(frame: &mut Frame, rect: Rect, title: &str, hint: &str) -> Rect {
    frame.render_widget(Clear, rect);
    let border_text_w = rect.width.saturating_sub(4) as usize;
    let title_disp = truncate_str(title, border_text_w);
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_subtle()).bg(bg_elevated()))
        .style(Style::default().bg(bg_elevated()))
        .title(Span::styled(
            format!(" {title_disp} "),
            Style::default()
                .fg(text_primary())
                .bg(bg_elevated())
                .add_modifier(Modifier::BOLD),
        ));
    if border_text_w >= 8 && !hint.is_empty() {
        let hint_disp = truncate_str(hint, border_text_w);
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(" {hint_disp} "),
                Style::default().fg(text_muted()).bg(bg_elevated()),
            ))
            .right_aligned(),
        );
    }
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    inner
}

pub(crate) fn draw_approval_frame(frame: &mut Frame, rect: Rect, hint: &str) -> Rect {
    frame.render_widget(Clear, rect);
    let border_text_w = rect.width.saturating_sub(4) as usize;
    let badge = " ⚠ APPROVAL REQUIRED ";
    let title_line = if border_text_w >= badge.width() + 20 {
        Line::from(vec![
            Span::styled(
                badge,
                Style::default()
                    .fg(bg_canvas())
                    .bg(yellow_warn())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " Approve Tool Call ",
                Style::default()
                    .fg(yellow_warn())
                    .bg(bg_approval())
                    .add_modifier(Modifier::BOLD),
            ),
        ])
    } else {
        Line::from(Span::styled(
            truncate_str(badge, border_text_w),
            Style::default()
                .fg(bg_canvas())
                .bg(yellow_warn())
                .add_modifier(Modifier::BOLD),
        ))
    };
    let mut block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(yellow_warn()).bg(bg_approval()))
        .style(Style::default().bg(bg_approval()))
        .title(title_line);
    if border_text_w >= 8 && !hint.is_empty() {
        let hint_disp = truncate_str(hint, border_text_w);
        block = block.title_bottom(
            Line::from(Span::styled(
                format!(" {hint_disp} "),
                Style::default().fg(text_muted()).bg(bg_approval()),
            ))
            .right_aligned(),
        );
    }
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    inner
}

pub(crate) fn search_input_line(query: &str, width: usize) -> Line<'static> {
    let prefix = " > ";
    let shown = if query.is_empty() { "Search..." } else { query };
    let color = if query.is_empty() {
        text_muted()
    } else {
        text_primary()
    };
    let pad = width.saturating_sub(prefix.width() + shown.width());
    Line::from(vec![
        Span::styled(
            prefix,
            Style::default().fg(accent_coral()).bg(bg_elevated()),
        ),
        Span::styled(
            shown.to_string(),
            Style::default().fg(color).bg(bg_elevated()),
        ),
        Span::styled(" ".repeat(pad), Style::default().bg(bg_elevated())),
    ])
}

/// Compute responsive `(width, height)` for a modal on `screen`.
/// Preserves `(margin_x, margin_y)` and caps at `(max_w, max_h)` on roomy screens,
/// then gracefully shrinks margins down to 2 (or 0 on ultra-tiny screens) as the
/// terminal gets smaller so popups stay legible and centered without overflowing.
pub(crate) fn responsive_modal_size(
    screen: Rect,
    margin_x: u16,
    margin_y: u16,
    pref_min_w: u16,
    max_w: u16,
    pref_min_h: u16,
    max_h: u16,
) -> (u16, u16) {
    let avail_w = if screen.width >= pref_min_w.saturating_add(margin_x) {
        screen.width.saturating_sub(margin_x)
    } else if screen.width > 12 {
        screen.width.saturating_sub(2)
    } else {
        screen.width
    };
    let avail_h = if screen.height >= pref_min_h.saturating_add(margin_y) {
        screen.height.saturating_sub(margin_y)
    } else if screen.height > 6 {
        screen.height.saturating_sub(2)
    } else {
        screen.height
    };
    let w = avail_w.min(max_w).max(screen.width.min(1));
    let h = avail_h.min(max_h).max(screen.height.min(1));
    (w, h)
}

pub(crate) fn centered_rect(area: Rect, w: u16, h: u16) -> Rect {
    let width = w.min(area.width);
    let height = h.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    }
}
