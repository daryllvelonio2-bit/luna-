use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use harness_mobile::{
    ActionCall, ActionContext, ActionError, ActionHandler, ActionId, AgentEvent, EngineEvent,
    LunaEngine, MobileConfig, ScreenRuntime, StatePatch,
};
use harness_ui_spec::{Node, NodeKind, Screen, TextRole};
use serde_json::Value;
use slint::{ComponentHandle, ModelRc, TimerMode, VecModel};

use crate::{AppWindow, RowItem};

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(60);
const WORKER_THREADS: usize = 2;
const DEFAULT_PROVIDER: &str = "openai";
const DEFAULT_MODEL: &str = "gpt-4o-mini";
const DEMO_ACTIONS: [&str; 3] = ["publish", "save_draft", "schedule"];

enum AppEvent {
    Line(String),
    Status(String),
    Screen(Screen),
    Runtime(ScreenRuntime),
}

type Queue = Arc<Mutex<VecDeque<AppEvent>>>;

struct ViewState {
    lines: Vec<String>,
    screen: Option<ScreenRuntime>,
}

pub fn run(data_dir: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let ui = AppWindow::new()?;
    let data_dir = data_dir.unwrap_or_else(default_data_dir);
    std::fs::create_dir_all(&data_dir)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKER_THREADS)
        .enable_all()
        .build()?;
    let mut engine = runtime.block_on(LunaEngine::new(mobile_config(data_dir)))?;
    register_demo_actions(&mut engine)?;
    let engine = Arc::new(engine);

    let queue: Queue = Arc::new(Mutex::new(VecDeque::new()));
    forward_engine_events(&runtime, &engine, queue.clone());

    let view = Rc::new(RefCell::new(ViewState {
        lines: vec!["Luna mobile ready. Ask for a screen.".to_string()],
        screen: None,
    }));
    sync_transcript(&ui, &view);

    let timer = slint::Timer::default();
    {
        let ui_weak = ui.as_weak();
        let view = view.clone();
        let queue = queue.clone();
        timer.start(TimerMode::Repeated, EVENT_POLL_INTERVAL, move || {
            let Some(ui) = ui_weak.upgrade() else {
                return;
            };
            let events = match queue.lock() {
                Ok(mut queue) => queue.drain(..).collect::<Vec<_>>(),
                Err(_) => return,
            };
            for event in events {
                apply_event(&ui, &view, event);
            }
        });
    }

    wire_send_prompt(&ui, &runtime, &engine, queue.clone());
    wire_actions(&ui, &runtime, &engine, &view, queue.clone());
    wire_edits(&ui, &view);

    ui.run()?;
    Ok(())
}

fn forward_engine_events(
    runtime: &tokio::runtime::Runtime,
    engine: &Arc<LunaEngine>,
    queue: Queue,
) {
    let mut events = engine.subscribe();
    runtime.spawn(async move {
        while let Ok(event) = events.recv().await {
            let translated = match event {
                EngineEvent::ScreenSaved(screen) => AppEvent::Screen(screen),
                EngineEvent::Agent(AgentEvent::ToolExecuting { name, .. }) => {
                    AppEvent::Status(format!("running {name}"))
                }
                EngineEvent::Agent(AgentEvent::ToolExecuted { name, is_error, .. }) => {
                    AppEvent::Status(format!(
                        "{name} {}",
                        if is_error { "failed" } else { "done" }
                    ))
                }
                EngineEvent::Agent(_) => continue,
            };
            push_event(&queue, translated);
        }
    });
}

fn wire_send_prompt(
    ui: &AppWindow,
    runtime: &tokio::runtime::Runtime,
    engine: &Arc<LunaEngine>,
    queue: Queue,
) {
    let ui_weak = ui.as_weak();
    let engine = engine.clone();
    let handle = runtime.handle().clone();
    ui.on_send_prompt(move |text| {
        let prompt = text.trim().to_string();
        if prompt.is_empty() {
            return;
        }
        if let Some(ui) = ui_weak.upgrade() {
            ui.set_status("thinking...".into());
        }
        push_event(&queue, AppEvent::Line(format!("you: {prompt}")));

        let engine = engine.clone();
        let queue = queue.clone();
        handle.spawn(async move {
            match engine.send_prompt(prompt).await {
                Ok(outcome) => {
                    push_event(&queue, AppEvent::Line(format!("luna: {}", outcome.reply)));
                    push_event(&queue, AppEvent::Status("idle".into()));
                }
                Err(error) => {
                    push_event(&queue, AppEvent::Line(format!("error: {error}")));
                    push_event(&queue, AppEvent::Status("idle".into()));
                }
            }
        });
    });
}

