//! The JSON schema of each desktop command.
//!
//! The schema is what the model sees when it chooses a tool, so it says what
//! the argument means, which type it takes and which of them are required. A
//! required argument that the command cannot do without is stated; everything
//! else is optional. The type matters: a flag declared as a string arrives as
//! `"true"`, which no boolean reader accepts.

use serde_json::{Value, json};

use super::run::DesktopCommand;

/// A string property.
const TEXT: &str = "string";
/// A true/false property.
const FLAG: &str = "boolean";
/// A whole-number property.
const COUNT: &str = "integer";

/// The schema for one command.
pub fn schema(command: &DesktopCommand) -> Value {
    match command {
        DesktopCommand::Status | DesktopCommand::ListApps => object(&[], &[]),
        DesktopCommand::ListWindows => object(
            &[(
                "app",
                TEXT,
                "The application to list the windows of. Omit for every window.",
            )],
            &[],
        ),
        DesktopCommand::Snapshot => object(
            &[
                (
                    "app",
                    TEXT,
                    "The application to read, named as desktop_list_apps prints it.",
                ),
                (
                    "interactive",
                    FLAG,
                    "Keep only the elements that can be acted on.",
                ),
                (
                    "skeleton",
                    FLAG,
                    "Map the tree shallowly: keep the shape, cut the branches.",
                ),
                ("compact", FLAG, "Print the tree compactly."),
                ("max_depth", COUNT, "Stop descending at this depth."),
                (
                    "root",
                    TEXT,
                    "A ref to drill into instead of reading the whole window.",
                ),
            ],
            &["app"],
        ),
        DesktopCommand::Find => object(
            &[
                (
                    "app",
                    TEXT,
                    "The application to look in, named as desktop_list_apps prints it.",
                ),
                (
                    "role",
                    TEXT,
                    "The role to look for, for example 'push button'.",
                ),
                ("name", TEXT, "The name to look for."),
                (
                    "exact",
                    FLAG,
                    "Match the name exactly instead of as a substring.",
                ),
            ],
            &["app"],
        ),
        DesktopCommand::Get => object(
            &[
                (
                    "ref",
                    TEXT,
                    "The element to read, for example '@s8f3k2p9:e12'.",
                ),
                ("property", TEXT, "What to read: 'value', 'state' or 'actions'."),
            ],
            &["ref", "property"],
        ),
        DesktopCommand::Is => object(
            &[
                (
                    "ref",
                    TEXT,
                    "The element to ask about, for example '@s8f3k2p9:e12'.",
                ),
                (
                    "predicate",
                    TEXT,
                    "The state to ask about: 'enabled', 'focused', 'selected' or 'checked'.",
                ),
            ],
            &["ref", "predicate"],
        ),
        DesktopCommand::Click => object(
            &[(
                "ref",
                TEXT,
                "The element to activate, for example '@s8f3k2p9:e12'.",
            )],
            &["ref"],
        ),
        DesktopCommand::Type => json!({
            "type": "object",
            "properties": {
                "ref": {
                    "type": "string",
                    "description": "The text field to write to, for example '@s8f3k2p9:e12'.",
                },
                "text": {
                    "type": "string",
                    "description": "The text to put in the field. This replaces what it holds.",
                },
                "mode": {
                    "type": "string",
                    "enum": ["auto", "value", "paste"],
                    "description": "How to write: 'auto' tries the accessibility interface then falls back to a clipboard paste, 'value' only uses the accessibility interface, 'paste' clicks the field and pastes.",
                },
            },
            "required": ["ref", "text"],
        }),
        DesktopCommand::SetValue => object(
            &[
                (
                    "ref",
                    TEXT,
                    "The control to set, for example '@s8f3k2p9:e12'.",
                ),
                (
                    "value",
                    TEXT,
                    "The value to set. A number goes to a slider or a spinner; anything else goes to a text field.",
                ),
            ],
            &["ref", "value"],
        ),
        DesktopCommand::Toggle => object(
            &[
                (
                    "ref",
                    TEXT,
                    "The check box or radio button to flip, for example '@s8f3k2p9:e12'.",
                ),
                (
                    "checked",
                    FLAG,
                    "Set the control to this state instead of flipping it.",
                ),
            ],
            &["ref"],
        ),
        DesktopCommand::Focus => object(
            &[(
                "app",
                TEXT,
                "The application whose window to focus, named as desktop_list_apps prints it. Omit to report the window that is focused now.",
            )],
            &[],
        ),
        DesktopCommand::Key => object(
            &[(
                "keys",
                TEXT,
                "The key chords to send, for example 'ctrl+v' or 'ctrl+a ctrl+v'. A chord joins keys with '+' and chords are separated by spaces.",
            )],
            &["keys"],
        ),
        DesktopCommand::Paste => object(
            &[(
                "text",
                TEXT,
                "The text to put on the clipboard and paste into the focused window.",
            )],
            &["text"],
        ),
        DesktopCommand::ClickAt => json!({
            "type": "object",
            "properties": {
                "ref": {
                    "type": "string",
                    "description": "A ref whose on-screen center to click, for example '@s8f3k2p9:e12'.",
                },
                "x": {
                    "type": "integer",
                    "description": "The screen x coordinate, when no ref is given.",
                },
                "y": {
                    "type": "integer",
                    "description": "The screen y coordinate, when no ref is given.",
                },
                "button": {
                    "type": "integer",
                    "enum": [0, 1, 2],
                    "description": "0 for left, 1 for right, 2 for middle. Defaults to left.",
                },
            },
            "required": [],
        }),
        DesktopCommand::Screenshot => object(
            &[
                (
                    "app",
                    TEXT,
                    "The application whose window to capture. Omit to capture the whole screen.",
                ),
                (
                    "path",
                    TEXT,
                    "Where to write the PNG. Defaults to a file in the system temp directory.",
                ),
            ],
            &[],
        ),
    }
}

