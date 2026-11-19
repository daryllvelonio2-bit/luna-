//! cards

use super::*;

pub(crate) fn build_chrome_lines(st: &TuiState, snap: &TuiLiveSnapshot, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(ref err) = snap.transient_error {
        lines.push(Line::from(vec![
            Span::styled("✖ ", Style::default().fg(red_err()).bg(bg_canvas())),
            Span::styled(
                truncate_str(&strip_ansi(err), width.saturating_sub(3)),
                Style::default().fg(red_err()).bg(bg_canvas()),
            ),
        ]));
    }
    if let Some((forced, secs)) = snap.compacting {
        let kind = if forced {
            "Compacting"
        } else {
            "Auto-compacting"
        };
        let spinner = SPINNER_FRAMES[st.spinner_tick % SPINNER_FRAMES.len()];
        lines.push(Line::from(vec![Span::styled(
            format!("{spinner} {kind} context… ({secs:.1}s)"),
            Style::default().fg(yellow_warn()).bg(bg_canvas()),
        )]));
    }
    if let Some((ref model, secs)) = snap.memory_distilling {
        let spinner = SPINNER_FRAMES[st.spinner_tick % SPINNER_FRAMES.len()];
        let model_label = if model.is_empty() {
            String::new()
        } else {
            format!(" · {model}")
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{spinner} "),
                Style::default().fg(accent_coral()).bg(bg_canvas()),
            ),
            Span::styled(
                "Distilling memory",
                Style::default()
                    .fg(text_primary())
                    .add_modifier(Modifier::BOLD)
                    .bg(bg_canvas()),
            ),
            Span::styled(
                format!("{model_label}… ({secs:.1}s)"),
                Style::default().fg(text_muted()).bg(bg_canvas()),
            ),
        ]));
    }
    for (storage, name, detail, status) in &snap.memory_notices {
        let status_color = match status.as_str() {
            "saved" => green_ok(),
            "updated" => yellow_warn(),
            "deleted" => red_err(),
            "merged" => cyan_link(),
            _ => text_muted(),
        };
        let (icon, storage_color) = if storage == "note" {
            ("📝 ", note_accent())
        } else {
            ("🧠 ", cyan_link())
        };
        let max_name_len = 32.min(width.saturating_sub(20));
        let short_name = truncate_str(name, max_name_len);
        let mut spans = vec![
            Span::styled(icon, Style::default().bg(bg_canvas())),
            Span::styled(
                format!("{storage} "),
                Style::default().fg(storage_color).bg(bg_canvas()),
            ),
            Span::styled(
                short_name.to_string(),
                Style::default()
                    .fg(text_primary())
                    .add_modifier(Modifier::BOLD)
                    .bg(bg_canvas()),
            ),
            Span::styled(
                format!(" {status}"),
                Style::default().fg(status_color).bg(bg_canvas()),
            ),
        ];
        if let Some(ref d) = detail {
            let used_w = 3 + storage.width() + 1 + short_name.width() + 1 + status.width() + 2;
            let avail = width.saturating_sub(used_w);
            if avail >= 8 {
                spans.push(Span::styled(
                    format!(": {}", truncate_str(d, avail)),
                    Style::default().fg(text_muted()).bg(bg_canvas()),
                ));
            }
        }
        lines.push(Line::from(spans));
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
                Style::default().fg(text_muted()).bg(bg_canvas()),
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
            let body = format!("{spinner} Subagent{more}: {goal} — {act}");
            let hint = " (Ctrl+B / click to inspect)";
            let avail_body = width.saturating_sub(hint.len() + 1);
            let spans = if width >= 50 && avail_body >= 20 {
                vec![
                    Span::styled(
                        truncate_str(&body, avail_body),
                        Style::default()
                            .fg(cyan_link())
                            .add_modifier(Modifier::BOLD)
                            .bg(bg_canvas()),
                    ),
                    Span::styled(hint, Style::default().fg(text_muted()).bg(bg_canvas())),
                ]
            } else {
                vec![Span::styled(
                    truncate_str(&body, width),
                    Style::default()
                        .fg(cyan_link())
                        .add_modifier(Modifier::BOLD)
                        .bg(bg_canvas()),
                )]
            };
            lines.push(Line::from(spans));
        }
    }
    if !st.queue.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("⏸ {} queued — will run next", st.queue.len()),
            Style::default().fg(yellow_warn()).bg(bg_canvas()),
        )));
    }
    if let Some(armed_at) = st.ctrl_c_armed {
        if armed_at.elapsed().as_secs() < 2 {
            lines.push(Line::from(Span::styled(
                "Press Ctrl+C again to exit Luna",
                Style::default().fg(yellow_warn()).bg(bg_canvas()),
            )));
        }
    }
    if let Some((message, at)) = &st.selection_notice {
        if at.elapsed().as_secs() < 3 {
            lines.push(Line::from(vec![
                Span::styled("✓ ", Style::default().fg(green_ok()).bg(bg_canvas())),
                Span::styled(
                    truncate_str(message, width.saturating_sub(3)),
                    Style::default().fg(green_ok()).bg(bg_canvas()),
                ),
            ]));
        }
    }
    lines
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_single_settled_turn(
    out: &mut Vec<Line<'static>>,
    hit_targets: &mut Vec<(usize, TranscriptClickTarget)>,
    idx: usize,
    turn: &crate::commands::chat::render::TurnRecord,
    meta: Option<&TurnMeta>,
    process_expanded: bool,
    block_limit: Option<usize>,
    expanded_thinking: &std::collections::HashSet<(usize, usize)>,
    expanded_tools: &std::collections::HashSet<(usize, usize)>,
    fallback_mode: &str,
    fallback_model: &str,
    width: usize,
    card_inner_w: usize,
) {
    let is_compaction = turn.user_query == "Compaction" || turn.user_query == "/compact";
    // 1. User message card (`#1e1e20` + coral `▎` left bar) or Compaction banner
    if !turn.user_query.trim().is_empty() {
        if is_compaction {
            push_compaction_card(out, width);
        } else {
            push_user_card(out, &turn.user_query, width, card_inner_w);
        }
        out.push(Line::default());
    }

    // 2 & 3. Chronological reasoning, prose, & OpenCode tool cards (clickable, or Ctrl+O to toggle all)
    render_completed_turn_process(
        out,
        hit_targets,
        idx,
        turn,
        process_expanded,
        block_limit,
        expanded_thinking,
        expanded_tools,
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
            let mut spans = vec![Span::styled("  ", Style::default().bg(bg_canvas()))];
            spans.extend(mline.spans);
            out.push(Line::from(spans));
        }
    }

    // 5. Turn footer (`Build · Model Name · 2.2s · 5.6 tok/s`)
    let mode_str = if is_compaction {
        "Compacted"
    } else {
        meta.map(|m| m.mode.as_str()).unwrap_or(fallback_mode)
    };
    let mode_color = if is_compaction {
        yellow_warn()
    } else {
        mode_accent(mode_str)
    };
    let model_raw = meta.map(|m| m.model.as_str()).unwrap_or(fallback_model);
    let model_disp = friendly_model_name(model_raw);
    let dur_secs = turn.duration.as_secs_f64();
    let dur_label = if dur_secs < 60.0 {
        format!("{dur_secs:.1}s")
    } else {
        format_elapsed_secs(turn.duration.as_secs())
    };

    let mut footer_spans = vec![
        Span::styled("  ", Style::default().bg(bg_canvas())),
        Span::styled(
            mode_str.to_string(),
            Style::default().fg(mode_color).bg(bg_canvas()),
        ),
        Span::styled(
            format!(" · {model_disp} · {dur_label}"),
            Style::default().fg(text_muted()).bg(bg_canvas()),
        ),
    ];
    if let Some(toks) = meta.and_then(|m| m.output_tokens) {
        if dur_secs > 0.05 && toks > 0 {
            let tps = toks as f64 / dur_secs;
            footer_spans.push(Span::styled(
                format!(" · {tps:.1} tok/s"),
                Style::default().fg(text_muted()).bg(bg_canvas()),
            ));
        }
    }
    out.push(Line::default());
    out.push(Line::from(footer_spans));
    out.push(Line::default());
}

