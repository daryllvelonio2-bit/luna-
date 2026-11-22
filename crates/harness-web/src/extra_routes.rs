use std::sync::atomic::Ordering;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use harness_app::events::AppEvent;
use harness_app::memory_writer::build_saver_for;
use harness_core::config::{
    config_file_path, validate_worker_name, MemoryMode, ProviderOverride, ThemeMode, WorkerProfile,
};
use harness_core::profiles::{
    catalog as profile_catalog, default_profile, load as load_profile, paths_for, profile_dir,
    validate_name as validate_profile_name, DEFAULT_PROFILE, PROFILE_FILE,
};
use harness_core::tools::todo::{read_snapshot as read_todo_snapshot, TodoListTool};
use harness_core::usage::{GroupBy, UsageFilter, UsageLedger};
use harness_core::{
    GoalRunState, GoalVerdict, MemoryEngine, ModelProvider, NotesEngine, ReasoningEffort,
    SessionStore, Tool, ToolRegistry,
};
use harness_plugins::SkillLoader;
use harness_providers::{
    build_http_client, detect_antigravity_credentials, detect_codex_credentials,
    detect_opencode_credentials, get_provider_catalog, store_antigravity_credentials,
    GenericProviderClient, RotationStrategy,
};

use crate::state::{ActiveGoalHandle, WebGoalStatus, WebState};

pub fn extra_router() -> Router<Arc<WebState>> {
    Router::new()
        // Credentials & accounts
        .route("/api/credentials", get(list_credentials))
        .route(
            "/api/credentials/{provider}/{credential_id}",
            delete(remove_credential),
        )
        .route(
            "/api/credentials/{provider}/priority",
            post(set_priority_credential).delete(clear_priority_credential),
        )
        .route(
            "/api/credentials/{provider}/strategy",
            post(set_rotation_strategy),
        )
        .route("/api/credentials/reset-cooldowns", post(reset_cooldowns))
        .route("/api/credentials/import", post(import_credentials))
        .route("/api/providers/custom", post(add_custom_provider))
        // Session controls, state, mode, reasoning, compaction, summarize, memory scope
        .route("/api/sessions/{session_id}/state", get(get_session_state))
        .route("/api/sessions/{session_id}/mode", post(set_session_mode))
        .route(
            "/api/sessions/{session_id}/reasoning",
            post(set_session_reasoning),
        )
        .route("/api/sessions/{session_id}/compact", post(compact_session))
        .route(
            "/api/sessions/{session_id}/summarize",
            post(summarize_session),
        )
        .route(
            "/api/sessions/{session_id}/remember",
            post(remember_session),
        )
        .route(
            "/api/sessions/{session_id}/memory-scope",
            post(set_memory_scope),
        )
        // Session todos
        .route(
            "/api/sessions/{session_id}/todos",
            get(get_todos).post(mutate_todo),
        )
        // Checkpoints: diff & rewind
        .route(
            "/api/sessions/{session_id}/checkpoints",
            get(list_checkpoints),
        )
        .route("/api/sessions/{session_id}/rewind", post(rewind_checkpoint))
        // Goal loop, subagents, background jobs, workers
        .route("/api/sessions/{session_id}/goal", post(manage_goal))
        .route("/api/sessions/{session_id}/tasks", get(list_tasks))
        .route(
            "/api/sessions/{session_id}/tasks/{task_id}/kill",
            post(kill_task),
        )
        .route("/api/sessions/{session_id}/worker", post(run_worker))
        // Usage, settings, profiles, heartbeat
        .route("/api/usage", get(get_usage))
        .route("/api/settings", get(get_settings).post(update_settings))
        .route("/api/workers", post(upsert_worker))
        .route("/api/workers/{name}", delete(delete_worker))
        .route("/api/profiles", get(list_profiles).post(create_profile))
        .route("/api/profiles/{name}", put(update_profile).delete(delete_profile))
        .route("/api/heartbeat", get(get_heartbeat))
}

fn persist_config(cfg: &harness_core::Config) -> Result<(), (StatusCode, String)> {
    let yaml = serde_yaml::to_string(cfg)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let path = config_file_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&path, yaml).map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

fn parse_reasoning_effort(
    raw: Option<&str>,
) -> Result<Option<ReasoningEffort>, (StatusCode, String)> {
    let Some(s) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    if matches!(
        s.to_ascii_lowercase().as_str(),
        "off" | "none" | "disabled" | "0" | "false"
    ) {
        return Ok(None);
    }
    ReasoningEffort::from_str_loose(s).map(Some).ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            format!("invalid reasoning effort: {s}"),
        )
    })
}

fn parse_rotation_strategy(raw: &str) -> Result<RotationStrategy, (StatusCode, String)> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "round_robin" | "round-robin" => Ok(RotationStrategy::RoundRobin),
        "fill_first" | "fill-first" => Ok(RotationStrategy::FillFirst),
        "least_used" | "least-used" => Ok(RotationStrategy::LeastUsed),
        "random" => Ok(RotationStrategy::Random),
        "never" | "none" => Ok(RotationStrategy::Never),
        other => Err((
            StatusCode::BAD_REQUEST,
            format!("unknown rotation strategy: {other}"),
        )),
    }
}

pub fn strategy_to_str(strategy: RotationStrategy) -> &'static str {
    match strategy {
        RotationStrategy::RoundRobin => "round_robin",
        RotationStrategy::FillFirst => "fill_first",
        RotationStrategy::Random => "random",
        RotationStrategy::LeastUsed => "least_used",
        RotationStrategy::Never => "never",
    }
}

// ---------------------------------------------------------------------------
// Credentials & Accounts (/connect + Account Picker)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
pub struct AccountDto {
    pub id: String,
    pub label: String,
    pub is_priority: bool,
    pub is_session_priority: bool,
    pub usage_count: u64,
    pub last_used: Option<String>,
    pub is_temporarily_disabled: bool,
}

