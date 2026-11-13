//! Session actions, slash-command execution, turn lifecycle, and tab management for the TUI.

use std::io;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use crossterm::event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture};
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use harness_core::{
    CompactionSetup, HarnessError, ReasoningEffort, SessionBinding, SessionStore, TurnOutcome,
};
use harness_providers::GenericProviderClient;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use super::super::commands::{
    command_entries, execute_slash_command, looks_like_path, SlashOutcome,
};
use super::super::helpers::finish_slash_command;
use super::super::history::append_prompt_history;
use super::super::interactive::state::{Env, Flow};
use super::super::keys::{self, AppKey};
use super::super::render::{parse_turns_from_history, TurnProcessItem};
use super::super::todo;
use super::super::turn::{
    cancel_running_turn, drain_queue, handle_composer_command, launch_internal_turn, launch_turn,
    turn_records, InterruptNotice,
};
use super::state::{
    ActiveModal, AgentMode, ModelPickerItem, PaletteAction, SessionTab, TuiState, TurnMeta,
};
use super::theme::friendly_model_name;
use crate::commands::context_window::context_window;
use crate::commands::sessions::{resolve_session, SessionSelection};

/// Rebuild the top session tabs from `SqliteSessionStore`, keeping the active tab visible.
pub async fn refresh_tabs(env: &Env<'_>, st: &mut TuiState) {
    let sessions = env.store.list().await.unwrap_or_default();
    let non_worker: Vec<_> = sessions
        .into_iter()
        .filter(|s| !s.name.starts_with("worker:"))
        .collect();
    st.total_sessions_count = non_worker.len();

    // Show up to 6 most recent non-worker sessions (oldest-to-newest left-to-right, active on right if fresh).
    let mut tabs: Vec<SessionTab> = non_worker
        .iter()
        .take(6)
        .map(|s| {
            let title = if !s.name.is_empty() {
                s.name.clone()
            } else if !s.title.is_empty() {
                s.title.clone()
            } else {
                "+ New session".to_string()
            };
            SessionTab {
                id: s.id.clone(),
                title,
            }
        })
        .collect();
    tabs.reverse();

    if !tabs.iter().any(|t| t.id == *env.session_id) {
        let cur_title = env
            .store
            .get(env.session_id)
            .await
            .map(|s| {
                if !s.name.is_empty() {
                    s.name
                } else if !s.title.is_empty() {
                    s.title
                } else {
                    "+ New session".to_string()
                }
            })
            .unwrap_or_else(|_| "+ New session".to_string());
        tabs.push(SessionTab {
            id: env.session_id.clone(),
            title: cur_title,
        });
    }

    st.tabs = tabs;
}

/// Update `st.slash_popup.matches` for the current composer text.
pub fn sync_slash_popup(env: &Env<'_>, st: &mut TuiState) {
    if !st.composer.text.starts_with('/')
        || st.composer.text.contains('\n')
        || st.composer.text[1..].contains(' ')
    {
        st.slash_popup.matches.clear();
        st.slash_popup.selected = 0;
        st.slash_popup.dismissed = false;
        return;
    }

    let query = st.composer.text.to_ascii_lowercase();
    let skills_arc = env.skills_arc.clone();
    let skills: &[harness_plugins::Skill] = skills_arc
        .as_deref()
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let all = command_entries(
        env.config,
        skills,
        env.feature_toggles
            .enabled(super::super::commands::Feature::Skills),
    );
    let filtered: Vec<_> = all
        .into_iter()
        .filter(|e| e.name.to_ascii_lowercase().starts_with(&query))
        .collect();
    st.slash_popup.matches = filtered;
    if st.slash_popup.selected >= st.slash_popup.matches.len() {
        st.slash_popup.selected = 0;
    }
}

