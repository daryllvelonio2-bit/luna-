//! Tauri IPC command handlers for the Luna native desktop application.

use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tauri::State;
use tokio_util::sync::CancellationToken;

use harness_app::events::AppEvent;
use harness_app::session::create_session as create_store_session;
use harness_core::{
    ApprovalDecision, ClarifyResponse, MemoryEngine, Message, NotesEngine, SessionRecord,
    SessionStore,
};
use harness_memory::build_memory_engine;
use harness_notes::FileNotesEngine;
use harness_plugins::{PluginAgentEntry, PluginBuilderSpec, PluginSummary};
use harness_providers::{get_provider_catalog, GenericProviderClient};
use harness_scheduler::{DeliveryTarget, ScheduleTask};

use crate::state::GuiState;

#[derive(Serialize)]
pub struct GuiSessionItem {
    pub record: SessionRecord,
    pub plugin_agent_id: Option<String>,
}

#[derive(Serialize)]
pub struct GuiProviderInfo {
    pub id: String,
    pub display_name: String,
    pub configured: bool,
    pub models: Vec<String>,
}

#[derive(Serialize)]
pub struct GuiBootstrapPayload {
    pub workspace_root: String,
    pub default_provider: String,
    pub default_model: String,
    pub sessions: Vec<GuiSessionItem>,
    pub sidebar_agents: Vec<PluginAgentEntry>,
    pub plugins: Vec<PluginSummary>,
    pub schedules: Vec<ScheduleTask>,
}

#[derive(Serialize)]
pub struct TranscriptMessageDto {
    pub role: String,
    pub content: Option<String>,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<serde_json::Value>,
    pub tool_call_id: Option<String>,
}

#[derive(Deserialize)]
pub struct CreateScheduleArgs {
    pub name: String,
    pub instruction: String,
    pub rule: String,
    pub target: Option<String>,
    pub session_id: Option<String>,
}

#[derive(Deserialize)]
pub struct UpdateScheduleArgs {
    pub id: String,
    pub action: String, // "pause" | "resume" | "delete" | "update"
    pub name: Option<String>,
    pub instruction: Option<String>,
    pub rule: Option<String>,
}

#[tauri::command]
pub async fn bootstrap_state(state: State<'_, Arc<GuiState>>) -> Result<GuiBootstrapPayload, String> {
    let cfg = state.config.read().unwrap_or_else(|e| e.into_inner()).clone();
    let sessions_raw = state.store.list().await.map_err(|e| e.to_string())?;
    let sessions = sessions_raw
        .into_iter()
        .map(|record| {
            let plugin_agent_id = state.get_session_agent(&record.id);
            GuiSessionItem {
                record,
                plugin_agent_id,
            }
        })
        .collect();

    Ok(GuiBootstrapPayload {
        workspace_root: cfg.paths.workspace_root.display().to_string(),
        default_provider: cfg.model.provider,
        default_model: cfg.model.default,
        sessions,
        sidebar_agents: state.plugin_manager.sidebar_agents(),
        plugins: state.plugin_manager.list_summaries(),
        schedules: state.schedule_store.list()?,
    })
}

#[tauri::command]
pub async fn list_sessions(state: State<'_, Arc<GuiState>>) -> Result<Vec<GuiSessionItem>, String> {
    let list = state.store.list().await.map_err(|e| e.to_string())?;
    Ok(list
        .into_iter()
        .map(|record| {
            let plugin_agent_id = state.get_session_agent(&record.id);
            GuiSessionItem {
                record,
                plugin_agent_id,
            }
        })
        .collect())
}

#[tauri::command]
pub async fn create_session(
    state: State<'_, Arc<GuiState>>,
    plugin_agent_id: Option<String>,
) -> Result<GuiSessionItem, String> {
    let id = create_store_session(&state.store)
        .await
        .map_err(|e| e.to_string())?;
    if let Some(pid) = &plugin_agent_id {
        state.bind_session_agent(&id, Some(pid));
        if let Some(agent) = state.find_plugin_agent(pid) {
            let _ = state
                .store
                .set_name(&id, &format!("{} chat", agent.name))
                .await;
        }
    }
    let record = state.store.get(&id).await.map_err(|e| e.to_string())?;
    state.emit_event("luna://sessions-updated", ());
    Ok(GuiSessionItem {
        record,
        plugin_agent_id,
    })
}