#[derive(Deserialize)]
struct CredentialsQuery {
    session_id: Option<String>,
}

#[derive(Serialize)]
struct ProviderCredentialsDto {
    provider_id: String,
    display_name: String,
    rotation_strategy: String,
    accounts: Vec<AccountDto>,
}

pub async fn accounts_for_provider(
    state: &WebState,
    provider_id: &str,
    session_id: Option<&str>,
) -> (String, Vec<AccountDto>) {
    let entries = state.cred_manager.get_credentials(provider_id).await;
    let strategy = state.cred_manager.get_rotation_strategy(provider_id).await;
    let session_prio = session_id.and_then(|sid| {
        state
            .cred_manager
            .get_session_priority_credential(sid, provider_id)
    });

    let accounts = entries
        .into_iter()
        .map(|c| {
            let is_session_priority = session_prio
                .as_deref()
                .is_some_and(|p| p == c.id || p == c.label);
            AccountDto {
                id: c.id,
                label: c.label,
                is_priority: c.is_priority,
                is_session_priority,
                usage_count: c.usage_count,
                last_used: c.last_used.map(|d| d.to_rfc3339()),
                is_temporarily_disabled: c.is_temporarily_disabled,
            }
        })
        .collect();

    (strategy_to_str(strategy).to_string(), accounts)
}

async fn list_credentials(
    State(state): State<Arc<WebState>>,
    Query(query): Query<CredentialsQuery>,
) -> Result<Json<Vec<ProviderCredentialsDto>>, (StatusCode, String)> {
    let mut ids: Vec<(String, String)> = get_provider_catalog()
        .into_iter()
        .map(|p| (p.id, p.display_name))
        .collect();
    {
        let cfg = state.config.read().await;
        for id in cfg.providers.keys() {
            if !ids
                .iter()
                .any(|(existing, _)| existing.eq_ignore_ascii_case(id))
            {
                ids.push((id.clone(), format!("Custom ({id})")));
            }
        }
    }
    for id in state.cred_manager.get_loaded_providers().await {
        if !ids
            .iter()
            .any(|(existing, _)| existing.eq_ignore_ascii_case(&id))
        {
            ids.push((id.clone(), format!("Custom ({id})")));
        }
    }

    let mut out = Vec::with_capacity(ids.len());
    for (id, display_name) in ids {
        let (rotation_strategy, accounts) =
            accounts_for_provider(&state, &id, query.session_id.as_deref()).await;
        out.push(ProviderCredentialsDto {
            provider_id: id,
            display_name,
            rotation_strategy,
            accounts,
        });
    }
    Ok(Json(out))
}

async fn remove_credential(
    State(state): State<Arc<WebState>>,
    Path((provider, credential_id)): Path<(String, String)>,
) -> Result<StatusCode, (StatusCode, String)> {
    state
        .cred_manager
        .remove_credential(&provider, &credential_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
struct PriorityReq {
    credential_id: Option<String>,
    scope: Option<String>,
    session_id: Option<String>,
}

async fn set_priority_credential(
    State(state): State<Arc<WebState>>,
    Path(provider): Path<String>,
    Json(payload): Json<PriorityReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let cred_id = payload
        .credential_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "credential_id is required".into()))?;

    let scope = payload.scope.as_deref().unwrap_or("session");
    if scope == "global" {
        let found = state
            .cred_manager
            .set_priority_credential(&provider, cred_id)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if !found {
            return Err((StatusCode::NOT_FOUND, "Credential not found".into()));
        }
    } else if let Some(sid) = payload.session_id.as_deref() {
        state
            .cred_manager
            .set_session_priority_credential(sid, &provider, cred_id);
        let _ = state
            .store
            .set_credential_priority(sid, &provider, cred_id)
            .await;
    } else {
        return Err((
            StatusCode::BAD_REQUEST,
            "session_id is required for session-scoped priority".into(),
        ));
    }

    Ok(StatusCode::OK)
}

async fn clear_priority_credential(
    State(state): State<Arc<WebState>>,
    Path(provider): Path<String>,
    Json(payload): Json<PriorityReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let scope = payload.scope.as_deref().unwrap_or("session");
    if scope == "global" {
        if let Some(cred_id) = payload.credential_id.as_deref() {
            state
                .cred_manager
                .clear_priority_credential(&provider, cred_id)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
    } else if let Some(sid) = payload.session_id.as_deref() {
        state
            .cred_manager
            .clear_session_priority_credential(sid, &provider);
        let _ = state.store.clear_credential_priority(sid, &provider).await;
    }
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
struct StrategyReq {
    strategy: String,
}

async fn set_rotation_strategy(
    State(state): State<Arc<WebState>>,
    Path(provider): Path<String>,
    Json(payload): Json<StrategyReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let strategy = parse_rotation_strategy(&payload.strategy)?;
    state
        .cred_manager
        .set_rotation_strategy(&provider, strategy)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
struct ResetCooldownsReq {
    provider: Option<String>,
}

async fn reset_cooldowns(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<ResetCooldownsReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let count = state
        .cred_manager
        .reset_cooldowns(payload.provider.as_deref())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "reset_count": count })))
}

#[derive(Deserialize)]
struct ImportCredentialsReq {
    kind: String,
}

