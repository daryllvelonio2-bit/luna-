//! Session actions, slash-command execution, turn lifecycle, and tab management for the TUI.

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use harness_core::{
    ApprovalRequest, CompactionSetup, CompletionOptions, HarnessError, MemoryEngine, Message,
    ModelProvider, ReasoningEffort, SessionBinding, SessionStore, TurnOutcome,
};
use harness_providers::{GenericProviderClient, RotationStrategy};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use super::super::commands::{
    apply_auto_compact_values, apply_dir_choice, command_entries, connected_providers,
    execute_slash_command, format_settings_summary, handle_settings_command, list_keys_text,
    looks_like_path, parse_dir_args, provider_id_of, provider_items, reset_workspace, Feature,
    SlashOutcome, RESERVED_COMMAND_NAMES,
};
use super::super::helpers::{finish_slash_command, IdleMemoryPass};
use super::super::history::append_prompt_history;
use super::super::interactive::state::{Env, Flow};
use super::super::observers::agent_observer;
use super::super::render::{
    compaction_report, parse_turns_from_history, TurnProcessItem, TurnRecord,
};
use super::super::status::{refresh_antigravity_quota_owned, refresh_antigravity_status_if_needed};
use super::super::todo;
use super::super::turn::{
    cancel_running_turn, drain_queue, handle_composer_command, launch_internal_turn, launch_turn,
    turn_records, InterruptNotice,
};
use super::super::ui::ChatUi;
use super::markdown::strip_ansi;
use super::state::{
    ActiveModal, AgentMode, ApprovalReply, BackgroundSession, InputAction, LiveThinkingView,
    ModelPickerItem, ModelTarget, PaletteAction, ProviderPickerItem, SelectAction, SessionTab,
    SubagentUiEntry, SystemNotice, TuiState, TurnMeta,
};
use super::theme::{friendly_model_name, friendly_provider_name};
use crate::commands::context_window::context_window;
use crate::commands::sessions::{resolve_session, SessionSelection};

/// Rebuild the top session tabs from `SqliteSessionStore`, keeping the active tab visible.
pub async fn refresh_tabs(env: &Env<'_>, st: &mut TuiState) {
    let sessions = env.store.list().await.unwrap_or_default();
    let non_worker: Vec<_> = sessions
        .into_iter()
        .filter(|s| {
            !s.name.starts_with("worker:") && (s.message_count > 0 || s.id == *env.session_id)
        })
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
        scroll_anchor_top: st.scroll_anchor_top,
        queue: std::mem::take(&mut st.queue),
        current_cancel: std::mem::take(&mut st.current_cancel),
        active_turn: st.active_turn.take(),
        active_goal: st.active_goal.take(),
        current_query: std::mem::take(&mut st.current_query),
        turn_start: st.turn_start,
        idle_pass: std::mem::replace(&mut st.idle_pass, fresh_idle),
        pending_images: st.pending_images.clone(),
        pending_job_turn: st.pending_job_turn.take(),
        memory_project_filter: st.memory_project_filter.take(),
        feature_toggles: env.feature_toggles.snapshot(),
    };
    st.background_sessions.insert(old_id, slot);
}

/// Park the current session if it contains conversation messages; otherwise discard and delete
/// it from the session store so abandoned/empty sessions never pollute history.
async fn park_or_discard_current_session(env: &Env<'_>, st: &mut TuiState) {
    let old_id = env.session_id.clone();
    let old_is_empty = st.turns.is_empty()
        && st.active_turn.is_none()
        && st.queue.is_empty()
        && env
            .store
            .get(&old_id)
            .await
            .map(|r| r.message_count == 0)
            .unwrap_or(true);

    if old_is_empty {
        let _ = env.store.delete_session(&old_id).await;
        st.tabs.retain(|t| t.id != old_id);
    } else {
        park_current_session(env, st);
    }
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
    st.prune_stale_notices();
    st.expanded_thinking = slot.expanded_thinking;
    st.expanded_tools = slot.expanded_tools;
    st.expanded_live_thinking = slot.expanded_live_thinking;
    st.expanded_live_tools = slot.expanded_live_tools;
    st.live_thinking_view = slot.live_thinking_view;
    st.process_expanded = slot.process_expanded;
    st.scroll_from_bottom = slot.scroll_from_bottom;
    st.scroll_anchor_top = slot.scroll_anchor_top;
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
    st.memory_project_filter = slot.memory_project_filter;
    env.feature_toggles.restore(slot.feature_toggles);

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
    let client = Arc::new(
        GenericProviderClient::new(
            provider.to_string(),
            model.to_string(),
            env.cred_manager.clone(),
            env.discovery.clone(),
            env.config.providers.get(provider),
        )?
        .with_session_id(session_id),
    );
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
                provider: Arc::new(comp_client.with_session_id(session_id)),
                model: model.to_string(),
            }));
        }
    }

    if env.config.memory.mode.is_background() {
        let memory_dyn: Arc<dyn MemoryEngine> = st.template_agent.memory().clone();
        let notes_dyn: Arc<dyn harness_core::NotesEngine> = st.template_agent.notes().clone();
        let saver_provider = env
            .config
            .memory
            .background_provider
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(provider);
        let saver_model = env
            .config
            .memory
            .background_model
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(model);
        if let Ok(saver) = crate::commands::chat::memory_writer::build_saver_for(
            env.config,
            &memory_dyn,
            notes_dyn,
            saver_provider,
            saver_model,
            env.cred_manager.clone(),
            env.discovery.clone(),
        ) {
            ag.set_background_saver(Some(Arc::new(saver)));
        }
    } else {
        ag.set_background_saver(None);
    }
    ag.set_memory_enabled(env.feature_toggles.enabled(Feature::Memory));
    ag.set_notes_enabled(env.feature_toggles.enabled(Feature::Notes));
    ag.set_skills_enabled(env.feature_toggles.enabled(Feature::Skills));
    ag.set_mcp_enabled(env.feature_toggles.enabled(Feature::Mcp));

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
        park_or_discard_current_session(env, st).await;
        restore_background_session(env, st, existing);
        sync_delegate_wiring(env).await;
        let ui_bg = env.ui.clone();
        let prov_bg = env.current_provider.clone();
        let mod_bg = env.current_model.clone();
        let cred_bg = env.cred_manager.clone();
        let sid_bg = env.session_id.clone();
        tokio::spawn(async move {
            refresh_antigravity_status_if_needed(
                &ui_bg,
                &prov_bg,
                &mod_bg,
                &cred_bg,
                Some(&sid_bg),
            )
            .await;
        });
        if !st.tabs.iter().any(|t| t.id == *env.session_id) {
            refresh_tabs(env, st).await;
        }
        return Ok(());
    }

    let record = env.store.get(target_id).await?;
    for (prov, cred_id) in &record.credential_priorities {
        env.cred_manager
            .set_session_priority_credential(target_id, prov, cred_id);
    }
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

    park_or_discard_current_session(env, st).await;

    *env.session_id = target_id.to_string();
    env.agent = agent.clone();
    env.ui = ui.clone();
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
    sync_delegate_wiring(env).await;

    st.turns = turns;
    st.turn_meta = turn_meta;
    st.settled_cache.clear();
    st.live_content_cache = Default::default();
    st.live_reasoning_cache = Default::default();
    st.system_notices.clear();
    st.expanded_thinking.clear();
    st.expanded_tools.clear();
    st.expanded_live_thinking.clear();
    st.expanded_live_tools.clear();
    st.live_thinking_view = LiveThinkingView::Tail;
    st.process_expanded = false;
    st.jump_to_latest();
    st.selection = None;
    st.pending_transcript_click = None;
    st.selection_notice = None;
    st.idle_pass = IdleMemoryPass::new(initial_len);
    st.pending_images = Arc::new(Mutex::new(Vec::new()));
    st.memory_project_filter = None;

    env.ui.set_status(&target_model, 0, ctx_max);
    env.ui.set_session_name(&record.name);
    env.ui.clear_live_state();
    env.ui.clear_transient_error();

    let ui_tokens = env.ui.clone();
    let agent_tokens = agent.clone();
    tokio::spawn(async move {
        let used = agent_tokens
            .lock()
            .await
            .estimate_session_tokens()
            .await
            .unwrap_or(0);
        ui_tokens.update_status(used, ctx_max);
    });

    let ui_bg = env.ui.clone();
    let prov_bg = target_provider.clone();
    let mod_bg = target_model.clone();
    let cred_bg = env.cred_manager.clone();
    let sid_bg = target_id.to_string();
    tokio::spawn(async move {
        refresh_antigravity_status_if_needed(&ui_bg, &prov_bg, &mod_bg, &cred_bg, Some(&sid_bg))
            .await;
    });

    if !st.tabs.iter().any(|t| t.id == *env.session_id) {
        refresh_tabs(env, st).await;
    }
    Ok(())
}

