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
    Focus,
    Key,
    Paste,
    ClickAt,
    Screenshot,
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
            Self::Focus,
            Self::Key,
            Self::Paste,
            Self::ClickAt,
            Self::Screenshot,
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
            Self::Focus => "desktop_focus",
            Self::Key => "desktop_key",
            Self::Paste => "desktop_paste",
            Self::ClickAt => "desktop_click_at",
            Self::Screenshot => "desktop_screenshot",
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
                "Replace the contents of the text field a ref names. Mode 'value' writes through the accessibility interface, 'paste' clicks the field and pastes through the clipboard (use it for rich web editors), and 'auto' tries value then falls back to paste."
            }
            Self::SetValue => {
                "Set the value of the control a ref names. A number goes to a slider or a spinner; anything else goes to a text field."
            }
            Self::Toggle => {
                "Flip the check box or radio button a ref names, or set it to a given state."
            }
            Self::Focus => {
                "Focus an application's window so that key chords and pastes land in it. Omit app to report the window that holds focus now."
            }
            Self::Key => {
                "Send key chords to the focused window: 'ctrl+v', 'enter', 'tab', 'ctrl+a ctrl+v'. Keys are sent at the OS level and do not use the accessibility tree."
            }
            Self::Paste => {
                "Put text on the clipboard and paste it into the focused window. This is the reliable path into a rich web editor that ignores desktop_type."
            }
            Self::ClickAt => {
                "Click with a real pointer at the on-screen center of a ref, or at explicit x and y coordinates. Use it when a semantic desktop_click does nothing."
            }
            Self::Screenshot => {
                "Capture the screen, or one application's window, to a PNG file. The answer carries the absolute path and the pixel size."
            }
        }
    }

    /// Whether the command changes the screen or takes over input.
    pub fn is_mutating(&self) -> bool {
        matches!(
            self,
            Self::Click
                | Self::Type
                | Self::SetValue
                | Self::Toggle
                | Self::Focus
                | Self::Key
                | Self::Paste
                | Self::ClickAt
        )
    }

    /// What a human needs to see before a mutating call runs.
    pub fn preview_target(&self, arguments: &Value) -> Option<String> {
        if !self.is_mutating() {
            return None;
        }
        match self {
            Self::Focus => Some(match arguments.get("app").and_then(Value::as_str) {
                Some(app) if !app.is_empty() => format!("focus {app}"),
                _ => "report the focused window".to_string(),
            }),
            Self::Key => Some(format!(
                "send {}",
                arguments.get("keys").and_then(Value::as_str).unwrap_or("?")
            )),
            Self::Paste => Some(format!(
                "paste {}",
                excerpt(arguments.get("text").and_then(Value::as_str).unwrap_or("?"))
            )),
            Self::ClickAt => Some(match arguments.get("ref").and_then(Value::as_str) {
                Some(reference) => format!("pointer click {reference}"),
                None => format!(
                    "pointer click {},{}",
                    arguments.get("x").and_then(Value::as_i64).unwrap_or(0),
                    arguments.get("y").and_then(Value::as_i64).unwrap_or(0)
                ),
            }),
            _ => {
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
    }
}

/// A short, single-line preview of a long payload.
fn excerpt(text: &str) -> String {
    let mut excerpt: String = text.chars().take(60).collect();
    if text.chars().count() > 60 {
        excerpt.push('…');
    }
    excerpt.replace('\n', " ")
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
            arguments
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("auto"),
        ),
        DesktopCommand::SetValue => commands::set_value(
            engine,
            &reference(arguments),
            &value_or_text(arguments, "value", "text"),
        ),
        DesktopCommand::Toggle => commands::toggle(
            engine,
            &reference(arguments),
            optional_flag(arguments, "checked"),
        ),
        DesktopCommand::Focus => {
            commands::focus(engine, optional_str(arguments, "app").as_deref())
        }
        DesktopCommand::Key => commands::key(engine, &required_str(arguments, "keys")),
        DesktopCommand::Paste => commands::paste(engine, &required_str(arguments, "text")),
        DesktopCommand::ClickAt => commands::click_at(
            engine,
            optional_str(arguments, "ref").as_deref(),
            optional_i32(arguments, "x"),
            optional_i32(arguments, "y"),
            arguments.get("button").and_then(Value::as_u64).unwrap_or(0) as u8,
        ),
        DesktopCommand::Screenshot => commands::screenshot(
            engine,
            optional_str(arguments, "app").as_deref(),
            &screenshot_path(arguments),
        ),
    }
}

/// The path a screenshot is written to, defaulting to the temp directory.
fn screenshot_path(arguments: &Value) -> String {
    if let Some(path) = optional_str(arguments, "path") {
        return path;
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    std::env::temp_dir()
        .join(format!("luna-screenshot-{stamp}.png"))
        .display()
        .to_string()
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

fn optional_i32(arguments: &Value, key: &str) -> Option<i32> {
    arguments
        .get(key)
        .and_then(Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
}

/// A boolean argument, tolerant of the string form a model sometimes sends.
fn flag(arguments: &Value, key: &str, default: bool) -> bool {
    optional_flag(arguments, key).unwrap_or(default)
}

fn optional_flag(arguments: &Value, key: &str) -> Option<bool> {
    match arguments.get(key)? {
        Value::Bool(value) => Some(*value),
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Some(true),
            "false" | "0" | "no" => Some(false),
            _ => None,
        },
        Value::Number(number) => number.as_i64().map(|value| value != 0),
        _ => None,
    }
}

/// The tree options a snapshot asks for.
fn snapshot_options(arguments: &Value) -> harness_desktop::tree::options::TreeOptions {
    let mut options = harness_desktop::tree::options::TreeOptions {
        interactive_only: flag(arguments, "interactive", true),
        skeleton: flag(arguments, "skeleton", false),
        compact: flag(arguments, "compact", true),
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
        exact: flag(arguments, "exact", false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_boolean_argument_is_read_from_a_bool_or_a_string() {
        assert_eq!(flag(&json!({ "exact": true }), "exact", false), true);
        assert_eq!(flag(&json!({ "exact": "true" }), "exact", false), true);
        assert_eq!(flag(&json!({ "exact": "false" }), "exact", true), false);
        assert_eq!(flag(&json!({ "exact": 0 }), "exact", true), false);
        assert_eq!(flag(&json!({}), "exact", true), true);
    }

    #[test]
    fn a_missing_checked_argument_stays_unset() {
        assert_eq!(optional_flag(&json!({}), "checked"), None);
        assert_eq!(optional_flag(&json!({ "checked": "yes" }), "checked"), Some(true));
    }

    #[test]
    fn a_snapshot_flag_survives_the_string_form_a_model_sends() {
        // The transcript of the Flow session sent `"compact": "true"`.
        let options = snapshot_options(&json!({
            "app": "Firefox",
            "compact": "true",
            "interactive": "false",
            "skeleton": "false",
        }));
        assert!(options.compact);
        assert!(!options.interactive_only);
        assert!(!options.skeleton);
    }
}