async fn import_credentials(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<ImportCredentialsReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut imported = Vec::new();
    match payload.kind.trim().to_ascii_lowercase().as_str() {
        "antigravity" => {
            let detected = detect_antigravity_credentials()
                .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
            let http = build_http_client(10)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            let (id, label) = store_antigravity_credentials(&state.cred_manager, &http, detected)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            imported.push(format!("antigravity ({label} · {id})"));
        }
        "opencode" => {
            let detected = detect_opencode_credentials()
                .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
            for cred in detected {
                state
                    .cred_manager
                    .add_credential(&cred.provider, &cred.label, &cred.api_key, cred.is_priority)
                    .await
                    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
                imported.push(format!("{} ({})", cred.provider, cred.label));
            }
        }
        "codex" => {
            let creds =
                detect_codex_credentials().map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
            let label = creds.label();
            state
                .cred_manager
                .add_credential("codex", &label, &creds.to_secret(), true)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            let _ = state
                .cred_manager
                .set_rotation_strategy("codex", RotationStrategy::FillFirst)
                .await;
            imported.push(format!("codex ({label})"));
        }
        other => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("unknown import kind '{other}'"),
            ));
        }
    }

    Ok(Json(serde_json::json!({
        "imported": imported,
    })))
}

#[derive(Deserialize)]
struct CustomProviderReq {
    id: String,
    base_url: String,
    api_key: Option<String>,
    label: Option<String>,
    context_window: Option<u32>,
}

async fn add_custom_provider(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<CustomProviderReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let id = payload.id.trim().to_ascii_lowercase();
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err((
            StatusCode::BAD_REQUEST,
            "Provider id must be non-empty alphanumeric, '-' or '_'".into(),
        ));
    }
    let base_url = payload.base_url.trim().to_string();
    if base_url.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "Base URL is required".into()));
    }

    {
        let mut cfg = state.config.write().await;
        cfg.providers.insert(
            id.clone(),
            ProviderOverride {
                base_url: Some(base_url.clone()),
                models_endpoint: None,
                timeout_secs: None,
                context_window: payload.context_window,
            },
        );
        persist_config(&cfg)?;
    }

    let api_key = payload
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("none");
    let label = payload
        .label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("{id}-primary"));

    state
        .cred_manager
        .add_credential(&id, &label, api_key, true)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    state.invalidate_idle_sessions().await;

    Ok(Json(serde_json::json!({
        "id": id,
        "base_url": base_url,
    })))
}

// ---------------------------------------------------------------------------
// Session State, Mode, Reasoning, Compaction, Summarize, Memory Scope
// ---------------------------------------------------------------------------

async fn get_session_state(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let map = state.sessions.lock().await;
    if let Some(handle) = map.get(&session_id).cloned() {
        drop(map);
        let queued_messages: Vec<String> = handle
            .queued_messages
            .lock()
            .await
            .iter()
            .cloned()
            .collect();
        let goal = handle
            .active_goal
            .lock()
            .await
            .as_ref()
            .map(|g| g.status.clone());
        let rt = handle.runtime.lock().await;
        let mode = if rt.plan_mode.load(Ordering::SeqCst) {
            "plan"
        } else if rt.allow_all_session.load(Ordering::SeqCst) {
            "auto"
        } else {
            "build"
        };
        let todo_snap = read_todo_snapshot(&rt.todo_scope.path());
        let (reasoning_effort, memory_project_filter, context_tokens, context_window) = {
            if let Ok(ag) = rt.agent.try_lock() {
                let effort = ag.reasoning_effort().map(|e| e.to_string());
                let filter = ag.memory_project_filter().map(str::to_string);
                let tokens = ag.estimate_session_tokens().await.unwrap_or(0);
                let window = harness_app::context_window::context_window(
                    &rt.config,
                    &rt.discovery,
                    &rt.current_provider,
                    &rt.current_model,
                );
                (effort, filter, tokens, window)
            } else {
                let window = harness_app::context_window::context_window(
                    &rt.config,
                    &rt.discovery,
                    &rt.current_provider,
                    &rt.current_model,
                );
                (None, None, 0, window)
            }
        };

        return Ok(Json(serde_json::json!({
            "mode": mode,
            "reasoning_effort": reasoning_effort,
            "context_tokens": context_tokens,
            "context_window": context_window,
            "memory_project_filter": memory_project_filter,
            "goal": goal,
            "queued_messages": queued_messages,
            "todos": todo_snap.items,
            "turn_running": handle.turn_running.load(Ordering::SeqCst),
        })));
    }
    drop(map);

    let cfg = state.config.read().await;
    let todos_path =
        harness_core::tools::TodoScope::new(cfg.paths.todos_dir.clone(), &session_id).path();
    let todo_snap = read_todo_snapshot(&todos_path);
    let mode = if cfg.tools.approval == harness_core::ApprovalMode::Allow {
        "auto"
    } else {
        "build"
    };
    let (prov, modl) = match state
        .store
        .get(&session_id)
        .await
        .ok()
        .and_then(|r| r.model)
    {
        Some(pair) => pair,
        None => (cfg.model.provider.clone(), cfg.model.default.clone()),
    };
    let window = harness_app::context_window::context_window(&cfg, &state.discovery, &prov, &modl);

    Ok(Json(serde_json::json!({
        "mode": mode,
        "reasoning_effort": null,
        "context_tokens": 0,
        "context_window": window,
        "memory_project_filter": null,
        "goal": null,
        "queued_messages": [],
        "todos": todo_snap.items,
        "turn_running": false,
    })))
}

#[derive(Deserialize)]
struct ModeReq {
    mode: String,
}

async fn set_session_mode(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
    Json(payload): Json<ModeReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let handle = state
        .get_or_open_session(&session_id, None)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let rt = handle.runtime.lock().await;
    let mode = match payload.mode.trim().to_ascii_lowercase().as_str() {
        "plan" => {
            rt.plan_mode.store(true, Ordering::SeqCst);
            rt.allow_all_session.store(false, Ordering::SeqCst);
            "plan"
        }
        "auto" | "yolo" | "allow" => {
            rt.plan_mode.store(false, Ordering::SeqCst);
            rt.allow_all_session.store(true, Ordering::SeqCst);
            "auto"
        }
        "build" | "ask" | "normal" => {
            rt.plan_mode.store(false, Ordering::SeqCst);
            rt.allow_all_session.store(false, Ordering::SeqCst);
            "build"
        }
        other => {
            return Err((StatusCode::BAD_REQUEST, format!("unknown mode: {other}")));
        }
    };
    Ok(Json(serde_json::json!({ "mode": mode })))
}