pub(crate) fn ensure_settled_transcript(
    st: &mut TuiState,
    info: &FooterInfo<'_>,
    width: usize,
    card_inner_w: usize,
) {
    let cache_valid = st.settled_cache.session_id == info.session_id
        && st.settled_cache.width == width
        && st.settled_cache.card_inner_w == card_inner_w
        && st.settled_cache.process_expanded == st.process_expanded
        && st.settled_cache.block_limit == st.block_limit
        && st.settled_cache.theme == crate::commands::chat::tui::theme::active_theme()
        && st.settled_cache.expanded_thinking == st.expanded_thinking
        && st.settled_cache.expanded_tools == st.expanded_tools;

    if cache_valid && st.settled_cache.turns_len == st.turns.len() {
        return;
    }

    let start_idx = if cache_valid && st.turns.len() > st.settled_cache.turns_len {
        st.settled_cache.turns_len
    } else {
        st.settled_cache.session_id = info.session_id.to_string();
        st.settled_cache.width = width;
        st.settled_cache.card_inner_w = card_inner_w;
        st.settled_cache.process_expanded = st.process_expanded;
        st.settled_cache.block_limit = st.block_limit;
        st.settled_cache.theme = crate::commands::chat::tui::theme::active_theme();
        st.settled_cache.expanded_thinking = st.expanded_thinking.clone();
        st.settled_cache.expanded_tools = st.expanded_tools.clone();
        st.settled_cache.lines.clear();
        st.settled_cache.hit_targets.clear();
        st.settled_cache.turns_len = 0;
        0
    };

    let fallback_mode = st.mode.label().to_string();
    let fallback_model = info.model.to_string();

    for idx in start_idx..st.turns.len() {
        let turn = &st.turns[idx];
        let meta = st.turn_meta.get(idx).cloned();
        render_single_settled_turn(
            &mut st.settled_cache.lines,
            &mut st.settled_cache.hit_targets,
            idx,
            turn,
            meta.as_ref(),
            st.process_expanded,
            st.block_limit,
            &st.expanded_thinking,
            &st.expanded_tools,
            &fallback_mode,
            &fallback_model,
            width,
            card_inner_w,
        );
    }
    st.settled_cache.turns_len = st.turns.len();
}

