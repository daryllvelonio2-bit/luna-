//! Session actions, slash-command execution, turn lifecycle, and tab management for the TUI.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use harness_core::{
    CompactionSetup, HarnessError, ReasoningEffort, SessionBinding, SessionStore, TurnOutcome,
};
use harness_providers::{GenericProviderClient, RotationStrategy};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use super::super::commands::{
    apply_auto_compact_values, apply_dir_choice, command_entries, connected_providers,
    execute_slash_command, handle_settings_command, list_keys_text, looks_like_path,
    parse_dir_args, provider_id_of, provider_items, reset_workspace, SlashOutcome,
    RESERVED_COMMAND_NAMES,
};
use super::super::helpers::{finish_slash_command, IdleMemoryPass};
use super::super::history::append_prompt_history;
use super::super::interactive::state::{Env, Flow};
use super::super::observers::agent_observer;
use super::super::render::{parse_turns_from_history, TurnProcessItem};
use super::super::status::refresh_antigravity_status_if_needed;
use super::super::todo;
use super::super::turn::{
    cancel_running_turn, drain_queue, handle_composer_command, launch_internal_turn, launch_turn,
    turn_records, InterruptNotice,
};
use super::super::ui::ChatUi;
use super::markdown::strip_ansi;
use super::state::{
    ActiveModal, AgentMode, BackgroundSession, InputAction, LiveThinkingView, ModelPickerItem,
    ModelTarget, PaletteAction, ProviderPickerItem, SelectAction, SessionTab, TuiState, TurnMeta,
};
use super::theme::{friendly_model_name, friendly_provider_name};
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
    let skills: &[harness_plugins::Skill] = skills_arc.as_deref().map(Vec::as_slice).unwrap_or(&[]);
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

/// Park the currently displayed session into `st.background_sessions` so we can switch away
/// without interrupting an active turn or losing its live state.
fn park_current_session(env: &Env<'_>, st: &mut TuiState) {
    let old_id = env.session_id.clone();
    let fresh_idle = IdleMemoryPass::new(0);
    let slot = BackgroundSession {
        session_id: old_id.clone(),
        agent: env.agent.clone(),
        ui: env.ui.clone(),
        in_flight_tools: env.in_flight_tools.clone(),
        interruptions: env.interruptions.clone(),
        steer_handle: env.steer_handle.clone(),
        interrupt_handle: env.interrupt_handle.clone(),
        compacting: env.compacting.clone(),
        provider: env.current_provider.clone(),
        model: env.current_model.clone(),
        reasoning_effort: st.reasoning_effort,
        mode: st.mode,
        turns: std::mem::take(&mut st.turns),
        turn_meta: std::mem::take(&mut st.turn_meta),
        settled_cache: std::mem::take(&mut st.settled_cache),
        system_notices: std::mem::take(&mut st.system_notices),
        expanded_thinking: std::mem::take(&mut st.expanded_thinking),
        expanded_tools: std::mem::take(&mut st.expanded_tools),
        expanded_live_thinking: std::mem::take(&mut st.expanded_live_thinking),
        expanded_live_tools: std::mem::take(&mut st.expanded_live_tools),
        live_thinking_view: st.live_thinking_view,
        process_expanded: st.process_expanded,
        scroll_from_bottom: st.scroll_from_bottom,
        queue: std::mem::take(&mut st.queue),
        current_cancel: std::mem::take(&mut st.current_cancel),
        active_turn: st.active_turn.take(),
        active_goal: st.active_goal.take(),
        current_query: std::mem::take(&mut st.current_query),
        turn_start: st.turn_start,
        idle_pass: std::mem::replace(&mut st.idle_pass, fresh_idle),
        pending_images: st.pending_images.clone(),
        pending_job_turn: st.pending_job_turn.take(),
    };
    st.background_sessions.insert(old_id, slot);
}

fn restore_background_session(env: &mut Env<'_>, st: &mut TuiState, slot: BackgroundSession) {
    *env.session_id = slot.session_id;
    env.agent = slot.agent;
    env.ui = slot.ui;
    env.in_flight_tools = slot.in_flight_tools;
    env.interruptions = slot.interruptions;
    env.steer_handle = slot.steer_handle;
    env.interrupt_handle = slot.interrupt_handle;
    env.compacting = slot.compacting;
    *env.current_provider = slot.provider;
    *env.current_model = slot.model;
    st.reasoning_effort = slot.reasoning_effort;
    st.mode = slot.mode;
    st.turns = slot.turns;
    st.turn_meta = slot.turn_meta;
    st.settled_cache = slot.settled_cache;
    st.live_content_cache = super::state::LiveContentCache::default();
    st.live_reasoning_cache = super::state::LiveReasoningCache::default();
    st.system_notices = slot.system_notices;
    st.expanded_thinking = slot.expanded_thinking;
    st.expanded_tools = slot.expanded_tools;
    st.expanded_live_thinking = slot.expanded_live_thinking;
    st.expanded_live_tools = slot.expanded_live_tools;
    st.live_thinking_view = slot.live_thinking_view;
    st.process_expanded = slot.process_expanded;
    st.scroll_from_bottom = slot.scroll_from_bottom;
    st.selection = None;
    st.pending_transcript_click = None;
    st.selection_notice = None;
    st.queue = slot.queue;
    st.current_cancel = slot.current_cancel;
    st.active_turn = slot.active_turn;
    st.active_goal = slot.active_goal;
    st.current_query = slot.current_query;
    st.turn_start = slot.turn_start;
    st.idle_pass = slot.idle_pass;
    st.pending_images = slot.pending_images;
    st.pending_job_turn = slot.pending_job_turn;

    env.todo_scope.set_session(env.session_id.as_str());
    todo::refresh_panel(&env.ui, &env.todo_scope);
    apply_agent_mode(env, st, st.mode);
}

type SessionRuntimeBundle = (
    Arc<tokio::sync::Mutex<super::super::ChatAgent>>,
    Arc<ChatUi>,
    Arc<Mutex<Vec<TurnProcessItem>>>,
    Arc<Mutex<Vec<InterruptNotice>>>,
    Arc<Mutex<VecDeque<String>>>,
    Arc<harness_core::InterruptHandle>,
    Arc<AtomicBool>,
    u32,
);

/// Build an independent `ChatAgent` + `ChatUi` bundle for a newly-activated session without
/// touching any currently running session's `agent` lock.
fn instantiate_session_runtime(
    env: &Env<'_>,
    st: &TuiState,
    session_id: &str,
    provider: &str,
    model: &str,
    reasoning_effort: Option<ReasoningEffort>,
) -> Result<SessionRuntimeBundle, HarnessError> {
    let client = Arc::new(GenericProviderClient::new(
        provider.to_string(),
        model.to_string(),
        env.cred_manager.clone(),
        env.discovery.clone(),
        env.config.providers.get(provider),
    )?);
    let ctx_max = context_window(env.config, env.discovery.as_ref(), provider, model);

    let ui = Arc::new(ChatUi::new_with_tui_notify(
        env.ui.is_attended,
        env.ui.tui_notifier(),
    ));
    ui.set_tui_mode(true);
    ui.set_display_block_limit(env.config.display.block_limit);

    let in_flight_tools: Arc<Mutex<Vec<TurnProcessItem>>> = Arc::new(Mutex::new(Vec::new()));
    let interruptions: Arc<Mutex<Vec<InterruptNotice>>> = Arc::new(Mutex::new(Vec::new()));
    let compacting = Arc::new(AtomicBool::new(false));
    let todo_scope =
        harness_core::tools::TodoScope::new(env.config.paths.todos_dir.clone(), session_id);
    let observer = agent_observer(
        ui.clone(),
        in_flight_tools.clone(),
        interruptions.clone(),
        todo_scope,
        compacting.clone(),
        env.display_settings.clone(),
    );

    let mut ag = st.template_agent.fork_for_session(
        client,
        SessionBinding {
            store: env.store.clone(),
            session_id: session_id.to_string(),
            resume_max_messages: env.config.session.resume_max_messages,
        },
        observer,
        reasoning_effort,
        Some(ctx_max as usize),
    );
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

    let steer_handle = ag.steer_handle();
    let interrupt_handle = ag.interrupt_handle();
    let agent = Arc::new(tokio::sync::Mutex::new(ag));
    Ok((
        agent,
        ui,
        in_flight_tools,
        interruptions,
        steer_handle,
        interrupt_handle,
        compacting,
        ctx_max,
    ))
}