#[derive(Deserialize)]
struct ReasoningReq {
    effort: Option<String>,
}

async fn set_session_reasoning(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
    Json(payload): Json<ReasoningReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let parsed = parse_reasoning_effort(payload.effort.as_deref())?;
    let handle = state
        .get_or_open_session(&session_id, None)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let rt = handle.runtime.lock().await;
    let mut ag = rt.agent.lock().await;
    ag.set_reasoning_effort(parsed);
    Ok(Json(serde_json::json!({
        "reasoning_effort": parsed.map(|e| e.to_string()),
    })))
}

async fn compact_session(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let handle = state
        .get_or_open_session(&session_id, None)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if handle.turn_running.load(Ordering::SeqCst) {
        return Err((
            StatusCode::CONFLICT,
            "Cannot compact while a turn is running".into(),
        ));
    }
    let rt = handle.runtime.lock().await;
    let ag = rt.agent.lock().await;
    let notice = ag
        .compact_current_session()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let tokens = ag.estimate_session_tokens().await.unwrap_or(0);
    Ok(Json(serde_json::json!({
        "turns": notice.turns,
        "messages": notice.messages,
        "summary": notice.summary,
        "context_tokens": tokens,
    })))
}

async fn summarize_session(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let handle = state
        .get_or_open_session(&session_id, None)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if handle.turn_running.load(Ordering::SeqCst) {
        return Err((
            StatusCode::CONFLICT,
            "Cannot summarize while a turn is running".into(),
        ));
    }
    let summary = {
        let rt = handle.runtime.lock().await;
        let ag = rt.agent.lock().await;
        ag.summarize_current_session(&rt.current_model)
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?
    };

    let new_id = harness_app::session::create_session(&state.store)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let intro = format!("Summary of previous conversation ({session_id}):\n\n{summary}");
    let _ = state
        .store
        .append(&new_id, &harness_core::Message::system(intro), true)
        .await;
    let _ = state
        .store
        .set_name(&new_id, &format!("Summary of {session_id}"))
        .await;

    Ok(Json(serde_json::json!({
        "session_id": new_id,
        "summary": summary,
    })))
}

async fn remember_session(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let handle = state
        .get_or_open_session(&session_id, None)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let rt = handle.runtime.lock().await;
    let ag = rt.agent.lock().await;
    let report = ag
        .run_memory_pass_now()
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(serde_json::json!({
        "memories": report.memories,
        "notes": report.notes,
        "summary": report.summary(),
    })))
}

#[derive(Deserialize)]
struct MemoryScopeReq {
    project: Option<String>,
}

async fn set_memory_scope(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
    Json(payload): Json<MemoryScopeReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let handle = state
        .get_or_open_session(&session_id, None)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let rt = handle.runtime.lock().await;
    let mut ag = rt.agent.lock().await;
    ag.set_memory_project_filter(payload.project.clone());
    Ok(Json(serde_json::json!({
        "memory_project_filter": ag.memory_project_filter(),
    })))
}

// ---------------------------------------------------------------------------
// Todos
// ---------------------------------------------------------------------------

async fn get_todos(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let todos_dir = state.config.read().await.paths.todos_dir.clone();
    let scope = harness_core::tools::TodoScope::new(todos_dir, &session_id);
    let snap = read_todo_snapshot(&scope.path());
    Ok(Json(serde_json::json!({ "items": snap.items })))
}

#[derive(Deserialize)]
struct MutateTodoReq {
    action: String,
    id: Option<u64>,
    text: Option<String>,
    status: Option<String>,
}

async fn mutate_todo(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
    Json(payload): Json<MutateTodoReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let todos_dir = state.config.read().await.paths.todos_dir.clone();
    let scope = harness_core::tools::TodoScope::new(todos_dir, &session_id);
    let tool = TodoListTool::new(scope.clone());
    let mut args = serde_json::Map::new();
    args.insert("action".into(), serde_json::json!(payload.action));
    if let Some(id) = payload.id {
        args.insert("id".into(), serde_json::json!(id));
    }
    if let Some(text) = payload.text {
        args.insert("text".into(), serde_json::json!(text));
    }
    if let Some(status) = payload.status {
        args.insert("status".into(), serde_json::json!(status));
    }
    let cancel = CancellationToken::new();
    let res = tool
        .execute("web-todo", &serde_json::Value::Object(args), &cancel)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if res.is_error {
        return Err((StatusCode::BAD_REQUEST, res.content));
    }
    let snap = read_todo_snapshot(&scope.path());
    Ok(Json(serde_json::json!({ "items": snap.items })))
}

// ---------------------------------------------------------------------------
// Checkpoints: Diff & Rewind
// ---------------------------------------------------------------------------

