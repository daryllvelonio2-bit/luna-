use serde::{Deserialize, Serialize};

use super::identity::NodeIdentity;
use super::presentation::NodePresentation;

/// A platform-specific address for an element.
///
/// The core never reads it; it carries the element's bus address so a platform
/// can find the same object again when it acts. It is not serialized: a
/// snapshot that leaves the harness must not carry a platform's private
/// addressing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ElementHandle {
    /// The platform's own address, opaque to the core.
    pub token: String,
}

impl ElementHandle {
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.token.is_empty()
    }
}

/// One node of an accessibility tree.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AccessibilityNode {
    /// The qualified ref, `@<snapshot_id>:e<N>`. Minted by the core, not by the
    /// platform: a ref names a position in one snapshot, and only the core
    /// knows which snapshot that is.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
    pub role: String,
    #[serde(flatten)]
    pub identity: NodeIdentity,
    #[serde(flatten)]
    pub presentation: NodePresentation,
    /// The platform's private address for this element.
    #[serde(skip)]
    pub handle: Option<ElementHandle>,
    /// How many children the node has, when the read could count them.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub children_count: Option<u32>,
    /// Set when the node's descendants were cut short.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub subtree_truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<AccessibilityNode>,
}

impl AccessibilityNode {
    /// Whether this node can be acted on, and so deserves a ref.
    ///
    /// A node qualifies when the platform advertises an action for it, or when
    /// it is a text field the agent can write into. Everything else — static
    /// labels, layout containers — is read-only context.
    pub fn is_addressable(&self) -> bool {
        let has_real_action = self
            .presentation
            .available_actions
            .iter()
            .any(|a| !a.eq_ignore_ascii_case("clickancestor"));
        if has_real_action {
            return true;
        }
        is_editable_role(&self.role)
    }

    /// The resolution identity for this node, including its platform handle if present.
    pub fn element_identity(&self) -> super::identity::ElementIdentity {
        let mut id = self.identity.key(&self.role);
        id.handle = self.handle.as_ref().map(|h| h.token.clone());
        id
    }

    /// This node and every node below it, in tree order.
    pub fn flatten(&self) -> Vec<&AccessibilityNode> {
        let mut out = vec![self];
        for child in &self.children {
            out.extend(child.flatten());
        }
        out
    }

    /// The first node with this ref, anywhere in the tree.
    pub fn find_by_ref(&self, ref_id: &str) -> Option<&AccessibilityNode> {
        self.flatten()
            .into_iter()
            .find(|node| node.ref_id.as_deref() == Some(ref_id))
    }
}

/// Whether a role names a field the agent can type into.
pub fn is_editable_role(role: &str) -> bool {
    let role = role.to_ascii_lowercase();
    matches!(role.as_str(), "text" | "entry" | "password text" | "passwordtext")
        || (role.contains("text") && (role.contains("entry") || role.contains("field")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(role: &str, actions: &[&str]) -> AccessibilityNode {
        AccessibilityNode {
            ref_id: None,
            role: role.to_string(),
            identity: NodeIdentity::default(),
            presentation: NodePresentation {
                available_actions: actions.iter().map(|a| a.to_string()).collect(),
                ..Default::default()
            },
            handle: None,
            children_count: None,
            subtree_truncated: false,
            children: Vec::new(),
        }
    }

    #[test]
    fn a_node_with_an_action_is_addressable() {
        assert!(node("push button", &["click"]).is_addressable());
    }

    #[test]
    fn a_node_with_only_click_ancestor_is_not_addressable() {
        assert!(!node("section", &["clickAncestor"]).is_addressable());
        assert!(node("section", &["clickAncestor", "jump"]).is_addressable());
    }

    #[test]
    fn a_static_label_is_not_addressable() {
        assert!(!node("label", &[]).is_addressable());
    }

    #[test]
    fn a_text_field_is_addressable_even_without_an_advertised_action() {
        assert!(node("text", &[]).is_addressable());
        assert!(node("entry", &[]).is_addressable());
        assert!(!node("text label", &[]).is_addressable());
    }

    #[test]
    fn flattening_walks_the_whole_subtree() {
        let mut root = node("window", &[]);
        let mut child = node("push button", &["click"]);
        child.ref_id = Some("@s1:e2".to_string());
        root.children.push(child);
        assert_eq!(root.flatten().len(), 2);
        assert!(root.find_by_ref("@s1:e2").is_some());
        assert!(root.find_by_ref("@s1:e9").is_none());
    }
}