/// Switch agent mode (`Build` -> `Auto` -> `Plan`) and sync harness flags + UI status.
pub fn apply_agent_mode(env: &Env<'_>, st: &mut TuiState, mode: AgentMode) {
    st.mode = mode;
    match mode {
        AgentMode::Build => {
            env.allow_all_session.store(false, Ordering::SeqCst);
            env.plan_mode.store(false, Ordering::SeqCst);
            env.ui.set_yolo(false);
            env.ui.set_plan_mode(false);
        }
        AgentMode::Auto => {
            env.allow_all_session.store(true, Ordering::SeqCst);
            env.plan_mode.store(false, Ordering::SeqCst);
            env.ui.set_yolo(true);
            env.ui.set_plan_mode(false);
        }
        AgentMode::Plan => {
            env.allow_all_session.store(false, Ordering::SeqCst);
            env.plan_mode.store(true, Ordering::SeqCst);
            env.ui.set_yolo(false);
            env.ui.set_plan_mode(true);
        }
    }
}

/// Switch the active TUI session in-place without leaving the alternate screen.
pub async fn switch_to_session(
    env: &mut Env<'_>,
    st: &mut TuiState,
    target_id: &str,
) -> Result<(), HarnessError> {
    if target_id == *env.session_id {
        return Ok(());
    }
    if st.active_turn.is_some() {
        env.ui
            .set_transient_error(Some("Wait for or interrupt the active turn before switching sessions.".into()));
        return Ok(());
    }

    let record = env.store.get(target_id).await?;
    *env.session_id = target_id.to_string();
    env.todo_scope.set_session(target_id);
    todo::refresh_panel(&env.ui, &env.todo_scope);

    if let Some((provider, model)) = record.model.clone() {
        if !provider.is_empty() && !model.is_empty() {
            let _ = apply_model_selection(env, st, &provider, &model, false).await;
        }
    }

    let replayed = env
        .store
        .replay_transcript(target_id, env.config.session.resume_max_messages)
        .await?;
    let mut ag = env.agent.lock().await;
    ag.set_session(SessionBinding {
        store: env.store.clone(),
        session_id: target_id.to_string(),
        resume_max_messages: env.config.session.resume_max_messages,
    });
    let used = ag.estimate_session_tokens().await.unwrap_or(0);
    drop(ag);

    st.turns = parse_turns_from_history(&replayed.history);
    st.turn_meta = st
        .turns
        .iter()
        .map(|_| TurnMeta {
            mode: st.mode.label().to_string(),
            model: env.current_model.clone(),
            output_tokens: None,
        })
        .collect();
    st.system_notices.clear();
    st.scroll_from_bottom = 0;

    let ctx_max = context_window(
        env.config,
        env.discovery.as_ref(),
        env.current_provider,
        env.current_model,
    );
    env.ui.set_status(env.current_model, used, ctx_max);
    env.ui.set_session_name(&record.name);
    env.ui.clear_live_state();
    env.ui.clear_transient_error();
    refresh_tabs(env, st).await;
    Ok(())
}

/// Open a brand new session tab and switch into it.
pub async fn create_new_session(env: &mut Env<'_>, st: &mut TuiState) -> Result<(), HarnessError> {
    if st.active_turn.is_some() {
        env.ui
            .set_transient_error(Some("Interrupt or finish the current turn before starting a new session.".into()));
        return Ok(());
    }
    let new_id = resolve_session(env.store.as_ref(), SessionSelection::New).await?;
    let _ = env
        .store
        .set_model(&new_id, env.current_provider, env.current_model)
        .await;
    *env.session_id = new_id.clone();
    env.todo_scope.set_session(&new_id);
    env.ui.set_todo_panel(None);
    env.agent.lock().await.set_session(SessionBinding {
        store: env.store.clone(),
        session_id: new_id,
        resume_max_messages: env.config.session.resume_max_messages,
    });

    st.turns.clear();
    st.turn_meta.clear();
    st.system_notices.clear();
    st.scroll_from_bottom = 0;

    let ctx_max = context_window(
        env.config,
        env.discovery.as_ref(),
        env.current_provider,
        env.current_model,
    );
    env.ui.set_status(env.current_model, 0, ctx_max);
    env.ui.set_session_name("");
    env.ui.clear_live_state();
    env.ui.clear_transient_error();
    refresh_tabs(env, st).await;
    Ok(())
}

