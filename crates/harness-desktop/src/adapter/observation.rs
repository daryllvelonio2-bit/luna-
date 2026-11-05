use crate::deadline::Deadline;
use crate::error::DesktopError;
use crate::tree::identity::ElementIdentity;
use crate::tree::node::AccessibilityNode;
use crate::tree::options::TreeOptions;
use crate::tree::state::ElementState;

use super::types::{AppInfo, ObservedTree, WindowInfo};

/// What a platform can read.
pub trait ObservationOps: Send + Sync {
    /// The applications the desktop can see.
    fn list_apps(&self, _deadline: Deadline) -> Result<Vec<AppInfo>, DesktopError> {
        Err(DesktopError::not_supported("list_apps"))
    }

    /// The windows the desktop can see.
    fn list_windows(
        &self,
        _app: Option<&str>,
        _deadline: Deadline,
    ) -> Result<Vec<WindowInfo>, DesktopError> {
        Err(DesktopError::not_supported("list_windows"))
    }

    /// Read the tree of one application.
    ///
    /// The tree comes back without refs: a ref belongs to a snapshot, and the
    /// snapshot is the core's to name.
    fn observe(
        &self,
        _app: &str,
        _options: &TreeOptions,
        _deadline: Deadline,
    ) -> Result<ObservedTree, DesktopError> {
        Err(DesktopError::not_supported("observe"))
    }

    /// The element's current value, read again from the live tree.
    fn read_value(
        &self,
        _app: &str,
        _identity: &ElementIdentity,
        _deadline: Deadline,
    ) -> Result<Option<String>, DesktopError> {
        Err(DesktopError::not_supported("read_value"))
    }

    /// The element's current states, read again from the live tree.
    fn read_state(
        &self,
        _app: &str,
        _identity: &ElementIdentity,
        _deadline: Deadline,
    ) -> Result<Option<ElementState>, DesktopError> {
        Err(DesktopError::not_supported("read_state"))
    }

    /// The actions the element offers, read again from the live tree.
    fn read_actions(
        &self,
        _app: &str,
        _identity: &ElementIdentity,
        _deadline: Deadline,
    ) -> Result<Option<Vec<String>>, DesktopError> {
        Err(DesktopError::not_supported("read_actions"))
    }

    /// Find the one node in the tree that matches an identity.
    ///
    /// A platform that has no stable element address resolves a ref this way:
    /// it reads the tree again and counts the matches. Zero matches is a stale
    /// ref; more than one is an ambiguous target. The helper is here so every
    /// platform reaches the same verdict the same way.
    fn resolve_identity<'a>(
        &self,
        tree: &'a AccessibilityNode,
        identity: &ElementIdentity,
    ) -> Result<&'a AccessibilityNode, DesktopError> {
        if let Some(target_handle) = &identity.handle {
            if let Some(node) = tree.flatten().into_iter().find(|n| {
                n.handle.as_ref().map(|h| &h.token) == Some(target_handle)
                    && n.role == identity.role
            }) {
                return Ok(node);
            }
        }

        let mut matches = tree.flatten().into_iter().filter(|node| {
            node.identity.key(&node.role).matches(identity)
        });
        let Some(first) = matches.next() else {
            return Err(DesktopError::new(
                crate::error::ErrorCode::StaleRef,
                format!("No live element matches {}", identity.describe()),
            )
            .with_suggestion("Take a fresh snapshot and use the ref it prints")
            .with_details(serde_json::json!({ "kind": "saved_ref_absent" }))
            .not_delivered());
        };
        if matches.next().is_some() {
            return Err(DesktopError::new(
                crate::error::ErrorCode::AmbiguousTarget,
                format!(
                    "More than one live element matches {}, so the action was not sent",
                    identity.describe()
                ),
            )
            .with_suggestion("Act on a control that is unique in the window")
            .with_details(serde_json::json!({ "kind": "identity_not_unique" }))
            .not_delivered());
        }
        Ok(first)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::identity::NodeIdentity;
    use crate::tree::presentation::NodePresentation;

    struct Noop;

    impl ObservationOps for Noop {}

    fn node(role: &str, name: &str) -> AccessibilityNode {
        AccessibilityNode {
            ref_id: None,
            role: role.to_string(),
            identity: NodeIdentity {
                name: Some(name.to_string()),
                ..Default::default()
            },
            presentation: NodePresentation::default(),
            handle: None,
            children_count: None,
            subtree_truncated: false,
            children: Vec::new(),
        }
    }

    #[test]
    fn an_unimplemented_capability_refuses_rather_than_answering_empty() {
        let error = Noop.list_apps(Deadline::none()).expect_err("no apps");
        assert_eq!(
            error.code,
            crate::error::ErrorCode::PlatformNotSupported
        );
    }

    #[test]
    fn one_match_resolves() {
        let mut root = node("window", "Calculator");
        root.children.push(node("push button", "7"));
        let identity = ElementIdentity {
            role: "push button".to_string(),
            name: Some("7".to_string()),
            ..Default::default()
        };
        let found = Noop.resolve_identity(&root, &identity).expect("one match");
        assert_eq!(found.role, "push button");
    }

    #[test]
    fn no_match_is_a_stale_ref() {
        let root = node("window", "Calculator");
        let identity = ElementIdentity {
            role: "push button".to_string(),
            name: Some("7".to_string()),
            ..Default::default()
        };
        let error = Noop.resolve_identity(&root, &identity).expect_err("no match");
        assert_eq!(error.code, crate::error::ErrorCode::StaleRef);
    }

    #[test]
    fn two_matches_are_an_ambiguous_target() {
        let mut root = node("window", "Calculator");
        root.children.push(node("push button", "7"));
        root.children.push(node("push button", "7"));
        let identity = ElementIdentity {
            role: "push button".to_string(),
            name: Some("7".to_string()),
            ..Default::default()
        };
        let error = Noop.resolve_identity(&root, &identity).expect_err("two matches");
        assert_eq!(error.code, crate::error::ErrorCode::AmbiguousTarget);
    }

    #[test]
    fn handle_disambiguates_identical_elements() {
        let mut root = node("window", "Calculator");
        let mut btn1 = node("push button", "7");
        btn1.handle = Some(crate::tree::node::ElementHandle::new("app|btn1"));
        let mut btn2 = node("push button", "7");
        btn2.handle = Some(crate::tree::node::ElementHandle::new("app|btn2"));
        root.children.push(btn1);
        root.children.push(btn2);
        let identity = ElementIdentity {
            role: "push button".to_string(),
            name: Some("7".to_string()),
            handle: Some("app|btn2".to_string()),
            ..Default::default()
        };
        let found = Noop.resolve_identity(&root, &identity).expect("resolves via handle");
        assert_eq!(found.handle.as_ref().unwrap().token, "app|btn2");
    }
}
