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
mod theme;

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
use futures_util::{FutureExt, StreamExt};
use harness_core::{HarnessError, SessionBinding, SessionStore, TurnOutcome};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio_util::sync::CancellationToken;

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
use render::{draw, FooterInfo};
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

    let tui_notifier = env.ui.tui_notifier();
    let mut events = EventStream::new();

    let mut animation = tokio::time::interval(ANIMATION_INTERVAL);
    animation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    let mut idle_tick = tokio::time::interval(Duration::from_secs(2));
    idle_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        let t_draw = std::time::Instant::now();
        // Render current frame (never await env.agent.lock() here — the running turn holds it!).
        // Keep the thinking auto-collapse toggle in step with `/settings` changes.
        st.auto_hide_thinking = env.config.display.auto_hide_thinking;
        let snap = {
            let w = terminal.size().map(|s| s.width as usize).unwrap_or(100);
            env.ui.tui_snapshot(w)
        };
        let reasoning_effort = st.reasoning_effort;
        let tools_snapshot = env
            .in_flight_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let info = FooterInfo {
            provider: env.current_provider,
            model: env.current_model,
            reasoning_effort,
            session_id: env.session_id,
            in_flight_tools: &tools_snapshot,
        };
        terminal.draw(|frame| draw(frame, &mut st, &snap, &info))?;
        trace_event(&format!("draw {:?}", t_draw.elapsed()));

        let has_bg_active = st
            .background_sessions
            .values()
            .any(|s| s.active_turn.is_some());

        tokio::select! {
            biased;

            maybe_ev = events.next() => {
                let mut current_ev = match maybe_ev {
                    Some(Ok(ev)) => ev,
                    Some(Err(_)) | None => break,
                };
                loop {
                    if input::handle_event(&mut env, &mut st, &mut terminal, current_ev).await? == Flow::Break {
                        return Ok(());
                    }
                    match events.next().now_or_never() {
                        Some(Some(Ok(next_ev))) => current_ev = next_ev,
                        Some(Some(Err(_))) | Some(None) => return Ok(()),
                        None => break,
                    }
                }
            }

            _ = tui_notifier.notified() => {
                drain_tui_notices(&env, &mut st);
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
                }
            }

            maybe_picker = st.picker_rx.recv() => {
                if let Some(load) = maybe_picker {
                    actions::apply_picker_load(&mut st, load);
                }
            }

            _ = animation.tick(), if st.active_turn.is_some()
                || has_bg_active
                || env.compacting.load(Ordering::SeqCst)
                || snap.delegate_count > 0
                || !st.queue.is_empty() => {
                st.spinner_tick = st.spinner_tick.wrapping_add(1);
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
                }
            }

            _ = idle_tick.tick() => {
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
                }
                for slot in st.background_sessions.values_mut() {
                    if slot.active_turn.is_none()
                        && !slot.compacting.load(Ordering::SeqCst)
                        && !slot.queue.is_empty()
                    {
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
                    }
                }

                if st.quota_refreshed_at.elapsed() >= QUOTA_REFRESH_INTERVAL {
                    st.quota_refreshed_at = std::time::Instant::now();
                    tokio::spawn(refresh_antigravity_quota_owned(
                        env.ui.clone(),
                        env.current_provider.clone(),
                        env.current_model.clone(),
                        env.cred_manager.clone(),
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
                if let Some((req, resp_tx)) = maybe_approval {
                    if !resp_tx.is_closed() {
                        st.modal = ActiveModal::Approval {
                            req,
                            resp_tx,
                            selected: 0,
                        };
                    }
                }
            }

            maybe_sudo = env.sudo_rx.recv() => {
                if let Some((prompt, resp_tx)) = maybe_sudo {
                    if !resp_tx.is_closed() {
                        st.modal = ActiveModal::Sudo {
                            prompt,
                            resp_tx,
                            input: String::new(),
                        };
                    }
                }
            }

            maybe_clarify = env.clarify_rx.recv() => {
                if let Some((req, resp_tx)) = maybe_clarify {
                    if !resp_tx.is_closed() {
                        st.modal = ActiveModal::Clarify {
                            req,
                            resp_tx,
                            question_idx: 0,
                            answers: Vec::new(),
                            selected: 0,
                            custom: String::new(),
                            editing_custom: false,
                        };
                    }
                }
            }

            maybe_job = env.job_rx.recv() => {
                if let Some(comp) = maybe_job {
                    st.system_notices.push(format!(
                        "Job {} ({}): {}",
                        comp.id,
                        comp.status,
                        comp.command
                    ));
                    let msg = harness_core::Message::system(format_job_system_message(&comp));
                    if let Err(e) = env.store.append(env.session_id, &msg, false).await {
                        tracing::warn!("failed to record background job notice: {e}");
                    } else {
                        st.pending_job_turn = Some((env.session_id.clone(), std::time::Instant::now()));
                    }
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

    let session_name = env
        .store
        .get(env.session_id)
        .await
        .map(|s| s.name)
        .unwrap_or_default();
    let session_id_copy = env.session_id.clone();
    drop(_term_guard);
    runtime.ui.set_tui_mode(false);

    if session_name.is_empty() {
        println!("Resume: luna --session {}", session_id_copy);
    } else {
        println!(
            "Resume: luna --session {}    (name: {})",
            session_id_copy, session_name
        );
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
    let mut st = TuiState {
        turns,
        turn_meta,
        system_notices: Vec::new(),
        process_expanded: false,
        auto_hide_thinking: env.config.display.auto_hide_thinking,
        expanded_thinking: HashSet::new(),
        expanded_tools: HashSet::new(),
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
        spinner_tick: 0,
        cwd_label,
        version: env!("CARGO_PKG_VERSION").to_string(),
        update_notice: None,
        last_rendered_rect: (120, 34),
        provider_items: actions::build_provider_items(
            env.cred_manager.clone(),
            env.current_provider.as_str(),
        )
        .await,
        picker_loading: None,
        picker_tx: picker_tx.clone(),
        picker_rx,
        settled_cache: state::SettledTranscriptCache::default(),
        live_content_cache: state::LiveContentCache::default(),
        live_reasoning_cache: state::LiveReasoningCache::default(),
        template_agent,
        background_sessions: HashMap::new(),
    };
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
