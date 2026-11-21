use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::header::{HeaderMap, SET_COOKIE};
use axum::http::{StatusCode, Uri};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Json;
use axum::Router;
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use harness_app::events::AppEvent;
use harness_app::memory_writer::build_saver_for;
use harness_app::session::create_session as create_store_session;
use harness_core::{
    ApprovalDecision, ClarifyAnswer, ClarifyResponse, CompactionSetup, MemoryEngine, Message,
    ModelProvider, NotesEngine, SessionRecord, SessionStore,
};
use harness_plugins::{
    plugin_skill, PluginAgentEntry, PluginBuilderSpec, PluginSummary, SkillLoader,
    PLUGIN_SKILL_NAME,
};
use harness_providers::{get_provider_catalog, GenericProviderClient};
use harness_scheduler::{automation_skill, DeliveryTarget, ScheduleTask, AUTOMATION_SKILL_NAME};

use crate::extra_routes::{accounts_for_provider, AccountDto};
use crate::state::WebState;

#[derive(RustEmbed)]
#[folder = "../../web/dist"]
struct WebAssets;

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
    pub accounts: Vec<AccountDto>,
    pub rotation_strategy: String,
    pub is_custom: bool,
    pub base_url: Option<String>,
}

#[derive(Serialize)]
pub struct GuiBootstrapPayload {
    pub workspace_root: String,
    pub launch_workspace_root: String,
    pub default_provider: String,
    pub default_model: String,
    pub compact_provider: Option<String>,
    pub compact_model: Option<String>,
    pub background_provider: Option<String>,
    pub background_model: Option<String>,
    pub approval_provider: Option<String>,
    pub approval_model: Option<String>,
    pub sessions: Vec<GuiSessionItem>,
    pub sidebar_agents: Vec<PluginAgentEntry>,
    pub providers: Vec<GuiProviderInfo>,
    pub theme: String,
    pub approval_mode: String,
    pub display: serde_json::Value,
    pub auto_compact: serde_json::Value,
    pub features: serde_json::Value,
    pub workers: serde_json::Value,
    pub skills: Vec<serde_json::Value>,
    pub mcp_servers: Vec<serde_json::Value>,
}

#[derive(Serialize)]
pub struct TranscriptMessageDto {
    pub role: String,
    pub content: Option<String>,
    pub reasoning: Option<String>,
    pub tool_calls: Vec<serde_json::Value>,
    pub tool_call_id: Option<String>,
}