/// Switch the active TUI session in-place without leaving the alternate screen.
/// Works even while the current session is running a turn in the background.
pub async fn switch_to_session(
    env: &mut Env<'_>,
    st: &mut TuiState,
    target_id: &str,
) -> Result<(), HarnessError> {
    if target_id == *env.session_id {
        return Ok(());
    }

    if let Some(existing) = st.background_sessions.remove(target_id) {
        park_current_session(env, st);
        restore_background_session(env, st, existing);
        refresh_tabs(env, st).await;
        return Ok(());
    }

    let record = env.store.get(target_id).await?;
    let (target_provider, target_model) = match record.model.clone() {
        Some((p, m)) if !p.is_empty() && !m.is_empty() => (p, m),
        _ => (env.current_provider.clone(), env.current_model.clone()),
    };

    let replayed = env
        .store
        .replay_transcript(target_id, env.config.session.resume_max_messages)
        .await?;

    let (
        agent,
        ui,
        in_flight_tools,
        interruptions,
        steer_handle,
        interrupt_handle,
        compacting,
        ctx_max,
    ) = instantiate_session_runtime(
        env,
        st,
        target_id,
        &target_provider,
        &target_model,
        st.reasoning_effort,
    )?;

    let used = agent
        .lock()
        .await
        .estimate_session_tokens()
        .await
        .unwrap_or(0);
    let turns = parse_turns_from_history(&replayed.history);
    let turn_meta: Vec<TurnMeta> = turns
        .iter()
        .map(|_| TurnMeta {
            mode: st.mode.label().to_string(),
            model: target_model.clone(),
            output_tokens: None,
        })
        .collect();
    let initial_len = turns.len();

    park_current_session(env, st);

    *env.session_id = target_id.to_string();
    env.agent = agent;
    env.ui = ui;
    env.in_flight_tools = in_flight_tools;
    env.interruptions = interruptions;
    env.steer_handle = steer_handle;
    env.interrupt_handle = interrupt_handle;
    env.compacting = compacting;
    *env.current_provider = target_provider.clone();
    *env.current_model = target_model.clone();

    env.todo_scope.set_session(target_id);
    todo::refresh_panel(&env.ui, &env.todo_scope);
    apply_agent_mode(env, st, st.mode);

    st.turns = turns;
    st.turn_meta = turn_meta;
    st.system_notices.clear();
    st.expanded_thinking.clear();
    st.expanded_tools.clear();
    st.expanded_live_thinking.clear();
    st.expanded_live_tools.clear();
    st.live_thinking_view = LiveThinkingView::Tail;
    st.process_expanded = false;
    st.scroll_from_bottom = 0;
    st.selection = None;
    st.pending_transcript_click = None;
    st.selection_notice = None;
    st.idle_pass = IdleMemoryPass::new(initial_len);
    st.pending_images = Arc::new(Mutex::new(Vec::new()));

    env.ui.set_status(&target_model, used, ctx_max);
    env.ui.set_session_name(&record.name);
    env.ui.clear_live_state();
    env.ui.clear_transient_error();
    refresh_antigravity_status_if_needed(
        &env.ui,
        &target_provider,
        &target_model,
        &env.cred_manager,
    )
    .await;
    refresh_tabs(env, st).await;
    Ok(())
}

