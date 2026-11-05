//! The AT-SPI adapter: the whole platform surface of the desktop tools.

use harness_desktop::adapter::action::{ActionOps, ActionOutcome, ActionRequest};
use harness_desktop::adapter::observation::ObservationOps;
use harness_desktop::adapter::system::{DesktopStatus, SystemOps};
use harness_desktop::adapter::types::{AppInfo, ObservedTree, WindowInfo};
use harness_desktop::deadline::Deadline;
use harness_desktop::error::{DeliveryDisposition, DesktopError, ErrorCode};
use harness_desktop::tree::identity::ElementIdentity;
use harness_desktop::tree::node::AccessibilityNode;
use harness_desktop::tree::options::TreeOptions;
use harness_desktop::tree::state::ElementState;
use zbus::blocking::Connection;

use crate::actions::editable_text::{read_text, read_value, set_text_contents, set_value};
use crate::actions::do_action::{action_index_any, do_action, element_actions};
use crate::bus;
use crate::element::AtspiElement;
use crate::system::apps::{find_app, list_apps};
use crate::system::windows::list_windows;
use crate::tree::node::read_actions;
use crate::tree::walk::walk;

/// The verbs a click may be called, in order of preference.
///
/// A toolkit calls the same thing "click", "press" or "activate" depending on
/// the widget, so the adapter asks for all three rather than insisting on one.
const CLICK_VERBS: &[&str] = &[
    "click", "press", "activate", "jump", "open", "openlink", "open link", "select",
];
const TOGGLE_VERBS: &[&str] = &["toggle", "click", "press", "check"];

/// The Linux adapter.
pub struct LinuxDesktopAdapter;

impl LinuxDesktopAdapter {
    pub fn new() -> Self {
        Self
    }

    /// The connection, or the error that explains why there is none.
    fn bus(&self) -> Result<Connection, DesktopError> {
        bus::connection()
    }

    /// The whole tree of an application, read fresh.
    fn app_tree(
        &self,
        connection: &Connection,
        app: &str,
    ) -> Result<(AtspiElement, AccessibilityNode), DesktopError> {
        let element = find_app(connection, app)?;
        let tree = walk(connection, &element, &TreeOptions::default())?;
        Ok((element, tree))
    }

    /// The element behind a node, from the handle the walk recorded.
    fn element_of(node: &AccessibilityNode) -> Result<AtspiElement, DesktopError> {
        let handle = node
            .handle
            .as_ref()
            .ok_or_else(|| DesktopError::new(ErrorCode::Internal, "The node carries no address"))?;
        AtspiElement::decode(&handle.token).ok_or_else(|| {
            DesktopError::new(
                ErrorCode::Internal,
                "The node carries an address this platform cannot read",
            )
        })
    }
}

impl Default for LinuxDesktopAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ObservationOps for LinuxDesktopAdapter {
    fn list_apps(&self, _deadline: Deadline) -> Result<Vec<AppInfo>, DesktopError> {
        let connection = self.bus()?;
        list_apps(&connection)
    }

    fn list_windows(
        &self,
        app: Option<&str>,
        _deadline: Deadline,
    ) -> Result<Vec<WindowInfo>, DesktopError> {
        let connection = self.bus()?;
        list_windows(&connection, app)
    }

    fn observe(
        &self,
        app: &str,
        options: &TreeOptions,
        _deadline: Deadline,
    ) -> Result<ObservedTree, DesktopError> {
        let connection = self.bus()?;
        let element = find_app(&connection, app)?;

        let root = match &options.root_identity {
            Some(identity) => {
                // A drill-down starts at the element the ref named, found by
                // reading the whole tree and matching its identity.
                let tree = walk(&connection, &element, &TreeOptions::default())?;
                let node = self.resolve_identity(&tree, identity)?;
                Self::element_of(node)?
            }
            None => element,
        };

        let tree = walk(&connection, &root, options)?;
        Ok(ObservedTree::new(app, tree))
    }

    fn read_value(
        &self,
        app: &str,
        identity: &ElementIdentity,
        _deadline: Deadline,
    ) -> Result<Option<String>, DesktopError> {
        let connection = self.bus()?;
        let (_, tree) = self.app_tree(&connection, app)?;
        let node = self.resolve_identity(&tree, identity)?;
        let element = Self::element_of(node)?;
        read_value(&connection, &element)
    }

    fn read_state(
        &self,
        app: &str,
        identity: &ElementIdentity,
        _deadline: Deadline,
    ) -> Result<Option<ElementState>, DesktopError> {
        let connection = self.bus()?;
        let (_, tree) = self.app_tree(&connection, app)?;
        let node = self.resolve_identity(&tree, identity)?;
        let element = Self::element_of(node)?;
        let states = crate::tree::node::read_states(&connection, &element)?;
        Ok(Some(ElementState { states }))
    }

    fn read_actions(
        &self,
        app: &str,
        identity: &ElementIdentity,
        _deadline: Deadline,
    ) -> Result<Option<Vec<String>>, DesktopError> {
        let connection = self.bus()?;
        let (_, tree) = self.app_tree(&connection, app)?;
        let node = self.resolve_identity(&tree, identity)?;
        let element = Self::element_of(node)?;
        Ok(Some(read_actions(&connection, &element)?))
    }
}

