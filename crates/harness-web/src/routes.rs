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
use harness_app::session::create_session as create_store_session;
use harness_core::{
    ApprovalDecision, ClarifyAnswer, ClarifyResponse, MemoryEngine, Message, ModelProvider,
    NotesEngine, SessionRecord, SessionStore,
};
use harness_memory::build_memory_engine;
use harness_notes::FileNotesEngine;
use harness_plugins::{PluginAgentEntry, PluginBuilderSpec, PluginSummary};
use harness_providers::{get_provider_catalog, GenericProviderClient};
use harness_scheduler::{DeliveryTarget, ScheduleTask};

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
}

#[derive(Serialize)]
pub struct GuiBootstrapPayload {
    pub workspace_root: String,
    pub default_provider: String,
    pub default_model: String,
    pub sessions: Vec<GuiSessionItem>,
    pub sidebar_agents: Vec<PluginAgentEntry>,
    pub providers: Vec<GuiProviderInfo>,
    pub theme: String,
    pub approval_mode: String,
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
        .route("/api/models/default", post(set_default_model))
        .route("/api/credentials", post(save_credential))
        .route("/api/plugins", get(list_plugins))
        .route("/api/plugins/build", post(create_plugin_from_spec))
        .route("/api/plugins/{plugin_id}/toggle", post(toggle_plugin))
        .route(
            "/api/schedules",
            get(list_schedules).post(create_schedule_task),
        )
        .route(
            "/api/schedules/{task_id}/toggle",
            post(toggle_schedule_task),
        )
        .route("/api/schedules/{task_id}", delete(delete_schedule_task))
        .route("/api/memories", get(search_memories))
        .route("/api/notes", get(search_notes))
        .route("/api/notes/{note_id}", get(get_note))
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

async fn bootstrap(
    State(state): State<Arc<WebState>>,
) -> Result<Json<GuiBootstrapPayload>, (StatusCode, String)> {
    let records = state
        .store
        .list()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let agent_map = state.session_agents.read().await;

    let sessions = records
        .into_iter()
        .map(|r| {
            let pid = agent_map.get(&r.id).cloned();
            GuiSessionItem {
                record: r,
                plugin_agent_id: pid,
            }
        })
        .collect();

    let sidebar_agents = state.plugin_manager.sidebar_agents();

    let catalog = get_provider_catalog();
    let mut providers = Vec::new();
    for p in catalog {
        let creds = state.cred_manager.get_credentials(&p.id).await;
        let configured = !creds.is_empty();
        let models = state
            .discovery
            .get_models(&p.id, &state.cred_manager, false)
            .await
            .unwrap_or_default();
        providers.push(GuiProviderInfo {
            id: p.id,
            display_name: p.display_name,
            configured,
            models,
        });
    }

    Ok(Json(GuiBootstrapPayload {
        workspace_root: state.config.paths.workspace_root.display().to_string(),
        default_provider: state.config.model.provider.clone(),
        default_model: state.config.model.default.clone(),
        sessions,
        sidebar_agents,
        providers,
        theme: "dark".to_string(),
        approval_mode: format!("{:?}", state.config.tools.approval).to_lowercase(),
    }))
}

async fn list_sessions(
    State(state): State<Arc<WebState>>,
) -> Result<Json<Vec<GuiSessionItem>>, (StatusCode, String)> {
    let records = state
        .store
        .list()
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let agent_map = state.session_agents.read().await;

    let items = records
        .into_iter()
        .map(|r| {
            let pid = agent_map.get(&r.id).cloned();
            GuiSessionItem {
                record: r,
                plugin_agent_id: pid,
            }
        })
        .collect();
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

async fn get_session_transcript(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
) -> Result<Json<Vec<TranscriptMessageDto>>, (StatusCode, String)> {
    let outcome = state
        .store
        .replay(&session_id, 500)
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
    plugin_agent_id: Option<String>,
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

    if handle.turn_running.load(Ordering::SeqCst) {
        let rt = handle.runtime.lock().await;
        if let Ok(mut q) = rt.steer_handle.lock() {
            q.push_back(payload.message.clone());
        }
        rt.interrupt_handle.interrupt(&payload.message);
        return Ok(StatusCode::OK);
    }

    let cancel = CancellationToken::new();
    *handle
        .active_cancel
        .lock()
        .unwrap_or_else(|e| e.into_inner()) = Some(cancel.clone());
    handle.turn_running.store(true, Ordering::SeqCst);

    let st = state.clone();
    let sid = session_id.clone();
    let msg = payload.message.clone();

    tokio::spawn(async move {
        st.emit_event(AppEvent::TurnStarted {
            session_id: sid.clone(),
            query: msg.clone(),
        });
        let started = std::time::Instant::now();

        let result = {
            let rt = handle.runtime.lock().await;
            let ag = rt.agent.lock().await;
            ag.run_turn(&msg, cancel.clone()).await
        };

        handle.turn_running.store(false, Ordering::SeqCst);
        let elapsed = started.elapsed().as_millis() as u64;

        match result {
            Ok(turn_outcome) => {
                let usage = turn_outcome.usage;
                st.emit_event(AppEvent::TurnFinished {
                    session_id: sid,
                    reply: turn_outcome.reply,
                    duration_ms: elapsed,
                    prompt_tokens: usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                    completion_tokens: usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0),
                });
            }
            Err(err) => {
                st.emit_event(AppEvent::TurnFailed {
                    session_id: sid,
                    error: err.to_string(),
                });
            }
        }
    });

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
    let provider = state
        .config
        .tools
        .approval_provider
        .clone()
        .unwrap_or_else(|| state.config.model.provider.clone());
    let model = state
        .config
        .tools
        .approval_model
        .clone()
        .unwrap_or_else(|| state.config.model.default.clone());

    let client = GenericProviderClient::new(
        provider.clone(),
        model,
        state.cred_manager.clone(),
        state.discovery.clone(),
        state.config.providers.get(&provider),
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

async fn get_providers(
    State(state): State<Arc<WebState>>,
) -> Result<Json<Vec<GuiProviderInfo>>, (StatusCode, String)> {
    let catalog = get_provider_catalog();
    let mut out = Vec::new();
    for p in catalog {
        let creds = state.cred_manager.get_credentials(&p.id).await;
        let configured = !creds.is_empty();
        let models = state
            .discovery
            .get_models(&p.id, &state.cred_manager, false)
            .await
            .unwrap_or_default();
        out.push(GuiProviderInfo {
            id: p.id,
            display_name: p.display_name,
            configured,
            models,
        });
    }
    Ok(Json(out))
}

#[derive(Deserialize)]
struct SetDefaultModelReq {
    provider: String,
    model: String,
}

async fn set_default_model(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<SetDefaultModelReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let mut cfg = state.config.clone();
    cfg.model.provider = payload.provider;
    cfg.model.default = payload.model;
    let yaml = serde_yaml::to_string(&cfg)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let path = harness_core::config::config_file_path();
    std::fs::write(&path, yaml).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
struct SaveCredentialReq {
    provider: String,
    api_key: String,
}

async fn save_credential(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<SaveCredentialReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .cred_manager
        .add_credential(&payload.provider, "web-key", &payload.api_key, true)
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

#[derive(Deserialize)]
struct CreateScheduleReq {
    title: String,
    prompt: String,
    schedule_type: String,
    cron_expr: Option<String>,
    interval_secs: Option<u64>,
}

async fn create_schedule_task(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<CreateScheduleReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let rule = if payload.schedule_type == "interval" {
        format!("every:{}", payload.interval_secs.unwrap_or(3600))
    } else {
        payload
            .cron_expr
            .unwrap_or_else(|| "0 9 * * 1-5".to_string())
    };

    let task = state
        .scheduler
        .store()
        .create(
            None,
            payload.title,
            payload.prompt,
            rule,
            DeliveryTarget::NewSessionEachRun,
            None,
        )
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({ "id": task.id })))
}

async fn toggle_schedule_task(
    State(state): State<Arc<WebState>>,
    Path(task_id): Path<String>,
    Json(payload): Json<serde_json::Value>,
) -> Result<StatusCode, (StatusCode, String)> {
    let enabled = payload
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    state
        .scheduler
        .store()
        .update(&task_id, None, None, None, None, Some(!enabled))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
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

#[derive(Deserialize)]
struct SearchQuery {
    q: Option<String>,
}

async fn search_memories(
    State(state): State<Arc<WebState>>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    let q = query.q.unwrap_or_default();
    let engine = build_memory_engine(&state.config)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let records = engine
        .retrieve(&q, 30)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let out = records
        .into_iter()
        .map(|r| {
            serde_json::json!({
                "id": r.id,
                "category": r.category,
                "predicate": r.predicate,
                "text": r.raw_content,
                "updated_at": r.updated_at.to_rfc3339()
            })
        })
        .collect();
    Ok(Json(out))
}

async fn search_notes(
    State(state): State<Arc<WebState>>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<serde_json::Value>>, (StatusCode, String)> {
    let q = query.q.unwrap_or_default();
    let engine = FileNotesEngine::new(
        state.config.paths.notes_dir.clone(),
        Some(state.config.paths.notes_db.clone()),
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let notes = engine
        .search_notes(&q, 30)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let out = notes
        .into_iter()
        .map(|n| {
            serde_json::json!({
                "id": n.id,
                "title": n.title,
                "body": n.content,
                "summary_pointer": n.summary_pointer,
                "updated_at": n.updated_at.to_rfc3339()
            })
        })
        .collect();
    Ok(Json(out))
}

async fn get_note(
    State(state): State<Arc<WebState>>,
    Path(note_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let engine = FileNotesEngine::new(
        state.config.paths.notes_dir.clone(),
        Some(state.config.paths.notes_db.clone()),
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let note = engine
        .read_note(&note_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({
        "id": note.id,
        "title": note.title,
        "body": note.content,
        "summary_pointer": note.summary_pointer,
        "updated_at": note.updated_at.to_rfc3339()
    })))
}