/// Open a brand new session tab and switch into it (works even while another session is busy).
pub async fn create_new_session(env: &mut Env<'_>, st: &mut TuiState) -> Result<(), HarnessError> {
    let new_id = resolve_session(env.store.as_ref(), SessionSelection::New).await?;
    let provider = env.current_provider.clone();
    let model = env.current_model.clone();
    let _ = env.store.set_model(&new_id, &provider, &model).await;

    let (
        agent,
        ui,
        in_flight_tools,
        interruptions,
        steer_handle,
        interrupt_handle,
        compacting,
        ctx_max,
    ) = instantiate_session_runtime(env, st, &new_id, &provider, &model, st.reasoning_effort)?;

    park_current_session(env, st);

    *env.session_id = new_id.clone();
    env.agent = agent;
    env.ui = ui;
    env.in_flight_tools = in_flight_tools;
    env.interruptions = interruptions;
    env.steer_handle = steer_handle;
    env.interrupt_handle = interrupt_handle;
    env.compacting = compacting;

    env.todo_scope.set_session(&new_id);
    env.ui.set_todo_panel(None);
    apply_agent_mode(env, st, st.mode);

    st.turns.clear();
    st.turn_meta.clear();
    st.settled_cache.clear();
    st.system_notices.clear();
    st.expanded_thinking.clear();
    st.expanded_tools.clear();
    st.expanded_live_thinking.clear();
    st.expanded_live_tools.clear();
    st.live_thinking_view = LiveThinkingView::Tail;
    st.process_expanded = false;
    st.scroll_from_bottom = 0;
    st.selection = None;
    st.pending_transcript_click = None;
    st.selection_notice = None;
    st.idle_pass = IdleMemoryPass::new(0);
    st.pending_images = Arc::new(Mutex::new(Vec::new()));

    env.ui.set_status(&model, 0, ctx_max);
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

pub async fn open_provider_picker(env: &Env<'_>, st: &mut TuiState) {
    open_provider_picker_with_query(env, st, String::new()).await;
}

pub async fn open_provider_picker_for(env: &Env<'_>, st: &mut TuiState, target: ModelTarget) {
    open_provider_picker_with_target(env, st, target, String::new()).await;
}

pub async fn open_provider_picker_with_query(env: &Env<'_>, st: &mut TuiState, query: String) {
    open_provider_picker_with_target(env, st, ModelTarget::Session, query).await;
}

/// Snapshot of the provider rows for the first `/models` step.
///
/// Kept separate from the modal so it can run on a background task: it reads the credential file
/// and probes the OpenCode daemon/binary, neither of which belongs on the render loop.
pub async fn build_provider_items(
    cred_manager: Arc<harness_providers::CredentialManager>,
    current_provider: &str,
) -> Vec<ProviderPickerItem> {
    let loaded = cred_manager.get_loaded_providers().await;
    let (has_opencode_daemon, has_opencode_cli) = tokio::task::spawn_blocking(|| {
        (
            harness_providers::read_opencode_service_info().is_some(),
            harness_providers::find_opencode_binary().is_ok(),
        )
    })
    .await
    .unwrap_or((false, false));

    let agy_creds = cred_manager.get_credentials("antigravity").await;
    let agy_primary = agy_creds
        .iter()
        .find(|c| c.is_priority && !c.is_temporarily_disabled)
        .or_else(|| agy_creds.iter().find(|c| !c.is_temporarily_disabled));
    let antigravity_account: Option<String> = agy_primary.and_then(|primary| {
        if !primary.label.is_empty() {
            return Some(primary.label.clone());
        }
        serde_json::from_str::<serde_json::Value>(&primary.api_key)
            .ok()
            .and_then(|val| {
                val.get("email")
                    .and_then(|e| e.as_str())
                    .map(str::to_string)
            })
            .filter(|email| !email.is_empty())
    });

    let catalog = harness_providers::get_provider_catalog();
    let configured: Vec<_> = catalog
        .iter()
        .filter(|p| {
            loaded.iter().any(|l| l.eq_ignore_ascii_case(&p.id))
                || p.id.eq_ignore_ascii_case(current_provider)
                || (p.id == "opencode" && has_opencode_daemon)
                || (p.id == "opencode-cli" && has_opencode_cli)
        })
        .cloned()
        .collect();
    let available = if configured.is_empty() {
        catalog
    } else {
        configured
    };

    let mut items = Vec::with_capacity(available.len());
    for p in available {
        let is_current = p.id.eq_ignore_ascii_case(current_provider);
        let mut tags = Vec::new();
        if is_current {
            tags.push("active".to_string());
        }
        if loaded.iter().any(|l| l.eq_ignore_ascii_case(&p.id)) {
            tags.push("configured".to_string());
        }
        if p.id.eq_ignore_ascii_case("opencode") && has_opencode_daemon {
            tags.push("daemon".to_string());
        } else if p.id.eq_ignore_ascii_case("opencode-cli") && has_opencode_cli {
            tags.push("cli".to_string());
        } else if p.id.eq_ignore_ascii_case("antigravity") || p.id.eq_ignore_ascii_case("agy") {
            if let Some(account) = antigravity_account.clone() {
                tags.push(account);
            }
        }
        items.push(ProviderPickerItem {
            id: p.id,
            display_name: p.display_name,
            tag: tags.join(" · "),
            is_current,
        });
    }
    items
}

/// Open the provider picker immediately from the cached rows, then refresh them off-loop.
pub async fn open_provider_picker_with_target(
    env: &Env<'_>,
    st: &mut TuiState,
    target: ModelTarget,
    query: String,
) {
    let items = st.provider_items.clone();
    let selected = items.iter().position(|i| i.is_current).unwrap_or(0);
    st.picker_loading = None;
    st.modal = ActiveModal::ProviderPicker {
        target,
        query,
        items,
        selected,
    };
    super::trace_event("provider picker shown");

    let tx = st.picker_tx.clone();
    let cred_manager = env.cred_manager.clone();
    let current_provider = env.current_provider.clone();
    tokio::spawn(async move {
        let items = build_provider_items(cred_manager, &current_provider).await;
        let _ = tx.send(super::state::PickerLoad::Providers(items));
    });
}

pub async fn open_models_for_provider(
    env: &Env<'_>,
    st: &mut TuiState,
    provider: &str,
    query: String,
) {
    open_models_for_target(env, st, ModelTarget::Session, provider, query).await;
}

/// Load the model rows for one provider. Pure data, so it can run off the render loop.
pub async fn load_model_items(
    discovery: &harness_providers::ModelDiscovery,
    cred_manager: &harness_providers::CredentialManager,
    provider: &str,
    current_provider: &str,
    current_model: &str,
) -> Vec<ModelPickerItem> {
    let mut models = discovery
        .get_models(provider, cred_manager, false)
        .await
        .unwrap_or_default();
    let def = harness_providers::catalog::get_default_model(provider).to_string();
    if !def.is_empty() && !models.contains(&def) {
        models.insert(0, def);
    }
    if provider.eq_ignore_ascii_case(current_provider) && !models.iter().any(|m| m == current_model)
    {
        models.insert(0, current_model.to_string());
    }

    models
        .into_iter()
        .map(|m| {
            let is_current = provider.eq_ignore_ascii_case(current_provider) && m == current_model;
            ModelPickerItem {
                display_name: friendly_model_name(&m),
                provider: provider.to_string(),
                model: m,
                is_current,
            }
        })
        .collect()
}

/// Open the model picker on the first keystroke and fill it in when discovery answers.
pub async fn open_models_for_target(
    env: &Env<'_>,
    st: &mut TuiState,
    target: ModelTarget,
    provider: &str,
    query: String,
) {
    let provider_owned = provider.to_string();
    st.picker_loading = Some(provider_owned.clone());
    st.modal = ActiveModal::ModelPicker {
        target,
        provider: provider_owned.clone(),
        provider_display: friendly_provider_name(provider),
        query,
        items: Vec::new(),
        selected: 0,
    };

    let tx = st.picker_tx.clone();
    let discovery = env.discovery.clone();
    let cred_manager = env.cred_manager.clone();
    let current_provider = env.current_provider.clone();
    let current_model = env.current_model.clone();
    tokio::spawn(async move {
        let items = load_model_items(
            discovery.as_ref(),
            cred_manager.as_ref(),
            &provider_owned,
            &current_provider,
            &current_model,
        )
        .await;
        let _ = tx.send(super::state::PickerLoad::Models {
            provider: provider_owned,
            items,
        });
    });
}

/// Fold a background picker load into the modal that asked for it, preserving the user's query.
pub fn apply_picker_load(st: &mut TuiState, load: super::state::PickerLoad) {
    use super::state::PickerLoad;

    match load {
        PickerLoad::Providers(items) => {
            st.provider_items = items.clone();
            if let ActiveModal::ProviderPicker {
                items: current,
                selected,
                ..
            } = &mut st.modal
            {
                let keep = *selected;
                *current = items;
                *selected = keep.min(current.len().saturating_sub(1));
            }
        }
        PickerLoad::Models { provider, items } => {
            if st.picker_loading.as_deref() == Some(provider.as_str()) {
                st.picker_loading = None;
            }
            if let ActiveModal::ModelPicker {
                provider: open_provider,
                items: current,
                selected,
                ..
            } = &mut st.modal
            {
                if open_provider == &provider {
                    let keep = *selected;
                    *current = items;
                    *selected = if keep == 0 {
                        current.iter().position(|i| i.is_current).unwrap_or(0)
                    } else {
                        keep.min(current.len().saturating_sub(1))
                    };
                }
            }
        }
    }
}

pub async fn open_thinking_picker(_env: &Env<'_>, st: &mut TuiState) {
    let cur = st.reasoning_effort;
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
    if st.active_turn.is_some() {
        env.ui.set_transient_error(Some(
            "Wait for or interrupt the active turn in this session before changing models.".into(),
        ));
        return Ok(());
    }
    let client = Arc::new(GenericProviderClient::new(
        provider.to_string(),
        model.to_string(),
        env.cred_manager.clone(),
        env.discovery.clone(),
        env.config.providers.get(provider),
    )?);
    let ctx_max = context_window(env.config, env.discovery.as_ref(), provider, model);
    let Ok(mut ag) = env.agent.try_lock() else {
        env.ui.set_transient_error(Some(
            "Session agent is busy; wait for the turn to finish before changing models.".into(),
        ));
        return Ok(());
    };
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
    let used = ag.estimate_session_tokens().await.unwrap_or(0);
    drop(ag);
    *env.current_provider = provider.to_string();
    *env.current_model = model.to_string();
    env.ui.set_status(model, used, ctx_max);
    refresh_antigravity_status_if_needed(&env.ui, provider, model, &env.cred_manager).await;
    if persist {
        let _ = env.store.set_model(env.session_id, provider, model).await;
    }
    st.scroll_from_bottom = 0;
    Ok(())
}

/// Apply a provider → model choice to the slot the picker was opened for.
pub async fn apply_model_target(
    env: &mut Env<'_>,
    st: &mut TuiState,
    target: ModelTarget,
    provider: &str,
    model: &str,
) -> Result<(), HarnessError> {
    if st.active_turn.is_some() && !matches!(target, ModelTarget::Worker(_)) {
        env.ui.set_transient_error(Some(
            "Wait for or interrupt the active turn in this session before changing models.".into(),
        ));
        return Ok(());
    }

    match target {
        ModelTarget::Session => apply_model_selection(env, st, provider, model, true).await,
        ModelTarget::Default { global } => match global {
            Some(is_global) => apply_default_target(env, st, provider, model, is_global).await,
            None => {
                open_model_scope(
                    env,
                    st,
                    ModelTarget::Default { global: None },
                    provider,
                    model,
                );
                Ok(())
            }
        },
        ModelTarget::Compact { global } => match global {
            Some(is_global) => apply_compact_target(env, st, provider, model, is_global).await,
            None => {
                open_model_scope(
                    env,
                    st,
                    ModelTarget::Compact { global: None },
                    provider,
                    model,
                );
                Ok(())
            }
        },
        ModelTarget::BgSaver => {
            crate::commands::chat::commands::apply_bgsaver_model_choice(
                &env.ui,
                env.config,
                &env.cred_manager,
                &env.discovery,
                &env.agent,
                env.feature_toggles,
                env.current_provider,
                env.current_model,
                provider.to_string(),
                model.to_string(),
            )
            .await
        }
        ModelTarget::Worker(name) => apply_worker_target(env, st, &name, provider, model),
    }
    .map(|_| ())
}

async fn apply_default_target(
    env: &mut Env<'_>,
    _st: &mut TuiState,
    provider: &str,
    model: &str,
    is_global: bool,
) -> Result<(), HarnessError> {
    crate::commands::chat::commands::apply_default_model_choice(
        &env.ui,
        env.config,
        &env.cred_manager,
        &env.discovery,
        &env.agent,
        &env.store,
        env.session_id,
        env.current_provider,
        env.current_model,
        env.compact_model,
        provider.to_string(),
        model.to_string(),
        is_global,
    )
    .await
}

async fn apply_compact_target(
    env: &mut Env<'_>,
    _st: &mut TuiState,
    provider: &str,
    model: &str,
    is_global: bool,
) -> Result<(), HarnessError> {
    crate::commands::chat::commands::apply_compact_model_choice(
        &env.ui,
        env.config,
        &env.cred_manager,
        &env.discovery,
        &env.agent,
        &env.store,
        env.session_id,
        env.compact_provider,
        env.compact_model,
        provider.to_string(),
        model.to_string(),
        is_global,
    )
    .await
}

fn open_model_scope(
    _env: &mut Env<'_>,
    st: &mut TuiState,
    target: ModelTarget,
    provider: &str,
    model: &str,
) {
    st.modal = ActiveModal::SelectModal {
        title: "Apply model to".to_string(),
        items: vec![
            "This session only".to_string(),
            "Globally (save as default for all sessions)".to_string(),
        ],
        selected: 0,
        action: SelectAction::ModelScope {
            target,
            provider: provider.to_string(),
            model: model.to_string(),
        },
    };
}

fn apply_worker_target(
    env: &mut Env<'_>,
    st: &mut TuiState,
    name: &str,
    provider: &str,
    model: &str,
) -> Result<(), HarnessError> {
    let profile = harness_core::WorkerProfile {
        provider: provider.to_string(),
        model: model.to_string(),
        reasoning_effort: env
            .config
            .workers
            .get(name)
            .and_then(|w| w.reasoning_effort),
    };
    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    harness_core::config::set_worker_in_file(&layers, name, &profile)?;
    env.config.workers.insert(name.to_string(), profile);
    st.system_notices.push(format!(
        "✔ Worker /{name} → {model} ({provider}) · invoke with /{name} <task>"
    ));
    Ok(())
}

/// Put plain text into a scrollable TUI modal. ANSI styling is stripped because ratatui cells
/// render escape sequences literally.
fn show_text_modal(st: &mut TuiState, title: &str, body: &str) {
    let cleaned = dialoguer::console::strip_ansi_codes(body).to_string();
    let lines: Vec<String> = if cleaned.trim().is_empty() {
        vec!["(empty)".to_string()]
    } else {
        cleaned.lines().map(str::to_string).collect()
    };
    st.modal = ActiveModal::TextModal {
        title: title.to_string(),
        lines,
        scroll: 0,
    };
}

fn open_settings_menu(st: &mut TuiState, config: &harness_core::Config) {
    let limit = match config.display.block_limit {
        None => "uncapped".to_string(),
        Some(1) => "1 line".to_string(),
        Some(n) => format!("{n} lines"),
    };
    let collapse = if config.display.auto_collapse {
        "on"
    } else {
        "off"
    };
    let thinking = if config.display.auto_hide_thinking {
        "on"
    } else {
        "off"
    };
    st.modal = ActiveModal::SelectModal {
        title: "CLI Settings".to_string(),
        items: vec![
            format!("Display block limit · {limit}"),
            format!("Auto-collapse on turn finish · {collapse}"),
            format!("Auto-collapse thinking while a tool runs · {thinking}"),
            "View current settings summary".to_string(),
        ],
        selected: 0,
        action: SelectAction::SettingsMain,
    };
}

fn open_dir_input(st: &mut TuiState, scope: Option<bool>) {
    st.modal = ActiveModal::InputModal {
        title: "Change Workspace".to_string(),
        prompt: "Path to use as the workspace root (blank cancels)".to_string(),
        value: String::new(),
        secret: false,
        action: InputAction::DirPath { scope },
    };
}

fn open_dir_scope(st: &mut TuiState, path: String) {
    st.modal = ActiveModal::SelectModal {
        title: "Workspace Scope".to_string(),
        items: vec![
            "This session only".to_string(),
            "Globally (persist for all sessions)".to_string(),
        ],
        selected: 0,
        action: SelectAction::DirScope { path },
    };
}

fn open_connect_menu(st: &mut TuiState) {
    const ITEMS: [&str; 10] = [
        "Add provider API key",
        "List connected providers",
        "Set priority credential",
        "Remove a credential",
        "Import Antigravity / Google",
        "Import OpenCode / Shiina",
        "Import Codex / ChatGPT",
        "Set rotation strategy",
        "Reset cooldowns",
        "Back to chat",
    ];
    st.modal = ActiveModal::SelectModal {
        title: "Connect a provider".to_string(),
        items: ITEMS.iter().map(|item| item.to_string()).collect(),
        selected: 0,
        action: SelectAction::ConnectMenu,
    };
}

async fn open_connect_add_provider(env: &Env<'_>, st: &mut TuiState) {
    let providers = provider_items(&env.cred_manager).await;
    let ids: Vec<String> = providers
        .iter()
        .map(|(label, _)| provider_id_of(label).to_string())
        .collect();
    st.modal = ActiveModal::SelectModal {
        title: "Select provider".to_string(),
        items: providers.into_iter().map(|(label, _)| label).collect(),
        selected: 0,
        action: SelectAction::ConnectAddProvider { ids },
    };
}

async fn open_connect_remove_provider(env: &Env<'_>, st: &mut TuiState) {
    let ids = connected_providers(&env.cred_manager).await;
    if ids.is_empty() {
        env.ui
            .emit_above("Nothing to remove — no providers connected");
        return;
    }
    st.modal = ActiveModal::SelectModal {
        title: "Provider to remove from".to_string(),
        items: ids.clone(),
        selected: 0,
        action: SelectAction::ConnectRemoveProvider { ids },
    };
}

async fn open_connect_remove_credential(env: &Env<'_>, st: &mut TuiState, provider: String) {
    let creds = env.cred_manager.get_credentials(&provider).await;
    if creds.is_empty() {
        env.ui
            .emit_above(&format!("No credentials found for {provider}"));
        open_connect_menu(st);
        return;
    }
    let ids: Vec<String> = creds.iter().map(|cred| cred.id.clone()).collect();
    let items: Vec<String> = creds
        .iter()
        .map(|cred| format!("{} (used {})", cred.label, cred.usage_count))
        .collect();
    st.modal = ActiveModal::SelectModal {
        title: format!("Credential of {provider}"),
        items,
        selected: 0,
        action: SelectAction::ConnectRemoveCredential { provider, ids },
    };
}

async fn open_connect_priority_provider(env: &Env<'_>, st: &mut TuiState) {
    let ids = connected_providers(&env.cred_manager).await;
    if ids.is_empty() {
        env.ui
            .emit_above("Nothing to prioritize — no providers connected");
        return;
    }
    if ids.len() == 1 {
        let provider = ids[0].clone();
        open_connect_priority_credential(env, st, provider).await;
        return;
    }
    st.modal = ActiveModal::SelectModal {
        title: "Provider to set priority for".to_string(),
        items: ids.clone(),
        selected: 0,
        action: SelectAction::ConnectPriorityProvider { ids },
    };
}

async fn open_connect_priority_credential(env: &Env<'_>, st: &mut TuiState, provider: String) {
    let creds = env.cred_manager.get_credentials(&provider).await;
    if creds.is_empty() {
        env.ui
            .emit_above(&format!("No credentials found for {provider}"));
        open_connect_menu(st);
        return;
    }
    let mut selected = 0;
    let ids: Vec<String> = creds.iter().map(|cred| cred.id.clone()).collect();
    let items: Vec<String> = creds
        .iter()
        .enumerate()
        .map(|(index, cred)| {
            if cred.is_priority {
                selected = index;
            }
            let mut tags = Vec::new();
            if cred.is_priority {
                tags.push("current priority");
            }
            if cred.is_temporarily_disabled {
                tags.push("cooldown");
            }
            let tag_str = if tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", tags.join(", "))
            };
            format!("{} (used {}){}", cred.label, cred.usage_count, tag_str)
        })
        .collect();
    st.modal = ActiveModal::SelectModal {
        title: format!("Credential of {provider}"),
        items,
        selected,
        action: SelectAction::ConnectPriorityCredential { provider, ids },
    };
}

