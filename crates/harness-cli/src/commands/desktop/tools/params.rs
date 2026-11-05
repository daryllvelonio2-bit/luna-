//! The JSON schema of each desktop command.
//!
//! The schema is what the model sees when it chooses a tool, so it says what
//! the argument means and which of them are required. A required argument that
//! the command cannot do without is stated; everything else is optional.

use serde_json::{Value, json};

use super::run::DesktopCommand;

/// The schema for one command.
pub fn schema(command: &DesktopCommand) -> Value {
    match command {
        DesktopCommand::Status | DesktopCommand::ListApps => object(&[], &[]),
        DesktopCommand::ListWindows => object(
            &[(
                "app",
                "The application to list the windows of. Omit for every window.",
            )],
            &[],
        ),
        DesktopCommand::Snapshot => object(
            &[
                (
                    "app",
                    "The application to read, named as desktop_list_apps prints it.",
                ),
                (
                    "interactive",
                    "Keep only the elements that can be acted on.",
                ),
                (
                    "skeleton",
                    "Map the tree shallowly: keep the shape, cut the branches.",
                ),
                ("compact", "Print the tree compactly."),
                ("max_depth", "Stop descending at this depth."),
                (
                    "root",
                    "A ref to drill into instead of reading the whole window.",
                ),
            ],
            &["app"],
        ),
        DesktopCommand::Find => object(
            &[
                (
                    "app",
                    "The application to look in, named as desktop_list_apps prints it.",
                ),
                ("role", "The role to look for, for example 'push button'."),
                ("name", "The name to look for."),
                ("exact", "Match the name exactly instead of as a substring."),
            ],
            &["app"],
        ),
        DesktopCommand::Get => object(
            &[
                ("ref", "The element to read, for example '@s8f3k2p9:e12'."),
                ("property", "What to read: 'value', 'state' or 'actions'."),
            ],
            &["ref", "property"],
        ),
        DesktopCommand::Is => object(
            &[
                ("ref", "The element to ask about, for example '@s8f3k2p9:e12'."),
                (
                    "predicate",
                    "The state to ask about: 'enabled', 'focused', 'selected' or 'checked'.",
                ),
            ],
            &["ref", "predicate"],
        ),
        DesktopCommand::Click => object(
            &[("ref", "The element to activate, for example '@s8f3k2p9:e12'.")],
            &["ref"],
        ),
        DesktopCommand::Type => object(
            &[
                ("ref", "The text field to write to, for example '@s8f3k2p9:e12'."),
                ("text", "The text to put in the field. This replaces what it holds."),
            ],
            &["ref", "text"],
        ),
        DesktopCommand::SetValue => object(
            &[
                ("ref", "The control to set, for example '@s8f3k2p9:e12'."),
                (
                    "value",
                    "The value to set. A number goes to a slider or a spinner; anything else goes to a text field.",
                ),
            ],
            &["ref", "value"],
        ),
        DesktopCommand::Toggle => object(
            &[
                (
                    "ref",
                    "The check box or radio button to flip, for example '@s8f3k2p9:e12'.",
                ),
                ("checked", "Set the control to this state instead of flipping it."),
            ],
            &["ref"],
        ),
    }
}

fn object(
    properties: &[(&str, &str)],
    required: &[&str],
) -> Value {
    let properties: serde_json::Map<String, Value> = properties
        .iter()
        .map(|(name, description)| {
            (
                (*name).to_string(),
                json!({ "type": "string", "description": description }),
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
}
