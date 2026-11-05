//! The desktop commands, and the arguments each one takes.

use harness_desktop::commands::FindQuery;
use harness_desktop::envelope::Response;
use harness_desktop::{commands, DesktopEngine};
use serde_json::Value;

/// One desktop command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopCommand {
    Status,
    ListApps,
    ListWindows,
    Snapshot,
    Find,
    Get,
    Is,
    Click,
    Type,
    SetValue,
    Toggle,
}

impl DesktopCommand {
    /// Every command, in the order they are registered.
    pub fn all() -> Vec<DesktopCommand> {
        vec![
            Self::Status,
            Self::ListApps,
            Self::ListWindows,
            Self::Snapshot,
            Self::Find,
            Self::Get,
            Self::Is,
            Self::Click,
            Self::Type,
            Self::SetValue,
            Self::Toggle,
        ]
    }

    /// The tool name the model calls.
    pub fn tool_name(&self) -> &'static str {
        match self {
            Self::Status => "desktop_status",
            Self::ListApps => "desktop_list_apps",
            Self::ListWindows => "desktop_list_windows",
            Self::Snapshot => "desktop_snapshot",
            Self::Find => "desktop_find",
            Self::Get => "desktop_get",
            Self::Is => "desktop_is",
            Self::Click => "desktop_click",
            Self::Type => "desktop_type",
            Self::SetValue => "desktop_set_value",
            Self::Toggle => "desktop_toggle",
        }
    }

    /// What the model is told the command does.
    pub fn description(&self) -> &'static str {
        match self {
            Self::Status => {
                "Report what this desktop can do: the platform, whether the accessibility bus answered, and the surfaces that can be observed. Run this first when a desktop command refuses."
            }
            Self::ListApps => {
                "List the running desktop applications, with the names to pass to --app."
            }
            Self::ListWindows => {
                "List the open desktop windows, optionally of one application."
            }
            Self::Snapshot => {
                "Read the accessibility tree of a desktop application and return it with a ref for every element that can be acted on. Read this before acting: a ref belongs to one snapshot and goes stale when the screen changes."
            }
            Self::Find => {
                "Look for elements in a desktop application by role or name, and return them with refs. Use this when you know what you are looking for but not where it is."
            }
            Self::Get => {
                "Read one property of the element a ref names: 'value', 'state' or 'actions'. Use it to check what a control holds before or after acting on it."
            }
            Self::Is => {
                "Ask whether the element a ref names has a state: 'enabled', 'focused', 'selected' or 'checked'."
            }
            Self::Click => {
                "Activate the element a ref names. The action is semantic: it does not move the pointer or take focus."
            }
            Self::Type => {
                "Replace the contents of the text field a ref names. This overwrites whatever the field holds."
            }
            Self::SetValue => {
                "Set the value of the control a ref names. A number goes to a slider or a spinner; anything else goes to a text field."
            }
            Self::Toggle => {
                "Flip the check box or radio button a ref names, or set it to a given state."
            }
        }
    }

    /// Whether the command changes the screen.
    pub fn is_mutating(&self) -> bool {
        matches!(
            self,
            Self::Click | Self::Type | Self::SetValue | Self::Toggle
        )
    }

    /// What a human needs to see before a mutating call runs.
    pub fn preview_target(&self, arguments: &Value) -> Option<String> {
        if !self.is_mutating() {
            return None;
        }
        let reference = arguments.get("ref").and_then(Value::as_str)?;
        let detail = match self {
            Self::Type => arguments
                .get("text")
                .or_else(|| arguments.get("value"))
                .and_then(Value::as_str),
            Self::SetValue => arguments
                .get("value")
                .or_else(|| arguments.get("text"))
                .and_then(Value::as_str),
            Self::Toggle => Some("toggle"),
            _ => None,
        };
        Some(match detail {
            Some(detail) => format!("{reference} <- {detail}"),
            None => reference.to_string(),
        })
    }
}

/// Run a command and answer with the envelope.
pub fn execute(engine: &DesktopEngine, command: &DesktopCommand, arguments: &Value) -> Response {
    match command {
        DesktopCommand::Status => commands::status(engine),
        DesktopCommand::ListApps => commands::list_apps(engine),
        DesktopCommand::ListWindows => {
            commands::list_windows(engine, optional_str(arguments, "app").as_deref())
        }
        DesktopCommand::Snapshot => commands::snapshot(engine, &app(arguments), &snapshot_options(arguments)),
        DesktopCommand::Find => commands::find(engine, &app(arguments), &find_query(arguments)),
        DesktopCommand::Get => {
            commands::get(engine, &reference(arguments), &required_str(arguments, "property"))
        }
        DesktopCommand::Is => {
            commands::is(engine, &reference(arguments), &required_str(arguments, "predicate"))
        }
        DesktopCommand::Click => commands::click(engine, &reference(arguments)),
        DesktopCommand::Type => commands::type_text(
            engine,
            &reference(arguments),
            &value_or_text(arguments, "text", "value"),
        ),
        DesktopCommand::SetValue => commands::set_value(
            engine,
            &reference(arguments),
            &value_or_text(arguments, "value", "text"),
        ),
        DesktopCommand::Toggle => commands::toggle(
            engine,
            &reference(arguments),
            arguments.get("checked").and_then(Value::as_bool),
        ),
    }
}

fn value_or_text(arguments: &Value, primary: &str, secondary: &str) -> String {
    arguments
        .get(primary)
        .or_else(|| arguments.get(secondary))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The application the command works on.
fn app(arguments: &Value) -> String {
    required_str(arguments, "app")
}

/// The element the command works on.
fn reference(arguments: &Value) -> String {
    required_str(arguments, "ref")
}

fn required_str(arguments: &Value, key: &str) -> String {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn optional_str(arguments: &Value, key: &str) -> Option<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

/// The tree options a snapshot asks for.
fn snapshot_options(arguments: &Value) -> harness_desktop::tree::options::TreeOptions {
    let mut options = harness_desktop::tree::options::TreeOptions {
        interactive_only: arguments
            .get("interactive")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        skeleton: arguments
            .get("skeleton")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        compact: arguments
            .get("compact")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        ..Default::default()
    };
    if let Some(depth) = arguments.get("max_depth").and_then(Value::as_u64) {
        options.max_depth = Some(depth as u32);
    }
    options.root = optional_str(arguments, "root");
    options
}

/// What a find looks for.
fn find_query(arguments: &Value) -> FindQuery {
    FindQuery {
        role: optional_str(arguments, "role"),
        name: optional_str(arguments, "name"),
        exact: arguments
            .get("exact")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}