fn wire_actions(
    ui: &AppWindow,
    runtime: &tokio::runtime::Runtime,
    engine: &Arc<LunaEngine>,
    view: &Rc<RefCell<ViewState>>,
    queue: Queue,
) {
    let view = view.clone();
    let engine = engine.clone();
    let handle = runtime.handle().clone();
    ui.on_press(move |action| {
        let action = action.to_string();
        let screen = view.borrow_mut().screen.take();
        let Some(mut screen) = screen else {
            return;
        };
        let engine = engine.clone();
        let queue = queue.clone();
        handle.spawn(async move {
            match screen
                .dispatch(engine.actions(), &action, serde_json::json!({}))
                .await
            {
                Ok(_) => push_event(&queue, AppEvent::Line(format!("action {action}: ok"))),
                Err(error) => {
                    push_event(&queue, AppEvent::Line(format!("action {action}: {error}")))
                }
            }
            push_event(&queue, AppEvent::Runtime(screen));
        });
    });
}

fn wire_edits(ui: &AppWindow, view: &Rc<RefCell<ViewState>>) {
    let view = view.clone();
    ui.on_edit(move |bind, text| {
        let Ok(mut view) = view.try_borrow_mut() else {
            return;
        };
        let Some(screen) = view.screen.as_mut() else {
            return;
        };
        let value = match screen.state_value(&bind) {
            Some(Value::Bool(_)) => Value::Bool(text.as_str() == "true"),
            _ => Value::String(text.to_string()),
        };
        let _ = screen.set_input(&bind, value);
    });
}

fn apply_event(ui: &AppWindow, view: &Rc<RefCell<ViewState>>, event: AppEvent) {
    match event {
        AppEvent::Line(line) => {
            view.borrow_mut().lines.push(line);
            sync_transcript(ui, view);
        }
        AppEvent::Status(status) => ui.set_status(status.into()),
        AppEvent::Screen(screen) => match ScreenRuntime::new(screen) {
            Ok(runtime) => show_screen(ui, view, runtime),
            Err(error) => {
                view.borrow_mut()
                    .lines
                    .push(format!("screen rejected: {error}"));
                sync_transcript(ui, view);
            }
        },
        AppEvent::Runtime(runtime) => show_screen(ui, view, runtime),
    }
}

fn show_screen(ui: &AppWindow, view: &Rc<RefCell<ViewState>>, runtime: ScreenRuntime) {
    let rows = render_rows(&runtime);
    let title = runtime.screen().title.clone();
    ui.set_rows(ModelRc::new(VecModel::from(rows)));
    ui.set_status(format!("screen: {title}").into());
    view.borrow_mut().screen = Some(runtime);
}

fn push_event(queue: &Queue, event: AppEvent) {
    if let Ok(mut queue) = queue.lock() {
        queue.push_back(event);
    }
}

fn sync_transcript(ui: &AppWindow, view: &Rc<RefCell<ViewState>>) {
    let lines = view
        .borrow()
        .lines
        .iter()
        .map(|line| line.as_str().into())
        .collect::<Vec<_>>();
    ui.set_transcript(ModelRc::new(VecModel::from(lines)));
}

fn render_rows(runtime: &ScreenRuntime) -> Vec<RowItem> {
    let mut rows = Vec::new();
    walk(runtime, &runtime.screen().root, &mut rows);
    rows
}

