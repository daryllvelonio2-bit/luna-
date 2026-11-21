use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use harness_plugins::PluginSummary;

use crate::state::WebState;

pub fn plugin_router() -> Router<Arc<WebState>> {
    Router::new()
        .route("/api/plugins/{plugin_id}", get(get_plugin))
        .route("/api/plugins/{plugin_id}/reload", post(reload_plugin))
        .route(
            "/api/plugins/{plugin_id}/commands/{command_name}",
            post(run_plugin_command),
        )
        .route(
            "/api/plugins/{plugin_id}/tools/{tool_name}",
            post(run_plugin_tool),
        )
        .route(
            "/api/plugins/{plugin_id}/settings",
            get(get_plugin_settings).post(update_plugin_settings),
        )
        .route("/api/plugins/{plugin_id}/ui", get(serve_plugin_ui_index))
        .route("/api/plugins/{plugin_id}/ui/", get(serve_plugin_ui_index))
        .route(
            "/api/plugins/{plugin_id}/ui/{*asset_path}",
            get(serve_plugin_ui_asset),
        )
}

#[derive(Serialize)]
struct PluginExecResponse {
    ok: bool,
    output: String,
    summary: Option<PluginSummary>,
}

#[derive(Deserialize)]
struct UiThemeQuery {
    theme: Option<String>,
}

async fn get_plugin(
    State(state): State<Arc<WebState>>,
    Path(plugin_id): Path<String>,
) -> Result<Json<PluginSummary>, (StatusCode, String)> {
    state
        .plugin_manager
        .get_summary(&plugin_id)
        .map(Json)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("plugin '{plugin_id}' not found")))
}

async fn reload_plugin(
    State(state): State<Arc<WebState>>,
    Path(plugin_id): Path<String>,
) -> Result<Json<PluginSummary>, (StatusCode, String)> {
    let summary = state
        .plugin_manager
        .reload(&plugin_id)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    state.sync_plugins_after_change().await;
    Ok(Json(summary))
}

async fn run_plugin_command(
    State(state): State<Arc<WebState>>,
    Path((plugin_id, command_name)): Path<(String, String)>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<PluginExecResponse>, (StatusCode, String)> {
    let args = body
        .map(|Json(v)| v)
        .unwrap_or_else(|| serde_json::json!({}));
    let output = state
        .plugin_manager
        .invoke_command(&plugin_id, &command_name, args)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let summary = state.plugin_manager.get_summary(&plugin_id);
    Ok(Json(PluginExecResponse {
        ok: true,
        output,
        summary,
    }))
}

async fn run_plugin_tool(
    State(state): State<Arc<WebState>>,
    Path((plugin_id, tool_name)): Path<(String, String)>,
    body: Option<Json<serde_json::Value>>,
) -> Result<Json<PluginExecResponse>, (StatusCode, String)> {
    let args = body
        .map(|Json(v)| v)
        .unwrap_or_else(|| serde_json::json!({}));
    let output = state
        .plugin_manager
        .invoke_tool(&plugin_id, &tool_name, args)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    let summary = state.plugin_manager.get_summary(&plugin_id);
    Ok(Json(PluginExecResponse {
        ok: true,
        output,
        summary,
    }))
}

async fn get_plugin_settings(
    State(state): State<Arc<WebState>>,
    Path(plugin_id): Path<String>,
) -> Result<Json<BTreeMap<String, serde_json::Value>>, (StatusCode, String)> {
    let summary = state
        .plugin_manager
        .get_summary(&plugin_id)
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("plugin '{plugin_id}' not found")))?;
    Ok(Json(summary.settings))
}

async fn update_plugin_settings(
    State(state): State<Arc<WebState>>,
    Path(plugin_id): Path<String>,
    Json(patch): Json<BTreeMap<String, serde_json::Value>>,
) -> Result<Json<PluginSummary>, (StatusCode, String)> {
    let summary = state
        .plugin_manager
        .set_settings_patch(&plugin_id, patch)
        .await
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    Ok(Json(summary))
}