/// Open a brand new session tab and switch into it (works even while another session is busy).
pub async fn create_new_session(env: &mut Env<'_>, st: &mut TuiState) -> Result<(), HarnessError> {
    let new_id = resolve_session(env.store.as_ref(), SessionSelection::New).await?;
    let provider = env.current_provider.clone();
    let model = env.current_model.clone();
    let _ = env.store.set_model(&new_id, &provider, &model).await;

    let saved_toggles = env.feature_toggles.snapshot();
    env.feature_toggles.sync_from_config(env.config);
    let (
        agent,
        ui,
        in_flight_tools,
        interruptions,
        steer_handle,
        interrupt_handle,
        compacting,
        ctx_max,
    ) = match instantiate_session_runtime(env, st, &new_id, &provider, &model, st.reasoning_effort)
    {
        Ok(bundle) => bundle,
        Err(err) => {
            env.feature_toggles.restore(saved_toggles);
            return Err(err);
        }
    };
    let new_toggles = env.feature_toggles.snapshot();
    env.feature_toggles.restore(saved_toggles);

    park_or_discard_current_session(env, st).await;
    env.feature_toggles.restore(new_toggles);

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
    sync_delegate_wiring(env).await;

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
    st.jump_to_latest();
    st.selection = None;
    st.pending_transcript_click = None;
    st.selection_notice = None;
    st.idle_pass = IdleMemoryPass::new(0);
    st.pending_images = Arc::new(Mutex::new(Vec::new()));
    st.memory_project_filter = None;

    env.ui.set_status(&model, 0, ctx_max);
    env.ui.set_session_name("");
    env.ui.clear_live_state();
    env.ui.clear_transient_error();
    {
        let ui_bg = env.ui.clone();
        let prov_bg = provider;
        let mod_bg = model;
        let cred_bg = env.cred_manager.clone();
        let sid_bg = new_id.clone();
        tokio::spawn(async move {
            refresh_antigravity_status_if_needed(
                &ui_bg,
                &prov_bg,
                &mod_bg,
                &cred_bg,
                Some(&sid_bg),
            )
            .await;
        });
    }
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
        .filter(|s| !s.name.starts_with("worker:") && s.message_count > 0)
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
    config_providers: Option<
        &std::collections::BTreeMap<String, harness_core::config::ProviderOverride>,
    >,
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
    let mut configured: Vec<_> = catalog
        .iter()
        .filter(|p| {
            loaded.iter().any(|l| l.eq_ignore_ascii_case(&p.id))
                || p.id.eq_ignore_ascii_case(current_provider)
                || (p.id == "opencode" && has_opencode_daemon)
                || (p.id == "opencode-cli" && has_opencode_cli)
        })
        .cloned()
        .collect();

    let mut custom_ids = Vec::new();
    if let Some(cfgs) = config_providers {
        for id in cfgs.keys() {
            if id != "custom"
                && !custom_ids.contains(id)
                && !configured.iter().any(|p| p.id.eq_ignore_ascii_case(id))
            {
                custom_ids.push(id.clone());
            }
        }
    }
    for id in &loaded {
        let clean = super::super::connect::provider_id_of(id);
        if clean != "custom"
            && !custom_ids.contains(&clean.to_string())
            && !configured.iter().any(|p| p.id.eq_ignore_ascii_case(clean))
        {
            custom_ids.push(clean.to_string());
        }
    }
    for id in custom_ids {
        configured.push(harness_providers::catalog::custom_provider_descriptor(
            &id, "",
        ));
    }

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
    let highlight_provider = if matches!(target, ModelTarget::Approval) {
        resolve_approval_model(env, st).0
    } else {
        env.current_provider.clone()
    };
    let items = if highlight_provider.eq_ignore_ascii_case(env.current_provider) {
        st.provider_items.clone()
    } else {
        build_provider_items(
            env.cred_manager.clone(),
            Some(&env.config.providers),
            &highlight_provider,
        )
        .await
    };
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
    let config_providers = env.config.providers.clone();
    tokio::spawn(async move {
        let items =
            build_provider_items(cred_manager, Some(&config_providers), &highlight_provider).await;
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
    if !def.is_empty() && def != "default" && !models.contains(&def) {
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
    let is_approval_target = matches!(target, ModelTarget::Approval);
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
    let (current_provider, current_model) = if is_approval_target {
        let (p, m, _, _) = resolve_approval_model(env, st);
        (p, m)
    } else {
        (env.current_provider.clone(), env.current_model.clone())
    };
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
    open_thinking_picker_with_current(st, st.reasoning_effort);
}

pub fn open_thinking_picker_with_current(st: &mut TuiState, cur: Option<ReasoningEffort>) {
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
    drop(ag);
    *env.current_provider = provider.to_string();
    *env.current_model = model.to_string();
    let used = env.ui.current_context_used();
    env.ui.set_status(model, used, ctx_max);

    let ui_tokens = env.ui.clone();
    let agent_tokens = env.agent.clone();
    tokio::spawn(async move {
        let est = agent_tokens
            .lock()
            .await
            .estimate_session_tokens()
            .await
            .unwrap_or(0);
        ui_tokens.update_status(est, ctx_max);
    });

    let ui_bg = env.ui.clone();
    let prov_bg = provider.to_string();
    let mod_bg = model.to_string();
    let cred_bg = env.cred_manager.clone();
    let sid_bg = env.session_id.clone();
    tokio::spawn(async move {
        refresh_antigravity_status_if_needed(&ui_bg, &prov_bg, &mod_bg, &cred_bg, Some(&sid_bg))
            .await;
    });

    if persist {
        let store = env.store.clone();
        let session_id = env.session_id.clone();
        let prov = provider.to_string();
        let mod_ = model.to_string();
        tokio::spawn(async move {
            let _ = store.set_model(&session_id, &prov, &mod_).await;
        });
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
    if st.active_turn.is_some() && !matches!(target, ModelTarget::Worker(_) | ModelTarget::Approval)
    {
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
        ModelTarget::Approval => apply_approval_model_choice(
            env,
            st,
            provider,
            model,
            env.config.tools.approval_reasoning_effort,
        ),
        ModelTarget::Worker(name) => apply_worker_target(env, st, &name, provider, model),
    }
    .map(|_| ())
}

/// Resolve `(provider, model, reasoning_effort, is_default)` for approval popup questions.
pub fn resolve_approval_model(
    env: &Env<'_>,
    st: &TuiState,
) -> (String, String, Option<ReasoningEffort>, bool) {
    if let (Some(p), Some(m)) = (
        env.config
            .tools
            .approval_provider
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
        env.config
            .tools
            .approval_model
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    ) {
        return (
            p.to_string(),
            m.to_string(),
            env.config.tools.approval_reasoning_effort,
            false,
        );
    }
    let default_provider = env.config.model.provider.trim();
    let default_model = env.config.model.default.trim();
    if !default_provider.is_empty() && !default_model.is_empty() {
        let effort = if default_provider.eq_ignore_ascii_case(env.current_provider)
            && default_model == env.current_model.as_str()
        {
            st.reasoning_effort
        } else {
            None
        };
        return (
            default_provider.to_string(),
            default_model.to_string(),
            effort,
            true,
        );
    }
    (
        env.current_provider.clone(),
        env.current_model.clone(),
        st.reasoning_effort,
        true,
    )
}

fn format_approval_model_badge(env: &Env<'_>, st: &TuiState) -> String {
    let (provider, model, effort, is_default) = resolve_approval_model(env, st);
    let model_disp = friendly_model_name(&model);
    if is_default {
        format!("default ({model_disp})")
    } else if let Some(e) = effort {
        format!("{model_disp} ({provider} · {e})")
    } else {
        format!("{model_disp} ({provider})")
    }
}

pub fn apply_approval_model_choice(
    env: &mut Env<'_>,
    st: &mut TuiState,
    provider: &str,
    model: &str,
    effort: Option<ReasoningEffort>,
) -> Result<(), HarnessError> {
    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    harness_core::config::set_in_file(&layers, "tools.approval_provider", provider)?;
    harness_core::config::set_in_file(&layers, "tools.approval_model", model)?;
    if let Some(e) = effort {
        harness_core::config::set_in_file(&layers, "tools.approval_reasoning_effort", e.as_str())?;
    } else {
        let _ = harness_core::config::unset_in_file(&layers, "tools.approval_reasoning_effort")?;
    }
    env.config.tools.approval_provider = Some(provider.to_string());
    env.config.tools.approval_model = Some(model.to_string());
    env.config.tools.approval_reasoning_effort = effort;
    let effort_label = effort
        .map(|e| e.to_string())
        .unwrap_or_else(|| "off".to_string());
    notify_setting_change(
        st,
        format!(
            "✔ Approval model set to {} ({}, thinking: {})",
            friendly_model_name(model),
            provider,
            effort_label
        ),
    );
    open_settings_menu(env, st, 9);
    Ok(())
}

pub fn reset_approval_model_choice(
    env: &mut Env<'_>,
    st: &mut TuiState,
) -> Result<(), HarnessError> {
    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    let _ = harness_core::config::unset_in_file(&layers, "tools.approval_provider")?;
    let _ = harness_core::config::unset_in_file(&layers, "tools.approval_model")?;
    let _ = harness_core::config::unset_in_file(&layers, "tools.approval_reasoning_effort")?;
    env.config.tools.approval_provider = None;
    env.config.tools.approval_model = None;
    env.config.tools.approval_reasoning_effort = None;
    notify_setting_change(st, "✔ Approval model reset to default".to_string());
    open_settings_menu(env, st, 9);
    Ok(())
}

fn truncate_chars_with_ellipsis(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let end = text
        .char_indices()
        .nth(max_chars)
        .map(|(idx, _)| idx)
        .unwrap_or(text.len());
    format!("{}\n...(truncated)", &text[..end])
}

pub fn build_approval_question_prompt(req: &ApprovalRequest, question: &str) -> String {
    let mut out = format!("Pending tool call: {}\n", req.tool);
    if let Some(ref target) = req.target {
        out.push_str(&format!("Target: {target}\n"));
    }
    if let Some(ref diff) = req.diff {
        out.push_str("\nDiff:\n");
        out.push_str(&truncate_chars_with_ellipsis(diff, 8000));
        out.push('\n');
    } else if !req.arguments.is_null() {
        let pretty = serde_json::to_string_pretty(&req.arguments)
            .unwrap_or_else(|_| req.arguments.to_string());
        out.push_str("\nArguments:\n");
        out.push_str(&truncate_chars_with_ellipsis(&pretty, 4000));
        out.push('\n');
    }
    out.push_str("\nUser question / suggestion:\n");
    out.push_str(question.trim());
    out
}

pub fn ask_approval_model(env: &Env<'_>, st: &TuiState, req: ApprovalRequest, question: String) {
    let (provider, model, reasoning_effort, _) = resolve_approval_model(env, st);
    let cred_manager = env.cred_manager.clone();
    let discovery = env.discovery.clone();
    let provider_override = env.config.providers.get(&provider).cloned();
    let tx = st.approval_reply_tx.clone();
    let activity = env.approval_activity.clone();
    activity.notify_one();

    tokio::spawn(async move {
        let call_id = req.call_id.clone();
        let result = async {
            let client = GenericProviderClient::new(
                provider.clone(),
                model.clone(),
                cred_manager,
                discovery,
                provider_override.as_ref(),
            )
            .map_err(|e| {
                format!("failed to initialize approval model ({provider}/{model}): {e}")
            })?;

            let prompt = build_approval_question_prompt(&req, &question);
            let messages = vec![
                Message::system(
                    "You are assisting a user who is reviewing a pending tool approval request in Luna. \
                     Answer their question or evaluate their suggestion clearly and concisely based strictly \
                     on the tool call details shown. Do not invent file contents that were not provided.",
                ),
                Message::user(prompt),
            ];
            let options = CompletionOptions {
                model: Some(model.clone()),
                reasoning_effort,
                ..Default::default()
            };
            let reply = client
                .complete(&messages, &[], &options)
                .await
                .map_err(|e| format!("approval model error ({provider}/{model}): {e}"))?;
            let text = reply.content.as_deref().unwrap_or("").trim().to_string();
            if text.is_empty() {
                Ok("(model returned an empty response)".to_string())
            } else {
                Ok(text)
            }
        }
        .await;
        activity.notify_one();
        let _ = tx.send(ApprovalReply { call_id, result });
    });
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
    st.system_notices.push(SystemNotice::new(format!(
        "✔ Worker /{name} → {model} ({provider}) · invoke with /{name} <task>"
    )));
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

fn notify_setting_change(st: &mut TuiState, msg: impl Into<String>) {
    let text = msg.into();
    if st.turns.is_empty() {
        st.selection_notice = Some((text, std::time::Instant::now()));
    } else {
        st.system_notices.push(SystemNotice::new(text));
    }
}

fn feature_status_badge(env: &Env<'_>, feature: Feature) -> String {
    let live_on = env.feature_toggles.enabled(feature);
    let (global_on, until) = match feature {
        Feature::Mcp => (
            env.config.mcp.enabled && env.config.agent.mcp_enabled,
            env.config.features.mcp_disabled_until,
        ),
        Feature::Skills => (
            env.config.agent.skills_enabled,
            env.config.features.skills_disabled_until,
        ),
        Feature::Memory => (
            env.config.agent.memory_enabled,
            env.config.features.memory_disabled_until,
        ),
        Feature::Notes => (
            env.config.agent.notes_enabled,
            env.config.features.notes_disabled_until,
        ),
    };
    let now = harness_core::Config::current_unix_secs();
    if live_on {
        if !global_on || until.is_some_and(|u| u > now) {
            "ON (this session)".to_string()
        } else {
            "ON".to_string()
        }
    } else if let Some(u) = until.filter(|&u| u > now) {
        let days = (u - now).div_ceil(86_400);
        format!("OFF ({days}d left)")
    } else if !global_on {
        "OFF (global)".to_string()
    } else {
        "OFF (this session)".to_string()
    }
}

fn open_settings_menu(env: &Env<'_>, st: &mut TuiState, selected: usize) {
    let config = &env.config;
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
    let theme_label = match config.display.theme {
        harness_core::ThemeMode::Dark => "dark mode (default)",
        harness_core::ThemeMode::Light => "light mode",
    };
    let saver = match config.memory.mode {
        harness_core::MemoryMode::Session => "Session writer",
        harness_core::MemoryMode::Background => "Background saver",
    };
    let approval_model = format_approval_model_badge(env, st);
    let voice_label = if config.voice.enabled {
        format!("on ({})", config.voice.provider)
    } else {
        "off".to_string()
    };
    st.modal = ActiveModal::SelectModal {
        title: "CLI Settings".to_string(),
        items: vec![
            format!("Theme · {theme_label}"),
            format!("Display block limit · {limit}"),
            format!("Auto-collapse on turn finish · {collapse}"),
            format!("Auto-collapse thinking while a tool runs · {thinking}"),
            format!("MCP · {}", feature_status_badge(env, Feature::Mcp)),
            format!("Skills · {}", feature_status_badge(env, Feature::Skills)),
            format!("Memory · {}", feature_status_badge(env, Feature::Memory)),
            format!("Notes · {}", feature_status_badge(env, Feature::Notes)),
            format!("Memory saver · {saver}"),
            format!("Approval model · {approval_model}"),
            format!("Voice mode · {voice_label}"),
            "View current settings summary".to_string(),
        ],
        selected: selected.min(11),
        action: SelectAction::SettingsMain,
    };
}

/// Rotate/toggle a single row of the `CLI Settings` modal in place using Space (or Enter).
pub async fn toggle_settings_main_item(env: &mut Env<'_>, st: &mut TuiState, choice: usize) {
    match choice {
        0 => {
            let next = match env.config.display.theme {
                harness_core::ThemeMode::Dark => "theme light",
                harness_core::ThemeMode::Light => "theme dark",
            };
            if let Err(error) = handle_settings_command(
                None,
                &env.ui,
                env.config,
                &env.display_settings,
                &mut st.process_expanded,
                Some(next),
            )
            .await
            {
                env.ui
                    .set_transient_error(Some(format!("Settings failed: {error}")));
            } else {
                super::theme::set_active_theme(env.config.display.theme);
                st.settled_cache.clear();
                st.live_content_cache = Default::default();
                st.live_reasoning_cache = Default::default();
            }
            open_settings_menu(env, st, 0);
        }
        1 => {
            let next = match env.config.display.block_limit {
                None => Some(1),
                Some(n) if n < 10 => Some(n + 1),
                Some(_) => None,
            };
            let arg = match next {
                None => "limit all".to_string(),
                Some(n) => format!("limit {n}"),
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
            st.block_limit = env.config.display.block_limit;
            open_settings_menu(env, st, 1);
        }
        2 => {
            let arg = if env.config.display.auto_collapse {
                "collapse off"
            } else {
                "collapse on"
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
            } else {
                set_all_process_expanded(st, !env.config.display.auto_collapse);
            }
            open_settings_menu(env, st, 2);
        }
        3 => {
            let arg = if env.config.display.auto_hide_thinking {
                "thinking off"
            } else {
                "thinking on"
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
            open_settings_menu(env, st, 3);
        }
        4..=7 => {
            let feature = match choice {
                4 => Feature::Mcp,
                5 => Feature::Skills,
                6 => Feature::Memory,
                _ => Feature::Notes,
            };
            let live_on = env.feature_toggles.enabled(feature);
            let (global_on, until) = match feature {
                Feature::Mcp => (
                    env.config.agent.mcp_enabled,
                    env.config.features.mcp_disabled_until,
                ),
                Feature::Skills => (
                    env.config.agent.skills_enabled,
                    env.config.features.skills_disabled_until,
                ),
                Feature::Memory => (
                    env.config.agent.memory_enabled,
                    env.config.features.memory_disabled_until,
                ),
                Feature::Notes => (
                    env.config.agent.notes_enabled,
                    env.config.features.notes_disabled_until,
                ),
            };
            let timed_off = until.is_some_and(|u| u > harness_core::Config::current_unix_secs());

            if live_on && global_on && !timed_off {
                apply_feature_live(env, feature, false).await;
                notify_setting_change(
                    st,
                    format!("{} turned OFF for this session", feature.title()),
                );
            } else if !live_on && global_on && !timed_off {
                if let Err(err) = apply_feature_global(env, feature, false, None).await {
                    env.ui
                        .set_transient_error(Some(format!("Settings failed: {err}")));
                } else {
                    notify_setting_change(st, format!("{} turned OFF globally", feature.title()));
                }
            } else if let Err(err) = apply_feature_global(env, feature, true, None).await {
                env.ui
                    .set_transient_error(Some(format!("Settings failed: {err}")));
            } else {
                notify_setting_change(st, format!("{} turned ON", feature.title()));
            }
            open_settings_menu(env, st, choice);
        }
        8 => {
            let next_mode = if env.config.memory.mode.is_background() {
                harness_core::MemoryMode::Session
            } else {
                harness_core::MemoryMode::Background
            };
            let label = match next_mode {
                harness_core::MemoryMode::Session => "Session writer",
                harness_core::MemoryMode::Background => "Background saver",
            };
            if let Err(err) = apply_memory_mode_choice(env, next_mode).await {
                env.ui
                    .set_transient_error(Some(format!("Memory saver switch failed: {err}")));
            } else {
                notify_setting_change(st, format!("Memory saver set to {label} (saved)"));
            }
            open_settings_menu(env, st, 8);
        }
        9 => {
            if let Err(err) = reset_approval_model_choice(env, st) {
                env.ui
                    .set_transient_error(Some(format!("Approval model reset failed: {err}")));
                open_settings_menu(env, st, 9);
            }
        }
        10 => {
            let next = if env.config.voice.enabled {
                "voice off"
            } else {
                "voice on"
            };
            if let Err(error) = handle_settings_command(
                None,
                &env.ui,
                env.config,
                &env.display_settings,
                &mut st.process_expanded,
                Some(next),
            )
            .await
            {
                env.ui
                    .set_transient_error(Some(format!("Voice settings failed: {error}")));
            }
            open_settings_menu(env, st, 10);
        }
        _ => {
            open_settings_menu(env, st, choice);
        }
    }
}

async fn open_memory_project_picker(st: &mut TuiState) {
    let projects = st
        .template_agent
        .memory_engine()
        .list_projects()
        .await
        .unwrap_or_default();
    let active = st.memory_project_filter.as_deref();
    let mut items = Vec::with_capacity(projects.len() + 2);
    let default_mark = if active.is_none() { "● " } else { "" };
    items.push(format!("{default_mark}★ All projects (default / reset)"));
    for proj in &projects {
        let mark = if active == Some(proj.as_str()) {
            "● "
        } else {
            "  "
        };
        items.push(format!("{mark}{proj}"));
    }
    items.push("✎ Custom project name…".to_string());
    st.modal = ActiveModal::SelectModal {
        title: "Select Project Memory Scope".to_string(),
        items,
        selected: 0,
        action: SelectAction::MemoryProject { projects },
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
    let providers = provider_items(&env.cred_manager, Some(&env.config.providers)).await;
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

pub(crate) async fn open_connect_priority_credential(
    env: &Env<'_>,
    st: &mut TuiState,
    provider: String,
) {
    let creds = env.cred_manager.get_credentials(&provider).await;
    if creds.is_empty() {
        env.ui
            .emit_above(&format!("No credentials found for {provider}"));
        open_connect_menu(st);
        return;
    }
    let session_override = env
        .cred_manager
        .get_session_priority_credential(env.session_id, &provider);
    let mut selected = 0;
    let ids: Vec<String> = creds.iter().map(|cred| cred.id.clone()).collect();
    let items: Vec<String> = creds
        .iter()
        .enumerate()
        .map(|(index, cred)| {
            let is_session_prio = session_override.as_deref() == Some(&cred.id)
                || session_override.as_deref() == Some(&cred.label);
            if is_session_prio || (session_override.is_none() && cred.is_priority) {
                selected = index;
            }
            let mut tags = Vec::new();
            if is_session_prio {
                tags.push("session priority");
            }
            if cred.is_priority {
                tags.push(if session_override.is_some() {
                    "global priority"
                } else {
                    "current priority"
                });
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

fn read_subagent_log_tail(log_path: &str, max_lines: usize) -> Vec<String> {
    let Ok(body) = std::fs::read_to_string(log_path) else {
        return Vec::new();
    };
    let all: Vec<String> = body.lines().map(str::to_string).collect();
    if all.len() <= max_lines {
        all
    } else {
        all[all.len() - max_lines..].to_vec()
    }
}

fn worker_turns_from_history(
    mut turns: Vec<TurnRecord>,
    goal: &str,
    status: &str,
) -> Vec<TurnRecord> {
    let first_goal_line = goal.lines().next().unwrap_or(goal).trim();
    if let Some(pos) = turns.iter().rposition(|t| {
        let q = t.user_query.trim();
        q.starts_with("GOAL:")
            && (first_goal_line.is_empty()
                || q.contains(first_goal_line)
                || q.trim_start_matches("GOAL:").trim().is_empty())
    }) {
        turns.split_off(pos)
    } else if status != "running" && !turns.is_empty() {
        let n = turns.len();
        turns.split_off(n.saturating_sub(1))
    } else {
        Vec::new()
    }
}

fn worker_label_from_context(context: Option<&str>) -> Option<String> {
    context
        .and_then(|c| c.strip_prefix("worker profile: worker:"))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| format!("worker:{name}"))
}

async fn sync_delegate_wiring(env: &Env<'_>) {
    super::super::setup::wire_delegate_notifications(
        &env.delegate_tool,
        &env.ui,
        &env.store,
        env.session_id.as_str(),
    );
    env.delegate_tool.set_steer_handle(env.steer_handle.clone());
    let records = env.delegate_tool.list_tasks().await;
    for rec in records {
        if matches!(
            rec.status,
            harness_core::tools::delegate::SubagentStatus::Running
        ) {
            let label = match worker_label_from_context(rec.context.as_deref()) {
                Some(worker) => {
                    let name = worker.strip_prefix("worker:").unwrap_or(&worker);
                    format!("[{name}] {}", rec.goal)
                }
                None => rec.goal.clone(),
            };
            env.ui.start_delegate_task(&rec.id, &label);
        } else {
            env.ui.finish_delegate_task(&rec.id);
        }
    }
}

pub async fn collect_subagent_entries(env: &Env<'_>) -> Vec<SubagentUiEntry> {
    let mut entries = Vec::new();
    let tasks = super::super::viewer::collect_tasks(
        Some(env.bg_queue.as_ref()),
        Some(env.delegate_tool.as_ref()),
    )
    .await;
    let sub_records: std::collections::HashMap<
        String,
        harness_core::tools::delegate::SubagentTaskRecord,
    > = env
        .delegate_tool
        .list_tasks()
        .await
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();

    let live_tasks: std::collections::HashMap<String, (String, String)> = env
        .ui
        .active_delegate_tasks()
        .into_iter()
        .map(|(id, goal, act)| (id, (goal, act)))
        .collect();

    for task in tasks {
        let is_worker = task.command.starts_with("worker:");
        let is_sub =
            task.id.starts_with("sub_") || is_worker || task.command.starts_with("subagent:");

        let agent = if is_worker {
            sub_records
                .get(&task.id)
                .and_then(|rec| worker_label_from_context(rec.context.as_deref()))
                .unwrap_or_else(|| "Worker".to_string())
        } else if let Some(stripped) = task.command.strip_prefix("subagent: ") {
            if let Some(pos) = stripped.find(':') {
                let cand = stripped[..pos].trim();
                if !cand.is_empty()
                    && cand
                        .chars()
                        .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
                {
                    cand.to_string()
                } else {
                    "Subagent".to_string()
                }
            } else {
                "Subagent".to_string()
            }
        } else if task.id.starts_with("sub_") {
            "Subagent".to_string()
        } else {
            "Job".to_string()
        };

        let title = task
            .command
            .strip_prefix("worker: ")
            .or_else(|| task.command.strip_prefix("subagent: "))
            .unwrap_or(&task.command)
            .to_string();

        let live_act = live_tasks.get(&task.id).map(|(_, a)| a.clone());
        let activity = live_act.unwrap_or_else(|| {
            if task.status == "running" {
                "running".to_string()
            } else {
                task.status.clone()
            }
        });

        let mut turns = Vec::new();
        let mut log_tail = Vec::new();
        if is_sub {
            if let Ok(outcome) = env.store.replay_transcript(&task.id, usize::MAX).await {
                let parsed = parse_turns_from_history(&outcome.history);
                turns = if is_worker {
                    worker_turns_from_history(parsed, &title, &task.status)
                } else {
                    parsed
                };
            }
            if turns.is_empty() || task.status == "running" {
                log_tail = read_subagent_log_tail(&task.log_path, 80);
            }
        } else {
            let log_body = std::fs::read_to_string(&task.log_path).unwrap_or_default();
            turns.push(TurnRecord {
                user_query: task.command.clone(),
                reasoning: None,
                tools: Vec::new(),
                tool_items: Vec::new(),
                duration: std::time::Duration::from_secs(task.elapsed_secs),
                reply: if log_body.is_empty() {
                    "(no output yet)".to_string()
                } else {
                    log_body
                },
            });
        }

        entries.push(SubagentUiEntry {
            id: task.id,
            agent,
            title,
            status: task.status,
            activity,
            elapsed_secs: task.elapsed_secs,
            turns,
            log_tail,
        });
    }

    entries
}

pub async fn open_subagents_list(env: &Env<'_>, st: &mut TuiState) {
    let items = collect_subagent_entries(env).await;
    if items.is_empty() {
        env.ui
            .emit_above("No background tasks or subagents active or recorded");
        return;
    }
    let has_running = items.iter().any(|i| i.status == "running");
    st.modal = ActiveModal::SubagentsModal {
        items,
        selected: 0,
        active_only: has_running,
    };
}

pub async fn open_subagent_inspector(env: &Env<'_>, st: &mut TuiState, target_id: &str) {
    let items = collect_subagent_entries(env).await;
    if items.is_empty() {
        env.ui.emit_above("No subagents found");
        return;
    }
    let current_idx = items.iter().position(|e| e.id == target_id).unwrap_or(0);
    st.modal = ActiveModal::SubagentInspector {
        items,
        current_idx,
        scroll: 0,
    };
}

pub async fn interrupt_subagent(env: &Env<'_>, st: &mut TuiState, id: &str) {
    let killed = env.delegate_tool.kill(id).await;
    env.ui.finish_delegate_task(id);
    if killed {
        env.ui.emit_above(&format!("Stopped subagent [{id}]"));
    }
    match &mut st.modal {
        ActiveModal::SubagentsModal { items, .. } => {
            if let Some(item) = items.iter_mut().find(|i| i.id == id) {
                item.status = "cancelled".to_string();
                item.activity = "cancelled by user".to_string();
            }
        }
        ActiveModal::SubagentInspector {
            items, current_idx, ..
        } => {
            if let Some(item) = items.get_mut(*current_idx) {
                if item.id == id {
                    item.status = "cancelled".to_string();
                    item.activity = "cancelled by user".to_string();
                }
            }
        }
        _ => {}
    }
}

pub async fn refresh_subagents_if_open(env: &Env<'_>, st: &mut TuiState) {
    if !matches!(
        st.modal,
        ActiveModal::SubagentInspector { .. } | ActiveModal::SubagentsModal { .. }
    ) {
        return;
    }
    let sub_records: std::collections::HashMap<
        String,
        harness_core::tools::delegate::SubagentTaskRecord,
    > = env
        .delegate_tool
        .list_tasks()
        .await
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect();
    let now = chrono::Utc::now();

    match &mut st.modal {
        ActiveModal::SubagentInspector {
            items, current_idx, ..
        } => {
            if let Some(item) = items.get_mut(*current_idx) {
                let is_worker = item.agent == "Worker" || item.agent.starts_with("worker:");
                if let Some(rec) = sub_records.get(&item.id) {
                    let (status_str, end_at) = match &rec.status {
                        harness_core::tools::delegate::SubagentStatus::Running => {
                            ("running".to_string(), now)
                        }
                        harness_core::tools::delegate::SubagentStatus::Completed {
                            finished_at,
                            ..
                        } => ("completed".to_string(), *finished_at),
                        harness_core::tools::delegate::SubagentStatus::Failed {
                            error,
                            finished_at,
                        } => (format!("failed: {error}"), *finished_at),
                    };
                    item.status = status_str;
                    item.elapsed_secs = (end_at - rec.started_at).num_seconds().max(0) as u64;
                    if let Some(ref lp) = rec.log_path {
                        if item.turns.is_empty() || item.status == "running" {
                            item.log_tail = read_subagent_log_tail(lp, 80);
                        }
                    }
                }
                if let Ok(outcome) = env.store.replay_transcript(&item.id, usize::MAX).await {
                    let parsed = parse_turns_from_history(&outcome.history);
                    let replayed = if is_worker {
                        worker_turns_from_history(parsed, &item.title, &item.status)
                    } else {
                        parsed
                    };
                    if !replayed.is_empty() {
                        item.turns = replayed;
                    }
                }
                let tasks = env.ui.active_delegate_tasks();
                if let Some((_, _, act)) = tasks.into_iter().find(|(id, _, _)| id == &item.id) {
                    item.activity = act;
                } else if item.status != "running" {
                    item.activity = item.status.clone();
                }
            }
        }
        ActiveModal::SubagentsModal { items, .. } => {
            let tasks = env.ui.active_delegate_tasks();
            for item in items.iter_mut() {
                if let Some(rec) = sub_records.get(&item.id) {
                    let (status_str, end_at) = match &rec.status {
                        harness_core::tools::delegate::SubagentStatus::Running => {
                            ("running".to_string(), now)
                        }
                        harness_core::tools::delegate::SubagentStatus::Completed {
                            finished_at,
                            ..
                        } => ("completed".to_string(), *finished_at),
                        harness_core::tools::delegate::SubagentStatus::Failed {
                            error,
                            finished_at,
                        } => (format!("failed: {error}"), *finished_at),
                    };
                    item.status = status_str;
                    item.elapsed_secs = (end_at - rec.started_at).num_seconds().max(0) as u64;
                }
                if item.status == "running" {
                    if let Some((_, _, act)) = tasks.iter().find(|(id, _, _)| id == &item.id) {
                        item.activity = act.clone();
                    }
                } else {
                    item.activity = item.status.clone();
                }
            }
        }
        _ => {}
    }
}

async fn open_task_list(env: &Env<'_>, st: &mut TuiState) {
    open_subagents_list(env, st).await;
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
                let items = vec!["Dark Mode (default)".to_string(), "Light Mode".to_string()];
                let selected = match env.config.display.theme {
                    harness_core::ThemeMode::Dark => 0,
                    harness_core::ThemeMode::Light => 1,
                };
                st.modal = ActiveModal::SelectModal {
                    title: "Select Theme".to_string(),
                    items,
                    selected,
                    action: SelectAction::SettingsTheme,
                };
            }
            1 => {
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
            2..=8 => toggle_settings_main_item(env, st, choice).await,
            9 => open_provider_picker_for(env, st, ModelTarget::Approval).await,
            10 => {
                let items = vec![
                    if env.config.voice.enabled {
                        "Disable Voice Mode".to_string()
                    } else {
                        "Enable Voice Mode".to_string()
                    },
                    "Provider: Antigravity (Google Live Voice)".to_string(),
                    "Provider: OpenAI (Whisper STT + TTS)".to_string(),
                    "Provider: Groq (Whisper STT + Edge TTS)".to_string(),
                    format!(
                        "Toggle Barge-in Interrupt · [{}]",
                        if env.config.voice.enable_barge_in {
                            "Enabled"
                        } else {
                            "Disabled"
                        }
                    ),
                ];
                st.modal = ActiveModal::SelectModal {
                    title: "Voice Mode Settings".to_string(),
                    items,
                    selected: 0,
                    action: SelectAction::SettingsVoice,
                };
            }
            _ => {
                let summary = format_settings_summary(env.config, Some(env.feature_toggles));
                show_text_modal(st, "Current CLI Settings", &summary);
            }
        },
        SelectAction::SettingsVoice => {
            let arg = match choice {
                0 => {
                    if env.config.voice.enabled {
                        "voice off"
                    } else {
                        "voice on"
                    }
                }
                1 => "voice antigravity",
                2 => "voice openai",
                3 => "voice groq",
                4 => {
                    if env.config.voice.enable_barge_in {
                        "voice barge off"
                    } else {
                        "voice barge on"
                    }
                }
                _ => "voice show",
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
            } else {
                if env.config.voice.enabled {
                    activate_voice(env, st).await;
                } else if st.voice_active {
                    deactivate_voice(env, st).await;
                }
            }
            open_settings_menu(env, st, 10);
        }
        SelectAction::SettingsTheme => {
            let arg = if choice == 1 {
                "theme light"
            } else {
                "theme dark"
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
            } else {
                super::theme::set_active_theme(env.config.display.theme);
                st.settled_cache.clear();
                st.live_content_cache = Default::default();
                st.live_reasoning_cache = Default::default();
                open_settings_menu(env, st, 0);
            }
        }
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
                if provider == "custom" {
                    st.modal = ActiveModal::InputModal {
                        title: "Add Custom Provider — Name".to_string(),
                        prompt: "Custom provider name (e.g. coarena, vllm, local)".to_string(),
                        value: String::new(),
                        secret: false,
                        action: InputAction::CustomProviderName,
                    };
                } else {
                    st.modal = ActiveModal::InputModal {
                        title: "Add Provider — Label".to_string(),
                        prompt: format!("Credential label (blank uses '{provider}-primary')"),
                        value: String::new(),
                        secret: false,
                        action: InputAction::ApiKeyLabel {
                            provider: provider.clone(),
                        },
                    };
                }
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
                Ok(true) => {
                    env.ui
                        .emit_above(&format!("✔ Credential removed from {provider}"));
                    let ui_bg = env.ui.clone();
                    let prov_bg = env.current_provider.clone();
                    let mod_bg = env.current_model.clone();
                    let cred_bg = env.cred_manager.clone();
                    let sid_bg = env.session_id.clone();
                    tokio::spawn(async move {
                        refresh_antigravity_status_if_needed(
                            &ui_bg,
                            &prov_bg,
                            &mod_bg,
                            &cred_bg,
                            Some(&sid_bg),
                        )
                        .await;
                    });
                }
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
            let Some(id) = ids.get(choice).cloned() else {
                return;
            };
            let creds = env.cred_manager.get_credentials(&provider).await;
            let label = creds
                .iter()
                .find(|c| c.id == id)
                .map(|c| c.label.clone())
                .unwrap_or_else(|| id.clone());

            let has_override = env
                .cred_manager
                .get_session_priority_credential(env.session_id, &provider)
                .is_some();

            let mut items = vec![
                "This session only".to_string(),
                "Globally (default for all sessions)".to_string(),
            ];
            if has_override {
                items.push("Clear session priority (use global default)".to_string());
            }

            st.modal = ActiveModal::SelectModal {
                title: format!("Prioritize {label} ({provider}) for"),
                items,
                selected: 0,
                action: SelectAction::ConnectPriorityScope {
                    provider,
                    id,
                    has_override,
                },
            };
        }
        SelectAction::ConnectPriorityScope {
            provider,
            id,
            has_override,
        } => {
            match choice {
                0 => {
                    // This session only
                    env.cred_manager.set_session_priority_credential(
                        env.session_id,
                        &provider,
                        &id,
                    );
                    let _ = env
                        .store
                        .set_credential_priority(env.session_id, &provider, &id)
                        .await;
                    let current = env.cred_manager.get_rotation_strategy(&provider).await;
                    if current != RotationStrategy::FillFirst && current != RotationStrategy::Never
                    {
                        let _ = env
                            .cred_manager
                            .set_rotation_strategy(&provider, RotationStrategy::FillFirst)
                            .await;
                    }
                    env.ui.emit_above(&format!(
                        "✔ Priority credential set for this session ({provider})"
                    ));
                    let ui_bg = env.ui.clone();
                    let prov_bg = env.current_provider.clone();
                    let mod_bg = env.current_model.clone();
                    let cred_bg = env.cred_manager.clone();
                    let sid_bg = env.session_id.clone();
                    tokio::spawn(async move {
                        refresh_antigravity_status_if_needed(
                            &ui_bg,
                            &prov_bg,
                            &mod_bg,
                            &cred_bg,
                            Some(&sid_bg),
                        )
                        .await;
                    });
                }
                1 => {
                    // Globally (all sessions)
                    match env
                        .cred_manager
                        .set_priority_credential(&provider, &id)
                        .await
                    {
                        Ok(true) => {
                            env.cred_manager
                                .clear_session_priority_credential(env.session_id, &provider);
                            let _ = env
                                .store
                                .clear_credential_priority(env.session_id, &provider)
                                .await;
                            let current = env.cred_manager.get_rotation_strategy(&provider).await;
                            if current != RotationStrategy::FillFirst
                                && current != RotationStrategy::Never
                            {
                                let _ = env
                                    .cred_manager
                                    .set_rotation_strategy(&provider, RotationStrategy::FillFirst)
                                    .await;
                            }
                            env.ui.emit_above(&format!(
                                "✔ Priority credential set globally for {provider}"
                            ));
                            let ui_bg = env.ui.clone();
                            let prov_bg = env.current_provider.clone();
                            let mod_bg = env.current_model.clone();
                            let cred_bg = env.cred_manager.clone();
                            let sid_bg = env.session_id.clone();
                            tokio::spawn(async move {
                                refresh_antigravity_status_if_needed(
                                    &ui_bg,
                                    &prov_bg,
                                    &mod_bg,
                                    &cred_bg,
                                    Some(&sid_bg),
                                )
                                .await;
                            });
                        }
                        Ok(false) => env.ui.set_transient_error(Some(
                            "Failed to set priority credential".to_string(),
                        )),
                        Err(error) => env
                            .ui
                            .set_transient_error(Some(format!("Priority failed: {error}"))),
                    }
                }
                2 if has_override => {
                    // Clear session priority
                    env.cred_manager
                        .clear_session_priority_credential(env.session_id, &provider);
                    let _ = env
                        .store
                        .clear_credential_priority(env.session_id, &provider)
                        .await;
                    env.ui.emit_above(&format!(
                        "✔ Cleared session priority override for {provider} (following global default)"
                    ));
                    let ui_bg = env.ui.clone();
                    let prov_bg = env.current_provider.clone();
                    let mod_bg = env.current_model.clone();
                    let cred_bg = env.cred_manager.clone();
                    let sid_bg = env.session_id.clone();
                    tokio::spawn(async move {
                        refresh_antigravity_status_if_needed(
                            &ui_bg,
                            &prov_bg,
                            &mod_bg,
                            &cred_bg,
                            Some(&sid_bg),
                        )
                        .await;
                    });
                }
                _ => {}
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
                Ok(count) => {
                    env.ui
                        .emit_above(&format!("✔ {count} credential(s) re-enabled"));
                    let ui_bg = env.ui.clone();
                    let prov_bg = env.current_provider.clone();
                    let mod_bg = env.current_model.clone();
                    let cred_bg = env.cred_manager.clone();
                    let sid_bg = env.session_id.clone();
                    tokio::spawn(async move {
                        refresh_antigravity_status_if_needed(
                            &ui_bg,
                            &prov_bg,
                            &mod_bg,
                            &cred_bg,
                            Some(&sid_bg),
                        )
                        .await;
                    });
                }
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
        SelectAction::MemoryProject { projects } => {
            if choice == 0 {
                st.memory_project_filter = None;
                env.agent.lock().await.set_memory_project_filter(None);
                st.system_notices.push(SystemNotice::new(
                    "✓ Memory scope reset to all projects (default)",
                ));
            } else if choice <= projects.len() {
                let project = projects[choice - 1].clone();
                st.memory_project_filter = Some(project.clone());
                env.agent
                    .lock()
                    .await
                    .set_memory_project_filter(Some(project.clone()));
                st.system_notices.push(SystemNotice::new(format!(
                    "✓ Memory scoped to '{project}' for this session"
                )));
            } else {
                st.modal = ActiveModal::InputModal {
                    title: "Filter Memory by Project".to_string(),
                    prompt: "Project name / slug (blank resets to all projects)".to_string(),
                    value: st.memory_project_filter.clone().unwrap_or_default(),
                    secret: false,
                    action: InputAction::MemoryCustomProject,
                };
            }
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
    let ui_bg = env.ui.clone();
    let prov_bg = env.current_provider.clone();
    let mod_bg = env.current_model.clone();
    let cred_bg = env.cred_manager.clone();
    let sid_bg = env.session_id.clone();
    tokio::spawn(async move {
        refresh_antigravity_status_if_needed(&ui_bg, &prov_bg, &mod_bg, &cred_bg, Some(&sid_bg))
            .await;
    });
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
            let ui_bg = env.ui.clone();
            let prov_bg = env.current_provider.clone();
            let mod_bg = env.current_model.clone();
            let cred_bg = env.cred_manager.clone();
            let sid_bg = env.session_id.clone();
            tokio::spawn(async move {
                refresh_antigravity_status_if_needed(
                    &ui_bg,
                    &prov_bg,
                    &mod_bg,
                    &cred_bg,
                    Some(&sid_bg),
                )
                .await;
            });
            st.provider_items = build_provider_items(
                env.cred_manager.clone(),
                Some(&env.config.providers),
                env.current_provider,
            )
            .await;
            open_connect_menu(st);
        }
        InputAction::CustomProviderName => {
            let name = value.trim().to_lowercase();
            if name.is_empty() {
                env.ui.emit_above("No provider name entered — cancelled");
                open_connect_menu(st);
                return;
            }
            if !name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                env.ui.set_transient_error(Some(
                    "Provider name may only contain alphanumeric characters, hyphens, and underscores"
                        .to_string(),
                ));
                st.modal = ActiveModal::InputModal {
                    title: "Add Custom Provider — Name".to_string(),
                    prompt: "Invalid characters. Use alphanumeric, hyphen, underscore:".to_string(),
                    value: name,
                    secret: false,
                    action: InputAction::CustomProviderName,
                };
                return;
            }
            st.modal = ActiveModal::InputModal {
                title: format!("Add Custom Provider [{name}] — Endpoint"),
                prompt: format!("Base URL endpoint for {name} (e.g. https://api.example.com/v1)"),
                value: "http://localhost:8000/v1".to_string(),
                secret: false,
                action: InputAction::CustomProviderEndpoint { provider: name },
            };
        }
        InputAction::CustomProviderEndpoint { provider } => {
            let endpoint = value.trim().to_string();
            if endpoint.is_empty() {
                env.ui.emit_above("No endpoint entered — cancelled");
                open_connect_menu(st);
                return;
            }
            st.modal = ActiveModal::InputModal {
                title: format!("Add Custom Provider [{provider}] — API Key"),
                prompt: format!("API key for {provider} (masked, leave blank if unauthenticated)"),
                value: String::new(),
                secret: true,
                action: InputAction::CustomProviderApiKey { provider, endpoint },
            };
        }
        InputAction::CustomProviderApiKey { provider, endpoint } => {
            let key = if value.trim().is_empty() {
                "none".to_string()
            } else {
                value.trim().to_string()
            };
            let label = format!("{provider}-primary");

            let layers =
                harness_core::config::process_layers(crate::commands::config::catalog_defaults());
            if let Ok(ref layers) = layers {
                if let Err(err) = harness_core::config::set_in_file(
                    layers,
                    &format!("providers.{provider}.base_url"),
                    &endpoint,
                ) {
                    tracing::warn!(
                        "Failed to persist custom provider base_url to config file: {err}"
                    );
                }
            }

            let override_cfg = harness_core::config::ProviderOverride {
                base_url: Some(endpoint.clone()),
                models_endpoint: None,
                timeout_secs: None,
                context_window: None,
            };
            env.config
                .providers
                .insert(provider.clone(), override_cfg.clone());
            env.discovery.set_override(&provider, override_cfg);

            match env
                .cred_manager
                .add_credential(&provider, &label, &key, true)
                .await
            {
                Ok(id) => {
                    env.ui.emit_above(&format!(
                        "✔ Custom provider '{provider}' ({endpoint}) created with credential '{label}' ({id})"
                    ));
                }
                Err(error) => {
                    env.ui
                        .set_transient_error(Some(format!("Add credential failed: {error}")));
                }
            }

            st.provider_items = build_provider_items(
                env.cred_manager.clone(),
                Some(&env.config.providers),
                env.current_provider,
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
        InputAction::MemoryCustomProject => {
            let trimmed = value.trim();
            if trimmed.is_empty()
                || matches!(
                    trimmed.to_ascii_lowercase().as_str(),
                    "reset" | "all" | "default" | "clear"
                )
            {
                st.memory_project_filter = None;
                env.agent.lock().await.set_memory_project_filter(None);
                st.system_notices.push(SystemNotice::new(
                    "✓ Memory scope reset to all projects (default)",
                ));
            } else {
                let project = trimmed.to_string();
                st.memory_project_filter = Some(project.clone());
                env.agent
                    .lock()
                    .await
                    .set_memory_project_filter(Some(project.clone()));
                st.system_notices.push(SystemNotice::new(format!(
                    "✓ Memory scoped to '{project}' for this session"
                )));
            }
        }
    }
}

async fn apply_feature_live(env: &Env<'_>, feature: Feature, enabled: bool) {
    env.feature_toggles.set(feature, enabled);
    let registry = {
        let mut ag = env.agent.lock().await;
        match feature {
            Feature::Memory => ag.set_memory_enabled(enabled),
            Feature::Notes => ag.set_notes_enabled(enabled),
            Feature::Skills => ag.set_skills_enabled(enabled),
            Feature::Mcp => ag.set_mcp_enabled(enabled),
        }
        ag.registry().clone()
    };
    if enabled && feature == Feature::Mcp {
        if let Err(err) =
            crate::commands::chat::helpers::ensure_mcp_servers_registered(&registry, env.config)
                .await
        {
            env.ui
                .set_transient_error(Some(format!("Failed to start MCP servers: {err}")));
        }
    }
}

async fn apply_feature_global(
    env: &mut Env<'_>,
    feature: Feature,
    enabled: bool,
    disabled_until: Option<u64>,
) -> Result<(), HarnessError> {
    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    let flag_str = if enabled { "true" } else { "false" };
    let (agent_key, until_key) = match feature {
        Feature::Mcp => ("agent.mcp_enabled", "features.mcp_disabled_until"),
        Feature::Skills => ("agent.skills_enabled", "features.skills_disabled_until"),
        Feature::Memory => ("agent.memory_enabled", "features.memory_disabled_until"),
        Feature::Notes => ("agent.notes_enabled", "features.notes_disabled_until"),
    };

    harness_core::config::set_in_file(&layers, agent_key, flag_str)?;
    if feature == Feature::Mcp {
        harness_core::config::set_in_file(&layers, "mcp.enabled", flag_str)?;
    }
    match disabled_until {
        Some(until) => {
            harness_core::config::set_in_file(&layers, until_key, &until.to_string())?;
        }
        None => {
            let _ = harness_core::config::unset_in_file(&layers, until_key)?;
        }
    }

    match feature {
        Feature::Mcp => {
            env.config.mcp.enabled = enabled;
            env.config.agent.mcp_enabled = enabled;
            env.config.features.mcp_disabled_until = disabled_until;
        }
        Feature::Skills => {
            env.config.agent.skills_enabled = enabled;
            env.config.features.skills_disabled_until = disabled_until;
        }
        Feature::Memory => {
            env.config.agent.memory_enabled = enabled;
            env.config.features.memory_disabled_until = disabled_until;
        }
        Feature::Notes => {
            env.config.agent.notes_enabled = enabled;
            env.config.features.notes_disabled_until = disabled_until;
        }
    }

    let now = harness_core::Config::current_unix_secs();
    let effective = enabled && !disabled_until.is_some_and(|u| now < u);
    apply_feature_live(env, feature, effective).await;
    Ok(())
}

async fn apply_memory_mode_choice(
    env: &mut Env<'_>,
    mode: harness_core::MemoryMode,
) -> Result<(), HarnessError> {
    let layers = harness_core::config::process_layers(crate::commands::config::catalog_defaults())?;
    if mode.is_background() {
        let (memory_dyn, notes_dyn) = {
            let ag = env.agent.lock().await;
            let mem: Arc<dyn MemoryEngine> = ag.memory().clone();
            let notes: Arc<dyn harness_core::NotesEngine> = ag.notes().clone();
            (mem, notes)
        };
        let provider = env
            .config
            .memory
            .background_provider
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(env.current_provider.as_str());
        let model = env
            .config
            .memory
            .background_model
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(env.current_model.as_str());
        let saver = crate::commands::chat::memory_writer::build_saver_for(
            env.config,
            &memory_dyn,
            notes_dyn,
            provider,
            model,
            env.cred_manager.clone(),
            env.discovery.clone(),
        )?;
        harness_core::config::set_in_file(&layers, "memory.mode", mode.as_str())?;
        env.config.memory.mode = mode;
        let mut ag = env.agent.lock().await;
        ag.set_background_saver(Some(Arc::new(saver)));
        ag.set_memory_enabled(env.feature_toggles.enabled(Feature::Memory));
        ag.set_notes_enabled(env.feature_toggles.enabled(Feature::Notes));
    } else {
        harness_core::config::set_in_file(&layers, "memory.mode", mode.as_str())?;
        env.config.memory.mode = mode;
        env.agent.lock().await.set_background_saver(None);
    }
    Ok(())
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
        let text = submitted.expanded_text;
        env.in_flight_tools
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(TurnProcessItem::user_message(&text));
        st.jump_to_latest();
        match env.interrupt_handle.interrupt(&text) {
            harness_core::InterruptOutcome::Streaming => {
                st.system_notices.push(SystemNotice::new(
                    "↳ Redirecting — stopping this reply to take your message",
                ));
            }
            harness_core::InterruptOutcome::Queued => {
                st.system_notices.push(SystemNotice::new(
                    "↳ Held until the running tool finishes, then sent",
                ));
            }
            harness_core::InterruptOutcome::Idle => {
                st.queue.push_back(text);
                env.ui.set_queue_depth(st.queue.len());
            }
        }
        return Ok(Flow::Continue);
    }

    let display_opt = (submitted.display_text != submitted.expanded_text)
        .then_some(submitted.display_text.as_str());
    env.ui.clear_transient_error();
    st.jump_to_latest();
    st.system_notices.clear();
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

pub async fn handle_voice_slash_command(
    env: &mut Env<'_>,
    st: &mut TuiState,
    arg: Option<&str>,
) -> Result<(), HarnessError> {
    let sub = arg.map(str::trim).map(str::to_ascii_lowercase);
    match sub.as_deref() {
        Some("status") => {
            let status = if st.voice_active {
                format!(
                    "🎙 Voice mode is ON (provider: {}, wake word: '{}', barge-in: {})",
                    env.config.voice.provider,
                    env.config.voice.wake_word,
                    if env.config.voice.enable_barge_in {
                        "enabled"
                    } else {
                        "disabled"
                    }
                )
            } else {
                "🎙 Voice mode is OFF (run `/voice on` to enable or `/voice setup` to configure)"
                    .to_string()
            };
            st.system_notices.push(SystemNotice::new(status));
        }
        Some("setup" | "config" | "settings") => {
            open_settings_menu(env, st, 10);
        }
        Some("off" | "stop" | "disable") => {
            if st.voice_active {
                deactivate_voice(env, st).await;
            } else {
                st.system_notices
                    .push(SystemNotice::new("• Voice mode is already off"));
            }
        }
        Some("on" | "start" | "enable") => {
            if !st.voice_active {
                activate_voice(env, st).await;
            } else {
                st.system_notices.push(SystemNotice::new(
                    "✔ Voice mode is already active and listening",
                ));
            }
        }
        None => {
            if st.voice_active {
                deactivate_voice(env, st).await;
            } else {
                activate_voice(env, st).await;
            }
        }
        Some(other) => {
            st.system_notices.push(SystemNotice::new(format!(
                "• Unknown voice option '{other}'. Usage: /voice [on|off|status|setup]"
            )));
        }
    }
    Ok(())
}

pub async fn activate_voice(env: &Env<'_>, st: &mut TuiState) {
    st.voice_active = true;
    env.agent.lock().await.set_voice_mode(true);

    if let Some(old) = st.voice_cancel.take() {
        old.cancel();
    }
    let cancel = CancellationToken::new();
    st.voice_cancel = Some(cancel.clone());

    let voice_tx = st.voice_tx.clone();
    let is_playing = st.voice_is_playing.clone();
    let config = env.config.clone();
    let cred_manager = env.cred_manager.clone();
    let discovery = env.discovery.clone();
    tokio::spawn(async move {
        crate::commands::voice::run_background_voice_listener(
            config,
            cred_manager,
            discovery,
            voice_tx,
            is_playing,
            cancel,
        )
        .await;
    });

    st.system_notices.push(SystemNotice::new(
        "✔ Voice mode activated. Listening in background (type and talk anytime!). Voice-compatible output summaries enabled.",
    ));
}

pub async fn deactivate_voice(env: &Env<'_>, st: &mut TuiState) {
    st.voice_active = false;
    env.agent.lock().await.set_voice_mode(false);

    if let Some(cancel) = st.voice_cancel.take() {
        cancel.cancel();
    }
    if let Some(pb_cancel) = st.voice_playback_cancel.take() {
        pb_cancel.cancel();
    }
    st.voice_is_playing.store(false, Ordering::SeqCst);

    st.system_notices
        .push(SystemNotice::new("• Voice mode deactivated."));
}

pub async fn handle_voice_event(
    env: &Env<'_>,
    st: &mut TuiState,
    evt: crate::commands::voice::VoiceEvent,
) {
    match evt {
        crate::commands::voice::VoiceEvent::Transcript(query) => {
            handle_incoming_voice_input(env, st, query).await;
        }
        crate::commands::voice::VoiceEvent::Notice(msg) => {
            st.system_notices.push(SystemNotice::new(msg));
        }
        crate::commands::voice::VoiceEvent::Error(err) => {
            env.ui.set_transient_error(Some(err.clone()));
            st.system_notices
                .push(SystemNotice::new(format!("✖ {err}")));
        }
    }
}

pub async fn handle_incoming_voice_input(env: &Env<'_>, st: &mut TuiState, voice_query: String) {
    let trimmed = voice_query.trim();
    if trimmed.is_empty() {
        return;
    }
    // Barge-in: interrupt ongoing speech playback immediately
    if let Some(cancel) = st.voice_playback_cancel.take() {
        cancel.cancel();
    }

    st.system_notices
        .push(SystemNotice::new(format!("🎤 Heard: \"{trimmed}\"")));
    if st.active_turn.is_some() {
        match env.interrupt_handle.interrupt(trimmed) {
            harness_core::InterruptOutcome::Streaming => {
                st.system_notices
                    .push(SystemNotice::new("↳ Voice interrupting current turn"));
            }
            harness_core::InterruptOutcome::Queued => {
                st.system_notices
                    .push(SystemNotice::new("↳ Voice queued after current tool"));
            }
            harness_core::InterruptOutcome::Idle => {
                st.queue.push_back(trimmed.to_string());
                env.ui.set_queue_depth(st.queue.len());
            }
        }
    } else if env.compacting.load(Ordering::SeqCst) {
        st.queue.push_back(trimmed.to_string());
        env.ui.set_queue_depth(st.queue.len());
    } else {
        st.jump_to_latest();
        let display = format!("🎤 {trimmed}");
        launch_turn(
            trimmed,
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
                let find_prov = |id: &str| {
                    harness_providers::catalog::find_provider(id).or_else(|| {
                        env.config.providers.get(id).map(|o| {
                            harness_providers::catalog::custom_provider_descriptor(
                                id,
                                o.base_url.as_deref().unwrap_or(""),
                            )
                        })
                    })
                };
                if let Some((prov_prefix, model_rest)) = target.split_once('/') {
                    if let Some(prov) = find_prov(prov_prefix) {
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
                if let Some(prov) = find_prov(target) {
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
        "/automation" if arg.is_none() => {
            st.composer
                .set_text(harness_scheduler::AUTOMATION_PREFILL.to_string());
            return Ok(Flow::Continue);
        }
        "/thinking" | "/think" | "/reasoning" if arg.is_none() => {
            open_thinking_picker(env, st).await;
            return Ok(Flow::Continue);
        }
        "/voice" => {
            handle_voice_slash_command(env, st, arg).await?;
            return Ok(Flow::Continue);
        }
        "/settings" if arg.is_none() => {
            open_settings_menu(env, st, 0);
            return Ok(Flow::Continue);
        }
        "/auto-compact" | "/autocompact" if arg.is_none() => {
            open_auto_compact_threshold(env, st).await;
            return Ok(Flow::Continue);
        }
        "/compact" if arg.is_none() => {
            if st.active_turn.is_some()
                || env.compacting.load(Ordering::SeqCst)
                || env.ui.is_compacting()
            {
                env.ui.set_transient_error(Some(
                    "Cannot compact while a turn is running or compaction is in progress"
                        .to_string(),
                ));
                return Ok(Flow::Continue);
            }
            env.ui.start_compaction(true);
            env.compacting.store(true, Ordering::SeqCst);
            env.ui.notify_tui();
            let ui = env.ui.clone();
            let agent = env.agent.clone();
            let compacting = env.compacting.clone();
            let context_max = crate::commands::context_window::context_window(
                env.config,
                env.discovery.as_ref(),
                env.current_provider,
                env.current_model,
            );
            tokio::spawn(async move {
                let Ok(ag) = agent.try_lock() else {
                    compacting.store(false, Ordering::SeqCst);
                    ui.finish_compaction();
                    ui.emit_above(
                        "✖ Cannot compact while a turn is running: interrupt it, then run /compact",
                    );
                    return;
                };
                match ag.compact_current_session().await {
                    Ok(_notice) => {
                        let context_used = ag.estimate_session_tokens().await.unwrap_or(0);
                        ui.update_status(context_used, context_max);
                    }
                    Err(e) => {
                        compacting.store(false, Ordering::SeqCst);
                        ui.finish_compaction();
                        ui.emit_above(&format!("✖ Failed to compact session: {e}"));
                    }
                }
            });
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
        "/memory" => {
            let trimmed = arg.map(str::trim).filter(|s| !s.is_empty());
            match trimmed {
                None => {
                    open_memory_project_picker(st).await;
                    return Ok(Flow::Continue);
                }
                Some(val) => {
                    let lower = val.to_ascii_lowercase();
                    if matches!(lower.as_str(), "list" | "projects") {
                        open_memory_project_picker(st).await;
                        return Ok(Flow::Continue);
                    }
                    if matches!(lower.as_str(), "reset" | "all" | "default" | "clear") {
                        st.memory_project_filter = None;
                    } else if !matches!(
                        lower.as_str(),
                        "on" | "off"
                            | "enable"
                            | "disable"
                            | "enabled"
                            | "disabled"
                            | "true"
                            | "false"
                    ) {
                        st.memory_project_filter = Some(val.to_string());
                    }
                }
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
            st.jump_to_latest();
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
            st.prune_stale_notices();
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
            st.settled_cache.clear();
            if st.turns.len() < prev_len {
                st.turn_meta.truncate(st.turns.len());
            }
            sync_turn_meta_len(env, st);
            drain_tui_notices(env, st);
            refresh_tabs(env, st).await;
            st.jump_to_latest();
            Ok(Flow::Continue)
        }
        SlashOutcome::Turn { prompt, display } => {
            let _ = env.ui.end_command_capture();
            if env.compacting.load(Ordering::SeqCst) {
                st.queue.push_back(prompt);
                env.ui.set_queue_depth(st.queue.len());
            } else {
                st.jump_to_latest();
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
            st.jump_to_latest();
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
        SlashOutcome::WorkerSpawned(info) => {
            let _ = env.ui.end_command_capture();
            env.ui.start_delegate_task(
                &info.child_id,
                &format!("[{}] {}", info.worker_name, info.task),
            );
            let args = serde_json::json!({
                "task_id": info.child_id,
                "worker": info.worker_name,
                "provider": info.provider,
                "model": info.model,
                "tasks": [{ "goal": info.task }],
            });
            let item = TurnProcessItem {
                tool_name: "delegate_task".to_string(),
                call_formatted: format!("Subagent [{}]: {}", info.worker_name, info.task),
                result_formatted: "running · starting (click or Ctrl+B to inspect)".to_string(),
                diff_lines: Vec::new(),
                output: String::new(),
                output_dropped: 0,
                arguments: Some(args),
                duration_secs: None,
                is_error: false,
            };
            st.turns.push(TurnRecord {
                user_query: display.trim().to_string(),
                reasoning: None,
                tools: Vec::new(),
                tool_items: vec![item],
                duration: std::time::Duration::ZERO,
                reply: String::new(),
            });
            st.turn_meta.push(TurnMeta {
                mode: format!("Subagent · {}", info.worker_name),
                model: format!("{}/{}", info.provider, info.model),
                output_tokens: None,
            });
            sync_turn_meta_len(env, st);
            st.settled_cache.clear();
            drain_tui_notices(env, st);
            st.jump_to_latest();
            Ok(Flow::Continue)
        }
        SlashOutcome::Exit => Ok(Flow::Break),
    }
}

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
            if st.voice_active && !outcome.reply.trim().is_empty() {
                if let Some(prev) = st.voice_playback_cancel.take() {
                    prev.cancel();
                }
                let pb_cancel = CancellationToken::new();
                st.voice_playback_cancel = Some(pb_cancel.clone());
                let reply_to_speak = outcome.reply.clone();
                let config_clone = env.config.clone();
                let cred_manager_clone = env.cred_manager.clone();
                let voice_tx_clone = st.voice_tx.clone();
                let is_playing_clone = st.voice_is_playing.clone();
                tokio::spawn(async move {
                    crate::commands::voice::play_tts_response(
                        &reply_to_speak,
                        &config_clone,
                        &cred_manager_clone,
                        Some(voice_tx_clone),
                        is_playing_clone,
                        pb_cancel,
                    )
                    .await;
                });
            }
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

fn advance_goal_tui(env: &Env<'_>, st: &mut TuiState, reply: &str) {
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
    if let Ok(replayed) = env
        .store
        .replay_transcript(
            env.session_id.as_str(),
            env.config.session.resume_max_messages,
        )
        .await
    {
        st.turns = parse_turns_from_history(&replayed.history);
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
    if let Some(cancel) = st.voice_playback_cancel.take() {
        cancel.cancel();
    }
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

    #[test]
    fn compaction_notice_expires_after_ttl() {
        use super::SystemNotice;
        let notice = SystemNotice {
            text: "✓ Compacted 1 turns (10 messages) into 1 summary".to_string(),
            created_at: std::time::Instant::now() - std::time::Duration::from_secs(6),
        };
        assert!(notice.is_stale(std::time::Instant::now(), true, false));

        let fresh_notice = SystemNotice::new("✓ Compacted 1 turns (10 messages) into 1 summary");
        assert!(!fresh_notice.is_stale(std::time::Instant::now(), true, false));
    }

    #[test]
    fn subagent_ui_entry_state_and_cancellation() {
        use super::SubagentUiEntry;
        let mut entry = SubagentUiEntry {
            id: "sub_100".to_string(),
            agent: "Explore".to_string(),
            title: "Inspect directory".to_string(),
            status: "running".to_string(),
            activity: "starting".to_string(),
            elapsed_secs: 5,
            turns: Vec::new(),
            log_tail: Vec::new(),
        };
        assert_eq!(entry.status, "running");
        entry.status = "cancelled".to_string();
        entry.activity = "cancelled by user".to_string();
        assert_eq!(entry.status, "cancelled");
        assert_eq!(entry.activity, "cancelled by user");
    }

    #[test]
    fn worker_turns_from_history_excludes_parent_fork_history() {
        use super::worker_turns_from_history;
        use crate::commands::chat::render::TurnRecord;

        let turn = |q: &str, r: &str| TurnRecord {
            user_query: q.to_string(),
            reasoning: None,
            tools: Vec::new(),
            tool_items: Vec::new(),
            duration: std::time::Duration::ZERO,
            reply: r.to_string(),
        };

        let parent_only = vec![turn("hello", "hi"), turn("check status", "ok")];
        // While running before the worker persists its own turn, parent turns must not appear.
        assert!(worker_turns_from_history(parent_only.clone(), "fix bug", "running").is_empty());

        let with_worker = vec![
            turn("hello", "hi"),
            turn("check status", "ok"),
            turn("GOAL: fix bug", "fixed"),
        ];
        let sliced = worker_turns_from_history(with_worker, "fix bug", "running");
        assert_eq!(sliced.len(), 1);
        assert_eq!(sliced[0].user_query, "GOAL: fix bug");
        assert_eq!(sliced[0].reply, "fixed");
    }

    #[test]
    fn build_approval_question_prompt_includes_diff_and_question() {
        let req = harness_core::ApprovalRequest {
            tool: "patch".to_string(),
            call_id: "c1".to_string(),
            target: Some("src/main.rs".to_string()),
            diff: Some(
                "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-old\n+new\n".to_string(),
            ),
            arguments: serde_json::json!({ "path": "src/main.rs" }),
        };
        let prompt = super::build_approval_question_prompt(&req, "Is +new safe?");
        assert!(prompt.contains("Pending tool call: patch"));
        assert!(prompt.contains("Target: src/main.rs"));
        assert!(prompt.contains("+new"));
        assert!(prompt.contains("Is +new safe?"));
    }

    #[tokio::test]
    async fn priority_scope_modal_actions() {
        let temp_dir = tempfile::tempdir().unwrap();
        let cred_path = temp_dir.path().join("credentials.json");
        let cred_mgr = harness_providers::CredentialManager::new(Some(cred_path)).unwrap();
        cred_mgr
            .add_credential("antigravity", "account-1", "token-1", true)
            .await
            .unwrap();
        cred_mgr
            .add_credential("antigravity", "account-2", "token-2", false)
            .await
            .unwrap();

        // 1. Session scope overrides for session s1
        cred_mgr.set_session_priority_credential("s1", "antigravity", "account-2");
        assert_eq!(
            cred_mgr.get_session_priority_credential("s1", "antigravity"),
            Some("account-2".to_string())
        );
        assert_eq!(
            cred_mgr.get_session_priority_credential("s2", "antigravity"),
            None
        );

        // 2. Global scope clears session override for s1 and sets global priority
        cred_mgr
            .set_priority_credential("antigravity", "account-2")
            .await
            .unwrap();
        cred_mgr.clear_session_priority_credential("s1", "antigravity");
        assert_eq!(
            cred_mgr.get_session_priority_credential("s1", "antigravity"),
            None
        );
    }
}
