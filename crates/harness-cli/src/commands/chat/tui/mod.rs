//! Full-screen `ratatui` TUI modeled after OpenCode's interface.
//!
//! Layout & components:
//! - [`theme`]: OpenCode charcoal/coral palette, geometric `luna` logo, model/provider formatting
//! - [`markdown`]: `pulldown-cmark` to wrapped `ratatui::text::Line`s
//! - [`state`]: `TuiState`, `AgentMode`, `ComposerState`, `ActiveModal`, session tabs
//! - [`render`]: top session tabs, centered landing screen, scrollable transcript, pinned composer
//! - [`input`]: `crossterm::event::EventStream` keyboard, mouse, and paste handling
//! - [`actions`]: turn lifecycle, slash commands, session switching, pickers

mod actions;
mod input;
mod markdown;
mod render;
mod state;
pub(in crate::commands::chat) mod theme;

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::io;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    EventStream,
};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use futures_util::StreamExt;
use harness_core::{HarnessError, SessionBinding, SessionStore, TurnOutcome};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio_util::sync::CancellationToken;

use super::commands::Feature;
use super::helpers::IdleMemoryPass;
use super::history::load_prompt_history;
use super::interactive::state::{Env, Flow};
use super::render::{
    background_job_resume_prompt, format_job_system_message, parse_turns_from_history,
};
use super::setup::ChatRuntime;
use super::status::{
    refresh_antigravity_quota_owned, refresh_antigravity_status_if_needed, QUOTA_REFRESH_INTERVAL,
};
use super::todo;
use super::turn::{can_auto_resume, drain_queue, launch_internal_turn};
use super::ui::ANIMATION_INTERVAL;
use crate::commands::context_window::context_window;
use actions::{drain_tui_notices, handle_background_turn_end, handle_turn_end, refresh_tabs};
use render::{draw, FeatureLandingItem, FooterInfo, LandingFeaturesStatus};
use state::{
    ActiveModal, AgentMode, BackgroundSession, ComposerState, LiveThinkingView, SlashPopupState,
    TuiState, TurnMeta,
};
use theme::shorten_cwd;

/// Ensures raw mode, mouse capture, bracketed paste, and the alternate screen are always restored.
struct TuiTerminalGuard;

impl TuiTerminalGuard {
    fn enter() -> Result<Self, HarnessError> {
        crossterm::terminal::enable_raw_mode()?;
        if let Err(e) = crossterm::execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableMouseCapture,
            EnableBracketedPaste
        ) {
            let _ = crossterm::terminal::disable_raw_mode();
            return Err(e.into());
        }
        Ok(Self)
    }
}

impl Drop for TuiTerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::execute!(
            io::stdout(),
            DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen,
            crossterm::cursor::Show
        );
        let _ = crossterm::terminal::disable_raw_mode();
    }
}

/// Append a timestamped line to `$LUNA_TUI_TRACE` when tracing is enabled.
///
/// Diagnostics only: it lets a pty probe separate time spent inside a key handler from time spent
/// drawing, which a screenshot alone cannot show.
pub(crate) fn trace_event(label: &str) {
    use std::io::Write;
    let Some(path) = std::env::var_os("LUNA_TUI_TRACE") else {
        return;
    };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    let _ = writeln!(file, "{:?} {label}", std::time::SystemTime::now());
}