#[tauri::command]
pub async fn get_session_transcript(
    state: State<'_, Arc<GuiState>>,
    session_id: String,
) -> Result<Vec<TranscriptMessageDto>, String> {
    let outcome = state
        .store
        .replay_transcript(&session_id, 500)
        .await
        .map_err(|e| e.to_string())?;
    Ok(outcome
        .history
        .into_iter()
        .map(|sm| message_to_dto(sm.message))
        .collect())
}

fn message_to_dto(m: Message) -> TranscriptMessageDto {
    let role = format!("{:?}", m.role).to_lowercase();
    let tool_calls = m
        .tool_calls
        .into_iter()
        .map(|tc| {
            serde_json::json!({
                "id": tc.id,
                "name": tc.name,
                "arguments": tc.arguments,
            })
        })
        .collect();
    TranscriptMessageDto {
        role,
        content: m.content,
        reasoning: m.reasoning,
        tool_calls,
        tool_call_id: m.tool_result.as_ref().map(|tr| tr.call_id.clone()),
    }
}

#[tauri::command]
pub async fn rename_session(
    state: State<'_, Arc<GuiState>>,
    session_id: String,
    name: String,
) -> Result<(), String> {
    state
        .store
        .set_name(&session_id, name.trim())
        .await
        .map_err(|e| e.to_string())?;
    state.emit_event("luna://sessions-updated", ());
    Ok(())
}

#[tauri::command]
pub async fn fork_session(
    state: State<'_, Arc<GuiState>>,
    session_id: String,
) -> Result<GuiSessionItem, String> {
    let new_id = state
        .store
        .fork_session(&session_id, None)
        .await
        .map_err(|e| e.to_string())?;
    let plugin_agent_id = state.get_session_agent(&session_id);
    if let Some(pid) = &plugin_agent_id {
        state.bind_session_agent(&new_id, Some(pid));
    }
    let record = state.store.get(&new_id).await.map_err(|e| e.to_string())?;
    state.emit_event("luna://sessions-updated", ());
    Ok(GuiSessionItem {
        record,
        plugin_agent_id,
    })
}

#[tauri::command]
pub async fn delete_session(
    state: State<'_, Arc<GuiState>>,
    session_id: String,
) -> Result<bool, String> {
    state.sessions.lock().await.remove(&session_id);
    state.bind_session_agent(&session_id, None);
    let ok = state
        .store
        .delete_session(&session_id)
        .await
        .map_err(|e| e.to_string())?;
    state.emit_event("luna://sessions-updated", ());
    Ok(ok)
}

#[tauri::command]
pub async fn send_message(
    state: State<'_, Arc<GuiState>>,
    session_id: String,
    message: String,
    plugin_agent_id: Option<String>,
) -> Result<(), String> {
    let handle = state
        .get_or_open_session(&session_id, plugin_agent_id)
        .await?;

    if handle.turn_running.load(Ordering::SeqCst) {
        // Steer / interrupt active turn cleanly
        let rt = handle.runtime.lock().await;
        if let Ok(mut q) = rt.steer_handle.lock() {
            q.push_back(message.clone());
        }
        rt.interrupt_handle.interrupt(&message);
        return Ok(());
    }

    let cancel = CancellationToken::new();
    *handle.active_cancel.lock().unwrap_or_else(|e| e.into_inner()) = Some(cancel.clone());
    handle.turn_running.store(true, Ordering::SeqCst);

    let st = state.inner().clone();
    let sid = session_id.clone();
    tokio::spawn(async move {
        st.emit_app_event(&AppEvent::TurnStarted {
            session_id: sid.clone(),
            query: message.clone(),
        });
        let started = std::time::Instant::now();

        let result = {
            let rt = handle.runtime.lock().await;
            let provider_name = rt.current_provider.clone();
            let model_name = rt.current_model.clone();
            let res = {
                let ag = rt.agent.lock().await;
                ag.run_turn(&message, cancel).await
            };
            if let Ok(ref outcome) = res {
                rt.usage
                    .record_chat(&provider_name, &model_name, Some(&sid), outcome)
                    .await;
            }
            res
        };

        handle.turn_running.store(false, Ordering::SeqCst);
        *handle.active_cancel.lock().unwrap_or_else(|e| e.into_inner()) = None;

        match result {
            Ok(outcome) => {
                let (prompt_tokens, completion_tokens) = outcome
                    .usage
                    .as_ref()
                    .map(|u| (u.prompt_tokens, u.completion_tokens))
                    .unwrap_or((0, 0));
                st.emit_app_event(&AppEvent::TurnFinished {
                    session_id: sid,
                    reply: outcome.reply,
                    duration_ms: started.elapsed().as_millis() as u64,
                    prompt_tokens,
                    completion_tokens,
                });
                st.emit_event("luna://sessions-updated", ());
            }
            Err(err) => {
                st.emit_app_event(&AppEvent::TurnFailed {
                    session_id: sid,
                    error: err.to_string(),
                });
            }
        }
    });

    Ok(())
}