pub async fn open_session_picker(env: &Env<'_>, st: &mut TuiState) {
    let sessions: Vec<_> = env
        .store
        .list()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|s| !s.name.starts_with("worker:"))
        .collect();
    let selected = sessions
        .iter()
        .position(|s| s.id == *env.session_id)
        .unwrap_or(0);
    st.modal = ActiveModal::SessionPicker {
        query: String::new(),
        sessions,
        selected,
    };
}

pub async fn open_model_picker(env: &Env<'_>, st: &mut TuiState) {
    let mut providers = env.cred_manager.get_loaded_providers().await;
    if !providers.iter().any(|p| p == env.current_provider) {
        providers.insert(0, env.current_provider.clone());
    }

    let mut items = Vec::new();
    for prov in providers {
        let mut models = env
            .discovery
            .get_models(&prov, &env.cred_manager, false)
            .await
            .unwrap_or_default();
        let def = harness_providers::catalog::get_default_model(&prov).to_string();
        if !def.is_empty() && !models.contains(&def) {
            models.insert(0, def);
        }
        if prov == *env.current_provider && !models.contains(env.current_model) {
            models.insert(0, env.current_model.clone());
        }
        for m in models {
            let is_current = prov == *env.current_provider && m == *env.current_model;
            items.push(ModelPickerItem {
                display_name: friendly_model_name(&m),
                provider: prov.clone(),
                model: m,
                is_current,
            });
        }
    }

    let selected = items.iter().position(|i| i.is_current).unwrap_or(0);
    st.modal = ActiveModal::ModelPicker {
        query: String::new(),
        items,
        selected,
    };
}

pub async fn open_thinking_picker(env: &Env<'_>, st: &mut TuiState) {
    let cur = env.agent.lock().await.reasoning_effort();
    let selected = match cur {
        None => 0,
        Some(ReasoningEffort::Minimal) => 1,
        Some(ReasoningEffort::Low) => 2,
        Some(ReasoningEffort::Medium) => 3,
        Some(ReasoningEffort::High) => 4,
        Some(ReasoningEffort::Xhigh) => 5,
        Some(ReasoningEffort::Max) => 6,
    };
    st.modal = ActiveModal::ThinkingPicker {
        current: cur,
        selected,
    };
}

pub async fn apply_model_selection(
    env: &mut Env<'_>,
    st: &mut TuiState,
    provider: &str,
    model: &str,
    persist: bool,
) -> Result<(), HarnessError> {
    let client = Arc::new(GenericProviderClient::new(
        provider.to_string(),
        model.to_string(),
        env.cred_manager.clone(),
        env.discovery.clone(),
        env.config.providers.get(provider),
    )?);
    let ctx_max = context_window(env.config, env.discovery.as_ref(), provider, model);
    {
        let mut ag = env.agent.lock().await;
        ag.set_provider(client);
        ag.set_context_window(Some(ctx_max as usize));
        if env.compact_model.is_none() {
            if let Ok(comp_client) = GenericProviderClient::new(
                provider.to_string(),
                model.to_string(),
                env.cred_manager.clone(),
                env.discovery.clone(),
                env.config.providers.get(provider),
            ) {
                ag.set_compaction(Some(CompactionSetup {
                    provider: Arc::new(comp_client),
                    model: model.to_string(),
                }));
            }
        }
    }
    *env.current_provider = provider.to_string();
    *env.current_model = model.to_string();
    let used = env.agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
    env.ui.set_status(model, used, ctx_max);
    if persist {
        let _ = env.store.set_model(env.session_id, provider, model).await;
    }
    st.scroll_from_bottom = 0;
    Ok(())
}