/// Run the full-screen OpenCode-style TUI until the user exits.
pub(in crate::commands::chat) async fn run(runtime: &mut ChatRuntime) -> Result<(), HarnessError> {
    runtime.ui.set_tui_mode(true);
    // The system prompt is built per turn, so flipping this before the first turn is enough to
    // add the rich-output block for the full-screen renderer without touching inline/piped runs.
    runtime.agent.lock().await.set_tui_output(true);
    let _term_guard = TuiTerminalGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut env = Env::new(runtime);
    theme::set_active_theme(env.config.display.theme);
    let mut st = init_tui_state(&mut env).await?;

    // Warm model catalog in the background so `/model` and context window sizing stay crisp.
    {
        let discovery = env.discovery.clone();
        let cred_manager = env.cred_manager.clone();
        let provider = env.current_provider.clone();
        tokio::spawn(async move {
            let _ = discovery.get_models(&provider, &cred_manager, false).await;
        });
    }

    let update_config = env.config.clone();
    let (update_tx, mut update_rx) =
        tokio::sync::mpsc::unbounded_channel::<crate::commands::update::UpdateStatus>();
    let mut update_rx_open = true;
    tokio::spawn(async move {
        if let Some(status) = crate::commands::update::check_at_launch(&update_config).await {
            let _ = update_tx.send(status);
        }
    });

    let tui_notifier = env.ui.tui_notifier();
    let mut events = EventStream::new();

    let mut animation = tokio::time::interval(ANIMATION_INTERVAL);
    animation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut idle_tick = tokio::time::interval(Duration::from_secs(2));
    idle_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut needs_redraw = true;
    let mut snap = {
        let w = terminal.size().map(|s| s.width as usize).unwrap_or(100);
        env.ui.tui_snapshot(w)
    };

    loop {
        if needs_redraw {
            let t_draw = std::time::Instant::now();
            // Render current frame (never await env.agent.lock() here — the running turn holds it!).
            // Keep the thinking display, theme, and voice settings in step with `/settings` changes.
            st.auto_hide_thinking = env.config.display.auto_hide_thinking;
            st.block_limit = env.config.display.block_limit;
            st.voice_auto_send = env.config.voice.auto_send;
            theme::set_active_theme(env.config.display.theme);
            let w = terminal.size().map(|s| s.width as usize).unwrap_or(100);
            snap = env.ui.tui_snapshot(w);
            let reasoning_effort = st.reasoning_effort;
            let tools_snapshot = env
                .in_flight_tools
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            let now_secs = harness_core::Config::current_unix_secs();
            let days_left = |until: Option<u64>| -> Option<u64> {
                until
                    .filter(|&u| u > now_secs)
                    .map(|u| (u - now_secs).div_ceil(86_400))
            };
            let mut pinned_skills = env.config.agent.pinned_skills.clone();
            for s in &st.session_pinned_skills {
                if !pinned_skills
                    .iter()
                    .any(|p| harness_plugins::names_match(p, s))
                {
                    pinned_skills.push(s.clone());
                }
            }
            let features = LandingFeaturesStatus {
                mcp: FeatureLandingItem {
                    enabled: env.feature_toggles.enabled(Feature::Mcp),
                    disabled_days_left: days_left(env.config.features.mcp_disabled_until),
                },
                skills: FeatureLandingItem {
                    enabled: env.feature_toggles.enabled(Feature::Skills),
                    disabled_days_left: days_left(env.config.features.skills_disabled_until),
                },
                memory: FeatureLandingItem {
                    enabled: env.feature_toggles.enabled(Feature::Memory),
                    disabled_days_left: days_left(env.config.features.memory_disabled_until),
                },
                notes: FeatureLandingItem {
                    enabled: env.feature_toggles.enabled(Feature::Notes),
                    disabled_days_left: days_left(env.config.features.notes_disabled_until),
                },
                memory_mode: env.config.memory.mode,
                pinned_skills,
            };
            let info = FooterInfo {
                provider: env.current_provider,
                model: env.current_model,
                reasoning_effort,
                session_id: env.session_id,
                in_flight_tools: &tools_snapshot,
                features,
            };
            terminal.draw(|frame| draw(frame, &mut st, &snap, &info))?;
            trace_event(&format!("draw {:?}", t_draw.elapsed()));
            needs_redraw = false;
        }

        let has_bg_active = st
            .background_sessions
            .values()
            .any(|s| s.active_turn.is_some());

        tokio::select! {
            biased;

            maybe_ev = events.next() => {
                match maybe_ev {
                    Some(Ok(ev)) => {
                        let should_redraw = match &ev {
                            crossterm::event::Event::Mouse(m) => {
                                m.kind != crossterm::event::MouseEventKind::Moved
                                    || st.composer.image_count() > 0
                            }
                            crossterm::event::Event::Key(k) => {
                                k.kind != crossterm::event::KeyEventKind::Release
                            }
                            crossterm::event::Event::FocusGained
                            | crossterm::event::Event::FocusLost => false,
                            _ => true,
                        };
                        if input::handle_event(&mut env, &mut st, &mut terminal, ev).await? == Flow::Break {
                            break;
                        }
                        if st.promote_pending_modal() {
                            needs_redraw = true;
                        }
                        if should_redraw {
                            needs_redraw = true;
                        }
                    }
                    Some(Err(_)) | None => break,
                }
            }

            _ = tui_notifier.notified() => {
                needs_redraw = true;
                actions::pump_voice_streaming_tts(&env, &mut st);
                drain_tui_notices(&env, &mut st);
                actions::sync_worker_turns(&env, &mut st).await;
                if env.ui.take_compaction_completed() {
                    actions::apply_compaction_completed(&env, &mut st).await;
                }
                if st.active_turn.is_none()
                    && !env.compacting.load(Ordering::SeqCst)
                    && !st.queue.is_empty()
                {
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
            }

            turn_res = async {
                if let Some(ref mut handle) = st.active_turn {
                    handle.await
                } else {
                    std::future::pending().await
                }
            } => {
                handle_turn_end(&env, &mut st, turn_res).await;
            }

            bg_done = poll_any_background_turn(&mut st.background_sessions), if has_bg_active => {
                if let Some((bg_id, turn_res)) = bg_done {
                    handle_background_turn_end(&env, &mut st, &bg_id, turn_res).await;
                    needs_redraw = true;
                }
            }

            maybe_picker = st.picker_rx.recv() => {
                if let Some(load) = maybe_picker {
                    actions::apply_picker_load(&mut st, load);
                    needs_redraw = true;
                }
            }

            maybe_voice_evt = st.voice_rx.recv() => {
                if let Some(evt) = maybe_voice_evt {
                    actions::handle_voice_event(&env, &mut st, evt).await;
                    needs_redraw = true;
                }
            }

            maybe_update = update_rx.recv(), if update_rx_open => {
                update_rx_open = false;
                if let Some(status) = maybe_update {
                    st.update_notice = Some(status.latest_label());
                    needs_redraw = true;
                }
            }

            _ = async {
                match st.voice_pending_autosend_at {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                actions::flush_voice_pending_autosend(&mut env, &mut st, &mut terminal).await?;
                needs_redraw = true;
            }

            maybe_approval_reply = st.approval_reply_rx.recv() => {
                if let Some(msg) = maybe_approval_reply {
                    if let ActiveModal::Approval {
                        req,
                        asking,
                        reply,
                        reply_scroll,
                        error,
                        ..
                    } = &mut st.modal
                    {
                        if req.call_id == msg.call_id {
                            *asking = false;
                            match msg.result {
                                Ok(text) => {
                                    *reply = Some(text);
                                    *reply_scroll = 0;
                                    *error = None;
                                }
                                Err(err) => {
                                    *error = Some(err);
                                }
                            }
                            env.approval_activity.notify_one();
                            needs_redraw = true;
                        }
                    }
                }
            }

            _ = animation.tick(), if st.active_turn.is_some()
                || has_bg_active
                || env.compacting.load(Ordering::SeqCst)
                || env.ui.is_compacting()
                || env.ui.is_memory_distilling()
                || snap.compacting.is_some()
                || snap.memory_distilling.is_some()
                || snap.delegate_count > 0
                || actions::has_pending_worker_turns(&st)
                || matches!(
                    st.modal,
                    ActiveModal::SubagentInspector { .. }
                        | ActiveModal::SubagentsModal { .. }
                        | ActiveModal::Approval {
                            asking: true,
                            ..
                        }
                )
                || st.voice_speaking_detected
                || st.voice_transcribing
                || st.voice_is_playing.load(Ordering::SeqCst)
                || st.is_diff_animating()
                || !st.queue.is_empty() => {
                needs_redraw = true;
                st.spinner_tick = st.spinner_tick.wrapping_add(1);
                if matches!(
                    st.modal,
                    ActiveModal::Approval {
                        editing_input: true,
                        ..
                    } | ActiveModal::Approval { asking: true, .. }
                ) {
                    env.approval_activity.notify_one();
                }
                st.prune_stale_notices();
                actions::sync_worker_turns(&env, &mut st).await;
                actions::refresh_subagents_if_open(&env, &mut st).await;
                if env.ui.take_compaction_completed() {
                    actions::apply_compaction_completed(&env, &mut st).await;
                }
                if st.active_turn.is_none()
                    && !env.compacting.load(Ordering::SeqCst)
                    && !st.queue.is_empty()
                {
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
            }

            _ = idle_tick.tick() => {
                needs_redraw = true;
                if matches!(
                    st.modal,
                    ActiveModal::Approval {
                        editing_input: true,
                        ..
                    } | ActiveModal::Approval { asking: true, .. }
                        | ActiveModal::Approval {
                            reply: Some(_),
                            ..
                        }
                ) {
                    env.approval_activity.notify_one();
                }
                st.prune_stale_notices();
                actions::sync_worker_turns(&env, &mut st).await;
                actions::refresh_subagents_if_open(&env, &mut st).await;
                if env.ui.take_compaction_completed() {
                    actions::apply_compaction_completed(&env, &mut st).await;
                }
                if st.active_turn.is_none()
                    && !env.compacting.load(Ordering::SeqCst)
                    && !st.queue.is_empty()
                {
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
                for slot in st.background_sessions.values_mut() {
                    slot.prune_stale_notices();
                    if slot.active_turn.is_none()
                        && !slot.compacting.load(Ordering::SeqCst)
                        && !slot.queue.is_empty()
                    {
                        slot.has_unread_done = false;
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
                }

                if st.quota_refreshed_at.elapsed() >= QUOTA_REFRESH_INTERVAL {
                    st.quota_refreshed_at = std::time::Instant::now();
                    tokio::spawn(refresh_antigravity_quota_owned(
                        env.ui.clone(),
                        env.current_provider.clone(),
                        env.current_model.clone(),
                        env.cred_manager.clone(),
                        Some(env.session_id.clone()),
                    ));
                }
                // Background job auto-resume + idle memory pass
                if let Some((pending_session, at)) = st.pending_job_turn.take() {
                    if pending_session == env.session_id.as_str()
                        && !(st.active_turn.is_some() && st.turn_start > at)
                    {
                        let images_len = st.pending_images.lock().unwrap_or_else(|e| e.into_inner()).len()
                            + st.composer.image_count();
                        let modal_up = !matches!(st.modal, ActiveModal::None);
                        if can_auto_resume(
                            st.active_turn.is_some(),
                            env.compacting.load(Ordering::SeqCst),
                            st.queue.len(),
                            &st.composer.text,
                            images_len,
                            modal_up,
                            env.plan_mode.load(Ordering::SeqCst),
                        ) {
                            launch_internal_turn(
                                &background_job_resume_prompt(),
                                &env.ui,
                                &env.agent,
                                &env.in_flight_tools,
                                &mut st.current_cancel,
                                &mut st.current_query,
                                &mut st.turn_start,
                                &mut st.active_turn,
                            );
                        } else {
                            st.pending_job_turn = Some((pending_session, at));
                        }
                    }
                }

                let busy = st.active_turn.is_some()
                    || env.compacting.load(Ordering::SeqCst)
                    || !st.queue.is_empty();
                if st.idle_pass.tick(
                    st.turns.len(),
                    busy,
                    env.config.memory.background_idle_secs,
                    std::time::Instant::now(),
                ) {
                    let agent = env.agent.clone();
                    tokio::spawn(async move {
                        let ag = agent.lock().await;
                        ag.spawn_memory_pass_now().await;
                    });
                }
            }

            maybe_approval = env.approval_rx.recv() => {
                if let Some((sid, req, resp_tx)) = maybe_approval {
                    if !resp_tx.is_closed() {
                        let modal = state::PendingModal::Approval { req, resp_tx };
                        let origin = sid.as_deref().unwrap_or(env.session_id.as_str());
                        if origin == env.session_id.as_str() {
                            st.pending_modals.push_back(modal);
                            if st.promote_pending_modal() {
                                needs_redraw = true;
                            }
                        } else if let Some(slot) = st.background_sessions.get_mut(origin) {
                            slot.pending_modals.push_back(modal);
                            needs_redraw = true;
                        }
                    }
                }
            }

            maybe_sudo = env.sudo_rx.recv() => {
                if let Some((sid, prompt, resp_tx)) = maybe_sudo {
                    if !resp_tx.is_closed() {
                        let modal = state::PendingModal::Sudo { prompt, resp_tx };
                        let origin = sid.as_deref().unwrap_or(env.session_id.as_str());
                        if origin == env.session_id.as_str() {
                            st.pending_modals.push_back(modal);
                            if st.promote_pending_modal() {
                                needs_redraw = true;
                            }
                        } else if let Some(slot) = st.background_sessions.get_mut(origin) {
                            slot.pending_modals.push_back(modal);
                            needs_redraw = true;
                        }
                    }
                }
            }

            maybe_clarify = env.clarify_rx.recv() => {
                if let Some((sid, req, resp_tx)) = maybe_clarify {
                    if !resp_tx.is_closed() {
                        let modal = state::PendingModal::Clarify { req, resp_tx };
                        let origin = sid.as_deref().unwrap_or(env.session_id.as_str());
                        if origin == env.session_id.as_str() {
                            st.pending_modals.push_back(modal);
                            if st.promote_pending_modal() {
                                needs_redraw = true;
                            }
                        } else if let Some(slot) = st.background_sessions.get_mut(origin) {
                            slot.pending_modals.push_back(modal);
                            needs_redraw = true;
                        }
                    }
                }
            }

            maybe_job = env.job_rx.recv() => {
                if let Some(comp) = maybe_job {
                    // A job belongs to the session that started it. When the user is looking at
                    // another tab, the completion is stored in the session that owns it and
                    // waits there for that session to pick it up; it must not wake this one.
                    let owner = comp
                        .session_id
                        .clone()
                        .unwrap_or_else(|| env.session_id.clone());
                    let msg = harness_core::Message::system(format_job_system_message(&comp));
                    if owner != *env.session_id {
                        if let Err(e) = env.store.append(&owner, &msg, false).await {
                            tracing::warn!("failed to record background job notice: {e}");
                        } else if let Some(slot) = st.background_sessions.get_mut(&owner) {
                            slot.system_notices.push(state::SystemNotice::new(format!(
                                "Job {} ({}): {}",
                                comp.id,
                                comp.status,
                                comp.command
                            )));
                            slot.pending_job_turn =
                                Some((owner.clone(), std::time::Instant::now()));
                        }
                    } else {
                        needs_redraw = true;
                        st.system_notices.push(state::SystemNotice::new(format!(
                            "Job {} ({}): {}",
                            comp.id,
                            comp.status,
                            comp.command
                        )));
                        if let Err(e) = env.store.append(env.session_id, &msg, false).await {
                            tracing::warn!("failed to record background job notice: {e}");
                        } else {
                            st.pending_job_turn =
                                Some((env.session_id.clone(), std::time::Instant::now()));
                        }
                    }
                }
            }
        }
    }

    // Cancel any running turns cleanly so exit does not hang on a held agent mutex.
    st.current_cancel.cancel();
    for slot in st.background_sessions.values() {
        slot.current_cancel.cancel();
    }

    // Emit SessionEnd hook cleanly
    if let Ok(ag) = env.agent.try_lock() {
        if let Err(error) = ag
            .emit_hook_event(harness_core::HookEvent::SessionEnd, None)
            .await
        {
            tracing::warn!("session_end hook failed: {}", error);
        }
    }

    // Clean up current active session if it was never used (no conversation).
    if st.turns.is_empty() && st.active_turn.is_none() {
        if let Ok(record) = env.store.get(env.session_id).await {
            if record.message_count == 0 {
                let _ = env.store.delete_session(env.session_id).await;
            }
        }
    }
    // Also clean up any empty background sessions that were never used.
    for (bg_id, slot) in &st.background_sessions {
        if slot.turns.is_empty() && slot.active_turn.is_none() {
            if let Ok(record) = env.store.get(bg_id).await {
                if record.message_count == 0 {
                    let _ = env.store.delete_session(bg_id).await;
                }
            }
        }
    }

    let session_record = env.store.get(env.session_id).await.ok();
    let session_id_copy = env.session_id.clone();
    drop(_term_guard);
    runtime.ui.set_tui_mode(false);

    if let Some(record) = session_record {
        if record.message_count > 0 {
            if record.name.is_empty() {
                println!("Resume: luna --session {}", session_id_copy);
            } else {
                println!(
                    "Resume: luna --session {}    (name: {})",
                    session_id_copy, record.name
                );
            }
        }
    }
    Ok(())
}

async fn init_tui_state(env: &mut Env<'_>) -> Result<TuiState, HarnessError> {
    let replayed = env
        .store
        .replay_transcript(
            env.session_id.as_str(),
            env.config.session.resume_max_messages,
        )
        .await?;
    let turns = parse_turns_from_history(&replayed.history);
    let turn_meta: Vec<TurnMeta> = turns
        .iter()
        .map(|_| TurnMeta {
            mode: "Build".to_string(),
            model: env.current_model.clone(),
            output_tokens: None,
        })
        .collect();

    let ctx_max = context_window(
        env.config,
        env.discovery.as_ref(),
        env.current_provider.as_str(),
        env.current_model.as_str(),
    );
    let (ctx_used, reasoning_effort, template_agent) = {
        let ag = env.agent.lock().await;
        let used = ag.estimate_session_tokens().await.unwrap_or(0);
        let effort = ag.reasoning_effort();
        let tmpl = ag.fork_for_session(
            ag.provider().clone(),
            SessionBinding {
                store: env.store.clone(),
                session_id: env.session_id.clone(),
                resume_max_messages: env.config.session.resume_max_messages,
            },
            Arc::new(|_| {}),
            effort,
            ag.config().context_window,
        );
        (used, effort, Arc::new(tmpl))
    };
    env.ui
        .set_status(env.current_model.as_str(), ctx_used, ctx_max);
    refresh_antigravity_status_if_needed(
        &env.ui,
        env.current_provider.as_str(),
        env.current_model.as_str(),
        &env.cred_manager,
        Some(env.session_id.as_str()),
    )
    .await;

    let stored_name = env
        .store
        .get(env.session_id.as_str())
        .await
        .map(|s| s.name)
        .unwrap_or_default();
    env.ui.set_session_name(&stored_name);
    todo::refresh_panel(&env.ui, &env.todo_scope);

    let mode = if env.plan_mode.load(Ordering::SeqCst) {
        AgentMode::Plan
    } else if env.allow_all_session.load(Ordering::SeqCst) {
        AgentMode::Auto
    } else {
        AgentMode::Build
    };
    env.ui.set_yolo(mode == AgentMode::Auto);
    env.ui.set_plan_mode(mode == AgentMode::Plan);

    let history_path = env
        .config
        .paths
        .sessions_db
        .parent()
        .map(|p| p.join("history.txt"));
    let prompt_history = load_prompt_history(history_path.as_deref(), &turns);

    let cwd_label = shorten_cwd(&env.config.paths.workspace_root);

    let initial_turns_len = turns.len();
    let (picker_tx, picker_rx) = tokio::sync::mpsc::unbounded_channel();
    let (approval_reply_tx, approval_reply_rx) = tokio::sync::mpsc::unbounded_channel();
    let (voice_tx, voice_rx) = tokio::sync::mpsc::unbounded_channel();
    let voice_speech_queue = crate::commands::voice::VoiceSpeechQueue::new();
    let voice_is_playing = voice_speech_queue.is_playing();
    // Voice mode defaults as turned off on startup and is only activated when explicitly triggered by the user.
    let voice_active = false;
    let mut st = TuiState {
        turns,
        turn_meta,
        system_notices: Vec::new(),
        process_expanded: !env.config.display.auto_collapse,
        auto_hide_thinking: env.config.display.auto_hide_thinking,
        block_limit: env.config.display.block_limit,
        expanded_thinking: HashSet::new(),
        expanded_tools: HashSet::new(),
        expanded_turns: HashSet::new(),
        collapsed_turns: HashSet::new(),
        expanded_live_thinking: HashSet::new(),
        expanded_live_tools: HashSet::new(),
        live_thinking_view: LiveThinkingView::Tail,
        reasoning_effort,
        pending_model_choice: None,
        mode,
        composer: ComposerState {
            prompt_history,
            ..Default::default()
        },
        tabs: Vec::new(),
        total_sessions_count: 0,
        hit_boxes: Vec::new(),
        transcript_hit_boxes: Vec::new(),
        composer_hit_boxes: Vec::new(),
        pointer: None,
        last_click: None,
        click_count: 0,
        modal_rect: None,
        composer_area: (0, 0, 0, 0),
        composer_input_area: (0, 0, 0, 0),
        composer_scroll_off: 0,
        transcript_area: (0, 0, 0, 0),
        transcript_start: 0,
        transcript_visible_text: Vec::new(),
        selection: None,
        pending_transcript_click: None,
        selection_notice: None,
        scroll_from_bottom: 0,
        scroll_anchor_top: None,
        max_scroll: 0,
        modal: ActiveModal::None,
        slash_popup: SlashPopupState::default(),
        queue: VecDeque::new(),
        current_cancel: CancellationToken::new(),
        active_turn: None,
        active_goal: None,
        current_query: String::new(),
        turn_start: std::time::Instant::now(),
        quota_refreshed_at: std::time::Instant::now(),
        idle_pass: IdleMemoryPass::new(initial_turns_len),
        pending_job_turn: None,
        pending_images: Arc::new(Mutex::new(Vec::new())),
        history_path,
        ctrl_c_armed: None,
        esc_armed: None,
        spinner_tick: 0,
        cwd_label,
        session_pinned_skills: env
            .store
            .get_pinned_skills(env.session_id)
            .await
            .unwrap_or_default(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        update_notice: None,
        last_rendered_rect: (120, 34),
        provider_items: actions::build_provider_items_with_usage(
            env.cred_manager.clone(),
            Some(&env.config.providers),
            env.current_provider.as_str(),
            Some(&env.config.paths.usage_db),
        )
        .await,
        picker_loading: None,
        picker_tx: picker_tx.clone(),
        picker_rx,
        approval_reply_tx,
        approval_reply_rx,
        voice_active,
        voice_cancel: None,
        voice_speech_queue,
        voice_is_playing,
        voice_speaking_detected: false,
        voice_transcribing: false,
        voice_audio_level: 0.0,
        voice_auto_send: env.config.voice.auto_send,
        voice_pending_autosend_at: None,
        voice_stream_offset: 0,
        voice_stream_in_code: false,
        voice_spoken_any: false,
        voice_ack_sent: false,
        voice_reply_cancel: None,
        voice_tx,
        voice_rx,
        settled_cache: state::SettledTranscriptCache::default(),
        live_content_cache: state::LiveContentCache::default(),
        live_reasoning_cache: state::LiveReasoningCache::default(),
        template_agent,
        background_sessions: HashMap::new(),
        pending_modals: VecDeque::new(),
        orchestrator_draft: None,
        active_orchestrator: None,
        memory_project_filter: None,
        compacted_added: 0,
        compacted_deleted: 0,
        session_max_added: 0,
        session_max_deleted: 0,
        diff_anim_from_added: 0,
        diff_anim_from_deleted: 0,
        diff_anim_target_added: 0,
        diff_anim_target_deleted: 0,
        diff_anim_start: None,
    };
    if let Some(saved_orch) = actions::load_session_orchestrator(env.session_id.as_str()) {
        if let Some(orch) = env.config.orchestrators.get(&saved_orch) {
            st.active_orchestrator = Some(saved_orch.clone());
            let mut pool: Vec<(
                std::sync::Arc<dyn harness_core::ModelProvider>,
                Option<harness_core::ReasoningEffort>,
            )> = Vec::with_capacity(orch.subagents.len());
            for sub in &orch.subagents {
                if let Ok(client) = harness_providers::GenericProviderClient::new(
                    sub.provider.clone(),
                    sub.model.clone(),
                    env.cred_manager.clone(),
                    env.discovery.clone(),
                    env.config.providers.get(&sub.provider),
                ) {
                    pool.push((
                        std::sync::Arc::new(client.with_session_id(env.session_id.as_str())),
                        sub.thinking,
                    ));
                }
            }
            env.delegate_tool.set_subagent_pool(pool);
            let prompt_block = actions::build_orchestrator_prompt_block(&saved_orch, orch);
            if let Ok(mut ag) = env.agent.try_lock() {
                ag.set_orchestrator_prompt(Some(prompt_block));
            }
        }
    }
    if voice_active {
        actions::activate_voice(env, &mut st).await;
    }
    refresh_tabs(env, &mut st).await;
    Ok(st)
}

async fn poll_any_background_turn(
    bg: &mut HashMap<String, BackgroundSession>,
) -> Option<(
    String,
    Result<Result<TurnOutcome, HarnessError>, tokio::task::JoinError>,
)> {
    std::future::poll_fn(|cx| {
        let mut finished_id: Option<String> = None;
        let mut finished_res = None;
        for (id, slot) in bg.iter_mut() {
            if let Some(ref mut handle) = slot.active_turn {
                if let std::task::Poll::Ready(res) = std::pin::Pin::new(handle).poll(cx) {
                    finished_id = Some(id.clone());
                    finished_res = Some(res);
                    break;
                }
            }
        }
        if let (Some(id), Some(res)) = (finished_id, finished_res) {
            if let Some(slot) = bg.get_mut(&id) {
                slot.active_turn = None;
            }
            std::task::Poll::Ready(Some((id, res)))
        } else {
            std::task::Poll::Pending
        }
    })
    .await
}