async fn list_checkpoints(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cfg = state.config.read().await;
    let store = harness_core::checkpoint::CheckpointStore::new(
        cfg.paths.checkpoints_dir.clone(),
        cfg.paths.workspace_root.clone(),
    );
    let diffs = store
        .diff_all(&session_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(Json(serde_json::json!({ "turns": diffs })))
}

#[derive(Deserialize)]
struct RewindReq {
    seq: u32,
}

async fn rewind_checkpoint(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
    Json(payload): Json<RewindReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cfg = state.config.read().await;
    let store = harness_core::checkpoint::CheckpointStore::new(
        cfg.paths.checkpoints_dir.clone(),
        cfg.paths.workspace_root.clone(),
    );
    let report = store
        .restore(&session_id, payload.seq)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    Ok(Json(serde_json::json!({
        "restored": report.restored,
        "deleted": report.deleted,
        "skipped": report.skipped,
    })))
}

// ---------------------------------------------------------------------------
// Goal Loop, Subagents, Background Jobs, Workers
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct GoalReq {
    action: String,
    objective: Option<String>,
    max_iterations: Option<usize>,
}

async fn manage_goal(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
    Json(payload): Json<GoalReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let handle = state
        .get_or_open_session(&session_id, None)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if payload.action.eq_ignore_ascii_case("stop") {
        let mut guard = handle.active_goal.lock().await;
        if let Some(goal) = guard.take() {
            goal.cancel.cancel();
        }
        return Ok(Json(serde_json::json!({ "stopped": true })));
    }

    if handle.turn_running.load(Ordering::SeqCst) {
        return Err((
            StatusCode::CONFLICT,
            "Wait for the current turn to finish before starting a goal loop".into(),
        ));
    }

    {
        let guard = handle.active_goal.lock().await;
        if guard.is_some() {
            return Err((
                StatusCode::CONFLICT,
                "A goal loop is already active for this session".into(),
            ));
        }
    }

    {
        let rt = handle.runtime.lock().await;
        if rt.plan_mode.load(Ordering::SeqCst) {
            return Err((
                StatusCode::BAD_REQUEST,
                "Disable Plan mode before starting an autonomous goal loop".into(),
            ));
        }
    }

    let default_max = state.config.read().await.goal.max_iterations;
    let max_iter = payload.max_iterations.unwrap_or(default_max);
    let objective = payload.objective.unwrap_or_default();
    let mut goal_state = GoalRunState::new(objective, max_iter)
        .map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;

    let goal_cancel = CancellationToken::new();
    let initial_status = WebGoalStatus {
        objective: goal_state.objective.clone(),
        iteration: goal_state.iteration,
        max_iterations: goal_state.max_iterations,
        running: true,
    };
    *handle.active_goal.lock().await = Some(ActiveGoalHandle {
        status: initial_status.clone(),
        cancel: goal_cancel.clone(),
    });

    let st = state.clone();
    let sid = session_id.clone();
    let h = handle.clone();
    tokio::spawn(async move {
        loop {
            if goal_cancel.is_cancelled() {
                break;
            }
            {
                let mut g = h.active_goal.lock().await;
                if let Some(active) = g.as_mut() {
                    active.status.iteration = goal_state.iteration;
                }
            }
            let Ok(prompt) = goal_state.prompt_for_current_iteration() else {
                break;
            };
            let turn_cancel = goal_cancel.child_token();
            *h.active_cancel.lock().unwrap_or_else(|e| e.into_inner()) = Some(turn_cancel.clone());
            h.turn_running.store(true, Ordering::SeqCst);

            let display_query = if goal_state.iteration == 1 {
                format!("/goal {}", goal_state.objective)
            } else {
                format!(
                    "[goal iteration {}/{}]",
                    goal_state.iteration, goal_state.max_iterations
                )
            };

            st.emit_event(AppEvent::TurnStarted {
                session_id: sid.clone(),
                query: display_query,
            });
            let started = std::time::Instant::now();
            let result = {
                let rt = h.runtime.lock().await;
                let ag = rt.agent.lock().await;
                ag.run_turn(&prompt, turn_cancel).await
            };
            h.turn_running.store(false, Ordering::SeqCst);
            let elapsed = started.elapsed().as_millis() as u64;

            match result {
                Ok(outcome) => {
                    {
                        let rt = h.runtime.lock().await;
                        rt.usage
                            .record_chat(
                                &rt.current_provider,
                                &rt.current_model,
                                Some(&sid),
                                &outcome,
                            )
                            .await;
                    }
                    let usage = outcome.usage.clone();
                    let reply = outcome.reply.clone();
                    st.emit_event(AppEvent::TurnFinished {
                        session_id: sid.clone(),
                        reply: reply.clone(),
                        duration_ms: elapsed,
                        prompt_tokens: usage.as_ref().map(|u| u.prompt_tokens).unwrap_or(0),
                        completion_tokens: usage.as_ref().map(|u| u.completion_tokens).unwrap_or(0),
                    });

                    match GoalRunState::evaluate_reply(&reply) {
                        GoalVerdict::Complete(_) | GoalVerdict::Blocked(_) => break,
                        GoalVerdict::Continue => {
                            if !goal_state.advance() {
                                break;
                            }
                        }
                    }
                }
                Err(err) => {
                    st.emit_event(AppEvent::TurnFailed {
                        session_id: sid.clone(),
                        error: err.to_string(),
                    });
                    break;
                }
            }
        }
        *h.active_goal.lock().await = None;
    });

    Ok(Json(serde_json::json!({ "goal": initial_status })))
}

async fn list_tasks(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let map = state.sessions.lock().await;
    if let Some(handle) = map.get(&session_id).cloned() {
        drop(map);
        let goal = handle
            .active_goal
            .lock()
            .await
            .as_ref()
            .map(|g| g.status.clone());
        let rt = handle.runtime.lock().await;
        let subagents = rt.delegate_tool.list_tasks().await;
        let jobs = rt.bg_queue.list();
        return Ok(Json(serde_json::json!({
            "subagents": subagents,
            "jobs": jobs,
            "goal": goal,
        })));
    }
    Ok(Json(serde_json::json!({
        "subagents": [],
        "jobs": [],
        "goal": null,
    })))
}

#[derive(Deserialize)]
struct KillTaskReq {
    kind: String,
}

async fn kill_task(
    State(state): State<Arc<WebState>>,
    Path((session_id, task_id)): Path<(String, String)>,
    Json(payload): Json<KillTaskReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let map = state.sessions.lock().await;
    let handle = map
        .get(&session_id)
        .cloned()
        .ok_or_else(|| (StatusCode::NOT_FOUND, "Session not active".into()))?;
    drop(map);
    let rt = handle.runtime.lock().await;
    let killed = match payload.kind.as_str() {
        "subagent" => rt.delegate_tool.kill(&task_id).await,
        "job" => rt
            .bg_queue
            .kill(&task_id)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        _ => false,
    };
    Ok(Json(serde_json::json!({ "killed": killed })))
}

#[derive(Deserialize)]
struct RunWorkerReq {
    worker_name: String,
    task: String,
    images: Option<Vec<String>>,
}

async fn run_worker(
    State(state): State<Arc<WebState>>,
    Path(session_id): Path<String>,
    Json(payload): Json<RunWorkerReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let name = payload.worker_name.trim().to_ascii_lowercase();
    let mut profile = state
        .config
        .read()
        .await
        .workers
        .get(&name)
        .cloned()
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("Worker '{name}' not found")))?;

    let handle = state
        .get_or_open_session(&session_id, None)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let clean_task = match payload.images {
        Some(imgs) if !imgs.is_empty() => {
            format!("{}\n\n{}", payload.task.trim(), imgs.join("\n\n"))
        }
        _ => payload.task.trim().to_string(),
    };
    if clean_task.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Worker task cannot be empty".into(),
        ));
    }

    let cfg_snap = state.config.read().await.clone();
    let worker_client = GenericProviderClient::new(
        profile.provider.clone(),
        profile.model.clone(),
        state.cred_manager.clone(),
        state.discovery.clone(),
        cfg_snap.providers.get(&profile.provider),
    )
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let rt = handle.runtime.lock().await;
    if profile.reasoning_effort.is_none() {
        profile.reasoning_effort = rt
            .agent
            .try_lock()
            .ok()
            .and_then(|ag| ag.reasoning_effort());
    }
    let child_id = rt
        .delegate_tool
        .spawn_worker(
            &name,
            &clean_task,
            None,
            &session_id,
            Arc::new(worker_client) as Arc<dyn ModelProvider>,
            &profile,
        )
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(serde_json::json!({
        "task_id": child_id,
        "child_session_id": child_id,
        "worker": name,
        "provider": profile.provider,
        "model": profile.model,
    })))
}