pub async fn apply_thinking_selection(env: &Env<'_>, effort: Option<ReasoningEffort>) {
    env.agent.lock().await.set_reasoning_effort(effort);
}

/// Execute a `ctrl+p` Command Palette action.
pub async fn run_palette_action(
    env: &mut Env<'_>,
    st: &mut TuiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    action: PaletteAction,
) -> Result<Flow, HarnessError> {
    match action {
        PaletteAction::NewSession => {
            create_new_session(env, st).await?;
        }
        PaletteAction::SwitchSession => {
            open_session_picker(env, st).await;
        }
        PaletteAction::SwitchModel => {
            open_model_picker(env, st).await;
        }
        PaletteAction::CycleAgentMode => {
            apply_agent_mode(env, st, st.mode.cycle());
        }
        PaletteAction::SelectThinking => {
            open_thinking_picker(env, st).await;
        }
        PaletteAction::ToggleProcessDetails => {
            st.process_expanded = !st.process_expanded;
        }
        PaletteAction::ToggleTodoPanel => {
            env.ui.toggle_todo_collapsed();
        }
        PaletteAction::CompactContext => {
            return run_slash_command(env, st, terminal, "/compact", "/compact").await;
        }
        PaletteAction::ShowDiff => {
            return run_slash_command(env, st, terminal, "/diff", "/diff").await;
        }
        PaletteAction::ShowUsage => {
            return run_slash_command(env, st, terminal, "/usage", "/usage").await;
        }
        PaletteAction::ShowBackgroundJobs => {
            return run_slash_command(env, st, terminal, "/jobs", "/jobs").await;
        }
        PaletteAction::ShowHelp => {
            st.modal = ActiveModal::Help { scroll: 0 };
        }
        PaletteAction::SlashCommand(cmd) => {
            return run_slash_command(env, st, terminal, &cmd, &cmd).await;
        }
        PaletteAction::Quit => return Ok(Flow::Break),
    }
    Ok(Flow::Continue)
}

/// Submit whatever is currently in the composer.
pub async fn submit_composer(
    env: &mut Env<'_>,
    st: &mut TuiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<Flow, HarnessError> {
    let raw = std::mem::take(&mut st.composer.text);
    st.composer.clear();
    sync_slash_popup(env, st);

    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Flow::Continue);
    }

    if st.composer.prompt_history.last().map(String::as_str) != Some(trimmed) {
        st.composer.prompt_history.push(trimmed.to_string());
        if let Some(ref path) = st.history_path {
            append_prompt_history(Some(path), trimmed);
        }
    }

    if trimmed.eq_ignore_ascii_case("exit") || trimmed.eq_ignore_ascii_case("quit") {
        return Ok(Flow::Break);
    }

    if trimmed.starts_with('/') && !looks_like_path(trimmed) {
        return run_slash_command(env, st, terminal, trimmed, trimmed).await;
    }

    // Normal user prompt: queue/steer if a turn is active, else launch turn immediately.
    if env.compacting.load(Ordering::SeqCst) {
        st.queue.push_back(trimmed.to_string());
        env.ui.set_queue_depth(st.queue.len());
        return Ok(Flow::Continue);
    }

    if st.active_turn.is_some() {
        match env.interrupt_handle.interrupt(trimmed) {
            harness_core::InterruptOutcome::Streaming => {
                st.system_notices
                    .push("↳ Redirecting — stopping this reply to take your message".to_string());
            }
            harness_core::InterruptOutcome::Queued => {
                st.system_notices
                    .push("↳ Held until the running tool finishes, then sent".to_string());
            }
            harness_core::InterruptOutcome::Idle => {
                st.queue.push_back(trimmed.to_string());
                env.ui.set_queue_depth(st.queue.len());
            }
        }
        return Ok(Flow::Continue);
    }

    env.ui.clear_transient_error();
    st.scroll_from_bottom = 0;
    launch_turn(
        trimmed,
        None,
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
    Ok(Flow::Continue)
}

