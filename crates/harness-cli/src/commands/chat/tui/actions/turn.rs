//! turn

use super::*;

pub fn has_pending_worker_turns(st: &TuiState) -> bool {
    st.turns.iter().any(|turn| {
        turn.tool_items.iter().any(|item| {
            item.tool_name == "delegate_task"
                && item
                    .arguments
                    .as_ref()
                    .and_then(|a| a.get("task_id"))
                    .and_then(|v| v.as_str())
                    .is_some()
                && !item
                    .arguments
                    .as_ref()
                    .and_then(|a| a.get("finalized"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false)
        })
    })
}

pub async fn sync_worker_turns(env: &Env<'_>, st: &mut TuiState) -> bool {
    if !has_pending_worker_turns(st) {
        return false;
    }

    let records: std::collections::HashMap<
        String,
        harness_core::tools::delegate::SubagentTaskRecord,
    > = env
        .delegate_tool
        .list_tasks()
        .await
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();
    let live_tasks: std::collections::HashMap<String, String> = env
        .ui
        .active_delegate_tasks()
        .into_iter()
        .map(|(id, _, act)| (id, act))
        .collect();
    let now = chrono::Utc::now();
    let mut changed = false;

    for turn in &mut st.turns {
        for item in &mut turn.tool_items {
            if item.tool_name != "delegate_task" {
                continue;
            }
            let finalized = item
                .arguments
                .as_ref()
                .and_then(|a| a.get("finalized"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if finalized {
                continue;
            }
            let Some(task_id) = item
                .arguments
                .as_ref()
                .and_then(|a| a.get("task_id"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
            else {
                continue;
            };
            let Some(rec) = records.get(&task_id) else {
                continue;
            };

            match &rec.status {
                harness_core::tools::delegate::SubagentStatus::Running => {
                    let secs = (now - rec.started_at).num_seconds().max(0) as u64;
                    let act = live_tasks
                        .get(&task_id)
                        .map(String::as_str)
                        .unwrap_or("running");
                    let next_result =
                        format!("running · {act} · {secs}s (click or Ctrl+B to inspect)");
                    if item.result_formatted != next_result {
                        item.result_formatted = next_result;
                        turn.duration = std::time::Duration::from_secs(secs);
                        changed = true;
                    }
                }
                harness_core::tools::delegate::SubagentStatus::Completed { reply, finished_at } => {
                    env.ui.finish_delegate_task(&task_id);
                    let secs = (*finished_at - rec.started_at).num_seconds().max(0) as u64;
                    item.result_formatted = format!("completed in {secs}s (click to inspect)");
                    item.is_error = false;
                    turn.duration = std::time::Duration::from_secs(secs);
                    turn.reply = if reply.trim().is_empty() {
                        "(completed with no text output)".to_string()
                    } else {
                        reply.clone()
                    };
                    if let Some(args) = item.arguments.as_mut().and_then(|v| v.as_object_mut()) {
                        args.insert("finalized".to_string(), serde_json::Value::Bool(true));
                    }
                    changed = true;
                }
                harness_core::tools::delegate::SubagentStatus::Failed { error, finished_at } => {
                    env.ui.finish_delegate_task(&task_id);
                    let secs = (*finished_at - rec.started_at).num_seconds().max(0) as u64;
                    item.result_formatted = format!("failed in {secs}s (click to inspect)");
                    item.is_error = true;
                    turn.duration = std::time::Duration::from_secs(secs);
                    turn.reply = format!("✖ Worker failed: {error}");
                    if let Some(args) = item.arguments.as_mut().and_then(|v| v.as_object_mut()) {
                        args.insert("finalized".to_string(), serde_json::Value::Bool(true));
                    }
                    changed = true;
                }
            }
        }
    }

    if changed {
        st.settled_cache.clear();
    }
    changed
}

/// Handle completion of an active turn in the TUI.
pub async fn handle_turn_end(
    env: &Env<'_>,
    st: &mut TuiState,
    turn_res: Result<Result<TurnOutcome, HarnessError>, tokio::task::JoinError>,
) {
    st.active_turn = None;
    if st
        .pending_job_turn
        .as_ref()
        .is_some_and(|(pending_session, at)| {
            pending_session == env.session_id.as_str() && st.turn_start > *at
        })
    {
        st.pending_job_turn = None;
    }
    env.ui.set_turn_active(false);
    env.ui.stop_activity();
    let trailing_reasoning = env.ui.take_live_reasoning_with_duration();
    let trailing_prose = env.ui.take_live_content();
    env.ui.clear_live_state();
    st.expanded_live_thinking.clear();
    st.expanded_live_tools.clear();

    let mut current_tools: Vec<TurnProcessItem> = std::mem::take(
        &mut *env
            .in_flight_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
    );
    let interruptions: Vec<InterruptNotice> =
        std::mem::take(&mut *env.interruptions.lock().unwrap_or_else(|e| e.into_inner()));

    let turn_dur = st.turn_start.elapsed();
    let mut goal_reply: Option<String> = None;

    match turn_res {
        Ok(Ok(outcome)) => {
            env.usage
                .record_chat(
                    env.current_provider,
                    env.current_model,
                    Some(env.session_id.as_str()),
                    &outcome,
                )
                .await;

            let out_tokens = outcome.usage.as_ref().map(|u| u.completion_tokens);
            let has_prose_in_outcome = !outcome.reply.trim().is_empty();
            if let Some((r, secs)) = trailing_reasoning
                .as_ref()
                .filter(|(s, _)| !s.trim().is_empty())
            {
                if !current_tools.is_empty() || secs.is_some() {
                    current_tools.push(TurnProcessItem::thinking_with_duration(r.clone(), *secs));
                }
            }
            if !has_prose_in_outcome {
                if let Some(p) = trailing_prose.as_ref().filter(|s| !s.trim().is_empty()) {
                    current_tools.push(TurnProcessItem::assistant_text(p.clone()));
                }
            }
            let turn_reasoning = if current_tools.iter().any(|it| it.is_thinking()) {
                None
            } else {
                trailing_reasoning
                    .map(|(r, _)| r)
                    .or_else(|| outcome.reasoning.clone())
            };
            let added = turn_records(
                &st.current_query,
                interruptions,
                st.turn_start,
                turn_dur,
                turn_reasoning,
                outcome.tool_calls.clone(),
                current_tools,
                outcome.reply.clone(),
            );
            let count = added.len();
            st.turns.extend(added);
            for i in 0..count {
                st.turn_meta.push(TurnMeta {
                    mode: st.mode.label().to_string(),
                    model: env.current_model.clone(),
                    output_tokens: if i + 1 == count { out_tokens } else { None },
                });
            }

            if env.config.memory.mode == harness_core::MemoryMode::Background
                && env.config.memory.background_every_turns > 0
                && st
                    .turns
                    .len()
                    .is_multiple_of(env.config.memory.background_every_turns)
            {
                st.idle_pass.mark_passed(st.turns.len());
            }
            if env.config.display.auto_collapse {
                set_all_process_expanded(st, false);
            }
            st.live_thinking_view = LiveThinkingView::Tail;
            if st.voice_active && !st.voice_speech_queue.is_turn_muted() {
                let full_reply = &outcome.reply;
                let offset = st.voice_stream_offset.min(full_reply.len());
                let safe_offset = (offset..=full_reply.len())
                    .find(|&i| full_reply.is_char_boundary(i))
                    .unwrap_or(full_reply.len());
                let tail = full_reply[safe_offset..].trim();
                let cleaned = crate::commands::voice::clean_text_for_speech(tail);
                if !cleaned.is_empty() {
                    for sentence in crate::commands::voice::split_sentences_for_tts(&cleaned) {
                        st.voice_speech_queue.enqueue(
                            &sentence,
                            env.config,
                            &env.cred_manager,
                            Some(st.voice_tx.clone()),
                        );
                        st.voice_spoken_any = true;
                    }
                } else if (st.voice_stream_offset == 0 || !st.voice_spoken_any)
                    && !full_reply.trim().is_empty()
                {
                    let summary = crate::commands::voice::fallback_voice_summary(full_reply);
                    for sentence in crate::commands::voice::split_sentences_for_tts(&summary) {
                        st.voice_speech_queue.enqueue(
                            &sentence,
                            env.config,
                            &env.cred_manager,
                            Some(st.voice_tx.clone()),
                        );
                        st.voice_spoken_any = true;
                    }
                }
            }
            st.voice_stream_offset = 0;
            st.voice_stream_in_code = false;
            st.voice_spoken_any = false;
            st.voice_ack_sent = false;
            st.voice_speech_queue.set_turn_muted(false);
            if st.active_goal.is_some() {
                goal_reply = Some(outcome.reply);
            }
        }
        Ok(Err(HarnessError::Cancelled)) => {
            if let Some(c) = st.voice_reply_cancel.take() {
                c.cancel();
            }
            st.voice_stream_offset = 0;
            st.voice_stream_in_code = false;
            st.voice_spoken_any = false;
            st.voice_ack_sent = false;
            st.voice_speech_queue.set_turn_muted(false);
            if let Some((r, secs)) = trailing_reasoning
                .as_ref()
                .filter(|(s, _)| !s.trim().is_empty())
            {
                current_tools.push(TurnProcessItem::thinking_with_duration(r.clone(), *secs));
            }
            let added = turn_records(
                &st.current_query,
                interruptions,
                st.turn_start,
                turn_dur,
                None,
                Vec::new(),
                current_tools,
                "✖ Cancelled".to_string(),
            );
            let count = added.len();
            st.turns.extend(added);
            for _ in 0..count {
                st.turn_meta.push(TurnMeta {
                    mode: st.mode.label().to_string(),
                    model: env.current_model.clone(),
                    output_tokens: None,
                });
            }
            if st.active_goal.take().is_some() {
                env.ui.set_goal_status(None);
            }
        }
        Ok(Err(e)) => {
            // A failed turn is still a chat the user had: keep its records in the transcript
            // (the core has already persisted the prompt and any partial reply), so a dropped
            // connection does not make the latest exchange vanish from the TUI.
            if let Some((r, secs)) = trailing_reasoning
                .as_ref()
                .filter(|(s, _)| !s.trim().is_empty())
            {
                current_tools.push(TurnProcessItem::thinking_with_duration(r.clone(), *secs));
            }
            if let Some(p) = trailing_prose.as_ref().filter(|s| !s.trim().is_empty()) {
                current_tools.push(TurnProcessItem::assistant_text(p.clone()));
            }
            let added = turn_records(
                &st.current_query,
                interruptions,
                st.turn_start,
                turn_dur,
                None,
                Vec::new(),
                current_tools,
                format!("✖ Error: {e}"),
            );
            let count = added.len();
            st.turns.extend(added);
            for _ in 0..count {
                st.turn_meta.push(TurnMeta {
                    mode: st.mode.label().to_string(),
                    model: env.current_model.clone(),
                    output_tokens: None,
                });
            }
            env.ui.set_transient_error(Some(format!("Error: {e}")));
            if st.active_goal.take().is_some() {
                env.ui.set_goal_status(None);
            }
        }
        Err(e) => {
            env.ui
                .set_transient_error(Some(format!("Turn task panicked: {e}")));
            if st.active_goal.take().is_some() {
                env.ui.set_goal_status(None);
            }
        }
    }

    // Refresh context estimate
    let used = env
        .agent
        .lock()
        .await
        .estimate_session_tokens()
        .await
        .unwrap_or(0);
    let ctx_max = context_window(
        env.config,
        env.discovery.as_ref(),
        env.current_provider,
        env.current_model,
    );
    env.ui.update_status(used, ctx_max);
    st.quota_refreshed_at = std::time::Instant::now();
    tokio::spawn(refresh_antigravity_quota_owned(
        env.ui.clone(),
        env.current_provider.clone(),
        env.current_model.clone(),
        env.cred_manager.clone(),
        Some(env.session_id.clone()),
    ));

    // Any line the user submitted while the turn was finishing may still be held by the interrupt
    // handle. Move it into the session queue so it drains as the next turn instead of being lost.
    let held = env.interrupt_handle.take_leftover();
    if !held.is_empty() {
        st.queue.extend(held);
        env.ui.set_queue_depth(st.queue.len());
    }

    if let Some(reply) = goal_reply {
        advance_goal_tui(env, st, &reply);
    }

    st.current_query.clear();
    st.live_content_cache = Default::default();
    st.live_reasoning_cache = Default::default();

    if st.active_turn.is_none() && !env.compacting.load(Ordering::SeqCst) {
        drain_queue(
            &mut st.queue,
            &env.ui,
            &st.turns,
            st.process_expanded,
            &st.pending_images,
            &env.plan_mode,
            &env.agent,
            &env.in_flight_tools,
            &mut st.current_cancel,
            &mut st.current_query,
            &mut st.turn_start,
            &mut st.active_turn,
        );
        st.prune_stale_notices();
    }

    refresh_tabs(env, st).await;
    drain_tui_notices(env, st);
    st.prune_stale_notices();
}

/// Handle completion of a turn running in a background session (not currently viewed).
pub async fn handle_background_turn_end(
    env: &Env<'_>,
    st: &mut TuiState,
    bg_session_id: &str,
    turn_res: Result<Result<TurnOutcome, HarnessError>, tokio::task::JoinError>,
) {
    let Some(slot) = st.background_sessions.get_mut(bg_session_id) else {
        return;
    };
    slot.active_turn = None;
    slot.pending_modals.clear();
    slot.has_unread_done = true;
    if slot
        .pending_job_turn
        .as_ref()
        .is_some_and(|(pending_session, at)| {
            pending_session == bg_session_id && slot.turn_start > *at
        })
    {
        slot.pending_job_turn = None;
    }
    slot.ui.set_turn_active(false);
    slot.ui.stop_activity();
    let trailing_reasoning = slot.ui.take_live_reasoning_with_duration();
    let trailing_prose = slot.ui.take_live_content();
    slot.ui.clear_live_state();
    slot.expanded_live_thinking.clear();
    slot.expanded_live_tools.clear();
    slot.live_thinking_view = LiveThinkingView::Tail;

    let mut current_tools: Vec<TurnProcessItem> = std::mem::take(
        &mut *slot
            .in_flight_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
    );
    let interruptions: Vec<InterruptNotice> =
        std::mem::take(&mut *slot.interruptions.lock().unwrap_or_else(|e| e.into_inner()));

    let turn_dur = slot.turn_start.elapsed();
    let mut goal_reply: Option<String> = None;

    match turn_res {
        Ok(Ok(outcome)) => {
            env.usage
                .record_chat(&slot.provider, &slot.model, Some(bg_session_id), &outcome)
                .await;
            let out_tokens = outcome.usage.as_ref().map(|u| u.completion_tokens);
            let has_prose_in_outcome = !outcome.reply.trim().is_empty();
            if let Some((r, secs)) = trailing_reasoning
                .as_ref()
                .filter(|(s, _)| !s.trim().is_empty())
            {
                if !current_tools.is_empty() || secs.is_some() {
                    current_tools.push(TurnProcessItem::thinking_with_duration(r.clone(), *secs));
                }
            }
            if !has_prose_in_outcome {
                if let Some(p) = trailing_prose.as_ref().filter(|s| !s.trim().is_empty()) {
                    current_tools.push(TurnProcessItem::assistant_text(p.clone()));
                }
            }
            let turn_reasoning = if current_tools.iter().any(|it| it.is_thinking()) {
                None
            } else {
                trailing_reasoning
                    .map(|(r, _)| r)
                    .or_else(|| outcome.reasoning.clone())
            };
            let added = turn_records(
                &slot.current_query,
                interruptions,
                slot.turn_start,
                turn_dur,
                turn_reasoning,
                outcome.tool_calls.clone(),
                current_tools,
                outcome.reply.clone(),
            );
            let count = added.len();
            slot.turns.extend(added);
            for i in 0..count {
                slot.turn_meta.push(TurnMeta {
                    mode: slot.mode.label().to_string(),
                    model: slot.model.clone(),
                    output_tokens: if i + 1 == count { out_tokens } else { None },
                });
            }
            if env.config.memory.mode == harness_core::MemoryMode::Background
                && env.config.memory.background_every_turns > 0
                && slot
                    .turns
                    .len()
                    .is_multiple_of(env.config.memory.background_every_turns)
            {
                slot.idle_pass.mark_passed(slot.turns.len());
            }
            if env.config.display.auto_collapse {
                slot.process_expanded = false;
                slot.expanded_thinking.clear();
                slot.expanded_tools.clear();
                slot.expanded_live_thinking.clear();
                slot.expanded_live_tools.clear();
            }
            if slot.active_goal.is_some() {
                goal_reply = Some(outcome.reply);
            }
        }
        Ok(Err(HarnessError::Cancelled)) => {
            if let Some((r, secs)) = trailing_reasoning
                .as_ref()
                .filter(|(s, _)| !s.trim().is_empty())
            {
                current_tools.push(TurnProcessItem::thinking_with_duration(r.clone(), *secs));
            }
            let added = turn_records(
                &slot.current_query,
                interruptions,
                slot.turn_start,
                turn_dur,
                None,
                Vec::new(),
                current_tools,
                "✖ Cancelled".to_string(),
            );
            let count = added.len();
            slot.turns.extend(added);
            for _ in 0..count {
                slot.turn_meta.push(TurnMeta {
                    mode: slot.mode.label().to_string(),
                    model: slot.model.clone(),
                    output_tokens: None,
                });
            }
            if slot.active_goal.take().is_some() {
                slot.ui.set_goal_status(None);
            }
        }
        Ok(Err(e)) => {
            if let Some((r, secs)) = trailing_reasoning
                .as_ref()
                .filter(|(s, _)| !s.trim().is_empty())
            {
                current_tools.push(TurnProcessItem::thinking_with_duration(r.clone(), *secs));
            }
            if let Some(p) = trailing_prose.as_ref().filter(|s| !s.trim().is_empty()) {
                current_tools.push(TurnProcessItem::assistant_text(p.clone()));
            }
            let added = turn_records(
                &slot.current_query,
                interruptions,
                slot.turn_start,
                turn_dur,
                None,
                Vec::new(),
                current_tools,
                format!("✖ Error: {e}"),
            );
            let count = added.len();
            slot.turns.extend(added);
            for _ in 0..count {
                slot.turn_meta.push(TurnMeta {
                    mode: slot.mode.label().to_string(),
                    model: slot.model.clone(),
                    output_tokens: None,
                });
            }
            slot.ui.set_transient_error(Some(format!("Error: {e}")));
            if slot.active_goal.take().is_some() {
                slot.ui.set_goal_status(None);
            }
        }
        Err(e) => {
            slot.ui
                .set_transient_error(Some(format!("Turn task panicked: {e}")));
            if slot.active_goal.take().is_some() {
                slot.ui.set_goal_status(None);
            }
        }
    }

    let used = slot
        .agent
        .lock()
        .await
        .estimate_session_tokens()
        .await
        .unwrap_or(0);
    let ctx_max = context_window(
        env.config,
        env.discovery.as_ref(),
        &slot.provider,
        &slot.model,
    );
    slot.ui.update_status(used, ctx_max);

    if let Some(reply) = goal_reply {
        if let Some(ref mut goal) = slot.active_goal {
            match harness_core::GoalRunState::evaluate_reply(&reply) {
                harness_core::GoalVerdict::Complete(_) => {
                    slot.system_notices.push(SystemNotice::new(format!(
                        "✔ Goal achieved in {} iteration(s).",
                        goal.iteration
                    )));
                    slot.active_goal = None;
                    slot.ui.set_goal_status(None);
                }
                harness_core::GoalVerdict::Blocked(reason) => {
                    slot.system_notices.push(SystemNotice::new(format!(
                        "✖ Goal blocked ({}/{}): {reason}",
                        goal.iteration, goal.max_iterations
                    )));
                    slot.active_goal = None;
                    slot.ui.set_goal_status(None);
                }
                harness_core::GoalVerdict::Continue => {
                    if !goal.advance() {
                        slot.system_notices.push(SystemNotice::new(format!(
                            "■ Goal stopped: max iterations ({}) reached",
                            goal.max_iterations
                        )));
                        slot.active_goal = None;
                        slot.ui.set_goal_status(None);
                    } else if let Ok(prompt) = goal.prompt_for_current_iteration() {
                        slot.ui
                            .set_goal_status(Some((goal.iteration, goal.max_iterations)));
                        launch_internal_turn(
                            &prompt,
                            &slot.ui,
                            &slot.agent,
                            &slot.in_flight_tools,
                            &mut slot.current_cancel,
                            &mut slot.current_query,
                            &mut slot.turn_start,
                            &mut slot.active_turn,
                        );
                    }
                }
            }
        }
    }

    let held = slot.interrupt_handle.take_leftover();
    if !held.is_empty() {
        slot.queue.extend(held);
        slot.ui.set_queue_depth(slot.queue.len());
    }

    if slot.active_turn.is_none() && !slot.compacting.load(Ordering::SeqCst) {
        drain_queue(
            &mut slot.queue,
            &slot.ui,
            &slot.turns,
            slot.process_expanded,
            &slot.pending_images,
            &env.plan_mode,
            &slot.agent,
            &slot.in_flight_tools,
            &mut slot.current_cancel,
            &mut slot.current_query,
            &mut slot.turn_start,
            &mut slot.active_turn,
        );
        slot.prune_stale_notices();
    }

    slot.prune_stale_notices();
    refresh_tabs(env, st).await;
}

pub(crate) fn advance_goal_tui(env: &Env<'_>, st: &mut TuiState, reply: &str) {
    let Some(ref mut goal) = st.active_goal else {
        return;
    };
    match harness_core::GoalRunState::evaluate_reply(reply) {
        harness_core::GoalVerdict::Complete(_) => {
            st.system_notices.push(SystemNotice::new(format!(
                "✔ Goal achieved in {} iteration(s).",
                goal.iteration
            )));
            st.active_goal = None;
            env.ui.set_goal_status(None);
        }
        harness_core::GoalVerdict::Blocked(reason) => {
            st.system_notices.push(SystemNotice::new(format!(
                "✖ Goal blocked ({}/{}): {reason}",
                goal.iteration, goal.max_iterations
            )));
            st.active_goal = None;
            env.ui.set_goal_status(None);
        }
        harness_core::GoalVerdict::Continue => {
            if !goal.advance() {
                st.system_notices.push(SystemNotice::new(format!(
                    "■ Goal stopped: max iterations ({}) reached",
                    goal.max_iterations
                )));
                st.active_goal = None;
                env.ui.set_goal_status(None);
            } else if let Ok(prompt) = goal.prompt_for_current_iteration() {
                env.ui
                    .set_goal_status(Some((goal.iteration, goal.max_iterations)));
                launch_internal_turn(
                    &prompt,
                    &env.ui,
                    &env.agent,
                    &env.in_flight_tools,
                    &mut st.current_cancel,
                    &mut st.current_query,
                    &mut st.turn_start,
                    &mut st.active_turn,
                );
            }
        }
    }
}

pub(crate) fn is_transient_queue_notice(s: &str) -> bool {
    let plain = strip_ansi(s);
    plain.contains("Queued")
        || plain.contains("waiting)")
        || plain.contains("sent when the turn in flight finishes")
        || plain.contains("Held until the running tool finishes")
        || plain.contains("Redirecting — stopping this reply")
}

pub fn drain_tui_notices(env: &Env<'_>, st: &mut TuiState) {
    let notices = env.ui.take_tui_notices();
    for n in notices {
        if !n.trim().is_empty() {
            if is_transient_queue_notice(&n) && st.queue.is_empty() && st.active_turn.is_none() {
                continue;
            }
            st.system_notices.push(SystemNotice::new(n));
        }
    }
    st.prune_stale_notices();
}

pub fn sync_turn_meta_len(env: &Env<'_>, st: &mut TuiState) {
    while st.turn_meta.len() < st.turns.len() {
        st.turn_meta.push(TurnMeta {
            mode: st.mode.label().to_string(),
            model: env.current_model.clone(),
            output_tokens: None,
        });
    }
    if st.turn_meta.len() > st.turns.len() {
        st.turn_meta.truncate(st.turns.len());
    }
}

pub async fn apply_compaction_completed(env: &Env<'_>, st: &mut TuiState) {
    let result = env.ui.take_compaction_result();
    let pre_counts = st
        .turns
        .iter()
        .map(crate::commands::chat::render::turn_diff_counts)
        .fold((0usize, 0usize), |(acc_a, acc_d), (a, d)| {
            (acc_a + a, acc_d + d)
        });

    if let Ok(replayed) = env
        .store
        .replay_transcript(
            env.session_id.as_str(),
            env.config.session.resume_max_messages,
        )
        .await
    {
        let new_turns = parse_turns_from_history(&replayed.history);
        let post_counts = new_turns
            .iter()
            .map(crate::commands::chat::render::turn_diff_counts)
            .fold((0usize, 0usize), |(acc_a, acc_d), (a, d)| {
                (acc_a + a, acc_d + d)
            });
        st.compacted_added += pre_counts.0.saturating_sub(post_counts.0);
        st.compacted_deleted += pre_counts.1.saturating_sub(post_counts.1);
        st.turns = new_turns;
    }
    if let Some((notice, dur)) = result {
        if let Some(summary) = notice.summary.clone() {
            if !st
                .turns
                .iter()
                .any(|t| t.user_query == "Compaction" && t.reply == summary)
            {
                st.turns.push(TurnRecord {
                    user_query: "Compaction".to_string(),
                    reasoning: None,
                    tools: Vec::new(),
                    tool_items: Vec::new(),
                    duration: dur,
                    reply: summary,
                    command_output: false,
                });
            }
        }
        let report = compaction_report(&notice);
        st.system_notices.push(SystemNotice::new(report));
    }
    st.settled_cache.clear();
    sync_turn_meta_len(env, st);
}

/// Interrupt an active turn cleanly on `Esc` or `Ctrl+C`.
pub async fn interrupt_turn(env: &Env<'_>, st: &mut TuiState) {
    if let Some(c) = st.voice_reply_cancel.take() {
        c.cancel();
    }
    st.voice_speech_queue.interrupt();
    st.voice_speech_queue.set_turn_muted(false);
    st.voice_stream_offset = 0;
    st.voice_stream_in_code = false;
    st.voice_spoken_any = false;
    st.voice_ack_sent = false;
    st.active_goal = None;
    env.ui.set_goal_status(None);
    let ctx_max = context_window(
        env.config,
        env.discovery.as_ref(),
        env.current_provider,
        env.current_model,
    );
    cancel_running_turn(
        &mut st.active_turn,
        &st.current_cancel,
        &env.ui,
        &mut st.turns,
        &env.in_flight_tools,
        &env.interruptions,
        &env.interrupt_handle,
        &mut st.queue,
        st.turn_start,
        &st.current_query,
        st.process_expanded,
        &env.agent,
        ctx_max,
        false,
    )
    .await;
    sync_turn_meta_len(env, st);
}