// ---------------------------------------------------------------------------
// Usage, Settings, Workers, Profiles, Heartbeat
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct UsageQuery {
    session_id: Option<String>,
}

async fn get_usage(
    State(state): State<Arc<WebState>>,
    Query(query): Query<UsageQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cfg = state.config.read().await.clone();
    let ledger = UsageLedger::open(Some(cfg.paths.usage_db.clone()))
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let global_totals = ledger
        .totals(&UsageFilter::default())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let by_model = ledger
        .summarize(GroupBy::Model, &UsageFilter::default())
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let session_totals = if let Some(sid) = query.session_id.as_deref() {
        Some(
            ledger
                .totals(&UsageFilter {
                    session_id: Some(sid.to_string()),
                    ..Default::default()
                })
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?,
        )
    } else {
        None
    };

    Ok(Json(serde_json::json!({
        "enabled": cfg.usage.enabled,
        "session": session_totals,
        "global": global_totals,
        "by_model": by_model,
    })))
}

async fn get_settings(
    State(state): State<Arc<WebState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cfg = state.config.read().await;
    Ok(Json(serde_json::json!({
        "display": {
            "theme": cfg.display.theme.label(),
            "auto_collapse": cfg.display.auto_collapse,
            "auto_hide_thinking": cfg.display.auto_hide_thinking,
            "block_limit": cfg.display.block_limit,
        },
        "auto_compact": {
            "enabled": cfg.agent.auto_compact_enabled,
            "threshold": cfg.agent.auto_compact_threshold,
            "percent": cfg.agent.auto_compact_percent.unwrap_or(50),
        },
        "features": {
            "memory": cfg.memory_effective_enabled(),
            "notes": cfg.notes_effective_enabled(),
            "skills": cfg.skills_effective_enabled(),
            "mcp": cfg.mcp_effective_enabled(),
            "memory_mode": cfg.memory.mode.as_str(),
        },
        "models": {
            "default_provider": cfg.model.provider,
            "default_model": cfg.model.default,
            "compact_provider": cfg.agent.compaction_provider,
            "compact_model": cfg.agent.compaction_model,
            "background_provider": cfg.memory.background_provider,
            "background_model": cfg.memory.background_model,
            "approval_provider": cfg.tools.approval_provider,
            "approval_model": cfg.tools.approval_model,
        },
        "workers": cfg.workers,
    })))
}

#[derive(Deserialize)]
struct UpdateSettingsReq {
    theme: Option<String>,
    auto_collapse: Option<bool>,
    auto_hide_thinking: Option<bool>,
    block_limit: Option<Option<usize>>,
    auto_compact_enabled: Option<bool>,
    auto_compact_threshold: Option<usize>,
    auto_compact_percent: Option<usize>,
    memory_enabled: Option<bool>,
    notes_enabled: Option<bool>,
    skills_enabled: Option<bool>,
    mcp_enabled: Option<bool>,
    memory_mode: Option<String>,
    session_id: Option<String>,
}