fn walk(runtime: &ScreenRuntime, node: &Node, rows: &mut Vec<RowItem>) {
    match &node.kind {
        NodeKind::Column { children } | NodeKind::Row { children } => {
            for child in children {
                walk(runtime, child, rows);
            }
        }
        NodeKind::Repeat { children, .. } => {
            for child in children {
                walk(runtime, child, rows);
            }
        }
        NodeKind::Conditional {
            bind,
            equals,
            children,
            otherwise,
        } => {
            let taken = runtime
                .state_value(bind)
                .is_some_and(|value| value == equals);
            for child in if taken { children } else { otherwise } {
                walk(runtime, child, rows);
            }
        }
        NodeKind::Text { text, role } => {
            let kind = match role {
                TextRole::Heading => 1,
                TextRole::Caption => 2,
                TextRole::Body => 0,
            };
            rows.push(row(node, kind, text.clone(), String::new(), String::new()));
        }
        NodeKind::Input { bind, label, .. } => {
            rows.push(row(
                node,
                3,
                label.clone(),
                value_text(runtime, bind),
                String::new(),
            ));
        }
        NodeKind::Select {
            bind,
            label,
            options,
            ..
        } => {
            let values = options
                .iter()
                .map(|option| option.value.clone())
                .collect::<Vec<_>>();
            rows.push(row_with_options(
                node,
                4,
                label.clone(),
                value_text(runtime, bind),
                String::new(),
                values,
            ));
        }
        NodeKind::Toggle { bind, label, .. } => {
            rows.push(row(
                node,
                5,
                label.clone(),
                value_text(runtime, bind),
                String::new(),
            ));
        }
        NodeKind::Button { label, action } => {
            rows.push(row(node, 6, label.clone(), String::new(), action.clone()));
        }
        NodeKind::Table { bind, columns, .. } => {
            let header = columns
                .iter()
                .map(|column| column.label.as_str())
                .collect::<Vec<_>>()
                .join(" | ");
            rows.push(row(
                node,
                7,
                header,
                value_text(runtime, bind),
                String::new(),
            ));
        }
        NodeKind::Image { src, alt } => {
            let label = alt.as_deref().unwrap_or(src);
            rows.push(row(
                node,
                8,
                label.to_string(),
                String::new(),
                String::new(),
            ));
        }
        NodeKind::Spacer { .. } => {
            rows.push(row(node, 9, String::new(), String::new(), String::new()));
        }
    }
}

fn row(node: &Node, kind: i32, label: String, value: String, action: String) -> RowItem {
    row_with_options(node, kind, label, value, action, Vec::new())
}

fn row_with_options(
    node: &Node,
    kind: i32,
    label: String,
    value: String,
    action: String,
    options: Vec<String>,
) -> RowItem {
    let options = ModelRc::new(VecModel::from(
        options
            .into_iter()
            .map(slint::SharedString::from)
            .collect::<Vec<_>>(),
    ));
    RowItem {
        kind,
        id: node.id.as_str().into(),
        label: label.into(),
        value: value.into(),
        action: action.into(),
        options,
    }
}

fn value_text(runtime: &ScreenRuntime, bind: &str) -> String {
    match runtime.state_value(bind) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Bool(value)) => {
            if *value {
                "on".to_string()
            } else {
                "off".to_string()
            }
        }
        Some(value) => value.to_string(),
    }
}

fn mobile_config(data_dir: PathBuf) -> MobileConfig {
    let provider = std::env::var("LUNA_PROVIDER").unwrap_or_else(|_| DEFAULT_PROVIDER.to_string());
    let model = std::env::var("LUNA_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let config = MobileConfig::new(data_dir, provider, model);
    match std::env::var("LUNA_API_KEY") {
        Ok(key) => config.with_api_key(key),
        Err(_) => config,
    }
}

fn default_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LUNA_DATA_DIR") {
        return PathBuf::from(dir);
    }
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".luna-mobile")
}

fn register_demo_actions(engine: &mut LunaEngine) -> Result<(), Box<dyn std::error::Error>> {
    for name in DEMO_ACTIONS {
        engine.register_action(ActionId::new(name)?, Arc::new(DemoAction))?;
    }
    Ok(())
}

struct DemoAction;

#[async_trait]
impl ActionHandler for DemoAction {
    async fn invoke(
        &self,
        call: &ActionCall,
        _context: &ActionContext,
    ) -> Result<StatePatch, ActionError> {
        let mut patch = StatePatch::new();
        patch.set("last_action", serde_json::json!(call.id.as_str()));
        Ok(patch)
    }
}