async fn open_connect_strategy_provider(env: &Env<'_>, st: &mut TuiState) {
    let ids = connected_providers(&env.cred_manager).await;
    if ids.is_empty() {
        env.ui.emit_above("No providers connected yet");
        return;
    }
    st.modal = ActiveModal::SelectModal {
        title: "Provider to configure".to_string(),
        items: ids.clone(),
        selected: 0,
        action: SelectAction::ConnectStrategyProvider { ids },
    };
}

fn open_connect_strategy(st: &mut TuiState, provider: String) {
    st.modal = ActiveModal::SelectModal {
        title: format!("Strategy for {provider}"),
        items: connect_strategies()
            .iter()
            .map(|(label, _)| label.to_string())
            .collect(),
        selected: 0,
        action: SelectAction::ConnectStrategy { provider },
    };
}

fn connect_strategies() -> [(&'static str, RotationStrategy); 5] {
    [
        (
            "round_robin — cycle through keys evenly",
            RotationStrategy::RoundRobin,
        ),
        (
            "fill_first — use the first key until it fails",
            RotationStrategy::FillFirst,
        ),
        (
            "least_used — always pick the least-used key",
            RotationStrategy::LeastUsed,
        ),
        ("random — pick a key at random", RotationStrategy::Random),
        (
            "never — never rotate, only use priority key",
            RotationStrategy::Never,
        ),
    ]
}

async fn open_connect_reset_cooldowns(env: &Env<'_>, st: &mut TuiState) {
    let ids = connected_providers(&env.cred_manager).await;
    let mut items = vec!["All providers".to_string()];
    items.extend(ids.iter().cloned());
    st.modal = ActiveModal::SelectModal {
        title: "Reset cooldowns for".to_string(),
        items,
        selected: 0,
        action: SelectAction::ConnectResetCooldowns { ids },
    };
}

async fn open_task_list(env: &Env<'_>, st: &mut TuiState) {
    let tasks = super::super::viewer::collect_tasks(
        Some(env.bg_queue.as_ref()),
        Some(env.delegate_tool.as_ref()),
    )
    .await;
    if tasks.is_empty() {
        env.ui
            .emit_above("No background tasks or subagents active or recorded");
        return;
    }
    let items: Vec<String> = tasks
        .iter()
        .map(|task| {
            let kind = if task.command.starts_with("worker:") {
                "worker"
            } else if task.id.starts_with("sub_") || task.command.starts_with("subagent:") {
                "subagent"
            } else {
                "job"
            };
            let subject = task
                .command
                .strip_prefix("worker: ")
                .or_else(|| task.command.strip_prefix("subagent: "))
                .unwrap_or(&task.command);
            format!(
                "{} · {}s · {} · {}",
                task.status, task.elapsed_secs, kind, subject
            )
        })
        .collect();
    let pairs: Vec<(String, String)> = tasks
        .into_iter()
        .map(|task| (task.id, task.log_path))
        .collect();
    st.modal = ActiveModal::SelectModal {
        title: "Background Tasks".to_string(),
        items,
        selected: 0,
        action: SelectAction::TaskList { tasks: pairs },
    };
}

async fn open_auto_compact_threshold(env: &Env<'_>, st: &mut TuiState) {
    let (_, threshold, _) = env.agent.lock().await.auto_compact_settings();
    st.modal = ActiveModal::InputModal {
        title: "Auto-compact — Threshold".to_string(),
        prompt: "Token threshold, e.g. 200k. Blank keeps the default".to_string(),
        value: threshold.to_string(),
        secret: false,
        action: InputAction::AutoCompactThreshold,
    };
}

fn open_auto_compact_percent(st: &mut TuiState, threshold: usize, current: usize) {
    st.modal = ActiveModal::InputModal {
        title: "Auto-compact — Percentage".to_string(),
        prompt: "Percent of context to compact, 1–100".to_string(),
        value: current.to_string(),
        secret: false,
        action: InputAction::AutoCompactPercent { threshold },
    };
}

fn open_profile_create_name(st: &mut TuiState) {
    st.modal = ActiveModal::InputModal {
        title: "Create Profile — Name".to_string(),
        prompt: "Lowercase letters, digits and '-'".to_string(),
        value: String::new(),
        secret: false,
        action: InputAction::ProfileCreateName,
    };
}

fn open_profile_create_summary(st: &mut TuiState, name: String) {
    st.modal = ActiveModal::InputModal {
        title: "Create Profile — Summary".to_string(),
        prompt: "One line describing what this profile is for".to_string(),
        value: "Custom profile".to_string(),
        secret: false,
        action: InputAction::ProfileCreateSummary { name },
    };
}

fn open_profile_create_providers(st: &mut TuiState, name: String, summary: String) {
    st.modal = ActiveModal::SelectModal {
        title: "Create Profile — Providers".to_string(),
        items: vec![
            "Yes, copy the default profile's credentials".to_string(),
            "No, start with none".to_string(),
        ],
        selected: 0,
        action: SelectAction::ProfileCreateProviders { name, summary },
    };
}

/// Execute a choice from one of the generic [`ActiveModal::SelectModal`] menus.
pub async fn run_select_action(
    env: &mut Env<'_>,
    st: &mut TuiState,
    action: SelectAction,
    choice: usize,
) {
    match action {
        SelectAction::SettingsMain => match choice {
            0 => {
                let items: Vec<String> =
                    std::iter::once("Uncapped / Display everything (default)".to_string())
                        .chain((1..=10).map(|n| {
                            if n == 1 {
                                "1 line".to_string()
                            } else {
                                format!("{n} lines")
                            }
                        }))
                        .collect();
                let selected = env.config.display.block_limit.unwrap_or(0);
                st.modal = ActiveModal::SelectModal {
                    title: "Select Display Block Limit".to_string(),
                    items,
                    selected,
                    action: SelectAction::SettingsBlockLimit,
                };
            }
            1 => {
                let enabled = env.config.display.auto_collapse;
                st.modal = ActiveModal::SelectModal {
                    title: "Auto-collapse on Turn Completion".to_string(),
                    items: vec![
                        "Enabled (collapse when finished)".to_string(),
                        "Disabled (keep expanded)".to_string(),
                    ],
                    selected: if enabled { 0 } else { 1 },
                    action: SelectAction::SettingsCollapse,
                };
            }
            2 => {
                let enabled = env.config.display.auto_hide_thinking;
                st.modal = ActiveModal::SelectModal {
                    title: "Auto-collapse Thinking During Tool Calls".to_string(),
                    items: vec![
                        "Enabled (hide thinking while a tool runs)".to_string(),
                        "Disabled (keep thinking visible)".to_string(),
                    ],
                    selected: if enabled { 0 } else { 1 },
                    action: SelectAction::SettingsThinking,
                };
            }
            _ => {
                if let Err(error) = handle_settings_command(
                    None,
                    &env.ui,
                    env.config,
                    &env.display_settings,
                    &mut st.process_expanded,
                    Some("show"),
                )
                .await
                {
                    env.ui
                        .set_transient_error(Some(format!("Settings failed: {error}")));
                }
            }
        },
        SelectAction::SettingsBlockLimit => {
            let arg = if choice == 0 {
                "limit all".to_string()
            } else {
                format!("limit {choice}")
            };
            if let Err(error) = handle_settings_command(
                None,
                &env.ui,
                env.config,
                &env.display_settings,
                &mut st.process_expanded,
                Some(&arg),
            )
            .await
            {
                env.ui
                    .set_transient_error(Some(format!("Settings failed: {error}")));
            }
        }
        SelectAction::SettingsCollapse => {
            let arg = if choice == 0 {
                "collapse on"
            } else {
                "collapse off"
            };
            if let Err(error) = handle_settings_command(
                None,
                &env.ui,
                env.config,
                &env.display_settings,
                &mut st.process_expanded,
                Some(arg),
            )
            .await
            {
                env.ui
                    .set_transient_error(Some(format!("Settings failed: {error}")));
            }
        }
        SelectAction::SettingsThinking => {
            let arg = if choice == 0 {
                "thinking on"
            } else {
                "thinking off"
            };
            if let Err(error) = handle_settings_command(
                None,
                &env.ui,
                env.config,
                &env.display_settings,
                &mut st.process_expanded,
                Some(arg),
            )
            .await
            {
                env.ui
                    .set_transient_error(Some(format!("Settings failed: {error}")));
            }
            st.auto_hide_thinking = env.config.display.auto_hide_thinking;
        }
        SelectAction::DirScope { path } => {
            let is_global = choice == 1;
            if let Err(error) =
                apply_dir_choice(&env.ui, env.config, &env.agent, &path, is_global).await
            {
                env.ui
                    .set_transient_error(Some(format!("Workspace change failed: {error}")));
            }
        }
        SelectAction::ModelScope {
            target,
            provider,
            model,
        } => {
            let is_global = choice == 1;
            let result = match target {
                ModelTarget::Default { .. } => {
                    apply_default_target(env, st, &provider, &model, is_global).await
                }
                ModelTarget::Compact { .. } => {
                    apply_compact_target(env, st, &provider, &model, is_global).await
                }
                _ => Ok(()),
            };
            if let Err(error) = result {
                env.ui
                    .set_transient_error(Some(format!("Model switch failed: {error}")));
            }
        }
        SelectAction::ConnectMenu => match choice {
            0 => open_connect_add_provider(env, st).await,
            1 => {
                let text = list_keys_text(&env.cred_manager).await;
                show_text_modal(st, "Connected Providers", &text);
            }
            2 => open_connect_priority_provider(env, st).await,
            3 => open_connect_remove_provider(env, st).await,
            4 => {
                let result =
                    super::super::connect::import_antigravity(&env.ui, &env.cred_manager).await;
                finish_connect_action(env, st, result).await;
            }
            5 => {
                let result =
                    super::super::connect::import_opencode(&env.ui, &env.cred_manager).await;
                finish_connect_action(env, st, result).await;
            }
            6 => {
                let result = super::super::connect::import_codex(&env.ui, &env.cred_manager).await;
                finish_connect_action(env, st, result).await;
            }
            7 => open_connect_strategy_provider(env, st).await,
            8 => open_connect_reset_cooldowns(env, st).await,
            _ => {}
        },
        SelectAction::ConnectAddProvider { ids } => {
            if let Some(provider) = ids.get(choice) {
                st.modal = ActiveModal::InputModal {
                    title: "Add Provider — Label".to_string(),
                    prompt: "Credential label (blank uses '<provider>-primary')".to_string(),
                    value: String::new(),
                    secret: false,
                    action: InputAction::ApiKeyLabel {
                        provider: provider.clone(),
                    },
                };
            }
        }
        SelectAction::ConnectRemoveProvider { ids } => {
            if let Some(provider) = ids.get(choice) {
                open_connect_remove_credential(env, st, provider.clone()).await;
            }
        }
        SelectAction::ConnectRemoveCredential { provider, ids } => {
            let Some(id) = ids.get(choice) else {
                return;
            };
            match env.cred_manager.remove_credential(&provider, id).await {
                Ok(true) => env
                    .ui
                    .emit_above(&format!("✔ Credential removed from {provider}")),
                Ok(false) => env
                    .ui
                    .set_transient_error(Some("Credential not found".to_string())),
                Err(error) => env
                    .ui
                    .set_transient_error(Some(format!("Remove failed: {error}"))),
            }
            open_connect_menu(st);
        }
        SelectAction::ConnectPriorityProvider { ids } => {
            if let Some(provider) = ids.get(choice) {
                open_connect_priority_credential(env, st, provider.clone()).await;
            }
        }
        SelectAction::ConnectPriorityCredential { provider, ids } => {
            let Some(id) = ids.get(choice) else {
                return;
            };
            match env
                .cred_manager
                .set_priority_credential(&provider, id)
                .await
            {
                Ok(true) => {
                    let current = env.cred_manager.get_rotation_strategy(&provider).await;
                    if current != RotationStrategy::FillFirst && current != RotationStrategy::Never
                    {
                        let _ = env
                            .cred_manager
                            .set_rotation_strategy(&provider, RotationStrategy::FillFirst)
                            .await;
                    }
                    env.ui
                        .emit_above(&format!("✔ Priority credential set for {provider}"));
                }
                Ok(false) => env
                    .ui
                    .set_transient_error(Some("Failed to set priority credential".to_string())),
                Err(error) => env
                    .ui
                    .set_transient_error(Some(format!("Priority failed: {error}"))),
            }
            open_connect_menu(st);
        }
        SelectAction::ConnectStrategyProvider { ids } => {
            if let Some(provider) = ids.get(choice) {
                open_connect_strategy(st, provider.clone());
            }
        }
        SelectAction::ConnectStrategy { provider } => {
            let strategies = connect_strategies();
            let Some((label, strategy)) = strategies.get(choice) else {
                return;
            };
            match env
                .cred_manager
                .set_rotation_strategy(&provider, *strategy)
                .await
            {
                Ok(()) => {
                    let name = label.split(' ').next().unwrap_or("");
                    env.ui
                        .emit_above(&format!("✔ Rotation strategy for {provider} set to {name}"));
                }
                Err(error) => env
                    .ui
                    .set_transient_error(Some(format!("Strategy failed: {error}"))),
            }
            open_connect_menu(st);
        }
        SelectAction::ConnectResetCooldowns { ids } => {
            let target = if choice == 0 {
                None
            } else {
                let Some(provider) = ids.get(choice - 1) else {
                    return;
                };
                Some(provider.clone())
            };
            match env.cred_manager.reset_cooldowns(target.as_deref()).await {
                Ok(count) => env
                    .ui
                    .emit_above(&format!("✔ {count} credential(s) re-enabled")),
                Err(error) => env
                    .ui
                    .set_transient_error(Some(format!("Reset failed: {error}"))),
            }
            open_connect_menu(st);
        }
        SelectAction::ProfileDeleteConfirm { name } => {
            if choice == 0 {
                match crate::commands::profile::delete_profile(
                    &name,
                    &env.config.paths.profiles_dir,
                ) {
                    Ok(()) => env.ui.emit_above(&format!("✔ Deleted {name}")),
                    Err(error) => env
                        .ui
                        .set_transient_error(Some(format!("Delete failed: {error}"))),
                }
            } else {
                env.ui.emit_above("Cancelled.");
            }
        }
        SelectAction::ProfileCreateProviders { name, summary } => {
            let copy_providers = choice == 0;
            match create_profile(env, &name, summary, copy_providers) {
                Ok(dest) => {
                    env.ui.emit_above(&format!("✔ Created {}", dest.display()));
                    env.ui.emit_above(&format!(
                        "Open the new profile with `luna {name}` or `{name}`."
                    ));
                }
                Err(error) => env
                    .ui
                    .set_transient_error(Some(format!("Create failed: {error}"))),
            }
        }
        SelectAction::TaskList { tasks } => {
            let Some((id, log_path)) = tasks.get(choice) else {
                return;
            };
            let body = std::fs::read_to_string(log_path)
                .unwrap_or_else(|_| format!("No log yet for {id}."));
            show_text_modal(st, &format!("Task {id}"), &body);
        }
    }
}

async fn finish_connect_action(
    env: &mut Env<'_>,
    st: &mut TuiState,
    result: Result<(), HarnessError>,
) {
    if let Err(error) = result {
        env.ui
            .set_transient_error(Some(format!("Provider setup failed: {error}")));
    }
    refresh_antigravity_status_if_needed(
        &env.ui,
        env.current_provider,
        env.current_model,
        &env.cred_manager,
    )
    .await;
    open_connect_menu(st);
}

fn create_profile(
    env: &Env<'_>,
    name: &str,
    summary: String,
    copy_providers: bool,
) -> Result<std::path::PathBuf, HarnessError> {
    let seed = harness_core::profiles::default_profile()?;
    crate::commands::profile::create_profile_with(
        &env.config.paths.profiles_dir,
        env.config,
        name,
        summary,
        seed.persona,
        seed.behavior,
        copy_providers,
        &[],
        &[],
    )
}

/// Execute a value submitted from an [`ActiveModal::InputModal`].
pub async fn run_input_action(
    env: &mut Env<'_>,
    st: &mut TuiState,
    action: InputAction,
    value: String,
) {
    match action {
        InputAction::DirPath { scope } => {
            let path = value.trim();
            if path.is_empty() {
                env.ui.emit_above("Workspace unchanged");
                return;
            }
            let expanded = harness_core::config::expand_tilde(path);
            if !std::path::PathBuf::from(expanded).is_dir() {
                env.ui
                    .set_transient_error(Some(format!("Directory not found: {path}")));
                open_dir_input(st, scope);
                return;
            }
            match scope {
                Some(is_global) => {
                    if let Err(error) =
                        apply_dir_choice(&env.ui, env.config, &env.agent, path, is_global).await
                    {
                        env.ui
                            .set_transient_error(Some(format!("Workspace change failed: {error}")));
                    }
                }
                None => open_dir_scope(st, path.to_string()),
            }
        }
        InputAction::ApiKeyLabel { provider } => {
            let label = if value.trim().is_empty() {
                format!("{provider}-primary")
            } else {
                value.trim().to_string()
            };
            st.modal = ActiveModal::InputModal {
                title: "Add Provider — API Key".to_string(),
                prompt: format!("API key for {provider} (masked)"),
                value: String::new(),
                secret: true,
                action: InputAction::ApiKeySecret { provider, label },
            };
        }
        InputAction::ApiKeySecret { provider, label } => {
            if value.is_empty() {
                env.ui.emit_above("No key entered — nothing added");
                open_connect_menu(st);
                return;
            }
            match env
                .cred_manager
                .add_credential(&provider, &label, &value, true)
                .await
            {
                Ok(id) => env.ui.emit_above(&format!(
                    "✔ Credential '{label}' added for {provider} ({id})"
                )),
                Err(error) => env
                    .ui
                    .set_transient_error(Some(format!("Add credential failed: {error}"))),
            }
            refresh_antigravity_status_if_needed(
                &env.ui,
                env.current_provider,
                env.current_model,
                &env.cred_manager,
            )
            .await;
            open_connect_menu(st);
        }
        InputAction::AutoCompactThreshold => {
            match harness_onboarding::compact_step::parse_threshold(&value) {
                Ok(threshold) => {
                    let (_, _, current_percent) = env.agent.lock().await.auto_compact_settings();
                    open_auto_compact_percent(st, threshold, current_percent);
                }
                Err(message) => {
                    let typed = value;
                    env.ui.set_transient_error(Some(message.clone()));
                    st.modal = ActiveModal::InputModal {
                        title: "Auto-compact — Threshold".to_string(),
                        prompt: format!("{message}. Blank keeps the default"),
                        value: typed,
                        secret: false,
                        action: InputAction::AutoCompactThreshold,
                    };
                }
            }
        }
        InputAction::AutoCompactPercent { threshold } => {
            match harness_onboarding::compact_step::parse_percent(&value) {
                Ok(percent) => {
                    if let Err(error) =
                        apply_auto_compact_values(&env.ui, &env.agent, threshold, percent).await
                    {
                        env.ui.set_transient_error(Some(format!(
                            "Auto-compact update failed: {error}"
                        )));
                    }
                }
                Err(message) => {
                    let typed = value;
                    env.ui.set_transient_error(Some(message.clone()));
                    st.modal = ActiveModal::InputModal {
                        title: "Auto-compact — Percentage".to_string(),
                        prompt: format!("{message}. Enter 1–100"),
                        value: typed,
                        secret: false,
                        action: InputAction::AutoCompactPercent { threshold },
                    };
                }
            }
        }
        InputAction::ProfileCreateName => {
            let name = value.trim().to_lowercase();
            if let Err(error) = harness_core::profiles::validate_name(&name) {
                env.ui
                    .set_transient_error(Some(format!("Invalid name: {error}")));
                st.modal = ActiveModal::InputModal {
                    title: "Create Profile — Name".to_string(),
                    prompt: "Lowercase letters, digits and '-'".to_string(),
                    value,
                    secret: false,
                    action: InputAction::ProfileCreateName,
                };
                return;
            }
            open_profile_create_summary(st, name);
        }
        InputAction::ProfileCreateSummary { name } => {
            let summary = if value.trim().is_empty() {
                "Custom profile".to_string()
            } else {
                value.trim().to_string()
            };
            open_profile_create_providers(st, name, summary);
        }
    }
}

pub async fn apply_thinking_selection(
    env: &Env<'_>,
    st: &mut TuiState,
    effort: Option<ReasoningEffort>,
) {
    if st.active_turn.is_some() {
        env.ui.set_transient_error(Some(
            "Wait for or interrupt the active turn in this session before changing thinking level."
                .into(),
        ));
        return;
    }
    if let Ok(mut ag) = env.agent.try_lock() {
        ag.set_reasoning_effort(effort);
        st.reasoning_effort = effort;
    }
}

pub fn set_all_process_expanded(st: &mut TuiState, expanded: bool) {
    st.process_expanded = expanded;
    if expanded {
        st.expanded_thinking.clear();
        st.expanded_live_thinking.clear();
        st.live_thinking_view = LiveThinkingView::Expanded;
    } else {
        st.expanded_thinking.clear();
        st.expanded_tools.clear();
        st.expanded_live_thinking.clear();
        st.expanded_live_tools.clear();
        st.live_thinking_view = LiveThinkingView::Tail;
    }
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
            open_provider_picker(env, st).await;
        }
        PaletteAction::CycleAgentMode => {
            apply_agent_mode(env, st, st.mode.cycle());
        }
        PaletteAction::SelectThinking => {
            open_thinking_picker(env, st).await;
        }
        PaletteAction::ToggleProcessDetails => {
            set_all_process_expanded(st, !st.process_expanded);
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
    let submitted = st.composer.take_for_submit();
    sync_slash_popup(env, st);

    if submitted.display_text.is_empty() {
        return Ok(Flow::Continue);
    }

    if !submitted.image_data_urls.is_empty() {
        st.pending_images
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend(submitted.image_data_urls);
    }

    let history_entry =
        crate::commands::prompt_images::remove_image_markers(&submitted.expanded_text);
    let history_trimmed = history_entry.trim();
    if !history_trimmed.is_empty()
        && st.composer.prompt_history.last().map(String::as_str) != Some(history_trimmed)
    {
        st.composer.prompt_history.push(history_trimmed.to_string());
        if let Some(ref path) = st.history_path {
            append_prompt_history(Some(path), history_trimmed);
        }
    }

    let command_text =
        crate::commands::prompt_images::strip_leading_image_markers(&submitted.expanded_text);

    if command_text.eq_ignore_ascii_case("exit") || command_text.eq_ignore_ascii_case("quit") {
        return Ok(Flow::Break);
    }

    if command_text.starts_with('/') && !looks_like_path(command_text) {
        return run_slash_command(env, st, terminal, command_text, &submitted.display_text).await;
    }

    // Normal user prompt: queue/steer if a turn is active, else launch turn immediately.
    if env.compacting.load(Ordering::SeqCst) {
        st.queue.push_back(submitted.expanded_text);
        env.ui.set_queue_depth(st.queue.len());
        return Ok(Flow::Continue);
    }

    if st.active_turn.is_some() {
        match env.interrupt_handle.interrupt(&submitted.expanded_text) {
            harness_core::InterruptOutcome::Streaming => {
                st.system_notices
                    .push("↳ Redirecting — stopping this reply to take your message".to_string());
            }
            harness_core::InterruptOutcome::Queued => {
                st.system_notices
                    .push("↳ Held until the running tool finishes, then sent".to_string());
            }
            harness_core::InterruptOutcome::Idle => {
                st.queue.push_back(submitted.expanded_text);
                env.ui.set_queue_depth(st.queue.len());
            }
        }
        return Ok(Flow::Continue);
    }

    let display_opt = (submitted.display_text != submitted.expanded_text)
        .then_some(submitted.display_text.as_str());
    env.ui.clear_transient_error();
    st.scroll_from_bottom = 0;
    st.system_notices.retain(|n| !is_transient_queue_notice(n));
    launch_turn(
        &submitted.expanded_text,
        display_opt,
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

/// Map a `/default_model` or `/compact-model` argument to the scope it asks for.
fn model_scope_arg(arg: Option<&str>) -> Option<bool> {
    match arg
        .map(|value| value.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("global" | "--global" | "g") => Some(true),
        Some("session" | "--session" | "this" | "local" | "s") => Some(false),
        _ => None,
    }
}

/// `/compact-model` treats every non-reset argument as an explicit scope, where only
/// `global`/`--global` means global.
fn compact_scope_arg(arg: Option<&str>) -> Option<bool> {
    arg.map(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "global" | "--global"
        )
    })
}

fn show_profile_usage(st: &mut TuiState) {
    show_text_modal(
        st,
        "Profiles",
        "Usage: /profile list | show [name] [--full] | path [name] | create [name] | delete <name>",
    );
}

/// `/profile` in the TUI: plain reports open a text modal, while create and delete run the same
/// filesystem helpers as the CLI wizard without any of its terminal drawing.
fn route_profile_command(env: &mut Env<'_>, st: &mut TuiState, arg: Option<&str>) {
    let Some(arg) = arg else {
        show_profile_usage(st);
        return;
    };
    let mut parts = arg.split_whitespace();
    let Some(subcommand) = parts.next() else {
        show_profile_usage(st);
        return;
    };

    match subcommand.to_ascii_lowercase().as_str() {
        "list" | "ls" => {
            let text = crate::commands::profile::list_text(&env.config.paths.profiles_dir);
            show_text_modal(st, "Profiles", &text);
        }
        "show" => {
            let name = parts.next().map(str::to_string);
            let full = parts.any(|part| part == "--full");
            match crate::commands::profile::show_text(
                name.as_deref()
                    .unwrap_or(harness_core::profiles::DEFAULT_PROFILE),
                &env.config.paths.profiles_dir,
                full,
            ) {
                Ok(text) => show_text_modal(st, "Profile", &text),
                Err(error) => env
                    .ui
                    .set_transient_error(Some(format!("Profile failed: {error}"))),
            }
        }
        "path" => {
            let name = parts.next().map(str::to_string);
            match crate::commands::profile::path_text(
                name.as_deref()
                    .unwrap_or(harness_core::profiles::DEFAULT_PROFILE),
                env.config,
            ) {
                Ok(text) => show_text_modal(st, "Profile Path", &text),
                Err(error) => env
                    .ui
                    .set_transient_error(Some(format!("Profile failed: {error}"))),
            }
        }
        "create" | "new" => match parts.next().map(str::to_string) {
            Some(name) => match harness_core::profiles::validate_name(&name) {
                Ok(()) => open_profile_create_summary(st, name),
                Err(error) => env
                    .ui
                    .set_transient_error(Some(format!("Invalid name: {error}"))),
            },
            None => open_profile_create_name(st),
        },
        "delete" | "rm" => match parts.next().map(str::to_string) {
            Some(name) => {
                st.modal = ActiveModal::SelectModal {
                    title: "Delete Profile".to_string(),
                    items: vec![
                        format!("Yes, delete '{name}' and all of its data"),
                        "No, keep it".to_string(),
                    ],
                    selected: 1,
                    action: SelectAction::ProfileDeleteConfirm { name },
                };
            }
            None => show_profile_usage(st),
        },
        _ => show_profile_usage(st),
    }
}

/// Run a slash command inside the TUI entirely with native modals and transcript output.
pub async fn run_slash_command(
    env: &mut Env<'_>,
    st: &mut TuiState,
    _terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
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
    super::trace_event(&format!("slash start {name}"));
    match name {
        "/help" | "/?" if arg.is_none() => {
            st.modal = ActiveModal::Help { scroll: 0 };
            return Ok(Flow::Continue);
        }
        "/sessions" | "/session" if arg.is_none() => {
            open_session_picker(env, st).await;
            return Ok(Flow::Continue);
        }
        "/model" | "/models" | "/provider" => {
            if let Some(target) = arg {
                if let Some((prov_prefix, model_rest)) = target.split_once('/') {
                    if let Some(prov) = harness_providers::catalog::find_provider(prov_prefix) {
                        if !model_rest.trim().is_empty() {
                            if let Err(e) =
                                apply_model_selection(env, st, &prov.id, model_rest.trim(), true)
                                    .await
                            {
                                env.ui
                                    .set_transient_error(Some(format!("Model switch failed: {e}")));
                            }
                            return Ok(Flow::Continue);
                        }
                    }
                }
                if let Some(prov) = harness_providers::catalog::find_provider(target) {
                    open_models_for_provider(env, st, &prov.id, String::new()).await;
                } else {
                    open_provider_picker_with_query(env, st, target.to_string()).await;
                }
            } else {
                open_provider_picker(env, st).await;
            }
            return Ok(Flow::Continue);
        }
        "/expand" if arg.is_none() => {
            set_all_process_expanded(st, true);
            return Ok(Flow::Continue);
        }
        "/collapse" if arg.is_none() => {
            set_all_process_expanded(st, false);
            return Ok(Flow::Continue);
        }
        "/new" if arg.is_none() => {
            create_new_session(env, st).await?;
            return Ok(Flow::Continue);
        }
        "/thinking" | "/think" | "/reasoning" if arg.is_none() => {
            open_thinking_picker(env, st).await;
            return Ok(Flow::Continue);
        }
        "/settings" if arg.is_none() => {
            open_settings_menu(st, env.config);
            return Ok(Flow::Continue);
        }
        "/auto-compact" | "/autocompact" if arg.is_none() => {
            open_auto_compact_threshold(env, st).await;
            return Ok(Flow::Continue);
        }
        "/connect" => {
            open_connect_menu(st);
            return Ok(Flow::Continue);
        }
        "/jobs" | "/tasks" | "/subagents" => {
            open_task_list(env, st).await;
            return Ok(Flow::Continue);
        }
        "/profile" | "/profiles" => {
            route_profile_command(env, st, arg);
            return Ok(Flow::Continue);
        }
        "/dir" | "/workspace" => {
            let raw = arg.map(str::trim).filter(|value| !value.is_empty());
            let is_reset = raw.is_some_and(|value| {
                matches!(
                    value.to_ascii_lowercase().as_str(),
                    "reset" | "--reset" | "-r"
                )
            });
            if is_reset {
                if let Err(error) = reset_workspace(&env.ui, env.config, &env.agent).await {
                    env.ui
                        .set_transient_error(Some(format!("Workspace reset failed: {error}")));
                }
                return Ok(Flow::Continue);
            }
            let (path, scope) = parse_dir_args(raw);
            match path {
                Some(path) => match scope {
                    Some(is_global) => {
                        let expanded = harness_core::config::expand_tilde(&path);
                        if !std::path::PathBuf::from(&expanded).is_dir() {
                            env.ui
                                .set_transient_error(Some(format!("Directory not found: {path}")));
                        } else if let Err(error) =
                            apply_dir_choice(&env.ui, env.config, &env.agent, &path, is_global)
                                .await
                        {
                            env.ui.set_transient_error(Some(format!(
                                "Workspace change failed: {error}"
                            )));
                        }
                    }
                    None => open_dir_scope(st, path),
                },
                None => open_dir_input(st, scope),
            }
            return Ok(Flow::Continue);
        }
        "/default_model" | "/default-model" => {
            let global = model_scope_arg(arg);
            open_provider_picker_for(env, st, ModelTarget::Default { global }).await;
            return Ok(Flow::Continue);
        }
        "/compact-model" => {
            let is_reset = arg.is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "reset" | "default"
                )
            });
            if !is_reset {
                let global = compact_scope_arg(arg);
                open_provider_picker_for(env, st, ModelTarget::Compact { global }).await;
                return Ok(Flow::Continue);
            }
        }
        "/bgsaver-model" => {
            let is_reset = arg.is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "reset" | "default" | "off"
                )
            });
            if !is_reset {
                open_provider_picker_for(env, st, ModelTarget::BgSaver).await;
                return Ok(Flow::Continue);
            }
        }
        "/worker" => {
            if let Some(raw) = arg {
                let name = raw.trim().to_ascii_lowercase();
                let is_delete = name == "delete" || name.starts_with("delete ");
                if !name.is_empty() && name != "list" && !is_delete {
                    if let Err(error) = harness_core::config::validate_worker_name(&name) {
                        env.ui
                            .set_transient_error(Some(format!("Worker setup failed: {error}")));
                        return Ok(Flow::Continue);
                    }
                    if RESERVED_COMMAND_NAMES.iter().any(|reserved| {
                        reserved.trim_start_matches('/').eq_ignore_ascii_case(&name)
                    }) {
                        env.ui.set_transient_error(Some(format!(
                            "Worker name '{name}' is a built-in command; pick another name"
                        )));
                        return Ok(Flow::Continue);
                    }
                    open_provider_picker_for(env, st, ModelTarget::Worker(name)).await;
                    return Ok(Flow::Continue);
                }
            }
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
            st.settled_cache.clear();
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
        &st.current_cancel,
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
            st.system_notices.retain(|n| !is_transient_queue_notice(n));
        }
        return Ok(Flow::Continue);
    }

    let skills_arc = env.skills_arc.clone();
    let skills: &[harness_plugins::Skill] = skills_arc.as_deref().map(Vec::as_slice).unwrap_or(&[]);

    let prev_len = st.turns.len();
    env.ui.begin_command_capture();
    let outcome = execute_slash_command(
        cmd,
        None,
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
            if st.active_goal.is_some() {
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
        st.system_notices.retain(|n| !is_transient_queue_notice(n));
    }

    refresh_tabs(env, st).await;
    drain_tui_notices(env, st);
    if st.queue.is_empty() {
        st.system_notices.retain(|n| !is_transient_queue_notice(n));
    }
    st.scroll_from_bottom = 0;
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
                    slot.system_notices.push(format!(
                        "✔ Goal achieved in {} iteration(s).",
                        goal.iteration
                    ));
                    slot.active_goal = None;
                    slot.ui.set_goal_status(None);
                }
                harness_core::GoalVerdict::Blocked(reason) => {
                    slot.system_notices.push(format!(
                        "✖ Goal blocked ({}/{}): {reason}",
                        goal.iteration, goal.max_iterations
                    ));
                    slot.active_goal = None;
                    slot.ui.set_goal_status(None);
                }
                harness_core::GoalVerdict::Continue => {
                    if !goal.advance() {
                        slot.system_notices.push(format!(
                            "■ Goal stopped: max iterations ({}) reached",
                            goal.max_iterations
                        ));
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
        slot.system_notices
            .retain(|n| !is_transient_queue_notice(n));
    }

    if slot.queue.is_empty() {
        slot.system_notices
            .retain(|n| !is_transient_queue_notice(n));
    }

    refresh_tabs(env, st).await;
}