async fn update_settings(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<UpdateSettingsReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    {
        let mut cfg = state.config.write().await;
        if let Some(ref theme_str) = payload.theme {
            if let Ok(mode) = theme_str.parse::<ThemeMode>() {
                cfg.display.theme = mode;
            }
        }
        if let Some(v) = payload.auto_collapse {
            cfg.display.auto_collapse = v;
        }
        if let Some(v) = payload.auto_hide_thinking {
            cfg.display.auto_hide_thinking = v;
        }
        if let Some(limit_opt) = payload.block_limit {
            cfg.display.block_limit = limit_opt.map(|l| l.clamp(1, 10));
        }
        if let Some(v) = payload.auto_compact_enabled {
            cfg.agent.auto_compact_enabled = v;
        }
        if let Some(v) = payload.auto_compact_threshold {
            cfg.agent.auto_compact_threshold = v.clamp(
                harness_core::config::AUTO_COMPACT_MIN_THRESHOLD,
                harness_core::config::AUTO_COMPACT_MAX_THRESHOLD,
            );
        }
        if let Some(v) = payload.auto_compact_percent {
            cfg.agent.auto_compact_percent = Some(v.clamp(
                harness_core::config::AUTO_COMPACT_MIN_PERCENT,
                harness_core::config::AUTO_COMPACT_MAX_PERCENT,
            ));
        }
        if let Some(v) = payload.memory_enabled {
            cfg.agent.memory_enabled = v;
            cfg.features.memory_disabled_until = None;
        }
        if let Some(v) = payload.notes_enabled {
            cfg.agent.notes_enabled = v;
            cfg.features.notes_disabled_until = None;
        }
        if let Some(v) = payload.skills_enabled {
            cfg.agent.skills_enabled = v;
            cfg.features.skills_disabled_until = None;
        }
        if let Some(v) = payload.mcp_enabled {
            cfg.mcp.enabled = v;
            cfg.features.mcp_disabled_until = None;
        }
        if let Some(ref mode_str) = payload.memory_mode {
            if let Ok(mode) = mode_str.parse::<MemoryMode>() {
                cfg.memory.mode = mode;
            }
        }
        persist_config(&cfg)?;
    }

    // Apply live to active session if present
    let cfg_snap = state.config.read().await.clone();
    if let Some(sid) = payload.session_id.as_deref() {
        let handle = state.sessions.lock().await.get(sid).cloned();
        if let Some(h) = handle {
            let rt = h.runtime.lock().await;
            let mut ag = rt.agent.lock().await;
            if let Some(v) = payload.auto_compact_enabled {
                ag.set_auto_compact_enabled(v);
            }
            if let Some(_v) = payload.auto_compact_threshold {
                ag.set_auto_compact_threshold(cfg_snap.agent.auto_compact_threshold);
            }
            if let Some(v) = payload.auto_compact_percent {
                let _ = v;
                ag.set_auto_compact_percent(cfg_snap.agent.auto_compact_percent.unwrap_or(50));
            }
            if let Some(v) = payload.memory_enabled {
                ag.set_memory_enabled(v);
                rt.feature_toggles
                    .set(harness_app::features::Feature::Memory, v);
            }
            if let Some(v) = payload.notes_enabled {
                ag.set_notes_enabled(v);
                rt.feature_toggles
                    .set(harness_app::features::Feature::Notes, v);
            }
            if let Some(v) = payload.skills_enabled {
                ag.set_skills_enabled(v);
                rt.feature_toggles
                    .set(harness_app::features::Feature::Skills, v);
            }
            if let Some(v) = payload.mcp_enabled {
                ag.set_mcp_enabled(v);
                rt.feature_toggles
                    .set(harness_app::features::Feature::Mcp, v);
            }
            if payload.memory_mode.is_some() {
                if cfg_snap.memory.mode.is_background() {
                    let prov = cfg_snap
                        .memory
                        .background_provider
                        .clone()
                        .unwrap_or_else(|| rt.current_provider.clone());
                    let modl = cfg_snap
                        .memory
                        .background_model
                        .clone()
                        .unwrap_or_else(|| rt.current_model.clone());
                    let mem_dyn: Arc<dyn MemoryEngine> = rt.memory.clone();
                    let notes_dyn: Arc<dyn NotesEngine> = rt.notes.clone();
                    if let Ok(saver) = build_saver_for(
                        &cfg_snap,
                        &mem_dyn,
                        notes_dyn,
                        &prov,
                        &modl,
                        rt.cred_manager.clone(),
                        rt.discovery.clone(),
                    ) {
                        ag.set_background_saver(Some(Arc::new(saver)));
                    }
                } else {
                    ag.set_background_saver(None);
                }
            }
        }
    }

    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
struct UpsertWorkerReq {
    name: String,
    provider: String,
    model: String,
    reasoning_effort: Option<String>,
}

async fn upsert_worker(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<UpsertWorkerReq>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let name = payload.name.trim().to_ascii_lowercase();
    validate_worker_name(&name).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let effort = parse_reasoning_effort(payload.reasoning_effort.as_deref())?;
    let profile = WorkerProfile {
        provider: payload.provider.trim().to_string(),
        model: payload.model.trim().to_string(),
        reasoning_effort: effort,
    };
    if profile.provider.is_empty() || profile.model.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "Provider and model are required".into(),
        ));
    }

    let mut cfg = state.config.write().await;
    cfg.workers.insert(name.clone(), profile);
    persist_config(&cfg)?;
    Ok(Json(serde_json::json!({ "workers": cfg.workers })))
}

async fn delete_worker(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let mut cfg = state.config.write().await;
    cfg.workers.remove(&name.to_ascii_lowercase());
    persist_config(&cfg)?;
    Ok(Json(serde_json::json!({ "workers": cfg.workers })))
}

async fn list_profiles(
    State(state): State<Arc<WebState>>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let profiles_dir = state.config.read().await.paths.profiles_dir.clone();
    let items: Vec<serde_json::Value> = profile_catalog(&profiles_dir)
        .into_iter()
        .map(|summary| {
            let full = load_profile(&summary.name, &profiles_dir).ok();
            serde_json::json!({
                "name": summary.name,
                "summary": summary.summary,
                "builtin": summary.builtin,
                "persona": full.as_ref().map(|p| p.persona.clone()),
                "behavior": full.as_ref().map(|p| p.behavior.clone()),
            })
        })
        .collect();
    Ok(Json(serde_json::json!({ "profiles": items })))
}