/// Run a slash command inside the TUI, opening native modals where available and suspending the
/// alternate screen only for legacy interactive CLI commands (`/settings`, `/dir`, `/jobs`, etc.).
pub async fn run_slash_command(
    env: &mut Env<'_>,
    st: &mut TuiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    cmd: &str,
    display: &str,
) -> Result<Flow, HarnessError> {
    let parts: Vec<&str> = cmd.trim().splitn(2, ' ').collect();
    let name = parts[0];
    let arg = parts
        .get(1)
        .copied()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    // Handle native TUI routes first so there's zero screen flash.
    match name {
        "/help" | "/?" if arg.is_none() => {
            st.modal = ActiveModal::Help { scroll: 0 };
            return Ok(Flow::Continue);
        }
        "/sessions" | "/session" if arg.is_none() => {
            open_session_picker(env, st).await;
            return Ok(Flow::Continue);
        }
        "/model" | "/models" if arg.is_none() => {
            open_model_picker(env, st).await;
            return Ok(Flow::Continue);
        }
        "/thinking" | "/think" | "/reasoning" if arg.is_none() => {
            open_thinking_picker(env, st).await;
            return Ok(Flow::Continue);
        }
        "/plan" => {
            let next = if st.mode == AgentMode::Plan {
                AgentMode::Build
            } else {
                AgentMode::Plan
            };
            apply_agent_mode(env, st, next);
            return Ok(Flow::Continue);
        }
        "/yolo" => {
            let next = if st.mode == AgentMode::Auto {
                AgentMode::Build
            } else {
                AgentMode::Auto
            };
            apply_agent_mode(env, st, next);
            return Ok(Flow::Continue);
        }
        "/clear" => {
            st.turns.clear();
            st.turn_meta.clear();
            st.system_notices.clear();
            st.scroll_from_bottom = 0;
            return Ok(Flow::Continue);
        }
        "/exit" | "/quit" => {
            return Ok(Flow::Break);
        }
        _ => {}
    }

    // Composer-level control commands (`/interrupt`, `/stop`, `/queue ...`)
    if handle_composer_command(
        cmd,
        &mut st.queue,
        &env.steer_handle,
        &env.ui,
        &mut st.active_turn,
        &mut st.active_goal,
        &mut st.current_cancel,
        &env.interrupt_handle,
        &mut st.turns,
        &env.in_flight_tools,
        &env.interruptions,
        st.turn_start,
        &st.current_query,
        st.process_expanded,
        &env.agent,
        context_window(
            env.config,
            env.discovery.as_ref(),
            env.current_provider,
            env.current_model,
        ),
    )
    .await
    {
        sync_turn_meta_len(env, st);
        drain_tui_notices(env, st);
        return Ok(Flow::Continue);
    }

    // Legacy interactive modals that read `key_rx` directly: temporarily suspend TUI alt-screen.
    let needs_legacy_modal = matches!(
        name,
        "/dir"
            | "/settings"
            | "/connect"
            | "/profile"
            | "/jobs"
            | "/tasks"
            | "/subagents"
            | "/compact-model"
            | "/bgsaver-model"
            | "/default_model"
            | "/auto-compact"
            | "/autocompact"
    ) || (name == "/model" && arg.is_some() && !arg.unwrap().contains('/'));

    let skills_arc = env.skills_arc.clone();
    let skills: &[harness_plugins::Skill] = skills_arc
        .as_deref()
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    if needs_legacy_modal {
        let res = suspend_tui_for_legacy_modal(env, st, terminal, cmd, skills).await;
        drain_tui_notices(env, st);
        sync_turn_meta_len(env, st);
        refresh_tabs(env, st).await;
        return res;
    }

    let prev_len = st.turns.len();
    env.ui.begin_command_capture();
    let outcome = execute_slash_command(
        cmd,
        &mut mpsc::unbounded_channel::<AppKey>().1,
        &env.ui,
        env.config,
        &env.cred_manager,
        &env.discovery,
        &env.agent,
        &env.store,
        env.session_id,
        env.current_provider,
        env.current_model,
        env.compact_provider,
        env.compact_model,
        &mut st.turns,
        &mut st.process_expanded,
        &env.todo_scope,
        env.skills_readout.as_deref(),
        skills,
        env.feature_toggles,
        &env.plan_mode,
        st.active_goal.as_ref(),
        Some(env.bg_queue.as_ref()),
        Some(env.delegate_tool.as_ref()),
        Some(&st.pending_images),
        &env.display_settings,
    )
    .await?;

    match outcome {
        SlashOutcome::Continue => {
            finish_slash_command(&env.ui, &mut st.turns, display, st.process_expanded, false);
            if st.turns.len() < prev_len {
                st.turn_meta.truncate(st.turns.len());
            }
            sync_turn_meta_len(env, st);
            drain_tui_notices(env, st);
            refresh_tabs(env, st).await;
            st.scroll_from_bottom = 0;
            Ok(Flow::Continue)
        }
        SlashOutcome::Turn { prompt, display } => {
            let _ = env.ui.end_command_capture();
            if env.compacting.load(Ordering::SeqCst) {
                st.queue.push_back(prompt);
                env.ui.set_queue_depth(st.queue.len());
            } else {
                st.scroll_from_bottom = 0;
                launch_turn(
                    &prompt,
                    Some(&display),
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
            Ok(Flow::Continue)
        }
        SlashOutcome::StartGoal {
            state,
            prompt,
            display,
        } => {
            let _ = env.ui.end_command_capture();
            let iter = state.iteration;
            let max_iter = state.max_iterations;
            st.active_goal = Some(state);
            env.ui.set_goal_status(Some((iter, max_iter)));
            st.scroll_from_bottom = 0;
            launch_turn(
                &prompt,
                Some(&display),
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
            Ok(Flow::Continue)
        }
        SlashOutcome::Exit => Ok(Flow::Break),
    }
}

fn spawn_modal_key_reader() -> (Arc<std::sync::atomic::AtomicBool>, mpsc::UnboundedReceiver<AppKey>) {
    let (tx, rx) = mpsc::unbounded_channel::<AppKey>();
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag = stop.clone();
    std::thread::spawn(move || {
        while !stop_flag.load(Ordering::SeqCst) {
            match keys::read_key() {
                Ok(key) => {
                    if tx.send(key).is_err() {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    });
    (stop, rx)
}

async fn suspend_tui_for_legacy_modal(
    env: &mut Env<'_>,
    st: &mut TuiState,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    cmd: &str,
    skills: &[harness_plugins::Skill],
) -> Result<Flow, HarnessError> {
    // Leave TUI alternate screen and hand raw key reader temporarily to legacy dialoguer/modal code.
    env.ui.set_tui_mode(false);
    let _ = crossterm::execute!(
        io::stdout(),
        DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    );
    let _ = terminal.show_cursor();

    let (stop_reader, mut key_rx) = spawn_modal_key_reader();
    let res = if cmd == "/jobs" || cmd == "/tasks" || cmd == "/subagents" {
        super::super::viewer::show_background_task_viewer(
            &mut key_rx,
            Some(env.bg_queue.as_ref()),
            Some(env.delegate_tool.as_ref()),
            env.store.as_ref(),
            &env.ui,
            &st.turns,
            st.process_expanded,
        )
        .await;
        Ok(Flow::Continue)
    } else {
        env.ui.begin_command_capture();
        let outcome = execute_slash_command(
            cmd,
            &mut key_rx,
            &env.ui,
            env.config,
            &env.cred_manager,
            &env.discovery,
            &env.agent,
            &env.store,
            env.session_id,
            env.current_provider,
            env.current_model,
            env.compact_provider,
            env.compact_model,
            &mut st.turns,
            &mut st.process_expanded,
            &env.todo_scope,
            env.skills_readout.as_deref(),
            skills,
            env.feature_toggles,
            &env.plan_mode,
            st.active_goal.as_ref(),
            Some(env.bg_queue.as_ref()),
            Some(env.delegate_tool.as_ref()),
            Some(&st.pending_images),
            &env.display_settings,
        )
        .await?;
        finish_slash_command(&env.ui, &mut st.turns, cmd, st.process_expanded, false);
        match outcome {
            SlashOutcome::Exit => Ok(Flow::Break),
            _ => Ok(Flow::Continue),
        }
    };
    stop_reader.store(true, Ordering::SeqCst);
    drop(key_rx);

    env.ui.set_tui_mode(true);
    let _ = crossterm::terminal::enable_raw_mode();
    let _ = crossterm::execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    );
    let _ = terminal.clear();
    res
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
    env.ui.clear_live_state();

    let current_tools: Vec<TurnProcessItem> = std::mem::take(
        &mut *env
            .in_flight_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
    );
    let interruptions: Vec<InterruptNotice> = std::mem::take(
        &mut *env
            .interruptions
            .lock()
            .unwrap_or_else(|e| e.into_inner()),
    );

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
            let added = turn_records(
                &st.current_query,
                interruptions,
                st.turn_start,
                turn_dur,
                outcome.reasoning.clone(),
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
                st.process_expanded = false;
            }
            if st.active_goal.is_some() {
                goal_reply = Some(outcome.reply);
            }
        }
        Ok(Err(HarnessError::Cancelled)) => {
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
    let used = env.agent.lock().await.estimate_session_tokens().await.unwrap_or(0);
    let ctx_max = context_window(
        env.config,
        env.discovery.as_ref(),
        env.current_provider,
        env.current_model,
    );
    env.ui.update_status(used, ctx_max);

    if let Some(reply) = goal_reply {
        advance_goal_tui(env, st, &reply);
    }

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
    }

    refresh_tabs(env, st).await;
    drain_tui_notices(env, st);
    st.scroll_from_bottom = 0;
}

fn advance_goal_tui(env: &Env<'_>, st: &mut TuiState, reply: &str) {
    let Some(ref mut goal) = st.active_goal else {
        return;
    };
    match harness_core::GoalRunState::evaluate_reply(reply) {
        harness_core::GoalVerdict::Complete(_) => {
            st.system_notices
                .push(format!("✔ Goal achieved in {} iteration(s).", goal.iteration));
            st.active_goal = None;
            env.ui.set_goal_status(None);
        }
        harness_core::GoalVerdict::Blocked(reason) => {
            st.system_notices.push(format!(
                "✖ Goal blocked ({}/{}): {reason}",
                goal.iteration, goal.max_iterations
            ));
            st.active_goal = None;
            env.ui.set_goal_status(None);
        }
        harness_core::GoalVerdict::Continue => {
            if !goal.advance() {
                st.system_notices.push(format!(
                    "■ Goal stopped: max iterations ({}) reached",
                    goal.max_iterations
                ));
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

pub fn drain_tui_notices(env: &Env<'_>, st: &mut TuiState) {
    let notices = env.ui.take_tui_notices();
    for n in notices {
        if !n.trim().is_empty() {
            st.system_notices.push(n);
        }
    }
    if st.system_notices.len() > 20 {
        let excess = st.system_notices.len() - 20;
        st.system_notices.drain(..excess);
    }
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

/// Interrupt an active turn cleanly on `Esc` or `Ctrl+C`.
pub async fn interrupt_turn(env: &Env<'_>, st: &mut TuiState) {
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