#[tauri::command]
pub async fn interrupt_turn(
    state: State<'_, Arc<GuiState>>,
    session_id: String,
) -> Result<(), String> {
    if let Some(handle) = state.sessions.lock().await.get(&session_id).cloned() {
        if let Some(cancel) = handle
            .active_cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            cancel.cancel();
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn answer_approval(
    state: State<'_, Arc<GuiState>>,
    request_id: String,
    session_id: String,
    choice: String, // "once" | "all_session" | "deny"
) -> Result<(), String> {
    if choice == "all_session" {
        if let Some(h) = state.sessions.lock().await.get(&session_id).cloned() {
            let rt = h.runtime.lock().await;
            rt.allow_all_session.store(true, Ordering::SeqCst);
        }
    }
    let decision = match choice.as_str() {
        "once" | "all_session" => ApprovalDecision::Run,
        _ => ApprovalDecision::Deny,
    };
    if let Some(tx) = state
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .approvals
        .remove(&request_id)
    {
        let _ = tx.send(decision);
    }
    Ok(())
}

#[tauri::command]
pub async fn answer_clarify(
    state: State<'_, Arc<GuiState>>,
    request_id: String,
    response: ClarifyResponse,
) -> Result<(), String> {
    if let Some(tx) = state
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clarifies
        .remove(&request_id)
    {
        let _ = tx.send(response);
    }
    Ok(())
}

#[tauri::command]
pub async fn answer_sudo(
    state: State<'_, Arc<GuiState>>,
    request_id: String,
    password: Option<String>,
) -> Result<(), String> {
    if let Some(tx) = state
        .pending
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .sudos
        .remove(&request_id)
    {
        let _ = tx.send(password);
    }
    Ok(())
}

#[tauri::command]
pub async fn list_providers_and_models(
    state: State<'_, Arc<GuiState>>,
) -> Result<Vec<GuiProviderInfo>, String> {
    let loaded: std::collections::HashSet<String> = state
        .cred_manager
        .get_loaded_providers()
        .await
        .into_iter()
        .collect();
    let mut out = Vec::new();
    for desc in get_provider_catalog() {
        let configured = loaded.contains(&desc.id);
        let models = if configured {
            state
                .discovery
                .get_models(&desc.id, &state.cred_manager, false)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        out.push(GuiProviderInfo {
            id: desc.id,
            display_name: desc.display_name,
            configured,
            models,
        });
    }
    Ok(out)
}

#[tauri::command]
pub async fn switch_session_model(
    state: State<'_, Arc<GuiState>>,
    session_id: String,
    provider: String,
    model: String,
) -> Result<(), String> {
    state
        .store
        .set_model(&session_id, &provider, &model)
        .await
        .map_err(|e| e.to_string())?;
    let handle = state.get_or_open_session(&session_id, None).await?;
    let cfg = state.config.read().unwrap_or_else(|e| e.into_inner()).clone();
    let client = GenericProviderClient::new(
        provider.clone(),
        model.clone(),
        state.cred_manager.clone(),
        state.discovery.clone(),
        cfg.providers.get(&provider),
    )
    .map_err(|e| e.to_string())?
    .with_session_id(&session_id);

    {
        let mut rt = handle.runtime.lock().await;
        rt.current_provider = provider.clone();
        rt.current_model = model.clone();
        rt.agent.lock().await.set_provider(Arc::new(client));
    }

    state.emit_app_event(&AppEvent::ModelChanged {
        session_id,
        provider,
        model,
    });
    state.emit_event("luna://sessions-updated", ());
    Ok(())
}

#[tauri::command]
pub async fn list_credentials(
    state: State<'_, Arc<GuiState>>,
    provider: String,
) -> Result<Vec<serde_json::Value>, String> {
    let creds = state.cred_manager.get_credentials(&provider).await;
    Ok(creds
        .into_iter()
        .map(|c| {
            serde_json::json!({
                "id": c.id,
                "label": c.label,
                "is_priority": c.is_priority,
            })
        })
        .collect())
}

#[tauri::command]
pub async fn set_credential_priority(
    state: State<'_, Arc<GuiState>>,
    session_id: String,
    provider: String,
    cred_id: String,
    scope: String, // "session" | "global" | "clear_session"
) -> Result<(), String> {
    match scope.as_str() {
        "global" => {
            state
                .cred_manager
                .set_priority_credential(&provider, &cred_id)
                .await
                .map_err(|e| e.to_string())?;
        }
        "clear_session" => {
            state
                .cred_manager
                .clear_session_priority_credential(&session_id, &provider);
            state
                .store
                .clear_credential_priority(&session_id, &provider)
                .await
                .map_err(|e| e.to_string())?;
        }
        _ => {
            state
                .cred_manager
                .set_session_priority_credential(&session_id, &provider, &cred_id);
            state
                .store
                .set_credential_priority(&session_id, &provider, &cred_id)
                .await
                .map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn list_plugins(state: State<'_, Arc<GuiState>>) -> Result<Vec<PluginSummary>, String> {
    Ok(state.plugin_manager.list_summaries())
}

#[tauri::command]
pub async fn toggle_plugin(
    state: State<'_, Arc<GuiState>>,
    plugin_id: String,
    enabled: bool,
) -> Result<PluginSummary, String> {
    let summary = state.plugin_manager.set_enabled(&plugin_id, enabled)?;
    state.sync_plugin_tools_to_open_sessions().await;
    Ok(summary)
}

#[tauri::command]
pub async fn reload_plugin(
    state: State<'_, Arc<GuiState>>,
    plugin_id: String,
) -> Result<PluginSummary, String> {
    let summary = state.plugin_manager.reload(&plugin_id)?;
    state.sync_plugin_tools_to_open_sessions().await;
    Ok(summary)
}

#[tauri::command]
pub async fn delete_plugin(
    state: State<'_, Arc<GuiState>>,
    plugin_id: String,
) -> Result<(), String> {
    state.plugin_manager.remove(&plugin_id)?;
    state.sync_plugin_tools_to_open_sessions().await;
    Ok(())
}

#[tauri::command]
pub async fn read_plugin_source(
    state: State<'_, Arc<GuiState>>,
    plugin_id: String,
) -> Result<String, String> {
    state.plugin_manager.read_source(&plugin_id)
}

#[tauri::command]
pub async fn create_plugin_from_builder(
    state: State<'_, Arc<GuiState>>,
    spec: PluginBuilderSpec,
) -> Result<PluginSummary, String> {
    let summary = state.plugin_manager.install_from_builder(&spec)?;
    state.sync_plugin_tools_to_open_sessions().await;
    Ok(summary)
}

#[tauri::command]
pub async fn update_plugin_settings(
    state: State<'_, Arc<GuiState>>,
    plugin_id: String,
    settings: BTreeMap<String, serde_json::Value>,
) -> Result<(), String> {
    state
        .plugin_manager
        .update_settings(&plugin_id, settings)?;
    state.emit_event("luna://plugins-updated", ());
    Ok(())
}

#[tauri::command]
pub async fn invoke_plugin_command(
    state: State<'_, Arc<GuiState>>,
    plugin_id: String,
    command: String,
    args: serde_json::Value,
) -> Result<String, String> {
    state
        .plugin_manager
        .invoke_command(&plugin_id, &command, args)
        .await
}

#[tauri::command]
pub async fn list_schedules(state: State<'_, Arc<GuiState>>) -> Result<Vec<ScheduleTask>, String> {
    state.schedule_store.list()
}

#[tauri::command]
pub async fn create_schedule(
    state: State<'_, Arc<GuiState>>,
    args: CreateScheduleArgs,
) -> Result<ScheduleTask, String> {
    let target = match args.target.as_deref().unwrap_or("new_session_each_run") {
        "existing_session" => DeliveryTarget::ExistingSession {
            session_id: args
                .session_id
                .unwrap_or_else(|| "default".to_string()),
        },
        "single_new_session" => DeliveryTarget::SingleNewSession {
            session_id: args.session_id,
        },
        _ => DeliveryTarget::NewSessionEachRun,
    };
    let task = state
        .schedule_store
        .create(None, args.name, args.instruction, args.rule, target, None)?;
    state.emit_event("luna://schedules-updated", ());
    Ok(task)
}

#[tauri::command]
pub async fn update_schedule(
    state: State<'_, Arc<GuiState>>,
    args: UpdateScheduleArgs,
) -> Result<Option<ScheduleTask>, String> {
    let res = match args.action.as_str() {
        "delete" => {
            state.schedule_store.delete(&args.id)?;
            None
        }
        "pause" => Some(
            state
                .schedule_store
                .update(&args.id, None, None, None, None, Some(true))?,
        ),
        "resume" => Some(
            state
                .schedule_store
                .update(&args.id, None, None, None, None, Some(false))?,
        ),
        _ => Some(state.schedule_store.update(
            &args.id,
            args.name,
            args.instruction,
            args.rule,
            None,
            None,
        )?),
    };
    state.emit_event("luna://schedules-updated", ());
    Ok(res)
}

#[tauri::command]
pub async fn run_schedule_now(
    state: State<'_, Arc<GuiState>>,
    id: String,
) -> Result<ScheduleTask, String> {
    let handler = state.build_schedule_handler();
    let task = state.scheduler.run_now(&id, &handler).await?;
    state.emit_event("luna://schedules-updated", ());
    Ok(task)
}

#[tauri::command]
pub async fn search_memory(
    state: State<'_, Arc<GuiState>>,
    query: String,
) -> Result<Vec<serde_json::Value>, String> {
    let cfg = state.config.read().unwrap_or_else(|e| e.into_inner()).clone();
    let engine = build_memory_engine(&cfg).map_err(|e| e.to_string())?;
    let records = engine
        .retrieve_cards(&query, 25)
        .await
        .map_err(|e| e.to_string())?;
    Ok(records
        .into_iter()
        .map(|r| serde_json::to_value(r).unwrap_or_default())
        .collect())
}

#[tauri::command]
pub async fn list_notes(
    state: State<'_, Arc<GuiState>>,
) -> Result<Vec<serde_json::Value>, String> {
    let cfg = state.config.read().unwrap_or_else(|e| e.into_inner()).clone();
    let notes = FileNotesEngine::new(cfg.paths.notes_dir, Some(cfg.paths.notes_db))
        .map_err(|e| e.to_string())?;
    let items = notes.list_notes(50, 0).await.map_err(|e| e.to_string())?;
    Ok(items
        .into_iter()
        .map(|n| serde_json::to_value(n).unwrap_or_default())
        .collect())
}

#[tauri::command]
pub async fn read_note(
    state: State<'_, Arc<GuiState>>,
    id: String,
) -> Result<serde_json::Value, String> {
    let cfg = state.config.read().unwrap_or_else(|e| e.into_inner()).clone();
    let notes = FileNotesEngine::new(cfg.paths.notes_dir, Some(cfg.paths.notes_db))
        .map_err(|e| e.to_string())?;
    let note = notes.read_note(&id).await.map_err(|e| e.to_string())?;
    Ok(serde_json::to_value(note).unwrap_or_default())
}