#[derive(Deserialize)]
struct CreateProfileReq {
    name: String,
    summary: Option<String>,
    persona: Option<String>,
    behavior: Option<String>,
}

async fn create_profile(
    State(state): State<Arc<WebState>>,
    Json(payload): Json<CreateProfileReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    let name = payload.name.trim().to_ascii_lowercase();
    validate_profile_name(&name).map_err(|e| (StatusCode::BAD_REQUEST, e.to_string()))?;
    let (profiles_dir, creds_src) = {
        let cfg = state.config.read().await;
        (cfg.paths.profiles_dir.clone(), cfg.paths.credentials.clone())
    };
    let dir = profile_dir(&profiles_dir, &name);
    if dir.exists() {
        return Err((
            StatusCode::CONFLICT,
            format!("Profile '{name}' already exists"),
        ));
    }
    let def = default_profile().map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let paths = paths_for(&dir);
    paths
        .ensure_dirs()
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let summary = payload
        .summary
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&def.summary);
    let persona = payload
        .persona
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&def.persona);
    let behavior = payload
        .behavior
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&def.behavior);

    let md = format!("# Summary\n{summary}\n\n# Persona\n{persona}\n\n# Behavior\n{behavior}\n");
    std::fs::write(dir.join(PROFILE_FILE), md)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    if creds_src.exists() {
        let _ = std::fs::copy(&creds_src, &paths.credentials);
    }
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
struct UpdateProfileReq {
    summary: Option<String>,
    persona: Option<String>,
    behavior: Option<String>,
}

async fn update_profile(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
    Json(payload): Json<UpdateProfileReq>,
) -> Result<StatusCode, (StatusCode, String)> {
    if name == DEFAULT_PROFILE {
        return Err((
            StatusCode::BAD_REQUEST,
            "Cannot modify the built-in default profile".into(),
        ));
    }
    let profiles_dir = state.config.read().await.paths.profiles_dir.clone();
    let mut prof = load_profile(&name, &profiles_dir)
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;
    let dir = profile_dir(&profiles_dir, &name);

    if let Some(s) = payload.summary.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        prof.summary = s.to_string();
    }
    if let Some(p) = payload.persona.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        prof.persona = p.to_string();
    }
    if let Some(b) = payload.behavior.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        prof.behavior = b.to_string();
    }

    let md = format!(
        "# Summary\n{}\n\n# Persona\n{}\n\n# Behavior\n{}\n",
        prof.summary, prof.persona, prof.behavior
    );
    std::fs::write(dir.join(PROFILE_FILE), md)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::OK)
}

async fn delete_profile(
    State(state): State<Arc<WebState>>,
    Path(name): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    if name == DEFAULT_PROFILE {
        return Err((
            StatusCode::BAD_REQUEST,
            "Cannot delete the built-in default profile".into(),
        ));
    }
    let profiles_dir = state.config.read().await.paths.profiles_dir.clone();
    let dir = profile_dir(&profiles_dir, &name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    Ok(StatusCode::OK)
}

#[derive(Deserialize)]
struct HeartbeatQuery {
    session_id: Option<String>,
}

async fn get_heartbeat(
    State(state): State<Arc<WebState>>,
    Query(query): Query<HeartbeatQuery>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    let cfg = state.config.read().await.clone();
    let ws = cfg.paths.workspace_root.clone();

    let git_status = std::process::Command::new("git")
        .args(["status", "--porcelain", "-b"])
        .current_dir(&ws)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            let text = String::from_utf8_lossy(&o.stdout);
            let mut lines = text.lines();
            let branch = lines
                .next()
                .unwrap_or("## unknown")
                .strip_prefix("## ")
                .and_then(|s| s.split("...").next())
                .unwrap_or("unknown")
                .to_string();
            let dirty = lines.count();
            serde_json::json!({ "branch": branch, "dirty": dirty })
        });

    #[cfg(target_os = "linux")]
    let rss_mb = std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map(|pages| (pages * 4096) as f64 / (1024.0 * 1024.0));
    #[cfg(not(target_os = "linux"))]
    let rss_mb: Option<f64> = None;

    let active_sessions = state.sessions.lock().await.len();
    let mut skills_count = 0usize;
    if let Ok(skills) = SkillLoader::load_skills_from_dir(&cfg.paths.skills_dir) {
        skills_count += skills.len();
    }
    let ws_skills = ws.join(".harness/skills");
    if ws_skills.exists() {
        if let Ok(skills) = SkillLoader::load_skills_from_dir(&ws_skills) {
            skills_count += skills.len();
        }
    }

    let mut tools_list = Vec::new();
    let mut context_tokens = 0u32;
    let mut session_busy = false;
    if let Some(sid) = query.session_id.as_deref() {
        if let Some(h) = state.sessions.lock().await.get(sid).cloned() {
            session_busy = h.turn_running.load(Ordering::SeqCst);
            if let Ok(rt) = h.runtime.try_lock() {
                if let Ok(ag) = rt.agent.try_lock() {
                    if let Ok(tools) = ag.registry().list_tools().await {
                        tools_list = tools.into_iter().map(|t| t.name).collect();
                    }
                    context_tokens = ag.estimate_session_tokens().await.unwrap_or(0);
                }
            }
        }
    }

    Ok(Json(serde_json::json!({
        "pid": std::process::id(),
        "rss_mb": rss_mb,
        "workspace_root": ws.display().to_string(),
        "git": git_status,
        "active_sessions": active_sessions,
        "session_busy": session_busy,
        "tools_count": tools_list.len(),
        "tools": tools_list,
        "skills_count": skills_count,
        "mcp_servers_count": cfg.mcp.servers.len(),
        "context_tokens": context_tokens,
    })))
}