async fn serve_plugin_ui_index(
    State(state): State<Arc<WebState>>,
    Path(plugin_id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<UiThemeQuery>,
) -> Response {
    serve_ui_inner(state, &plugin_id, "index.html", headers, query.theme.as_deref())
}

async fn serve_plugin_ui_asset(
    State(state): State<Arc<WebState>>,
    Path((plugin_id, asset_path)): Path<(String, String)>,
    headers: HeaderMap,
    Query(query): Query<UiThemeQuery>,
) -> Response {
    serve_ui_inner(
        state,
        &plugin_id,
        &asset_path,
        headers,
        query.theme.as_deref(),
    )
}

fn serve_ui_inner(
    state: Arc<WebState>,
    plugin_id: &str,
    rel_path: &str,
    headers: HeaderMap,
    theme: Option<&str>,
) -> Response {
    if !state.auth.verify_trust_fence(&headers) {
        return (
            StatusCode::FORBIDDEN,
            "403 Forbidden: Host or Origin rejected by Luna trust fence",
        )
            .into_response();
    }

    let Ok((bytes, mime)) = state.plugin_manager.read_ui_asset(plugin_id, rel_path) else {
        return (
            StatusCode::NOT_FOUND,
            format!("UI asset '{}' not found for plugin '{}'", rel_path, plugin_id),
        )
            .into_response();
    };

    if mime.starts_with("text/html") {
        let raw_html = String::from_utf8_lossy(&bytes);
        let injected = inject_plugin_bridge(&raw_html, plugin_id, theme.unwrap_or("dark"));
        return (
            StatusCode::OK,
            [
                (CONTENT_TYPE, mime.as_str()),
                (CACHE_CONTROL, "no-store, max-age=0"),
            ],
            injected,
        )
            .into_response();
    }

    (
        StatusCode::OK,
        [
            (CONTENT_TYPE, mime.as_str()),
            (CACHE_CONTROL, "no-store, max-age=0"),
        ],
        bytes,
    )
        .into_response()
}

fn inject_plugin_bridge(html: &str, plugin_id: &str, initial_theme: &str) -> String {
    let pid_json = serde_json::to_string(plugin_id).unwrap_or_else(|_| "\"\"".to_string());
    let theme_norm = if initial_theme.eq_ignore_ascii_case("light") {
        "light"
    } else {
        "dark"
    };
    let encoded_id = urlencoding_segment(plugin_id);
    let base_tag = if html.to_ascii_lowercase().contains("<base") {
        String::new()
    } else {
        format!("<base href=\"/api/plugins/{encoded_id}/ui/\">")
    };

    let prelude = format!(
        r#"{base_tag}<style id="luna-plugin-theme-vars">
:root, :root[data-theme="dark"] {{
  color-scheme: dark;
  --bg: #0f1115;
  --surface: #161920;
  --surface-2: #1e232d;
  --text: #e6eaf2;
  --muted: #939db2;
  --border: #2a3140;
  --accent: #6e8efb;
  --accent-fg: #ffffff;
  --success: #3ecf8e;
  --danger: #f46868;
}}
:root[data-theme="light"] {{
  color-scheme: light;
  --bg: #f7f8fa;
  --surface: #ffffff;
  --surface-2: #f0f2f6;
  --text: #151821;
  --muted: #606a7d;
  --border: #dfe3ec;
  --accent: #4c6ef5;
  --accent-fg: #ffffff;
  --success: #16a34a;
  --danger: #dc2626;
}}
body {{
  background: var(--bg);
  color: var(--text);
  margin: 0;
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
}}
</style>
<script id="luna-plugin-bridge">
(function() {{
  var pid = {pid_json};
  var enc = encodeURIComponent(pid);
  var root = document.documentElement;
  root.setAttribute('data-theme', '{theme_norm}');
  window.addEventListener('message', function(ev) {{
    if (ev && ev.data && ev.data.type === 'luna:theme' && (ev.data.theme === 'dark' || ev.data.theme === 'light')) {{
      root.setAttribute('data-theme', ev.data.theme);
    }}
  }});
  async function postJson(url, payload) {{
    var res = await fetch(url, {{
      method: 'POST',
      headers: {{ 'Content-Type': 'application/json' }},
      credentials: 'same-origin',
      body: JSON.stringify(payload === undefined ? {{}} : payload)
    }});
    var text = await res.text();
    var data = text ? JSON.parse(text) : {{}};
    if (!res.ok) {{
      throw new Error((data && (data.error || data.message)) || text || ('HTTP ' + res.status));
    }}
    return data;
  }}
  window.lunaPlugin = {{
    pluginId: pid,
    getTheme: function() {{ return root.getAttribute('data-theme') || '{theme_norm}'; }},
    callTool: function(name, args) {{
      return postJson('/api/plugins/' + enc + '/tools/' + encodeURIComponent(name), args || {{}});
    }},
    runCommand: function(name, args) {{
      return postJson('/api/plugins/' + enc + '/commands/' + encodeURIComponent(name), args || {{}});
    }},
    getSettings: async function() {{
      var res = await fetch('/api/plugins/' + enc + '/settings', {{ credentials: 'same-origin' }});
      if (!res.ok) throw new Error('HTTP ' + res.status);
      return res.json();
    }},
    setSettings: function(patch) {{
      return postJson('/api/plugins/' + enc + '/settings', patch || {{}});
    }},
    getSummary: async function() {{
      var res = await fetch('/api/plugins/' + enc, {{ credentials: 'same-origin' }});
      if (!res.ok) throw new Error('HTTP ' + res.status);
      return res.json();
    }},
    sendToChat: function(message) {{
      window.parent.postMessage({{ type: 'luna:plugin:sendToChat', pluginId: pid, message: String(message || '') }}, '*');
    }},
    notify: function(message, kind) {{
      window.parent.postMessage({{ type: 'luna:plugin:notify', pluginId: pid, message: String(message || ''), kind: kind || 'info' }}, '*');
    }},
    refreshParent: function() {{
      window.parent.postMessage({{ type: 'luna:plugin:refresh', pluginId: pid }}, '*');
    }}
  }};
}})();
</script>"#
    );

    if let Some(pos) = html.to_ascii_lowercase().find("</head>") {
        let mut out = String::with_capacity(html.len() + prelude.len());
        out.push_str(&html[..pos]);
        out.push_str(&prelude);
        out.push_str(&html[pos..]);
        out
    } else if let Some(pos) = html.to_ascii_lowercase().find("<body") {
        let mut out = String::with_capacity(html.len() + prelude.len());
        out.push_str(&html[..pos]);
        out.push_str(&prelude);
        out.push_str(&html[pos..]);
        out
    } else {
        format!("{prelude}\n{html}")
    }
}

fn urlencoding_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char);
            }
            _ => {
                out.push_str(&format!("%{:02X}", b));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::config::Config;
    use harness_core::Tool;
    use harness_plugins::PluginCreateTool;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn inject_plugin_bridge_adds_base_theme_and_api() {
        let html = "<!doctype html><html><head><title>App</title></head><body><div id=\"app\"></div></body></html>";
        let out = inject_plugin_bridge(html, "kanban-board", "light");
        assert!(out.contains("<base href=\"/api/plugins/kanban-board/ui/\">"));
        assert!(out.contains("window.lunaPlugin"));
        assert!(out.contains("data-theme', 'light'"));
        assert!(out.contains("callTool"));
        assert!(out.contains("runCommand"));
    }

    #[tokio::test]
    async fn live_http_plugin_ui_tools_commands_panels_and_settings() {
        let tmp = tempfile::tempdir().unwrap();
        let data_dir = tmp.path().join("luna");
        let ws_dir = tmp.path().join("ws");
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::create_dir_all(&ws_dir).unwrap();

        let mut cfg = Config::default();
        cfg.paths.workspace_root = ws_dir.clone();
        cfg.paths.sessions_db = data_dir.join("sessions.db");
        cfg.paths.credentials = data_dir.join("credentials.json");
        cfg.paths.models_cache = data_dir.join("models_cache.json");
        cfg.paths.skills_dir = data_dir.join("skills");
        cfg.paths.todos_dir = data_dir.join("todos");

        let state = WebState::new(cfg, "127.0.0.1".to_string(), Vec::new(), Some("tok".into()))
            .await
            .unwrap();

        // Create a full-stack plugin with tools, commands, live panels, settings, and custom UI
        let create_tool = PluginCreateTool::new(state.plugin_manager.clone());
        let ts_source = r#"
import type { Context } from '@luna/sdk';

export const name = 'task-board';

export function apply(ctx: Context): void {
  const getTasks = (): Array<{ title: string; done: boolean }> => {
    return (ctx.storage.get('tasks') as Array<{ title: string; done: boolean }>) || [];
  };

  const syncPanel = () => {
    const tasks = getTasks();
    const doneCount = tasks.filter((t) => t.done).length;
    ctx.ui.updatePanel('board-panel', {
      title: 'Task Board',
      widgets: [
        { kind: 'stat_card', id: 'total', label: 'Total Tasks', value: String(tasks.length) },
        { kind: 'progress', id: 'prog', label: 'Done', value: doneCount, max: Math.max(1, tasks.length) }
      ]
    });
  };

  ctx.ui.registerPanel({
    id: 'board-panel',
    title: 'Task Board',
    widgets: [{ kind: 'stat_card', id: 'total', label: 'Total Tasks', value: '0' }]
  });

  ctx.tools.register(
    {
      name: 'add_task',
      description: 'Add a task to the board',
      mutating: true,
      parameters: {
        type: 'object',
        properties: { title: { type: 'string' } },
        required: ['title']
      }
    },
    (args: Record<string, unknown>) => {
      const tasks = getTasks();
      tasks.push({ title: String(args.title || 'Untitled'), done: false });
      ctx.storage.set('tasks', tasks);
      syncPanel();
      return JSON.stringify({ ok: true, count: tasks.length, tasks });
    }
  );

  ctx.commands.register('mark_first_done', 'Mark first task done', () => {
    const tasks = getTasks();
    if (tasks.length > 0) tasks[0].done = true;
    ctx.storage.set('tasks', tasks);
    syncPanel();
    return JSON.stringify({ tasks });
  });

  ctx.events.on('settings_changed', (s: Record<string, unknown>) => {
    ctx.storage.set('last_settings', s);
  });
}
"#;

        let res = create_tool
            .execute(
                "call-1",
                &serde_json::json!({
                    "id": "task-board",
                    "name": "Task Board",
                    "description": "Full interactive task board plugin",
                    "ts_source": ts_source,
                    "config_schema": [
                        { "key": "board_name", "label": "Board Name", "field_type": "string", "default_value": "Sprint 1" }
                    ],
                    "ui_files": {
                        "index.html": "<!doctype html><html><head><link rel=\"stylesheet\" href=\"app.css\"></head><body><div id=\"board\">Board</div><script src=\"app.js\"></script></body></html>",
                        "app.css": "body { padding: 12px; }",
                        "app.js": "console.log('task-board ready');"
                    }
                }),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(!res.is_error, "create failed: {}", res.content);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = crate::routes::create_router(state.clone());
        let srv = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });

        let client = reqwest::Client::new();
        let base = format!("http://{addr}");

        // 1. Fetch plugin summary
        let summary: PluginSummary = client
            .get(format!("{base}/api/plugins/task-board"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(summary.has_custom_ui);
        assert_eq!(summary.config_schema.len(), 1);
        assert_eq!(summary.tool_schemas.len(), 1);

        // 2. Fetch custom UI HTML + verify injected window.lunaPlugin bridge & CSS vars
        let ui_resp = client
            .get(format!("{base}/api/plugins/task-board/ui/?theme=dark"))
            .send()
            .await
            .unwrap();
        assert_eq!(ui_resp.status(), reqwest::StatusCode::OK);
        let ui_html = ui_resp.text().await.unwrap();
        assert!(ui_html.contains("window.lunaPlugin"));
        assert!(ui_html.contains("<base href=\"/api/plugins/task-board/ui/\">"));
        assert!(ui_html.contains("data-theme', 'dark'"));

        // 3. Fetch custom UI JS & CSS assets
        let js_resp = client
            .get(format!("{base}/api/plugins/task-board/ui/app.js"))
            .send()
            .await
            .unwrap();
        assert_eq!(js_resp.status(), reqwest::StatusCode::OK);
        assert!(js_resp.text().await.unwrap().contains("task-board ready"));

        // 4. Invoke plugin tool via REST bridge -> updates storage & live panel
        let tool_resp: serde_json::Value = client
            .post(format!("{base}/api/plugins/task-board/tools/add_task"))
            .json(&serde_json::json!({ "title": "Ship custom plugin UI" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(tool_resp["ok"], true);
        assert!(tool_resp["output"].as_str().unwrap().contains("Ship custom plugin UI"));
        assert_eq!(tool_resp["summary"]["panels"][0]["widgets"][0]["value"], "1");

        // 5. Invoke plugin command via REST bridge -> updates progress widget live
        let cmd_resp: serde_json::Value = client
            .post(format!("{base}/api/plugins/task-board/commands/mark_first_done"))
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(cmd_resp["ok"], true);
        assert_eq!(cmd_resp["summary"]["panels"][0]["widgets"][1]["value"], 1.0);

        // 6. Save plugin settings via REST bridge
        let settings_resp: PluginSummary = client
            .post(format!("{base}/api/plugins/task-board/settings"))
            .json(&serde_json::json!({ "board_name": "Q4 Launch" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(
            settings_resp.settings.get("board_name").and_then(|v| v.as_str()),
            Some("Q4 Launch")
        );

        srv.abort();
    }
}