fn advance_goal_tui(env: &Env<'_>, st: &mut TuiState, reply: &str) {
    let Some(ref mut goal) = st.active_goal else {
        return;
    };
    match harness_core::GoalRunState::evaluate_reply(reply) {
        harness_core::GoalVerdict::Complete(_) => {
            st.system_notices.push(format!(
                "✔ Goal achieved in {} iteration(s).",
                goal.iteration
            ));
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
            st.system_notices.push(n);
        }
    }
    if st.queue.is_empty() && st.active_turn.is_none() {
        st.system_notices.retain(|n| !is_transient_queue_notice(n));
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

#[cfg(test)]
mod tests {
    use super::{compact_scope_arg, model_scope_arg};

    #[test]
    fn default_model_scope_flags_are_recognised() {
        assert_eq!(model_scope_arg(Some("global")), Some(true));
        assert_eq!(model_scope_arg(Some("--global")), Some(true));
        assert_eq!(model_scope_arg(Some("session")), Some(false));
        assert_eq!(model_scope_arg(Some("--session")), Some(false));
    }

    #[test]
    fn a_bare_default_model_asks_for_scope() {
        assert_eq!(model_scope_arg(None), None);
        assert_eq!(model_scope_arg(Some("anything else")), None);
    }

    #[test]
    fn compact_model_scope_defaults_to_session_for_any_other_argument() {
        assert_eq!(compact_scope_arg(None), None);
        assert_eq!(compact_scope_arg(Some("global")), Some(true));
        assert_eq!(compact_scope_arg(Some("whatever")), Some(false));
    }

    #[test]
    fn transient_queue_notices_are_properly_detected() {
        use super::is_transient_queue_notice;
        assert!(is_transient_queue_notice(
            "⏸ Queued (1 waiting) — sent when the turn in flight finishes"
        ));
        assert!(is_transient_queue_notice(
            "↳ Held until the running tool finishes, then sent"
        ));
        assert!(is_transient_queue_notice(
            "↳ Redirecting — stopping this reply to take your message"
        ));
        assert!(!is_transient_queue_notice(
            "✓ Deleted session sa889751a and started fresh session s5227ec12"
        ));
        assert!(!is_transient_queue_notice(
            "Build · Nemotron 3.5 Super VL 120B A12b · 0.0s"
        ));
    }
}