impl ActionOps for LinuxDesktopAdapter {
    fn execute(
        &self,
        app: &str,
        identity: &ElementIdentity,
        request: &ActionRequest,
        _deadline: Deadline,
    ) -> Result<ActionOutcome, DesktopError> {
        let connection = self.bus()?;
        let (_, tree) = self.app_tree(&connection, app)?;
        let node = self.resolve_identity(&tree, identity)?;
        let element = Self::element_of(node)?;

        match request {
            ActionRequest::Click => {
                let names = element_actions(&connection, &element)?;
                let done = do_action(&connection, &element, &names, CLICK_VERBS)?;
                if !done {
                    return Err(DesktopError::new(
                        ErrorCode::ActionFailed,
                        "The application refused the action",
                    )
                    .with_suggestion("Read the element's state before trying again")
                    .not_delivered());
                }
                Ok(ActionOutcome::unverified(vec!["Action.DoAction".to_string()]))
            }
            ActionRequest::Type { text } => {
                let done = set_text_contents(&connection, &element, text)?;
                if !done {
                    return Err(DesktopError::new(
                        ErrorCode::ActionFailed,
                        "The field did not accept the text",
                    )
                    .with_suggestion("The field may be read-only; check its states")
                    .not_delivered());
                }
                let written = read_text(&connection, &element)?;
                if written.as_deref() == Some(text.as_str()) {
                    Ok(ActionOutcome::verified(vec![
                        "EditableText.SetTextContents".to_string(),
                    ]))
                } else {
                    Ok(ActionOutcome::unverified(vec![
                        "EditableText.SetTextContents".to_string(),
                    ]))
                }
            }
            ActionRequest::SetValue { value } => {
                let parsed = value.trim().parse::<f64>().ok();
                let steps = match parsed {
                    Some(number) => {
                        let done = set_value(&connection, &element, number)?;
                        if !done {
                            return Err(DesktopError::new(
                                ErrorCode::ActionFailed,
                                "The control did not accept the value",
                            )
                            .with_suggestion("Use desktop_type for a text field")
                            .not_delivered());
                        }
                        vec!["Value.SetCurrentValue".to_string()]
                    }
                    None => {
                        let done = set_text_contents(&connection, &element, value)?;
                        if !done {
                            return Err(DesktopError::new(
                                ErrorCode::ActionFailed,
                                "The control did not accept the value",
                            )
                            .not_delivered());
                        }
                        vec!["EditableText.SetTextContents".to_string()]
                    }
                };
                Ok(ActionOutcome::unverified(steps))
            }
            ActionRequest::Toggle { checked } => {
                let names = element_actions(&connection, &element)?;
                let Some(index) = action_index_any(&names, TOGGLE_VERBS) else {
                    return Err(DesktopError::new(
                        ErrorCode::ActionNotSupported,
                        "The element has no action that toggles it",
                    )
                    .with_details(serde_json::json!({ "available_actions": names }))
                    .not_delivered());
                };
                let _ = index;
                let before = crate::tree::node::read_states(&connection, &element)?;
                let was_checked = before.iter().any(|state| state == "checked");
                let done = do_action(&connection, &element, &names, TOGGLE_VERBS)?;
                if !done {
                    return Err(DesktopError::new(
                        ErrorCode::ActionFailed,
                        "The application refused the toggle",
                    )
                    .not_delivered());
                }
                let after = crate::tree::node::read_states(&connection, &element)?;
                let is_checked = after.iter().any(|state| state == "checked");
                let matches = match checked {
                    Some(wanted) => is_checked == *wanted,
                    None => is_checked != was_checked,
                };
                let outcome = if matches {
                    ActionOutcome::verified(vec!["Action.DoAction".to_string()])
                } else {
                    ActionOutcome::unverified(vec!["Action.DoAction".to_string()])
                };
                Ok(outcome)
            }
        }
    }
}

impl SystemOps for LinuxDesktopAdapter {
    fn platform(&self) -> &'static str {
        "linux"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn status(&self, _deadline: Deadline) -> Result<DesktopStatus, DesktopError> {
        let mut notes = Vec::new();
        let bus_available = match self.bus() {
            Ok(connection) => {
                // The registry answers even when no application is registered,
                // so it is the cheapest proof that the bus is alive.
                match crate::tree::node::read_children(&connection, &crate::system::apps::desktop_root())
                {
                    Ok(_) => true,
                    Err(error) => {
                        notes.push(error.message.clone());
                        false
                    }
                }
            }
            Err(error) => {
                notes.push(error.message.clone());
                if let Some(suggestion) = &error.suggestion {
                    notes.push(suggestion.clone());
                }
                false
            }
        };
        Ok(DesktopStatus {
            platform: "linux".to_string(),
            bus_available,
            supported_surfaces: vec![
                "window".to_string(),
                "dialog".to_string(),
                "menu".to_string(),
                "text".to_string(),
            ],
            notes,
        })
    }
}

/// The delivery a refused action carries.
#[allow(dead_code)]
fn not_delivered() -> harness_desktop::error::DeliverySemantics {
    harness_desktop::error::DeliverySemantics {
        delivery: DeliveryDisposition::NotDelivered,
        retry: harness_desktop::error::RetryDisposition::Safe,
    }
}