pub fn create_router(state: Arc<WebState>) -> Router {
    Router::new()
        // WebSocket stream
        .route("/api/remote.mux", get(ws_handler))
        .route("/api/ws", get(ws_handler))
        // API routes
        .route("/api/auth/status", get(auth_status))
        .route("/api/bootstrap", get(bootstrap))
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route(
            "/api/sessions/{session_id}/transcript",
            get(get_session_transcript),
        )
        .route("/api/sessions/{session_id}/send", post(send_message))
        .route("/api/sessions/{session_id}/interrupt", post(interrupt_turn))
        .route("/api/sessions/{session_id}/rename", post(rename_session))
        .route("/api/sessions/{session_id}/fork", post(fork_session))
        .route("/api/sessions/{session_id}", delete(delete_session))
        .route(
            "/api/sessions/{session_id}/approval",
            post(resolve_approval),
        )
        .route("/api/sessions/{session_id}/clarify", post(resolve_clarify))
        .route("/api/approval/question", post(ask_approval_question))
        .route("/api/providers", get(get_providers))
        .route(
            "/api/providers/{provider_id}/refresh",
            post(refresh_provider_models),
        )
        .route("/api/models/default", post(set_default_model))
        .route("/api/workspace/browse", get(browse_workspace))
        .route("/api/workspace", post(set_workspace))
        .route("/api/credentials", post(save_credential))
        .route("/api/plugins", get(list_plugins))
        .route("/api/plugins/build", post(create_plugin_from_spec))
        .route("/api/plugins/{plugin_id}/toggle", post(toggle_plugin))
        .route("/api/plugins/{plugin_id}", delete(delete_plugin))
        .route(
            "/api/schedules",
            get(list_schedules).post(create_schedule_task),
        )
        .route(
            "/api/schedules/{task_id}/toggle",
            post(toggle_schedule_task),
        )
        .route("/api/schedules/{task_id}/run", post(run_schedule_task))
        .route(
            "/api/schedules/{task_id}",
            get(get_schedule_task)
                .put(update_schedule_task)
                .patch(update_schedule_task)
                .delete(delete_schedule_task),
        )
        .merge(crate::knowledge_routes::knowledge_router())
        .merge(crate::plugin_routes::plugin_router())
        .merge(crate::extra_routes::extra_router())
        // Static assets fallback
        .fallback(static_handler)
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Static file & Auth middleware
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

async fn static_handler(
    State(state): State<Arc<WebState>>,
    uri: Uri,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Response {
    // 1. Host/Origin trust fence
    if !state.auth.verify_trust_fence(&headers) {
        return (
            StatusCode::FORBIDDEN,
            "403 Forbidden: Host or Origin rejected by Luna trust fence",
        )
            .into_response();
    }

    // 2. Launch token exchange
    if let Some(token) = query.token {
        if let Some(session_token) = state.auth.exchange_launch_token(&token) {
            let cookie_val = format!(
                "luna_session={}; HttpOnly; SameSite=Lax; Path=/",
                session_token
            );
            let mut redirect_headers = HeaderMap::new();
            redirect_headers.insert(SET_COOKIE, cookie_val.parse().unwrap());
            redirect_headers.insert("Location", "/".parse().unwrap());
            return (StatusCode::FOUND, redirect_headers, "").into_response();
        }
    }

    // 3. Check session authorization for assets
    let path = uri.path().trim_start_matches('/');
    let target = if path.is_empty() { "index.html" } else { path };

    if let Some(content) = WebAssets::get(target) {
        let mime = mime_guess::from_path(target).first_or_octet_stream();
        return (
            StatusCode::OK,
            [("Content-Type", mime.as_ref())],
            content.data.into_owned(),
        )
            .into_response();
    }

    // SPA fallback to index.html
    if let Some(index) = WebAssets::get("index.html") {
        return Html(index.data.into_owned()).into_response();
    }

    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

// ---------------------------------------------------------------------------
// WebSocket handler
// ---------------------------------------------------------------------------

async fn ws_handler(
    ws: WebSocketUpgrade,
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
) -> Response {
    if !state.auth.verify_trust_fence(&headers) {
        return (StatusCode::FORBIDDEN, "Forbidden").into_response();
    }

    ws.on_upgrade(|socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: Arc<WebState>) {
    let mut rx = state.event_tx.subscribe();

    loop {
        tokio::select! {
            event = rx.recv() => {
                match event {
                    Ok(evt) => {
                        if let Ok(json) = serde_json::to_string(&evt) {
                            if socket.send(WsMessage::Text(json.into())).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            msg = socket.recv() => {
                match msg {
                    Some(Ok(WsMessage::Close(_))) | None => break,
                    _ => {}
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// API Handlers
// ---------------------------------------------------------------------------

async fn auth_status(
    State(state): State<Arc<WebState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if !state.auth.verify_trust_fence(&headers) {
        return Err((StatusCode::FORBIDDEN, "Trust fence violation".into()));
    }
    Ok(Json(serde_json::json!({
        "authenticated": true,
        "user": "local"
    })))
}

async fn build_providers_snapshot(
    state: &WebState,
    session_id: Option<&str>,
) -> Vec<GuiProviderInfo> {
    let configured_set = state.cred_manager.configured_providers_snapshot().await;
    let cached_models = state.discovery.cached_models_snapshot();
    let catalog = get_provider_catalog();
    let cfg_providers = state.config.read().await.providers.clone();

    let mut providers = Vec::with_capacity(catalog.len() + cfg_providers.len());
    let mut seen_ids = std::collections::HashSet::new();

    for p in catalog {
        let lower = p.id.to_lowercase();
        seen_ids.insert(lower.clone());
        let configured = configured_set.contains(&lower);
        let mut models = cached_models.get(&p.id).cloned().unwrap_or_default();
        if models.is_empty() && !p.context_windows.is_empty() {
            models = p.context_windows.keys().cloned().collect();
            models.sort();
        }
        let (rotation_strategy, accounts) = accounts_for_provider(state, &p.id, session_id).await;
        let base_url = cfg_providers.get(&p.id).and_then(|o| o.base_url.clone());
        providers.push(GuiProviderInfo {
            id: p.id,
            display_name: p.display_name,
            configured: configured || !accounts.is_empty(),
            models,
            accounts,
            rotation_strategy,
            is_custom: false,
            base_url,
        });
    }

    for (id, ovr) in &cfg_providers {
        let lower = id.to_lowercase();
        if seen_ids.insert(lower.clone()) {
            let models = cached_models.get(id).cloned().unwrap_or_default();
            let (rotation_strategy, accounts) = accounts_for_provider(state, id, session_id).await;
            providers.push(GuiProviderInfo {
                id: id.clone(),
                display_name: format!("Custom ({id})"),
                configured: configured_set.contains(&lower) || !accounts.is_empty(),
                models,
                accounts,
                rotation_strategy,
                is_custom: true,
                base_url: ovr.base_url.clone(),
            });
        }
    }

    for id in state.cred_manager.get_loaded_providers().await {
        let lower = id.to_lowercase();
        if seen_ids.insert(lower.clone()) {
            let models = cached_models.get(&id).cloned().unwrap_or_default();
            let (rotation_strategy, accounts) = accounts_for_provider(state, &id, session_id).await;
            providers.push(GuiProviderInfo {
                id: id.clone(),
                display_name: format!("Custom ({id})"),
                configured: !accounts.is_empty(),
                models,
                accounts,
                rotation_strategy,
                is_custom: true,
                base_url: None,
            });
        }
    }

    providers
}

#[derive(Deserialize)]
struct BootstrapQuery {
    session_id: Option<String>,
}

async fn bootstrap(
    State(state): State<Arc<WebState>>,
    Query(query): Query<BootstrapQuery>,
) -> Result<Json<GuiBootstrapPayload>, (StatusCode, String)> {
    let records = state
        .store
        .list()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let agent_map = state.session_agents.read().await;

    let sessions: Vec<GuiSessionItem> = records
        .into_iter()
        .filter(|r| r.message_count > 0 || query.session_id.as_deref() == Some(&r.id))
        .take(120)
        .map(|r| {
            let pid = agent_map.get(&r.id).cloned();
            GuiSessionItem {
                record: r,
                plugin_agent_id: pid,
            }
        })
        .collect();

    let first_sid = query
        .session_id
        .or_else(|| sessions.first().map(|s| s.record.id.clone()));
    let sidebar_agents = state.plugin_manager.sidebar_agents();
    let providers = build_providers_snapshot(&state, first_sid.as_deref()).await;
    let cfg = state.config.read().await;

    let mut skills = Vec::new();
    if let Ok(loaded) = SkillLoader::load_skills_from_dir(&cfg.paths.skills_dir) {
        for s in loaded {
            skills.push(serde_json::json!({
                "name": s.name,
                "description": s.description,
                "source": "global",
            }));
        }
    }
    let ws_skills = cfg.paths.workspace_root.join(".harness/skills");
    if ws_skills.exists() {
        if let Ok(loaded) = SkillLoader::load_skills_from_dir(&ws_skills) {
            for s in loaded {
                skills.push(serde_json::json!({
                    "name": s.name,
                    "description": s.description,
                    "source": "workspace",
                }));
            }
        }
    }
    let has_skill = |list: &[serde_json::Value], target: &str| {
        list.iter().any(|item| {
            item.get("name")
                .and_then(|v| v.as_str())
                .is_some_and(|n| harness_plugins::names_match(n, target))
        })
    };
    if !has_skill(&skills, AUTOMATION_SKILL_NAME) {
        let s = automation_skill();
        skills.push(serde_json::json!({
            "name": s.name,
            "description": s.description,
            "source": "builtin",
        }));
    }
    if !has_skill(&skills, PLUGIN_SKILL_NAME) {
        let s = plugin_skill();
        skills.push(serde_json::json!({
            "name": s.name,
            "description": s.description,
            "source": "builtin",
        }));
    }

    let mcp_servers: Vec<serde_json::Value> = cfg
        .mcp
        .servers
        .iter()
        .map(|srv| {
            serde_json::json!({
                "name": srv.name,
                "command": srv.command,
                "enabled": srv.enabled,
            })
        })
        .collect();

    Ok(Json(GuiBootstrapPayload {
        workspace_root: cfg.paths.workspace_root.display().to_string(),
        launch_workspace_root: state.launch_workspace_root.display().to_string(),
        default_provider: cfg.model.provider.clone(),
        default_model: cfg.model.default.clone(),
        compact_provider: cfg.agent.compaction_provider.clone(),
        compact_model: cfg.agent.compaction_model.clone(),
        background_provider: cfg.memory.background_provider.clone(),
        background_model: cfg.memory.background_model.clone(),
        approval_provider: cfg.tools.approval_provider.clone(),
        approval_model: cfg.tools.approval_model.clone(),
        sessions,
        sidebar_agents,
        providers,
        theme: cfg.display.theme.label().to_string(),
        approval_mode: format!("{:?}", cfg.tools.approval).to_lowercase(),
        display: serde_json::json!({
            "theme": cfg.display.theme.label(),
            "auto_collapse": cfg.display.auto_collapse,
            "auto_hide_thinking": cfg.display.auto_hide_thinking,
            "block_limit": cfg.display.block_limit,
        }),
        auto_compact: serde_json::json!({
            "enabled": cfg.agent.auto_compact_enabled,
            "threshold": cfg.agent.auto_compact_threshold,
            "percent": cfg.agent.auto_compact_percent.unwrap_or(50),
        }),
        features: serde_json::json!({
            "memory": cfg.memory_effective_enabled(),
            "notes": cfg.notes_effective_enabled(),
            "skills": cfg.skills_effective_enabled(),
            "mcp": cfg.mcp_effective_enabled(),
            "memory_mode": cfg.memory.mode.as_str(),
        }),
        workers: serde_json::to_value(&cfg.workers).unwrap_or(serde_json::json!({})),
        skills,
        mcp_servers,
    }))
}

#[derive(Deserialize)]
struct ListSessionsQuery {
    limit: Option<usize>,
}

async fn list_sessions(
    State(state): State<Arc<WebState>>,
    Query(query): Query<ListSessionsQuery>,
) -> Result<Json<Vec<GuiSessionItem>>, (StatusCode, String)> {
    let records = state
        .store
        .list()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let agent_map = state.session_agents.read().await;
    let active_sessions = state.sessions.lock().await;
    let limit = query.limit.unwrap_or(150);

    let iter = records
        .into_iter()
        .filter(|r| r.message_count > 0 || active_sessions.contains_key(&r.id));
    let items = if limit > 0 {
        iter.take(limit)
            .map(|r| {
                let pid = agent_map.get(&r.id).cloned();
                GuiSessionItem {
                    record: r,
                    plugin_agent_id: pid,
                }
            })
            .collect()
    } else {
        iter.map(|r| {
            let pid = agent_map.get(&r.id).cloned();
            GuiSessionItem {
                record: r,
                plugin_agent_id: pid,
            }
        })
        .collect()
    };
    Ok(Json(items))
}

#[derive(Deserialize)]
struct CreateSessionReq {
    plugin_agent_id: Option<String>,
}

async fn create_session(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<CreateSessionReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let sid = create_store_session(&state.store)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if let Some(agent_id) = payload.plugin_agent_id {
        state
            .session_agents
            .write()
            .await
            .insert(sid.clone(), agent_id);
    }

    Ok(Json(serde_json::json!({ "session_id": sid })))
}

#[derive(Deserialize)]
struct TranscriptQuery {
    limit: Option<usize>,
}

async fn get_session_transcript(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
    Query(query): Query<TranscriptQuery>,
) -> Result<Json<Vec<TranscriptMessageDto>>, (StatusCode, String)> {
    let limit = query.limit.unwrap_or(150).clamp(1, 1000);
    let outcome = state
        .store
        .replay(&session_id, limit)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let messages = outcome
        .history
        .into_iter()
        .map(|sm| message_to_dto(sm.message))
        .collect();
    Ok(Json(messages))
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

#[derive(Deserialize)]
struct SendMessageReq {
    message: String,
    images: Option<Vec<String>>,
    mode: Option<String>,
    plugin_agent_id: Option<String>,
}

fn spawn_session_turn(
    state: Arc<WebState>,
    session_id: String,
    handle: Arc<crate::state::ActiveSessionHandle>,
    initial_msg: String,
) {
    tokio::spawn(async move {
        let mut next_msg = Some(initial_msg);
        while let Some(msg) = next_msg.take() {
            let cancel = CancellationToken::new();
            *handle
                .active_cancel
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(cancel.clone());
            handle.turn_running.store(true, Ordering::SeqCst);

            let full_msg = {
                let rt = handle.runtime.lock().await;
                if rt.plan_mode.load(Ordering::SeqCst) {
                    format!(
                        "[PLAN MODE ACTIVE: Formulate a detailed, step-by-step plan/blueprint. Do not attempt to modify files or execute mutating commands until the plan is approved.]\n\n{}",
                        msg
                    )
                } else {
                    msg.clone()
                }
            };

            state.emit_event(AppEvent::TurnStarted {
                session_id: session_id.clone(),
                query: msg.clone(),
            });
            {
                let pm = state.plugin_manager.clone();
                let s = session_id.clone();
                let q = msg.clone();
                tokio::spawn(async move {
                    pm.dispatch_event(
                        "turn_started",
                        serde_json::json!({ "sessionId": s, "query": q }),
                    )
                    .await;
                });
            }
            let started = std::time::Instant::now();
            let plugin_gen_before = state.plugin_manager.generation();

            let (result, provider, model) = {
                let rt = handle.runtime.lock().await;
                let ag = rt.agent.lock().await;
                let res = ag.run_turn(&full_msg, cancel.clone()).await;
                (res, rt.current_provider.clone(), rt.current_model.clone())
            };

            handle.turn_running.store(false, Ordering::SeqCst);
            if state.plugin_manager.generation() != plugin_gen_before {
                state.sync_plugins_after_change().await;
            }
            let elapsed = started.elapsed().as_millis() as u64;

            match result {
                Ok(turn_outcome) => {
                    {
                        let rt = handle.runtime.lock().await;
                        rt.usage
                            .record_chat(&provider, &model, Some(&session_id), &turn_outcome)
                            .await;
                    }
                    let usage = turn_outcome.usage;
                    {
                        let pm = state.plugin_manager.clone();
                        let s = session_id.clone();
                        let r = turn_outcome.reply.clone();
                        tokio::spawn(async move {
                            pm.dispatch_event(
                                "turn_finished",
                                serde_json::json!({
                                    "sessionId": s,
                                    "reply": r,
                                    "durationMs": elapsed,
                                }),
                            )
                            .await;
                        });
                    }
                    state.emit_event(AppEvent::TurnFinished {
                        session_id: session_id.clone(),
                        reply: turn_outcome.reply,
                        duration_ms: elapsed,
                        prompt_tokens: usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                        completion_tokens: usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0),
                    });
                }
                Err(err) => {
                    state.emit_event(AppEvent::TurnFailed {
                        session_id: session_id.clone(),
                        error: err.to_string(),
                    });
                    handle.queued_messages.lock().await.clear();
                    break;
                }
            }

            if !cancel.is_cancelled() {
                next_msg = handle.queued_messages.lock().await.pop_front();
            } else {
                handle.queued_messages.lock().await.clear();
            }
        }
    });
}

async fn send_message(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
    Json(payload): Json<SendMessageReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let handle = state
        .get_or_open_session(&session_id, payload.plugin_agent_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let mut composed = payload.message.clone();
    if let Some(images) = payload.images.as_ref().filter(|v| !v.is_empty()) {
        if composed.trim().is_empty() {
            composed = images.join("\n\n");
        } else {
            composed = format!("{}\n\n{}", composed, images.join("\n\n"));
        }
    }

    if handle.turn_running.load(Ordering::SeqCst) {
        if payload.mode.as_deref() == Some("queue") {
            handle.queued_messages.lock().await.push_back(composed);
        } else {
            let rt = handle.runtime.lock().await;
            if let Ok(mut q) = rt.steer_handle.lock() {
                q.push_back(composed.clone());
            }
            rt.interrupt_handle.interrupt(&composed);
        }
        return Ok(StatusCode::OK);
    }

    spawn_session_turn(state, session_id, handle, composed);
    Ok(StatusCode::OK)
}

async fn interrupt_turn(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let map = state.sessions.lock().await;
    if let Some(handle) = map.get(&session_id) {
        if let Some(c) = handle
            .active_cancel
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            c.cancel();
        }
        let rt = handle.runtime.lock().await;
        rt.interrupt_handle.interrupt("stop");
    }
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
struct RenameReq {
    name: String,
}

async fn rename_session(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
    Json(payload): Json<RenameReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .store
        .set_name(&session_id, &payload.name)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}

async fn fork_session(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let new_id = state
        .store
        .fork_session(&session_id, None)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let agent_id = state.session_agents.read().await.get(&session_id).cloned();
    if let Some(aid) = agent_id {
        state
            .session_agents
            .write()
            .await
            .insert(new_id.clone(), aid);
    }

    Ok(Json(serde_json::json!({ "session_id": new_id })))
}

async fn delete_session(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .store
        .delete_session(&session_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state.sessions.lock().await.remove(&session_id);
    state.session_agents.write().await.remove(&session_id);
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
struct ApprovalReq {
    request_id: Option<String>,
    decision: String,
    arguments: Option<serde_json::Value>,
    feedback: Option<String>,
}

async fn resolve_approval(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
    Json(payload): Json<ApprovalReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let map = state.sessions.lock().await;
    let handle = map
        .get(&session_id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "session not active".into()))?;

    let dec = match payload.decision.as_str() {
        "run" => ApprovalDecision::Run,
        "run_with_args" => {
            ApprovalDecision::RunWithArguments(payload.arguments.unwrap_or_default())
        }
        "deny_with_feedback" => {
            ApprovalDecision::DenyWithFeedback(payload.feedback.unwrap_or_default())
        }
        _ => ApprovalDecision::Deny,
    };

    let mut pending = handle.pending_approvals.lock().await;
    let sender = if let Some(req_id) = payload.request_id {
        pending.remove(&req_id)
    } else {
        // Pop the first pending if request_id not specified
        let first_key = pending.keys().next().cloned();
        first_key.and_then(|k| pending.remove(&k))
    };

    if let Some(tx) = sender {
        let _ = tx.send(dec);
    }

    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
struct ClarifyReq {
    request_id: Option<String>,
    answers: Vec<serde_json::Value>,
}

async fn resolve_clarify(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
    Json(payload): Json<ClarifyReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let map = state.sessions.lock().await;
    let handle = map
        .get(&session_id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, "session not active".into()))?;

    let answers: Vec<ClarifyAnswer> = payload
        .answers
        .into_iter()
        .map(|v| match v {
            serde_json::Value::String(s) => ClarifyAnswer {
                question: String::new(),
                answer: s,
                choice_index: None,
            },
            serde_json::Value::Array(arr) => ClarifyAnswer {
                question: String::new(),
                answer: arr
                    .into_iter()
                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                    .collect::<Vec<_>>()
                    .join(", "),
                choice_index: None,
            },
            other => ClarifyAnswer {
                question: String::new(),
                answer: other.to_string(),
                choice_index: None,
            },
        })
        .collect();

    let resp = ClarifyResponse {
        answer: answers.first().map(|a| a.answer.clone()),
        choice_index: None,
        answers,
    };

    let mut pending = handle.pending_clarifies.lock().await;
    let sender = if let Some(req_id) = payload.request_id {
        pending.remove(&req_id)
    } else {
        let first_key = pending.keys().next().cloned();
        first_key.and_then(|k| pending.remove(&k))
    };

    if let Some(tx) = sender {
        let _ = tx.send(resp);
    }

    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
struct AskQuestionReq {
    tool_name: String,
    target: Option<String>,
    question: String,
}

async fn ask_approval_question(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<AskQuestionReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let (provider, model, provider_override) = {
        let cfg = state.config.read().await;
        let provider = cfg
            .tools
            .approval_provider
            .clone()
            .unwrap_or_else(|| cfg.model.provider.clone());
        let model = cfg
            .tools
            .approval_model
            .clone()
            .unwrap_or_else(|| cfg.model.default.clone());
        let pov = cfg.providers.get(&provider).cloned();
        (provider, model, pov)
    };

    let client = GenericProviderClient::new(
        provider.clone(),
        model,
        state.cred_manager.clone(),
        state.discovery.clone(),
        provider_override.as_ref(),
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let prompt = format!(
        "The agent proposed tool call '{}'{}. User asks: '{}'. Explain concisely.",
        payload.tool_name,
        payload
            .target
            .as_ref()
            .map(|t| format!(" on target '{}'", t))
            .unwrap_or_default(),
        payload.question
    );

    let messages = vec![
        Message::system("You are Luna assistant answering questions about a pending tool execution before authorization."),
        Message::user(prompt),
    ];

    let resp = client
        .complete(&messages, &[], &harness_core::CompletionOptions::default())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({
        "reply": resp.content.unwrap_or_default()
    })))
}

#[derive(Deserialize)]
struct ProvidersQuery {
    session_id: Option<String>,
}

async fn get_providers(
    State(state): State<Arc<WebState>>,
    Query(query): Query<ProvidersQuery>,
) -> Result<Json<Vec<GuiProviderInfo>>, (StatusCode, String)> {
    Ok(Json(
        build_providers_snapshot(&state, query.session_id.as_deref()).await,
    ))
}

#[derive(Deserialize)]
struct RefreshProviderQuery {
    credential_id: Option<String>,
    force: Option<bool>,
}

async fn refresh_provider_models(
    State(state): State<Arc<WebState>>,
    Path(provider_id): Path<String>,
    Query(query): Query<RefreshProviderQuery>,
) -> Result<Json<Vec<String>>, (StatusCode, String)> {
    let force = query.force.unwrap_or(true);
    let models = if let Some(cid) = query.credential_id.as_deref().filter(|s| !s.is_empty()) {
        state
            .discovery
            .get_models_for_credential(&provider_id, &state.cred_manager, cid, force)
            .await
    } else {
        state
            .discovery
            .get_models(&provider_id, &state.cred_manager, force)
            .await
    }
    .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(Json(models))
}

#[derive(Deserialize)]
struct SetDefaultModelReq {
    provider: String,
    model: String,
    session_id: Option<String>,
    scope: Option<String>,
    target: Option<String>,
}

async fn set_default_model(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<SetDefaultModelReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let scope = payload.scope.as_deref().unwrap_or("session");
    let target = payload.target.as_deref().unwrap_or("session");
    let is_global =
        scope == "global" || target == "default" || target == "bgsaver" || target == "approval";

    if is_global || payload.session_id.is_none() {
        let mut cfg = state.config.write().await;
        match target {
            "compact" => {
                cfg.agent.compaction_provider = Some(payload.provider.clone());
                cfg.agent.compaction_model = Some(payload.model.clone());
            }
            "bgsaver" => {
                cfg.memory.background_provider = Some(payload.provider.clone());
                cfg.memory.background_model = Some(payload.model.clone());
            }
            "approval" => {
                cfg.tools.approval_provider = Some(payload.provider.clone());
                cfg.tools.approval_model = Some(payload.model.clone());
            }
            _ => {
                cfg.model.provider = payload.provider.clone();
                cfg.model.default = payload.model.clone();
            }
        }
        let yaml = serde_yaml::to_string(&*cfg)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        drop(cfg);

        let path = harness_core::config::config_file_path();
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&path, yaml)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    let cfg_snap = state.config.read().await.clone();

    if let Some(sid) = payload.session_id.as_deref() {
        match target {
            "compact" => {
                let _ = state
                    .store
                    .set_compact_model(sid, &payload.provider, &payload.model)
                    .await;
                if let Some(h) = state.sessions.lock().await.get(sid).cloned() {
                    let mut rt = h.runtime.lock().await;
                    rt.compact_provider = Some(payload.provider.clone());
                    rt.compact_model = Some(payload.model.clone());
                    if let Ok(client) = GenericProviderClient::new(
                        payload.provider.clone(),
                        payload.model.clone(),
                        state.cred_manager.clone(),
                        state.discovery.clone(),
                        cfg_snap.providers.get(&payload.provider),
                    ) {
                        let setup = CompactionSetup {
                            provider: Arc::new(client.with_session_id(sid))
                                as Arc<dyn ModelProvider>,
                            model: payload.model.clone(),
                        };
                        rt.agent.lock().await.set_compaction(Some(setup));
                    }
                }
            }
            "bgsaver" => {
                if let Some(h) = state.sessions.lock().await.get(sid).cloned() {
                    let rt = h.runtime.lock().await;
                    if cfg_snap.memory.mode.is_background() {
                        let mem_dyn: Arc<dyn MemoryEngine> = rt.memory.clone();
                        let notes_dyn: Arc<dyn NotesEngine> = rt.notes.clone();
                        if let Ok(saver) = build_saver_for(
                            &cfg_snap,
                            &mem_dyn,
                            notes_dyn,
                            &payload.provider,
                            &payload.model,
                            rt.cred_manager.clone(),
                            rt.discovery.clone(),
                        ) {
                            rt.agent
                                .lock()
                                .await
                                .set_background_saver(Some(Arc::new(saver)));
                        }
                    }
                }
            }
            "approval" => {}
            _ => {
                let _ = state
                    .store
                    .set_model(sid, &payload.provider, &payload.model)
                    .await;
                if let Some(h) = state.sessions.lock().await.get(sid).cloned() {
                    let mut rt = h.runtime.lock().await;
                    if let Ok(client) = GenericProviderClient::new(
                        payload.provider.clone(),
                        payload.model.clone(),
                        state.cred_manager.clone(),
                        state.discovery.clone(),
                        cfg_snap.providers.get(&payload.provider),
                    ) {
                        let client = Arc::new(client.with_session_id(sid));
                        let window = harness_app::context_window::context_window(
                            &cfg_snap,
                            &state.discovery,
                            &payload.provider,
                            &payload.model,
                        ) as usize;
                        rt.current_provider = payload.provider.clone();
                        rt.current_model = payload.model.clone();
                        let mut ag = rt.agent.lock().await;
                        ag.set_provider(client);
                        ag.set_context_window(Some(window));
                    }
                }
                state.emit_event(AppEvent::ModelChanged {
                    session_id: sid.to_string(),
                    provider: payload.provider.clone(),
                    model: payload.model.clone(),
                });
            }
        }
    } else if target == "default" {
        state.invalidate_idle_sessions().await;
    }

    Ok(Json(serde_json::json!({
        "provider": payload.provider,
        "model": payload.model,
        "scope": scope,
        "target": target,
    })))
}

#[derive(Deserialize)]
struct BrowseWorkspaceQuery {
    path: Option<String>,
}

#[derive(Serialize)]
struct WorkspaceEntryDto {
    name: String,
    path: String,
}

async fn browse_workspace(
    State(state): State<Arc<WebState>>,
    Query(query): Query<BrowseWorkspaceQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let current = match query
        .path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        Some(raw) => {
            if raw == "~" || raw.starts_with("~/") {
                if let Some(ref h) = home {
                    if raw == "~" {
                        h.clone()
                    } else {
                        h.join(raw.trim_start_matches("~/"))
                    }
                } else {
                    std::path::PathBuf::from(raw)
                }
            } else {
                std::path::PathBuf::from(raw)
            }
        }
        None => state.config.read().await.paths.workspace_root.clone(),
    };

    let canonical = current.canonicalize().unwrap_or(current);
    if !canonical.is_dir() {
        return Err((
            StatusCode::BAD_REQUEST,
            format!("Not a directory: {}", canonical.display()),
        ));
    }

    let mut entries = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(&canonical) {
        for entry in read_dir.flatten() {
            let p = entry.path();
            if p.is_dir() {
                if let Some(name) = entry.file_name().to_str() {
                    if !name.starts_with('.') {
                        entries.push(WorkspaceEntryDto {
                            name: name.to_string(),
                            path: p.display().to_string(),
                        });
                    }
                }
            }
        }
    }
    entries.sort_by_key(|a| a.name.to_lowercase());

    let parent = canonical.parent().map(|p| p.display().to_string());
    Ok(Json(serde_json::json!({
        "current": canonical.display().to_string(),
        "parent": parent,
        "home": home.map(|h| h.display().to_string()),
        "entries": entries,
    })))
}

#[derive(Deserialize)]
struct SetWorkspaceReq {
    path: Option<String>,
    scope: Option<String>,
    reset: Option<bool>,
}

async fn set_workspace(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<SetWorkspaceReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let canonical = if payload.reset.unwrap_or(false) {
        state.launch_workspace_root.clone()
    } else {
        let raw = payload.path.as_deref().map(str::trim).unwrap_or("");
        if raw.is_empty() {
            return Err((
                StatusCode::BAD_REQUEST,
                "Workspace path cannot be empty".into(),
            ));
        }
        let candidate = if raw == "~" || raw.starts_with("~/") {
            if let Some(h) = std::env::var_os("HOME").map(std::path::PathBuf::from) {
                if raw == "~" {
                    h
                } else {
                    h.join(raw.trim_start_matches("~/"))
                }
            } else {
                std::path::PathBuf::from(raw)
            }
        } else {
            std::path::PathBuf::from(raw)
        };

        if !candidate.is_dir() {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("Directory does not exist: {}", candidate.display()),
            ));
        }

        candidate
            .canonicalize()
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
    };

    {
        let mut cfg = state.config.write().await;
        cfg.paths.workspace_root = canonical.clone();
        if payload.scope.as_deref() == Some("global") {
            if let Ok(yaml) = serde_yaml::to_string(&*cfg) {
                let path = harness_core::config::config_file_path();
                let _ = std::fs::write(&path, yaml);
            }
        }
    }
    state.invalidate_idle_sessions().await;

    Ok(Json(serde_json::json!({
        "workspace_root": canonical.display().to_string(),
    })))
}

#[derive(Deserialize)]
struct SaveCredentialReq {
    provider: String,
    api_key: String,
    label: Option<String>,
    is_priority: Option<bool>,
}

async fn save_credential(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<SaveCredentialReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let label = payload
        .label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{}-web", payload.provider));
    state
        .cred_manager
        .add_credential(
            &payload.provider,
            &label,
            &payload.api_key,
            payload.is_priority.unwrap_or(true),
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}

async fn list_plugins(
    State(state): State<Arc<WebState>>,
) -> Result<Json<Vec<PluginSummary>>, (StatusCode, String)> {
    Ok(Json(state.plugin_manager.list_summaries()))
}

async fn create_plugin_from_spec(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<PluginBuilderSpec>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .plugin_manager
        .install_from_builder(&payload)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state.sync_plugins_after_change().await;
    Ok(StatusCode::OK)
}

async fn toggle_plugin(
    State(state): State<Arc<WebState>>,
    Path(plugin_id): Path<String>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, (StatusCode, String)> {
    let enabled = payload
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    state
        .plugin_manager
        .set_enabled(&plugin_id, enabled)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    state.sync_plugins_after_change().await;
    Ok(StatusCode::OK)
}

async fn delete_plugin(
    State(state): State<Arc<WebState>>,
    Path(plugin_id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    if let Some(summary) = state.plugin_manager.get_summary(&plugin_id) {
        if summary.builtin {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("cannot delete built-in plugin '{plugin_id}'"),
            ));
        }
    }
    state
        .plugin_manager
        .remove(&plugin_id)
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    state.sync_plugins_after_change().await;
    Ok(StatusCode::OK)
}

async fn list_schedules(
    State(state): State<Arc<WebState>>,
) -> Result<Json<Vec<ScheduleTask>>, (StatusCode, String)> {
    let tasks = state
        .scheduler
        .store()
        .list()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(tasks))
}

async fn get_schedule_task(
    State(state): State<Arc<WebState>>,
    Path(task_id): Path<String>,
) -> Result<Json<ScheduleTask>, (StatusCode, String)> {
    let task = state
        .scheduler
        .store()
        .get(&task_id)
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    Ok(Json(task))
}

fn parse_delivery_target(target: Option<&str>, session_id: Option<&str>) -> DeliveryTarget {
    match target.unwrap_or("new_session_each_run") {
        "existing_session" => DeliveryTarget::ExistingSession {
            session_id: session_id.unwrap_or_default().to_string(),
        },
        "single_new_session" => DeliveryTarget::SingleNewSession {
            session_id: session_id.filter(|s| !s.is_empty()).map(str::to_string),
        },
        _ => DeliveryTarget::NewSessionEachRun,
    }
}

#[derive(Deserialize)]
struct CreateScheduleReq {
    name: Option<String>,
    title: Option<String>,
    instruction: Option<String>,
    prompt: Option<String>,
    rule: Option<String>,
    schedule_type: Option<String>,
    cron_expr: Option<String>,
    interval_secs: Option<u64>,
    target: Option<String>,
    session_id: Option<String>,
    require_approval: Option<bool>,
}

async fn create_schedule_task(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<CreateScheduleReq>,
) -> Result<Json<ScheduleTask>, (StatusCode, String)> {
    let name = payload
        .name
        .or(payload.title)
        .unwrap_or_default()
        .trim()
        .to_string();
    let instruction = payload
        .instruction
        .or(payload.prompt)
        .unwrap_or_default()
        .trim()
        .to_string();
    if name.is_empty() || instruction.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "name and instruction are required".to_string(),
        ));
    }

    let rule = if let Some(r) = payload.rule.filter(|s| !s.trim().is_empty()) {
        r.trim().to_string()
    } else if payload.schedule_type.as_deref() == Some("interval") {
        format!("every:{}", payload.interval_secs.unwrap_or(3600))
    } else {
        payload
            .cron_expr
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "0 9 * * 1-5".to_string())
    };

    let target = parse_delivery_target(payload.target.as_deref(), payload.session_id.as_deref());
    let require_approval = payload.require_approval.unwrap_or(false);

    let task = state
        .scheduler
        .store()
        .create_with_approval(
            None,
            name,
            instruction,
            rule,
            target,
            None,
            require_approval,
        )
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    Ok(Json(task))
}

#[derive(Deserialize)]
struct UpdateScheduleReq {
    name: Option<String>,
    instruction: Option<String>,
    rule: Option<String>,
    paused: Option<bool>,
    target: Option<String>,
    session_id: Option<String>,
    require_approval: Option<bool>,
}

async fn update_schedule_task(
    State(state): State<Arc<WebState>>,
    Path(task_id): Path<String>,
    Json(payload): Json<UpdateScheduleReq>,
) -> Result<Json<ScheduleTask>, (StatusCode, String)> {
    let delivery_target = payload
        .target
        .as_deref()
        .map(|t| parse_delivery_target(Some(t), payload.session_id.as_deref()));

    let updated = state
        .scheduler
        .store()
        .update_with_approval(
            &task_id,
            payload.name,
            payload.instruction,
            payload.rule,
            delivery_target,
            payload.paused,
            payload.require_approval,
        )
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    Ok(Json(updated))
}

async fn run_schedule_task(
    State(state): State<Arc<WebState>>,
    Path(task_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let task = state
        .scheduler
        .store()
        .get(&task_id)
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;

    let session_id = state
        .resolve_schedule_session(&task)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    let bg_state = Arc::clone(&state);
    let bg_session_id = session_id.clone();
    let bg_task_id = task_id.clone();
    tokio::spawn(async move {
        let receipt = match bg_state
            .execute_schedule_in_session(&task, bg_session_id.clone())
            .await
        {
            Ok(r) => r,
            Err(err) => harness_scheduler::DeliveryReceipt {
                delivered_at: chrono::Utc::now(),
                session_id: bg_session_id,
                status: "failed".to_string(),
                summary: err,
            },
        };
        if let Err(err) = bg_state
            .scheduler
            .store()
            .record_delivery(&bg_task_id, receipt)
        {
            tracing::warn!("failed to record schedule delivery receipt: {}", err);
        }
    });

    Ok(Json(serde_json::json!({
        "task_id": task_id,
        "session_id": session_id,
        "status": "running"
    })))
}

async fn toggle_schedule_task(
    State(state): State<Arc<WebState>>,
    Path(task_id): Path<String>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<ScheduleTask>, (StatusCode, String)> {
    let paused = if let Some(p) = payload.get("paused").and_then(|v| v.as_bool()) {
        p
    } else {
        !payload
            .get("enabled")
            .and_then(|v| v.as_bool())
            .unwrap_or(true)
    };
    let updated = state
        .scheduler
        .store()
        .update(&task_id, None, None, None, None, Some(paused))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(updated))
}

async fn delete_schedule_task(
    State(state): State<Arc<WebState>>,
    Path(task_id): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .scheduler
        .store()
        .delete(&task_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}
