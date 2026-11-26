//! sessions

use super::*;

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
            .enabled(crate::commands::chat::commands::Feature::Skills),
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
pub(crate) fn park_current_session(env: &Env<'_>, st: &mut TuiState) {
    let old_id = env.session_id.clone();
    let fresh_idle = IdleMemoryPass::new(0);
    let mut pending_modals = std::mem::take(&mut st.pending_modals);
    if matches!(
        st.modal,
        ActiveModal::Approval { .. } | ActiveModal::Clarify { .. } | ActiveModal::Sudo { .. }
    ) {
        let modal = std::mem::replace(&mut st.modal, ActiveModal::None);
        match modal {
            ActiveModal::Approval { req, resp_tx, .. } => {
                pending_modals.push_front(
                    crate::commands::chat::tui::state::PendingModal::Approval { req, resp_tx },
                );
            }
            ActiveModal::Clarify { req, resp_tx, .. } => {
                pending_modals.push_front(
                    crate::commands::chat::tui::state::PendingModal::Clarify { req, resp_tx },
                );
            }
            ActiveModal::Sudo {
                prompt, resp_tx, ..
            } => {
                pending_modals.push_front(crate::commands::chat::tui::state::PendingModal::Sudo {
                    prompt,
                    resp_tx,
                });
            }
            _ => {}
        }
    }

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
        expanded_turns: std::mem::take(&mut st.expanded_turns),
        collapsed_turns: std::mem::take(&mut st.collapsed_turns),
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
        compacted_added: st.compacted_added,
        compacted_deleted: st.compacted_deleted,
        session_max_added: st.session_max_added,
        session_max_deleted: st.session_max_deleted,
        pending_modals,
        has_unread_done: false,
        active_orchestrator: st.active_orchestrator.take(),
    };
    st.background_sessions.insert(old_id, slot);
}

/// Park the current session if it contains conversation messages; otherwise discard and delete
/// it from the session store so abandoned/empty sessions never pollute history.
pub(crate) async fn park_or_discard_current_session(env: &Env<'_>, st: &mut TuiState) {
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

pub(crate) fn restore_background_session(
    env: &mut Env<'_>,
    st: &mut TuiState,
    slot: BackgroundSession,
) {
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
    st.live_content_cache = crate::commands::chat::tui::state::LiveContentCache::default();
    st.live_reasoning_cache = crate::commands::chat::tui::state::LiveReasoningCache::default();
    st.system_notices = slot.system_notices;
    st.prune_stale_notices();
    st.expanded_thinking = slot.expanded_thinking;
    st.expanded_tools = slot.expanded_tools;
    st.expanded_turns = slot.expanded_turns;
    st.collapsed_turns = slot.collapsed_turns;
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
    st.compacted_added = slot.compacted_added;
    st.compacted_deleted = slot.compacted_deleted;
    st.session_max_added = slot.session_max_added;
    st.session_max_deleted = slot.session_max_deleted;
    st.diff_anim_target_added = slot.session_max_added;
    st.diff_anim_target_deleted = slot.session_max_deleted;
    st.pending_modals = slot.pending_modals;
    st.active_orchestrator = slot.active_orchestrator;
    if let Some(ref name) = st.active_orchestrator {
        if let Some(orch) = env.config.orchestrators.get(name) {
            let mut pool: Vec<(
                Arc<dyn harness_core::ModelProvider>,
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
                        Arc::new(client.with_session_id(env.session_id.as_str())),
                        sub.thinking,
                    ));
                }
            }
            env.delegate_tool.set_subagent_pool(pool);
            let prompt_block = super::orchestrator::build_orchestrator_prompt_block(name, orch);
            if let Ok(mut ag) = env.agent.try_lock() {
                ag.set_orchestrator_prompt(Some(prompt_block));
            }
        }
    } else {
        env.delegate_tool.set_subagent_pool(Vec::new());
        if let Ok(mut ag) = env.agent.try_lock() {
            ag.set_orchestrator_prompt(None);
        }
    }
    st.promote_pending_modal();
    st.diff_anim_from_added = slot.session_max_added;
    st.diff_anim_from_deleted = slot.session_max_deleted;
    st.diff_anim_start = None;
    env.feature_toggles.restore(slot.feature_toggles);

    env.todo_scope.set_session(env.session_id.as_str());
    todo::refresh_panel(&env.ui, &env.todo_scope);
    apply_agent_mode(env, st, st.mode);
}

type SessionRuntimeBundle = (
    Arc<tokio::sync::Mutex<crate::commands::chat::ChatAgent>>,
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
pub(crate) fn instantiate_session_runtime(
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
            session_id,
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
    st.expanded_turns.clear();
    st.collapsed_turns.clear();
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
    st.expanded_turns.clear();
    st.collapsed_turns.clear();
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
