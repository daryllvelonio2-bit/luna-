//! Invoking an action on an element.

use harness_desktop::error::DesktopError;
use zbus::blocking::Connection;

use crate::element::AtspiElement;
use crate::tree::node::{call, read_actions};

const ACTION_INTERFACE: &str = "org.a11y.atspi.Action";

/// The index of the action with this name, if the element has one.
pub fn action_index(names: &[String], wanted: &str) -> Option<usize> {
    names
        .iter()
        .position(|name| name.eq_ignore_ascii_case(wanted))
}

/// The first action whose name is one of these, in order of preference.
pub fn action_index_any(names: &[String], wanted: &[&str]) -> Option<usize> {
    wanted
        .iter()
        .filter_map(|verb| action_index(names, verb))
        .next()
}

/// Run the action with this name on the element.
pub fn do_action(
    connection: &Connection,
    element: &AtspiElement,
    names: &[String],
    wanted: &[&str],
) -> Result<bool, DesktopError> {
    let Some(index) = action_index_any(names, wanted) else {
        return Err(DesktopError::new(
            harness_desktop::error::ErrorCode::ActionNotSupported,
            format!(
                "The element offers no action named {}",
                wanted
                    .iter()
                    .map(|verb| format!("'{verb}'"))
                    .collect::<Vec<_>>()
                    .join(" or ")
            ),
        )
        .with_suggestion("Read the element's available actions and pick one of them")
        .with_details(serde_json::json!({ "available_actions": names }))
        .not_delivered());
    };
    let done: bool = call(
        connection,
        element,
        ACTION_INTERFACE,
        "DoAction",
        &(index as i32,),
    )?;
    Ok(done)
}

/// The actions of an element, read from the bus.
pub fn element_actions(
    connection: &Connection,
    element: &AtspiElement,
) -> Result<Vec<String>, DesktopError> {
    read_actions(connection, element)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn an_action_is_found_without_regard_to_case() {
        let actions = names(&["Click", "Toggle"]);
        assert_eq!(action_index(&actions, "click"), Some(0));
        assert_eq!(action_index(&actions, "TOGGLE"), Some(1));
        assert_eq!(action_index(&actions, "press"), None);
    }

    #[test]
    fn the_first_preferred_verb_wins() {
        let actions = names(&["Toggle", "Click"]);
        assert_eq!(action_index_any(&actions, &["click", "press"]), Some(1));
        assert_eq!(action_index_any(&actions, &["activate"]), None);
    }

    #[test]
    fn an_element_with_no_actions_has_no_index() {
        assert_eq!(action_index_any(&[], &["click"]), None);
    }
}
