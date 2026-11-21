//! Embedded QuickJS plugin runtime (`rquickjs`) exposing the DeepSeek/Cordis-style `ctx` API.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rquickjs::{Context as JsContext, Function, Module, Runtime as JsRuntime, Value as JsValue};
use tokio::sync::{mpsc, oneshot};

use crate::manifest::{
    PluginAgentEntry, PluginCommandSpec, PluginManifest, PluginScheduleDecl, PluginUiPanel,
    PluginWidget,
};
use crate::storage::{load_settings, save_settings, PluginStorage};

/// Tool metadata registered by a plugin via `ctx.tools.register(...)`.
#[derive(Debug, Clone)]
pub struct RegisteredPluginTool {
    pub plugin_id: String,
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
    pub is_mutating: bool,
}

/// Snapshot of everything a plugin registered during `apply(ctx)`.
#[derive(Debug, Clone, Default)]
pub struct PluginRegistrations {
    pub tools: Vec<RegisteredPluginTool>,
    pub agents: Vec<PluginAgentEntry>,
    pub commands: Vec<PluginCommandSpec>,
    pub schedules: Vec<PluginScheduleDecl>,
    pub panels: Vec<PluginUiPanel>,
}

enum ActorCmd {
    CallTool {
        name: String,
        args_json: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
    CallCommand {
        name: String,
        args_json: String,
        reply: oneshot::Sender<Result<String, String>>,
    },
    EmitEvent {
        event: String,
        payload_json: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Dispose {
        reply: oneshot::Sender<()>,
    },
}

/// Handle to a running plugin QuickJS actor thread.
pub struct PluginRuntimeHandle {
    pub plugin_id: String,
    pub registrations: PluginRegistrations,
    live_panels: Arc<Mutex<Vec<PluginUiPanel>>>,
    live_custom_html: Arc<Mutex<Option<String>>>,
    cmd_tx: mpsc::UnboundedSender<ActorCmd>,
    join_handle: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl PluginRuntimeHandle {
    pub fn spawn(
        manifest: PluginManifest,
        plugin_dir: PathBuf,
        js_code: String,
    ) -> Result<Self, String> {
        let tokio_handle = tokio::runtime::Handle::try_current().ok();
        let (init_tx, init_rx) = std::sync::mpsc::channel::<
            Result<(PluginRegistrations, mpsc::UnboundedSender<ActorCmd>), String>,
        >();
        let live_panels = Arc::new(Mutex::new(Vec::new()));
        let live_custom_html = Arc::new(Mutex::new(None));
        let live_panels_clone = live_panels.clone();
        let live_custom_html_clone = live_custom_html.clone();

        let plugin_id = manifest.id.clone();
        let thread_name = format!("luna-plugin-{}", plugin_id);
        let handle = std::thread::Builder::new()
            .name(thread_name)
            .spawn(move || {
                run_plugin_actor(
                    manifest,
                    plugin_dir,
                    js_code,
                    tokio_handle,
                    live_panels_clone,
                    live_custom_html_clone,
                    init_tx,
                );
            })
            .map_err(|e| format!("failed to spawn plugin thread: {e}"))?;

        match init_rx.recv() {
            Ok(Ok((registrations, cmd_tx))) => Ok(Self {
                plugin_id,
                registrations,
                live_panels,
                live_custom_html,
                cmd_tx,
                join_handle: Mutex::new(Some(handle)),
            }),
            Ok(Err(err)) => {
                let _ = handle.join();
                Err(err)
            }
            Err(e) => {
                let _ = handle.join();
                Err(format!("plugin actor terminated during startup: {e}"))
            }
        }
    }

    pub fn live_panels(&self) -> Vec<PluginUiPanel> {
        self.live_panels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn live_custom_html(&self) -> Option<String> {
        self.live_custom_html
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub async fn call_tool(&self, name: &str, args: &serde_json::Value) -> Result<String, String> {
        let (tx, rx) = oneshot::channel();
        let args_json = serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string());
        self.cmd_tx
            .send(ActorCmd::CallTool {
                name: name.to_string(),
                args_json,
                reply: tx,
            })
            .map_err(|_| format!("plugin '{}' is not running", self.plugin_id))?;
        rx.await
            .map_err(|_| format!("plugin '{}' dropped tool response", self.plugin_id))?
    }

    pub async fn call_command(
        &self,
        name: &str,
        args: &serde_json::Value,
    ) -> Result<String, String> {
        let (tx, rx) = oneshot::channel();
        let args_json = serde_json::to_string(args).unwrap_or_else(|_| "{}".to_string());
        self.cmd_tx
            .send(ActorCmd::CallCommand {
                name: name.to_string(),
                args_json,
                reply: tx,
            })
            .map_err(|_| format!("plugin '{}' is not running", self.plugin_id))?;
        rx.await
            .map_err(|_| format!("plugin '{}' dropped command response", self.plugin_id))?
    }

    pub async fn emit_event(&self, event: &str, payload: &serde_json::Value) -> Result<(), String> {
        let (tx, rx) = oneshot::channel();
        let payload_json = serde_json::to_string(payload).unwrap_or_else(|_| "{}".to_string());
        self.cmd_tx
            .send(ActorCmd::EmitEvent {
                event: event.to_string(),
                payload_json,
                reply: tx,
            })
            .map_err(|_| format!("plugin '{}' is not running", self.plugin_id))?;
        rx.await
            .map_err(|_| format!("plugin '{}' dropped event response", self.plugin_id))?
    }

    pub fn shutdown(&self) {
        let (tx, rx) = oneshot::channel();
        let _ = self.cmd_tx.send(ActorCmd::Dispose { reply: tx });
        if tokio::runtime::Handle::try_current().is_err() {
            let _ = rx.blocking_recv();
            if let Ok(mut guard) = self.join_handle.lock() {
                if let Some(h) = guard.take() {
                    let _ = h.join();
                }
            }
        } else if let Ok(mut guard) = self.join_handle.lock() {
            if let Some(h) = guard.take() {
                std::thread::spawn(move || {
                    let _ = h.join();
                });
            }
        }
    }
}

impl Drop for PluginRuntimeHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

const SDK_PRELUDE: &str = r#"
globalThis.__luna_state = {
  tools: Object.create(null),
  commands: Object.create(null),
  events: Object.create(null),
  disposers: []
};

globalThis.__luna_create_ctx = function(pluginId) {
  return {
    pluginId: pluginId,
    tools: {
      register: function(def, handler) {
        if (!def || !def.name) throw new Error('tool definition requires a name');
        globalThis.__luna_state.tools[def.name] = handler;
        __luna_host.registerTool(JSON.stringify({
          name: String(def.name),
          description: String(def.description || ''),
          parameters: def.parameters || { type: 'object', properties: {} },
          mutating: Boolean(def.mutating)
        }));
      }
    },
    agents: {
      register: function(def) {
        if (!def || !def.name) throw new Error('agent definition requires a name');
        __luna_host.registerAgent(JSON.stringify(def));
      }
    },
    commands: {
      register: function(name, descOrFn, maybeFn) {
        const description = typeof descOrFn === 'string' ? descOrFn : '';
        const handler = typeof descOrFn === 'function' ? descOrFn : maybeFn;
        globalThis.__luna_state.commands[name] = handler;
        __luna_host.registerCommand(JSON.stringify({ name: String(name), description: String(description) }));
      }
    },
    schedules: {
      register: function(def) {
        if (!def || !def.name || !def.rule || !def.instruction) {
          throw new Error('schedule requires name, rule, and instruction');
        }
        __luna_host.registerSchedule(JSON.stringify(def));
      }
    },
    events: {
      on: function(eventName, handler) {
        if (!globalThis.__luna_state.events[eventName]) {
          globalThis.__luna_state.events[eventName] = [];
        }
        globalThis.__luna_state.events[eventName].push(handler);
      },
      emit: function(eventName, payload) {
        const list = globalThis.__luna_state.events[eventName] || [];
        for (const fn of list) {
          fn(payload);
        }
      }
    },
    fs: {
      readText: function(path) { return __luna_host.fsReadText(String(path)); },
      writeText: function(path, text) { return __luna_host.fsWriteText(String(path), String(text)); },
      exists: function(path) { return __luna_host.fsExists(String(path)); },
      listDir: function(path) { return JSON.parse(__luna_host.fsListDir(String(path))); },
      remove: function(path) { return __luna_host.fsRemove(String(path)); }
    },
    process: {
      exec: function(cmd, args, cwd) {
        const res = __luna_host.processExec(String(cmd), JSON.stringify(args || []), cwd ? String(cwd) : '');
        return JSON.parse(res);
      }
    },
    net: {
      fetch: function(url, options) {
        const res = __luna_host.netFetch(String(url), JSON.stringify(options || {}));
        return JSON.parse(res);
      }
    },
    storage: {
      get: function(key) {
        const raw = __luna_host.storageGet(String(key));
        return raw === '' ? null : JSON.parse(raw);
      },
      set: function(key, val) {
        __luna_host.storageSet(String(key), JSON.stringify(val === undefined ? null : val));
      },
      delete: function(key) { return __luna_host.storageDelete(String(key)); },
      list: function() { return JSON.parse(__luna_host.storageList()); }
    },
    settings: {
      get: function(key) {
        const all = JSON.parse(__luna_host.settingsAll());
        return key ? all[key] : all;
      },
      set: function(key, val) {
        __luna_host.settingsSet(String(key), JSON.stringify(val === undefined ? null : val));
      }
    },
    ui: {
      registerPanel: function(panel) {
        __luna_host.registerPanel(JSON.stringify(panel));
      },
      updatePanel: function(panelId, patch) {
        __luna_host.updatePanel(String(panelId || ''), JSON.stringify(patch || {}));
      },
      removePanel: function(panelId) {
        __luna_host.removePanel(String(panelId || ''));
      },
      getPanels: function() {
        return JSON.parse(__luna_host.getPanels());
      },
      setCustomHtml: function(html) {
        __luna_host.setCustomHtml(html === null || html === undefined ? '' : String(html));
      }
    },
    effect: function(disposer) {
      if (typeof disposer === 'function') {
        globalThis.__luna_state.disposers.push(disposer);
      }
    },
    log: function(level, msg) {
      __luna_host.log(String(level || 'info'), String(msg || ''));
    }
  };
};

globalThis.__luna_invoke_tool = function(name, argsJson) {
  const fn = globalThis.__luna_state.tools[name];
  if (!fn) throw new Error('unknown plugin tool: ' + name);
  const args = argsJson ? JSON.parse(argsJson) : {};
  const out = fn(args);
  return typeof out === 'string' ? out : JSON.stringify(out === undefined ? null : out);
};

globalThis.__luna_invoke_command = function(name, argsJson) {
  const fn = globalThis.__luna_state.commands[name];
  if (!fn) throw new Error('unknown plugin command: ' + name);
  const args = argsJson ? JSON.parse(argsJson) : {};
  const out = fn(args);
  return typeof out === 'string' ? out : JSON.stringify(out === undefined ? null : out);
};

globalThis.__luna_dispatch_event = function(eventName, payloadJson) {
  const list = globalThis.__luna_state.events[eventName] || [];
  const payload = payloadJson ? JSON.parse(payloadJson) : {};
  for (const fn of list) {
    fn(payload);
  }
};

globalThis.__luna_dispose_all = function() {
  for (const fn of globalThis.__luna_state.disposers) {
    try { fn(); } catch (_) {}
  }
  globalThis.__luna_state.disposers = [];
};
"#;

fn run_plugin_actor(
    manifest: PluginManifest,
    plugin_dir: PathBuf,
    js_code: String,
    tokio_handle: Option<tokio::runtime::Handle>,
    live_panels: Arc<Mutex<Vec<PluginUiPanel>>>,
    live_custom_html: Arc<Mutex<Option<String>>>,
    init_tx: std::sync::mpsc::Sender<
        Result<(PluginRegistrations, mpsc::UnboundedSender<ActorCmd>), String>,
    >,
) {
    let runtime = match JsRuntime::new() {
        Ok(rt) => rt,
        Err(e) => {
            let _ = init_tx.send(Err(format!("QuickJS runtime init failed: {e}")));
            return;
        }
    };
    let context = match JsContext::full(&runtime) {
        Ok(ctx) => ctx,
        Err(e) => {
            let _ = init_tx.send(Err(format!("QuickJS context init failed: {e}")));
            return;
        }
    };

    let registrations = Arc::new(Mutex::new(PluginRegistrations::default()));
    let storage = match PluginStorage::open(&plugin_dir.join("data.db")) {
        Ok(s) => Arc::new(s),
        Err(e) => {
            let _ = init_tx.send(Err(format!("plugin storage init failed: {e}")));
            return;
        }
    };

    let startup_res = context.with(|ctx| -> Result<(), String> {
        install_host_bindings(
            &ctx,
            &manifest.id,
            &plugin_dir,
            registrations.clone(),
            live_panels.clone(),
            live_custom_html.clone(),
            storage.clone(),
            tokio_handle.clone(),
        )
        .map_err(|e| format!("failed to install host bindings: {e}"))?;

        ctx.eval::<(), _>(SDK_PRELUDE)
            .map_err(|e| format!("SDK prelude failed: {e}"))?;

        let module = Module::declare(ctx.clone(), manifest.id.as_str(), js_code)
            .map_err(|e| format!("plugin module parse error: {e}"))?;
        let (evaluated, promise) = module
            .eval()
            .map_err(|e| format!("plugin module eval error: {e}"))?;
        let _ = promise;

        if let Ok(apply_fn) = evaluated.get::<_, Function>("apply") {
            let create_ctx: Function = ctx
                .globals()
                .get("__luna_create_ctx")
                .map_err(|e| format!("missing ctx factory: {e}"))?;
            let plugin_ctx: JsValue = create_ctx
                .call((manifest.id.as_str(),))
                .map_err(|e| format!("ctx creation failed: {e}"))?;
            apply_fn
                .call::<_, JsValue>((plugin_ctx,))
                .map_err(|e| format!("plugin apply(ctx) threw: {e}"))?;
        }

        Ok(())
    });

    if startup_res.is_ok() {
        while runtime.is_job_pending() {
            if let Err(err) = runtime.execute_pending_job() {
                let _ = init_tx.send(Err(format!("pending async job failed: {err:?}")));
                return;
            }
        }
    }

    if let Err(err) = startup_res {
        let _ = init_tx.send(Err(err));
        return;
    }

    let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel::<ActorCmd>();
    let regs_snapshot = registrations
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    if init_tx.send(Ok((regs_snapshot, cmd_tx))).is_err() {
        return;
    }

    while let Some(cmd) = cmd_rx.blocking_recv() {
        match cmd {
            ActorCmd::CallTool {
                name,
                args_json,
                reply,
            } => {
                let res = context.with(|ctx| -> Result<String, String> {
                    let invoker: Function = ctx
                        .globals()
                        .get("__luna_invoke_tool")
                        .map_err(|e| e.to_string())?;
                    invoker
                        .call::<_, String>((name, args_json))
                        .map_err(|e| e.to_string())
                });
                let _ = reply.send(res);
            }
            ActorCmd::CallCommand {
                name,
                args_json,
                reply,
            } => {
                let res = context.with(|ctx| -> Result<String, String> {
                    let invoker: Function = ctx
                        .globals()
                        .get("__luna_invoke_command")
                        .map_err(|e| e.to_string())?;
                    invoker
                        .call::<_, String>((name, args_json))
                        .map_err(|e| e.to_string())
                });
                let _ = reply.send(res);
            }
            ActorCmd::EmitEvent {
                event,
                payload_json,
                reply,
            } => {
                let res = context.with(|ctx| -> Result<(), String> {
                    let invoker: Function = ctx
                        .globals()
                        .get("__luna_dispatch_event")
                        .map_err(|e| e.to_string())?;
                    invoker
                        .call::<_, ()>((event, payload_json))
                        .map_err(|e| e.to_string())
                });
                let _ = reply.send(res);
            }
            ActorCmd::Dispose { reply } => {
                context.with(|ctx| {
                    if let Ok(disposer) = ctx.globals().get::<_, Function>("__luna_dispose_all") {
                        let _ = disposer.call::<_, ()>(());
                    }
                });
                let _ = reply.send(());
                break;
            }
        }
    }
}

fn parse_widgets_tolerant(val: &serde_json::Value) -> Vec<PluginWidget> {
    let Some(arr) = val.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|item| serde_json::from_value::<PluginWidget>(item.clone()).ok())
        .collect()
}

fn upsert_panel(panels: &mut Vec<PluginUiPanel>, panel: PluginUiPanel) {
    if let Some(existing) = panels.iter_mut().find(|p| p.id == panel.id) {
        *existing = panel;
    } else {
        panels.push(panel);
    }
}

fn install_host_bindings<'js>(
    ctx: &rquickjs::Ctx<'js>,
    plugin_id: &str,
    plugin_dir: &Path,
    regs: Arc<Mutex<PluginRegistrations>>,
    live_panels: Arc<Mutex<Vec<PluginUiPanel>>>,
    live_custom_html: Arc<Mutex<Option<String>>>,
    storage: Arc<PluginStorage>,
    tokio_handle: Option<tokio::runtime::Handle>,
) -> rquickjs::Result<()> {
    let host = rquickjs::Object::new(ctx.clone())?;

    // registerTool
    {
        let pid = plugin_id.to_string();
        let regs = regs.clone();
        host.set(
            "registerTool",
            Function::new(
                ctx.clone(),
                move |json_str: String| -> rquickjs::Result<()> {
                    let val: serde_json::Value =
                        serde_json::from_str(&json_str).unwrap_or_default();
                    let name = val
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let description = val
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let parameters = val
                        .get("parameters")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
                    let is_mutating = val
                        .get("mutating")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false);
                    if !name.is_empty() {
                        regs.lock().unwrap_or_else(|e| e.into_inner()).tools.push(
                            RegisteredPluginTool {
                                plugin_id: pid.clone(),
                                name,
                                description,
                                parameters,
                                is_mutating,
                            },
                        );
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    // registerAgent
    {
        let pid = plugin_id.to_string();
        let regs = regs.clone();
        host.set(
            "registerAgent",
            Function::new(
                ctx.clone(),
                move |json_str: String| -> rquickjs::Result<()> {
                    let val: serde_json::Value =
                        serde_json::from_str(&json_str).unwrap_or_default();
                    let id = val
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&pid)
                        .to_string();
                    let name = val
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or(&pid)
                        .to_string();
                    let description = val
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let icon = val
                        .get("icon")
                        .and_then(|v| v.as_str())
                        .unwrap_or("puzzle")
                        .to_string();
                    let system_prompt = val
                        .get("systemPrompt")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let model = val
                        .get("model")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let registered_tools = val
                        .get("tools")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    let panel_ids = val
                        .get("panelId")
                        .and_then(|v| v.as_str())
                        .map(|s| vec![s.to_string()])
                        .unwrap_or_default();

                    regs.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .agents
                        .push(PluginAgentEntry {
                            plugin_id: pid.clone(),
                            id,
                            name,
                            description,
                            icon,
                            system_prompt,
                            model,
                            registered_tools,
                            panel_ids,
                        });
                    Ok(())
                },
            )?,
        )?;
    }

    // registerCommand
    {
        let pid = plugin_id.to_string();
        let regs = regs.clone();
        host.set(
            "registerCommand",
            Function::new(
                ctx.clone(),
                move |json_str: String| -> rquickjs::Result<()> {
                    let val: serde_json::Value =
                        serde_json::from_str(&json_str).unwrap_or_default();
                    let name = val
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let description = val
                        .get("description")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    if !name.is_empty() {
                        regs.lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .commands
                            .push(PluginCommandSpec {
                                plugin_id: pid.clone(),
                                name,
                                description,
                            });
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    // registerSchedule
    {
        let pid = plugin_id.to_string();
        let regs = regs.clone();
        host.set(
            "registerSchedule",
            Function::new(
                ctx.clone(),
                move |json_str: String| -> rquickjs::Result<()> {
                    let val: serde_json::Value =
                        serde_json::from_str(&json_str).unwrap_or_default();
                    let id = val
                        .get("id")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("{}-{}", pid, uuid::Uuid::new_v4().simple()));
                    let name = val
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let rule = val
                        .get("rule")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let instruction = val
                        .get("instruction")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string();
                    let delivery_mode = val
                        .get("target")
                        .and_then(|v| v.as_str())
                        .unwrap_or("new_session_each_run")
                        .to_string();
                    regs.lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .schedules
                        .push(PluginScheduleDecl {
                            plugin_id: pid.clone(),
                            id,
                            name,
                            rule,
                            instruction,
                            delivery_mode,
                        });
                    Ok(())
                },
            )?,
        )?;
    }

    // registerPanel
    {
        let pid = plugin_id.to_string();
        let regs = regs.clone();
        let live_panels = live_panels.clone();
        host.set(
            "registerPanel",
            Function::new(
                ctx.clone(),
                move |json_str: String| -> rquickjs::Result<()> {
                    let val: serde_json::Value =
                        serde_json::from_str(&json_str).unwrap_or_default();
                    let id = val
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("panel")
                        .to_string();
                    let title = val
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("Panel")
                        .to_string();
                    let icon = val.get("icon").and_then(|v| v.as_str()).map(str::to_string);
                    let widgets = val
                        .get("widgets")
                        .map(parse_widgets_tolerant)
                        .unwrap_or_default();
                    let panel = PluginUiPanel {
                        plugin_id: pid.clone(),
                        id,
                        title,
                        icon,
                        widgets,
                    };
                    upsert_panel(
                        &mut regs.lock().unwrap_or_else(|e| e.into_inner()).panels,
                        panel.clone(),
                    );
                    upsert_panel(
                        &mut live_panels.lock().unwrap_or_else(|e| e.into_inner()),
                        panel,
                    );
                    Ok(())
                },
            )?,
        )?;
    }

    // updatePanel
    {
        let pid = plugin_id.to_string();
        let live_panels = live_panels.clone();
        host.set(
            "updatePanel",
            Function::new(
                ctx.clone(),
                move |panel_id: String, json_str: String| -> rquickjs::Result<()> {
                    let val: serde_json::Value =
                        serde_json::from_str(&json_str).unwrap_or_default();
                    let target_id = if panel_id.trim().is_empty() {
                        val.get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("panel")
                            .to_string()
                    } else {
                        panel_id
                    };
                    let mut guard = live_panels.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(existing) = guard.iter_mut().find(|p| p.id == target_id) {
                        if let Some(t) = val.get("title").and_then(|v| v.as_str()) {
                            existing.title = t.to_string();
                        }
                        if let Some(ic) = val.get("icon").and_then(|v| v.as_str()) {
                            existing.icon = Some(ic.to_string());
                        }
                        if let Some(w) = val.get("widgets") {
                            existing.widgets = parse_widgets_tolerant(w);
                        }
                    } else {
                        let title = val
                            .get("title")
                            .and_then(|v| v.as_str())
                            .unwrap_or("Panel")
                            .to_string();
                        let icon = val.get("icon").and_then(|v| v.as_str()).map(str::to_string);
                        let widgets = val
                            .get("widgets")
                            .map(parse_widgets_tolerant)
                            .unwrap_or_default();
                        guard.push(PluginUiPanel {
                            plugin_id: pid.clone(),
                            id: target_id,
                            title,
                            icon,
                            widgets,
                        });
                    }
                    Ok(())
                },
            )?,
        )?;
    }

    // removePanel
    {
        let live_panels = live_panels.clone();
        host.set(
            "removePanel",
            Function::new(
                ctx.clone(),
                move |panel_id: String| -> rquickjs::Result<()> {
                    let mut guard = live_panels.lock().unwrap_or_else(|e| e.into_inner());
                    guard.retain(|p| p.id != panel_id);
                    Ok(())
                },
            )?,
        )?;
    }

    // getPanels
    {
        let live_panels = live_panels;
        host.set(
            "getPanels",
            Function::new(ctx.clone(), move || -> String {
                let guard = live_panels.lock().unwrap_or_else(|e| e.into_inner());
                serde_json::to_string(&*guard).unwrap_or_else(|_| "[]".to_string())
            })?,
        )?;
    }

    // setCustomHtml
    {
        let custom_html = live_custom_html;
        host.set(
            "setCustomHtml",
            Function::new(ctx.clone(), move |html: String| -> rquickjs::Result<()> {
                let mut guard = custom_html.lock().unwrap_or_else(|e| e.into_inner());
                if html.trim().is_empty() {
                    *guard = None;
                } else {
                    *guard = Some(html);
                }
                Ok(())
            })?,
        )?;
    }

    // fs bindings (full local access per desktop-program model)
    host.set(
        "fsReadText",
        Function::new(ctx.clone(), |path: String| -> String {
            std::fs::read_to_string(&path).unwrap_or_else(|e| format!("ERR:{e}"))
        })?,
    )?;
    host.set(
        "fsWriteText",
        Function::new(ctx.clone(), |path: String, text: String| -> bool {
            if let Some(parent) = std::path::Path::new(&path).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            std::fs::write(&path, text).is_ok()
        })?,
    )?;
    host.set(
        "fsExists",
        Function::new(ctx.clone(), |path: String| -> bool {
            std::path::Path::new(&path).exists()
        })?,
    )?;
    host.set(
        "fsListDir",
        Function::new(ctx.clone(), |path: String| -> String {
            let entries: Vec<String> = std::fs::read_dir(&path)
                .ok()
                .map(|rd| {
                    rd.flatten()
                        .filter_map(|e| e.file_name().into_string().ok())
                        .collect()
                })
                .unwrap_or_default();
            serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string())
        })?,
    )?;
    host.set(
        "fsRemove",
        Function::new(ctx.clone(), |path: String| -> bool {
            let p = std::path::Path::new(&path);
            if p.is_dir() {
                std::fs::remove_dir_all(p).is_ok()
            } else {
                std::fs::remove_file(p).is_ok()
            }
        })?,
    )?;

    // processExec
    host.set(
        "processExec",
        Function::new(
            ctx.clone(),
            |cmd: String, args_json: String, cwd: String| -> String {
                let args: Vec<String> = serde_json::from_str(&args_json).unwrap_or_default();
                let mut command = std::process::Command::new(&cmd);
                command.args(&args);
                if !cwd.trim().is_empty() {
                    command.current_dir(cwd);
                }
                match command.output() {
                    Ok(out) => serde_json::json!({
                        "status": out.status.code().unwrap_or(-1),
                        "stdout": String::from_utf8_lossy(&out.stdout).to_string(),
                        "stderr": String::from_utf8_lossy(&out.stderr).to_string(),
                    })
                    .to_string(),
                    Err(e) => serde_json::json!({
                        "status": -1,
                        "stdout": "",
                        "stderr": e.to_string(),
                    })
                    .to_string(),
                }
            },
        )?,
    )?;

    // netFetch
    {
        let rt_handle = tokio_handle;
        host.set(
            "netFetch",
            Function::new(
                ctx.clone(),
                move |url: String, opts_json: String| -> String {
                    let opts: serde_json::Value =
                        serde_json::from_str(&opts_json).unwrap_or_default();
                    let method_str = opts
                        .get("method")
                        .and_then(|v| v.as_str())
                        .unwrap_or("GET")
                        .to_uppercase();
                    let body = opts
                        .get("body")
                        .and_then(|v| v.as_str())
                        .map(str::to_string);
                    let headers: BTreeMap<String, String> = opts
                        .get("headers")
                        .and_then(|v| serde_json::from_value(v.clone()).ok())
                        .unwrap_or_default();

                    let fut = async move {
                        let client = reqwest::Client::new();
                        let method = reqwest::Method::from_bytes(method_str.as_bytes())
                            .unwrap_or(reqwest::Method::GET);
                        let mut req = client.request(method, &url);
                        for (k, v) in headers {
                            req = req.header(k, v);
                        }
                        if let Some(b) = body {
                            req = req.body(b);
                        }
                        match req.send().await {
                            Ok(resp) => {
                                let status = resp.status().as_u16();
                                let text = resp.text().await.unwrap_or_default();
                                serde_json::json!({
                                    "ok": (200..300).contains(&status),
                                    "status": status,
                                    "body": text,
                                })
                            }
                            Err(e) => serde_json::json!({
                                "ok": false,
                                "status": 0,
                                "error": e.to_string(),
                                "body": "",
                            }),
                        }
                    };

                    let val = if let Some(h) = &rt_handle {
                        h.block_on(fut)
                    } else {
                        tokio::runtime::Runtime::new()
                            .map(|rt| rt.block_on(fut))
                            .unwrap_or_else(
                                |e| serde_json::json!({ "ok": false, "error": e.to_string() }),
                            )
                    };
                    val.to_string()
                },
            )?,
        )?;
    }

    // storage KV
    {
        let st = storage.clone();
        host.set(
            "storageGet",
            Function::new(ctx.clone(), move |key: String| -> String {
                match st.get(&key) {
                    Ok(Some(v)) => v.to_string(),
                    _ => String::new(),
                }
            })?,
        )?;
    }
    {
        let st = storage.clone();
        host.set(
            "storageSet",
            Function::new(ctx.clone(), move |key: String, val_json: String| {
                let val: serde_json::Value =
                    serde_json::from_str(&val_json).unwrap_or(serde_json::Value::Null);
                let _ = st.set(&key, &val);
            })?,
        )?;
    }
    {
        let st = storage.clone();
        host.set(
            "storageDelete",
            Function::new(ctx.clone(), move |key: String| -> bool {
                st.delete(&key).unwrap_or(false)
            })?,
        )?;
    }
    {
        let st = storage;
        host.set(
            "storageList",
            Function::new(ctx.clone(), move || -> String {
                let keys = st.list().unwrap_or_default();
                serde_json::to_string(&keys).unwrap_or_else(|_| "[]".to_string())
            })?,
        )?;
    }

    // settings
    {
        let dir = plugin_dir.to_path_buf();
        host.set(
            "settingsAll",
            Function::new(ctx.clone(), move || -> String {
                let map = load_settings(&dir);
                serde_json::to_string(&map).unwrap_or_else(|_| "{}".to_string())
            })?,
        )?;
    }
    {
        let dir = plugin_dir.to_path_buf();
        host.set(
            "settingsSet",
            Function::new(ctx.clone(), move |key: String, val_json: String| {
                let mut map = load_settings(&dir);
                let val: serde_json::Value =
                    serde_json::from_str(&val_json).unwrap_or(serde_json::Value::Null);
                map.insert(key, val);
                let _ = save_settings(&dir, &map);
            })?,
        )?;
    }

    // log
    {
        let pid = plugin_id.to_string();
        host.set(
            "log",
            Function::new(ctx.clone(), move |level: String, msg: String| {
                match level.as_str() {
                    "error" => tracing::error!(plugin = %pid, "{}", msg),
                    "warn" => tracing::warn!(plugin = %pid, "{}", msg),
                    _ => tracing::info!(plugin = %pid, "{}", msg),
                }
            })?,
        )?;
    }

    ctx.globals().set("__luna_host", host)?;
    Ok(())
}