fn object(properties: &[(&str, &str, &str)], required: &[&str]) -> Value {
    let properties: serde_json::Map<String, Value> = properties
        .iter()
        .map(|(name, kind, description)| {
            (
                (*name).to_string(),
                json!({ "type": kind, "description": description }),
            )
        })
        .collect();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_a_schema() {
        for command in DesktopCommand::all() {
            let schema = schema(&command);
            assert_eq!(schema["type"], "object", "{command:?}");
            assert!(schema.get("properties").is_some(), "{command:?}");
        }
    }

    #[test]
    fn the_commands_that_need_an_element_say_so() {
        for command in [
            DesktopCommand::Get,
            DesktopCommand::Is,
            DesktopCommand::Click,
            DesktopCommand::Type,
            DesktopCommand::SetValue,
            DesktopCommand::Toggle,
        ] {
            let schema = schema(&command);
            let required = schema["required"].as_array().expect("required");
            assert!(
                required.iter().any(|name| name == "ref"),
                "{command:?} must require a ref"
            );
        }
    }

    #[test]
    fn the_commands_that_need_an_application_say_so() {
        for command in [DesktopCommand::Snapshot, DesktopCommand::Find] {
            let schema = schema(&command);
            let required = schema["required"].as_array().expect("required");
            assert!(
                required.iter().any(|name| name == "app"),
                "{command:?} must require an app"
            );
        }
    }

    #[test]
    fn a_flag_is_declared_as_a_boolean() {
        // A flag declared as a string arrives as `"true"`, which no boolean
        // reader accepts, so the setting is silently ignored.
        for (command, name) in [
            (DesktopCommand::Snapshot, "interactive"),
            (DesktopCommand::Snapshot, "skeleton"),
            (DesktopCommand::Snapshot, "compact"),
            (DesktopCommand::Find, "exact"),
            (DesktopCommand::Toggle, "checked"),
        ] {
            let schema = schema(&command);
            assert_eq!(
                schema["properties"][name]["type"], "boolean",
                "{command:?}.{name} must be a boolean"
            );
        }
    }

    #[test]
    fn a_number_is_declared_as_an_integer() {
        let snapshot = schema(&DesktopCommand::Snapshot);
        assert_eq!(snapshot["properties"]["max_depth"]["type"], "integer");
        let click_at = schema(&DesktopCommand::ClickAt);
        assert_eq!(click_at["properties"]["x"]["type"], "integer");
        assert_eq!(click_at["properties"]["button"]["type"], "integer");
    }
}