pub(crate) fn build_active_and_notice_lines(
    st: &mut TuiState,
    snap: &TuiLiveSnapshot,
    info: &FooterInfo<'_>,
    width: usize,
    card_inner_w: usize,
    pty_pane_open: bool,
) -> (
    Vec<Line<'static>>,
    Vec<Line<'static>>,
    Vec<(usize, TranscriptClickTarget)>,
) {
    let now = std::time::Instant::now();
    let queue_empty = st.queue.is_empty();
    let turn_active = st.active_turn.is_some();
    let mut notice_lines: Vec<Line<'static>> = Vec::new();
    for notice in &st.system_notices {
        if notice.is_stale(now, queue_empty, turn_active) {
            continue;
        }
        for line in wrap_plain_text(&strip_ansi(&notice.text), card_inner_w) {
            notice_lines.push(Line::from(Span::styled(
                format!("  {line}"),
                Style::default().fg(text_muted()).bg(bg_canvas()),
            )));
        }
    }
    if !notice_lines.is_empty() {
        notice_lines.push(Line::default());
    }

    let mut active_lines: Vec<Line<'static>> = Vec::new();
    let mut active_hit_targets: Vec<(usize, TranscriptClickTarget)> = Vec::new();

    if st.active_turn.is_some() {
        if !st.current_query.trim().is_empty() {
            push_user_card(&mut active_lines, &st.current_query, width, card_inner_w);
            active_lines.push(Line::default());
        }

        render_active_turn_process(
            &mut active_lines,
            &mut active_hit_targets,
            &mut st.live_content_cache,
            &mut st.live_reasoning_cache,
            snap,
            info.in_flight_tools,
            st.process_expanded,
            st.auto_hide_thinking,
            st.block_limit,
            &st.expanded_live_thinking,
            &st.expanded_live_tools,
            st.live_thinking_view,
            st.spinner_tick,
            st.turn_start.elapsed().as_secs_f32(),
            width,
            card_inner_w,
            pty_pane_open,
        );
        active_lines.push(Line::default());
    }

    (notice_lines, active_lines, active_hit_targets)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_completed_turn_process(
    out: &mut Vec<Line<'static>>,
    hit_targets: &mut Vec<(usize, TranscriptClickTarget)>,
    turn_idx: usize,
    turn: &crate::commands::chat::render::TurnRecord,
    process_expanded: bool,
    block_limit: Option<usize>,
    expanded_thinking: &std::collections::HashSet<(usize, usize)>,
    expanded_tools: &std::collections::HashSet<(usize, usize)>,
    width: usize,
    card_inner_w: usize,
) {
    let mut thinking_idx = 0usize;
    let mut tool_group_idx = 0usize;
    if let Some(ref reasoning) = turn.reasoning {
        let trimmed = reasoning.trim();
        if !trimmed.is_empty() {
            let explicitly_expanded = expanded_thinking.contains(&(turn_idx, thinking_idx));
            let open = process_expanded || explicitly_expanded;
            let thinking_limit = if explicitly_expanded {
                None
            } else {
                block_limit
            };
            push_settled_thinking_block(
                out,
                hit_targets,
                trimmed,
                None,
                open,
                thinking_limit,
                TranscriptClickTarget::ToggleTurnThinking(turn_idx, thinking_idx),
                card_inner_w,
            );
            thinking_idx += 1;
        }
    }

    for item in &turn.tool_items {
        if item.is_thinking() {
            let trimmed = item.output.trim();
            if !trimmed.is_empty() {
                let explicitly_expanded = expanded_thinking.contains(&(turn_idx, thinking_idx));
                let open = process_expanded || explicitly_expanded;
                let thinking_limit = if explicitly_expanded {
                    None
                } else {
                    block_limit
                };
                push_settled_thinking_block(
                    out,
                    hit_targets,
                    trimmed,
                    item.duration_secs,
                    open,
                    thinking_limit,
                    TranscriptClickTarget::ToggleTurnThinking(turn_idx, thinking_idx),
                    card_inner_w,
                );
                thinking_idx += 1;
            }
        } else if item.is_assistant_text() {
            let trimmed = item.output.trim();
            if !trimmed.is_empty() {
                for mline in render_markdown(trimmed, card_inner_w) {
                    let mut spans = vec![Span::styled("  ", Style::default().bg(bg_canvas()))];
                    spans.extend(mline.spans);
                    out.push(Line::from(spans));
                }
                out.push(Line::default());
            }
        } else if item.is_user_message() {
            let trimmed = item.output.trim();
            if !trimmed.is_empty() {
                push_user_card(out, trimmed, width, card_inner_w);
                out.push(Line::default());
            }
        } else {
            let open = process_expanded || expanded_tools.contains(&(turn_idx, tool_group_idx));
            let click_target = if item.tool_name == "delegate_task" {
                match item
                    .arguments
                    .as_ref()
                    .and_then(|a| a.get("task_id"))
                    .and_then(|v| v.as_str())
                {
                    Some(id) if !id.is_empty() => {
                        TranscriptClickTarget::OpenSubagent(id.to_string())
                    }
                    _ => TranscriptClickTarget::OpenSubagents,
                }
            } else {
                TranscriptClickTarget::ToggleTurnTools(turn_idx, tool_group_idx)
            };
            push_completed_tool_card(
                out,
                hit_targets,
                item,
                open,
                click_target,
                width,
                card_inner_w,
            );
            tool_group_idx += 1;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn render_active_turn_process(
    out: &mut Vec<Line<'static>>,
    hit_targets: &mut Vec<(usize, TranscriptClickTarget)>,
    live_content_cache: &mut crate::commands::chat::tui::state::LiveContentCache,
    live_reasoning_cache: &mut crate::commands::chat::tui::state::LiveReasoningCache,
    snap: &TuiLiveSnapshot,
    in_flight_tools: &[crate::commands::chat::render::TurnProcessItem],
    process_expanded: bool,
    auto_hide_thinking: bool,
    block_limit: Option<usize>,
    collapsed_live_thinking: &std::collections::HashSet<usize>,
    expanded_live_tools: &std::collections::HashSet<usize>,
    live_thinking_view: LiveThinkingView,
    spinner_tick: usize,
    turn_secs: f32,
    width: usize,
    card_inner_w: usize,
    pty_pane_open: bool,
) {
    // A tool call is "active" while the live activity card is on screen or a recorded step has
    // not produced a result yet. That is exactly the moment the preceding thinking block should
    // fold away, so the working view stays on the tool.
    let tool_active = snap.activity_call.is_some()
        || in_flight_tools.iter().any(|item| {
            !item.is_thinking()
                && !item.is_assistant_text()
                && !item.is_user_message()
                && item.result_formatted.is_empty()
        });
    let auto_hide_thinking = auto_hide_thinking && tool_active;

    // Chronological settled thinking blocks, prose, and tool cards executed so far in this turn
    for (step_idx, item) in in_flight_tools.iter().enumerate() {
        if item.is_thinking() {
            let trimmed = item.output.trim();
            if !trimmed.is_empty() {
                let open = process_expanded
                    || (!auto_hide_thinking && !collapsed_live_thinking.contains(&step_idx));
                push_settled_thinking_block(
                    out,
                    hit_targets,
                    trimmed,
                    item.duration_secs,
                    open,
                    block_limit,
                    TranscriptClickTarget::ToggleLiveThinkingStep(step_idx),
                    card_inner_w,
                );
            }
            continue;
        }
        if item.is_assistant_text() {
            let trimmed = item.output.trim();
            if !trimmed.is_empty() {
                for mline in render_markdown(trimmed, card_inner_w) {
                    let mut spans = vec![Span::styled("  ", Style::default().bg(bg_canvas()))];
                    spans.extend(mline.spans);
                    out.push(Line::from(spans));
                }
                out.push(Line::default());
            }
            continue;
        }
        if item.is_user_message() {
            let trimmed = item.output.trim();
            if !trimmed.is_empty() {
                push_user_card(out, trimmed, width, card_inner_w);
                out.push(Line::default());
            }
            continue;
        }
        if item.result_formatted.is_empty() {
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

    if let Some(ref call) = snap.activity_call {
        let tool_name = snap.activity_tool_name.as_deref().unwrap_or("tool");
        let secs = snap.activity_elapsed_secs.unwrap_or(0.0);
        // When the side pane owns the output, the running card keeps only its header: two live
        // copies of the same lines would cost rows in a column that just got narrower.
        let (hidden_lines, tail): (usize, &[String]) = if pty_pane_open {
            (0, &[])
        } else {
            (
                snap.activity_tool_output_hidden,
                &snap.activity_tool_output_raw,
            )
        };
        push_running_tool_card(
            out,
            tool_name,
            snap.activity_args.as_ref(),
            Some(call),
            snap.activity_detail.as_deref(),
            secs,
            hidden_lines,
            tail,
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
        } else if auto_hide_thinking && live_thinking_view != LiveThinkingView::Expanded {
            // The next tool call is running: fold the finished thought away unless the reader
            // explicitly expanded it.
            LiveThinkingView::Collapsed
        } else {
            live_thinking_view
        };
        let actively_thinking = snap.content.trim().is_empty() && snap.activity_call.is_none();
        if actively_thinking {
            let spinner = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
            let header_idx = out.len();
            out.push(Line::from(vec![
                Span::styled("  │ ", Style::default().fg(border_subtle()).bg(bg_canvas())),
                Span::styled(
                    format!("{spinner} Thought: {secs:.1}s"),
                    Style::default().fg(accent_coral()).bg(bg_canvas()),
                ),
            ]));
            hit_targets.push((header_idx, TranscriptClickTarget::ToggleLiveThinking));

            if view_mode != LiveThinkingView::Collapsed {
                out.push(Line::from(Span::styled(
                    "  │",
                    Style::default().fg(border_subtle()).bg(bg_canvas()),
                )));
                let reasoning_inner_w = card_inner_w.saturating_sub(2);
                if live_reasoning_cache.reasoning != trimmed
                    || live_reasoning_cache.card_inner_w != reasoning_inner_w
                {
                    live_reasoning_cache.reasoning = trimmed.to_string();
                    live_reasoning_cache.card_inner_w = reasoning_inner_w;
                    live_reasoning_cache.lines = render_markdown(trimmed, reasoning_inner_w);
                }
                let skip = if view_mode == LiveThinkingView::Expanded {
                    0
                } else if let Some(limit) = block_limit {
                    live_reasoning_cache.lines.len().saturating_sub(limit)
                } else {
                    0
                };
                for rline in live_reasoning_cache.lines.iter().skip(skip) {
                    let mut spans = vec![Span::styled(
                        "  │ ",
                        Style::default().fg(border_subtle()).bg(bg_canvas()),
                    )];
                    for s in &rline.spans {
                        spans.push(Span::styled(
                            s.content.clone(),
                            s.style.fg(text_muted()).bg(bg_canvas()),
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
            let thinking_limit = if view_mode == LiveThinkingView::Expanded {
                None
            } else {
                block_limit
            };
            push_settled_thinking_block(
                out,
                hit_targets,
                trimmed,
                snap.reasoning_secs,
                open,
                thinking_limit,
                TranscriptClickTarget::ToggleLiveThinking,
                card_inner_w,
            );
        }
    }

    // Live streaming reply text
    if !snap.content.trim().is_empty() {
        if live_content_cache.content != snap.content
            || live_content_cache.card_inner_w != card_inner_w
        {
            live_content_cache.content = snap.content.clone();
            live_content_cache.card_inner_w = card_inner_w;
            live_content_cache.lines.clear();
            for mline in render_markdown(&snap.content, card_inner_w) {
                let mut spans = vec![Span::styled("  ", Style::default().bg(bg_canvas()))];
                spans.extend(mline.spans);
                live_content_cache.lines.push(Line::from(spans));
            }
        }
        out.extend(live_content_cache.lines.iter().cloned());
    } else {
        live_content_cache.content.clear();
        live_content_cache.lines.clear();
        if snap.reasoning.trim().is_empty() && snap.activity_call.is_none() {
            let spinner = SPINNER_FRAMES[spinner_tick % SPINNER_FRAMES.len()];
            out.push(Line::from(vec![
                Span::styled("  │ ", Style::default().fg(border_subtle()).bg(bg_canvas())),
                Span::styled(
                    format!("{spinner} Thinking… {turn_secs:.1}s"),
                    Style::default().fg(accent_coral()).bg(bg_canvas()),
                ),
            ]));
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_settled_thinking_block(
    out: &mut Vec<Line<'static>>,
    hit_targets: &mut Vec<(usize, TranscriptClickTarget)>,
    trimmed: &str,
    duration_secs: Option<f32>,
    open: bool,
    block_limit: Option<usize>,
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
            Span::styled("  │ ", Style::default().fg(border_subtle()).bg(bg_canvas())),
            Span::styled(label, Style::default().fg(accent_coral()).bg(bg_canvas())),
        ]));
        hit_targets.push((header_idx, click_target.clone()));
        let spacer_idx = out.len();
        out.push(Line::from(Span::styled(
            "  │",
            Style::default().fg(border_subtle()).bg(bg_canvas()),
        )));
        hit_targets.push((spacer_idx, click_target.clone()));
        let rendered = render_markdown(trimmed, card_inner_w.saturating_sub(2));
        let total = rendered.len();
        let show = match block_limit {
            Some(limit) => total.min(limit),
            None => total,
        };
        for rline in rendered.into_iter().take(show) {
            let mut spans = vec![Span::styled(
                "  │ ",
                Style::default().fg(border_subtle()).bg(bg_canvas()),
            )];
            for s in rline.spans {
                spans.push(Span::styled(
                    s.content,
                    s.style.fg(text_muted()).bg(bg_canvas()),
                ));
            }
            let row_idx = out.len();
            out.push(Line::from(spans));
            hit_targets.push((row_idx, click_target.clone()));
        }
        let hidden = total.saturating_sub(show);
        if hidden > 0 {
            let row_idx = out.len();
            out.push(Line::from(vec![
                Span::styled("  │ ", Style::default().fg(border_subtle()).bg(bg_canvas())),
                Span::styled(
                    format!("… ({hidden} more lines)"),
                    Style::default().fg(text_dim()).bg(bg_canvas()),
                ),
            ]));
            hit_targets.push((row_idx, click_target.clone()));
        }
        out.push(Line::default());
    } else {
        let line_count = trimmed.lines().count().max(1);
        let unit = if line_count == 1 { "line" } else { "lines" };
        let row_idx = out.len();
        out.push(Line::from(vec![
            Span::styled("  │ ", Style::default().fg(border_subtle()).bg(bg_canvas())),
            Span::styled(
                format!("▸ {label} ({line_count} {unit})"),
                Style::default().fg(accent_coral()).bg(bg_canvas()),
            ),
        ]));
        hit_targets.push((row_idx, click_target));
        out.push(Line::default());
    }
}

pub(crate) fn format_opencode_tool_header(
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
    let arg_num =
        |key: &str| -> Option<u64> { arguments.and_then(|a| a.get(key)).and_then(|v| v.as_u64()) };

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
            let worker = arg_str("worker");
            if let Some(tasks) = arguments
                .and_then(|a| a.get("tasks"))
                .and_then(|v| v.as_array())
            {
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
                    let label = match worker {
                        Some(name) => format!("Subagent [{name}]: {goal}{extra}"),
                        None => format!("Subagent: {goal}{extra}"),
                    };
                    return ("◈ ", label);
                }
            }
        }
        "memory_search" => {
            if let Some(q) = arg_str("query") {
                return ("🔍 ", format!("Memory search: \"{q}\""));
            }
            return ("🔍 ", "Memory search".to_string());
        }
        "search_notes" => {
            if let Some(q) = arg_str("query") {
                return ("📝 ", format!("Search notes: \"{q}\""));
            }
            return ("📝 ", "Search notes".to_string());
        }
        "read_note" => {
            if let Some(id) = arg_str("id") {
                return ("📝 ", format!("Read note: {id}"));
            }
            return ("📝 ", "Read note".to_string());
        }
        "save_memory" => {
            if let Some(s) = arg_str("subject") {
                return ("🧠 ", format!("Save memory: {s}"));
            }
            return ("🧠 ", "Save memory".to_string());
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

pub(crate) fn push_card_row(
    out: &mut Vec<Line<'static>>,
    hit_targets: Option<&mut Vec<(usize, TranscriptClickTarget)>>,
    click_target: Option<&TranscriptClickTarget>,
    border_color: Color,
    width: usize,
    content_spans: Vec<Span<'static>>,
) {
    let max_inner = width.saturating_sub(3);
    let mut clamped_spans = Vec::with_capacity(content_spans.len());
    let mut content_w = 0usize;

    for span in content_spans {
        let clean = clean_terminal_text(&span.content);
        let mut span_text = String::new();
        for ch in clean.chars() {
            let cw = ch.width().unwrap_or(0);
            if content_w + cw > max_inner {
                break;
            }
            span_text.push(ch);
            content_w += cw;
        }
        if !span_text.is_empty() {
            clamped_spans.push(Span::styled(span_text, span.style));
        }
        if content_w >= max_inner {
            break;
        }
    }

    let pad = width.saturating_sub(3 + content_w);
    let mut spans = vec![
        Span::styled("│", Style::default().fg(border_color).bg(bg_surface())),
        Span::styled("  ", Style::default().bg(bg_surface())),
    ];
    spans.extend(clamped_spans);
    if pad > 0 {
        spans.push(Span::styled(
            " ".repeat(pad),
            Style::default().bg(bg_surface()),
        ));
    }
    let row_idx = out.len();
    out.push(Line::from(spans));
    if let (Some(targets), Some(target)) = (hit_targets, click_target) {
        targets.push((row_idx, target.clone()));
    }
}

pub(crate) fn push_card_blank_row(
    out: &mut Vec<Line<'static>>,
    hit_targets: Option<&mut Vec<(usize, TranscriptClickTarget)>>,
    click_target: Option<&TranscriptClickTarget>,
    border_color: Color,
    width: usize,
) {
    let blank_fill = " ".repeat(width.saturating_sub(1));
    let row_idx = out.len();
    out.push(Line::from(vec![
        Span::styled("│", Style::default().fg(border_color).bg(bg_surface())),
        Span::styled(blank_fill, Style::default().bg(bg_surface())),
    ]));
    if let (Some(targets), Some(target)) = (hit_targets, click_target) {
        targets.push((row_idx, target.clone()));
    }
}

pub(crate) fn push_completed_tool_card(
    out: &mut Vec<Line<'static>>,
    hit_targets: &mut Vec<(usize, TranscriptClickTarget)>,
    item: &crate::commands::chat::render::TurnProcessItem,
    open: bool,
    click_target: TranscriptClickTarget,
    width: usize,
    card_inner_w: usize,
) {
    if item.is_user_message() {
        let trimmed = item.output.trim();
        if !trimmed.is_empty() {
            push_user_card(out, trimmed, width, card_inner_w);
            out.push(Line::default());
        }
        return;
    }

    if item.tool_name == "steer" || item.tool_name == "approval" {
        let clean = clean_terminal_text(&item.call_formatted);
        if !clean.trim().is_empty() {
            out.push(Line::from(Span::styled(
                format!("  {}", truncate_str(clean.trim(), card_inner_w)),
                Style::default().fg(text_muted()).bg(bg_canvas()),
            )));
            out.push(Line::default());
        }
        return;
    }

    let border_color = if item.is_error {
        red_err()
    } else {
        border_subtle()
    };
    let (prefix, header_body) = format_opencode_tool_header(
        &item.tool_name,
        item.arguments.as_ref(),
        Some(&item.call_formatted),
    );
    let clean_header = clean_terminal_text(&header_body);
    let prefix_w = prefix.width();
    let wrap_w = card_inner_w.saturating_sub(prefix_w).max(8);
    let header_lines = wrap_plain_text(&clean_header, wrap_w);

    push_card_blank_row(
        out,
        Some(hit_targets),
        Some(&click_target),
        border_color,
        width,
    );

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
                        .fg(if item.is_error {
                            red_err()
                        } else {
                            text_primary()
                        })
                        .bg(bg_surface())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    hline,
                    Style::default()
                        .fg(text_primary())
                        .bg(bg_surface())
                        .add_modifier(Modifier::BOLD),
                ),
            ],
        );
    }

    let has_diff = !item.diff_lines.is_empty() && !item.is_error;

    if has_diff {
        push_card_blank_row(
            out,
            Some(hit_targets),
            Some(&click_target),
            border_color,
            width,
        );
        let max_diff = if open { item.diff_lines.len() } else { 10 };
        for diff in item.diff_lines.iter().take(max_diff) {
            let clean = clean_terminal_text(diff);
            let color = if clean.trim_start().starts_with('+') {
                green_ok()
            } else if clean.trim_start().starts_with('-') {
                red_err()
            } else {
                text_muted()
            };
            push_card_row(
                out,
                Some(hit_targets),
                Some(&click_target),
                border_color,
                width,
                vec![Span::styled(
                    truncate_str(&clean, card_inner_w),
                    Style::default().fg(color).bg(bg_surface()),
                )],
            );
        }
        let diff_more = (item.diff_lines.len().saturating_sub(max_diff)) + item.output_dropped;
        if diff_more > 0 {
            push_card_row(
                out,
                Some(hit_targets),
                Some(&click_target),
                border_color,
                width,
                vec![Span::styled(
                    format!("({diff_more} more diff lines)"),
                    Style::default().fg(text_dim()).bg(bg_surface()),
                )],
            );
        }
    } else if !item.output.trim().is_empty() {
        let mut raw_lines: Vec<String> = Vec::new();
        let mut prev_empty = false;
        for line in item.output.trim().lines() {
            let trimmed = line.trim_end();
            let is_empty = trimmed.trim().is_empty();
            if is_empty && prev_empty {
                continue;
            }
            raw_lines.push(trimmed.to_string());
            prev_empty = is_empty;
        }
        if !raw_lines.is_empty() {
            push_card_blank_row(
                out,
                Some(hit_targets),
                Some(&click_target),
                border_color,
                width,
            );
            let max_lines = if open { raw_lines.len() } else { 10 };
            let body_color = if item.is_error && raw_lines.len() <= 6 {
                red_err()
            } else {
                text_muted()
            };
            for line in raw_lines.iter().take(max_lines) {
                let clean = clean_terminal_text(line);
                for wrapped in wrap_plain_text(&clean, card_inner_w) {
                    push_card_row(
                        out,
                        Some(hit_targets),
                        Some(&click_target),
                        border_color,
                        width,
                        vec![Span::styled(
                            wrapped,
                            Style::default().fg(body_color).bg(bg_surface()),
                        )],
                    );
                }
            }
            let more = (raw_lines.len().saturating_sub(max_lines)) + item.output_dropped;
            if more > 0 {
                push_card_row(
                    out,
                    Some(hit_targets),
                    Some(&click_target),
                    border_color,
                    width,
                    vec![Span::styled(
                        format!("({more} more lines)"),
                        Style::default().fg(text_dim()).bg(bg_surface()),
                    )],
                );
            }
        }
    } else if item.is_error && !item.result_formatted.is_empty() {
        push_card_blank_row(
            out,
            Some(hit_targets),
            Some(&click_target),
            border_color,
            width,
        );
        let res_clean = clean_terminal_text(&item.result_formatted);
        push_card_row(
            out,
            Some(hit_targets),
            Some(&click_target),
            border_color,
            width,
            vec![Span::styled(
                truncate_str(res_clean.trim(), card_inner_w),
                Style::default().fg(red_err()).bg(bg_surface()),
            )],
        );
    }

    push_card_blank_row(
        out,
        Some(hit_targets),
        Some(&click_target),
        border_color,
        width,
    );
    out.push(Line::default());
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_running_tool_card(
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

    push_card_blank_row(out, None, None, accent_coral(), width);
    for (i, hline) in header_lines.into_iter().enumerate() {
        if i == 0 {
            push_card_row(
                out,
                None,
                None,
                accent_coral(),
                width,
                vec![
                    Span::styled(
                        format!("{spinner} "),
                        Style::default().fg(accent_coral()).bg(bg_surface()),
                    ),
                    Span::styled(
                        prefix.to_string(),
                        Style::default()
                            .fg(text_primary())
                            .bg(bg_surface())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        hline,
                        Style::default()
                            .fg(text_primary())
                            .bg(bg_surface())
                            .add_modifier(Modifier::BOLD),
                    ),
                ],
            );
        } else {
            push_card_row(
                out,
                None,
                None,
                accent_coral(),
                width,
                vec![
                    Span::styled(" ".repeat(prefix_w), Style::default().bg(bg_surface())),
                    Span::styled(
                        hline,
                        Style::default()
                            .fg(text_primary())
                            .bg(bg_surface())
                            .add_modifier(Modifier::BOLD),
                    ),
                ],
            );
        }
    }

    if !raw_tail.is_empty() || hidden_lines > 0 {
        push_card_blank_row(out, None, None, accent_coral(), width);
        if hidden_lines > 0 {
            push_card_row(
                out,
                None,
                None,
                accent_coral(),
                width,
                vec![Span::styled(
                    format!("({hidden_lines} earlier lines)"),
                    Style::default().fg(text_muted()).bg(bg_surface()),
                )],
            );
        }
        for line in raw_tail {
            let clean = clean_terminal_text(line);
            for wrapped in wrap_plain_text(&clean, card_inner_w) {
                push_card_row(
                    out,
                    None,
                    None,
                    accent_coral(),
                    width,
                    vec![Span::styled(
                        wrapped,
                        Style::default().fg(text_muted()).bg(bg_surface()),
                    )],
                );
            }
        }
    }

    push_card_blank_row(out, None, None, accent_coral(), width);
    out.push(Line::default());
}

pub(crate) fn push_compaction_card(out: &mut Vec<Line<'static>>, width: usize) {
    let blank_fill = " ".repeat(width.saturating_sub(1));
    out.push(Line::from(vec![
        Span::styled("▎", Style::default().fg(yellow_warn()).bg(bg_surface())),
        Span::styled(blank_fill.clone(), Style::default().bg(bg_surface())),
    ]));
    let label = "⚡ Context Compacted";
    let pad = width.saturating_sub(4 + label.len());
    out.push(Line::from(vec![
        Span::styled("▎", Style::default().fg(yellow_warn()).bg(bg_surface())),
        Span::styled("  ", Style::default().bg(bg_surface())),
        Span::styled(
            label,
            Style::default()
                .fg(yellow_warn())
                .bg(bg_surface())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ".repeat(pad), Style::default().bg(bg_surface())),
    ]));
    out.push(Line::from(vec![
        Span::styled("▎", Style::default().fg(yellow_warn()).bg(bg_surface())),
        Span::styled(blank_fill, Style::default().bg(bg_surface())),
    ]));
}

pub(crate) fn push_user_card(out: &mut Vec<Line<'static>>, query: &str, width: usize, inner_w: usize) {
    let blank_fill = " ".repeat(width.saturating_sub(1));
    out.push(Line::from(vec![
        Span::styled("▎", Style::default().fg(accent_coral()).bg(bg_surface())),
        Span::styled(blank_fill.clone(), Style::default().bg(bg_surface())),
    ]));

    let max_inner = width.saturating_sub(3);
    for line in wrap_plain_text(query.trim(), inner_w.min(max_inner)) {
        let pad = width.saturating_sub(3 + line.width());
        let mut spans = vec![
            Span::styled("▎", Style::default().fg(accent_coral()).bg(bg_surface())),
            Span::styled("  ", Style::default().bg(bg_surface())),
        ];
        spans.extend(styled_composer_line_spans(&line));
        spans.push(Span::styled(
            " ".repeat(pad),
            Style::default().bg(bg_surface()),
        ));
        out.push(Line::from(spans));
    }

    out.push(Line::from(vec![
        Span::styled("▎", Style::default().fg(accent_coral()).bg(bg_surface())),
        Span::styled(blank_fill, Style::default().bg(bg_surface())),
    ]));
}

pub(crate) fn render_slash_popup(frame: &mut Frame, screen: Rect, composer_rect: Rect, st: &TuiState) {
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
        .border_style(Style::default().fg(border_subtle()).bg(bg_elevated()))
        .style(Style::default().bg(bg_elevated()))
        .title(Span::styled(
            " Commands ",
            Style::default().fg(text_muted()).bg(bg_elevated()),
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
            let bg = if sel { bg_selected() } else { bg_elevated() };
            let name_fg = if sel { accent_coral() } else { text_primary() };
            let desc_w = (inner.width as usize).saturating_sub(entry.name.width() + 4);
            let desc = truncate_str(&entry.details, desc_w);
            let pad = (inner.width as usize).saturating_sub(entry.name.width() + desc.width() + 3);
            Line::from(vec![
                Span::styled(
                    format!(" {} ", entry.name),
                    Style::default()
                        .fg(name_fg)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" ", Style::default().bg(bg)),
                Span::styled(desc, Style::default().fg(text_muted()).bg(bg)),
                Span::styled(" ".repeat(pad), Style::default().bg(bg)),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), inner);
}

