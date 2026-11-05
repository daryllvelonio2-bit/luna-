use serde::{Deserialize, Serialize};

use crate::tree::node::AccessibilityNode;

/// A running application the desktop can see.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppInfo {
    /// The name the agent addresses the app by.
    pub name: String,
    /// The process id, when the platform exposes one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// How many windows the app has open.
    #[serde(default)]
    pub windows: usize,
    /// The platform's own identifier for the app.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bundle_id: Option<String>,
}

/// A window the desktop can see.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WindowInfo {
    /// An opaque id, valid for the life of the window's process.
    pub id: String,
    /// The app the window belongs to.
    pub app: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<crate::tree::rect::Rect>,
}

/// A tree as the platform read it, before the core minted any refs.
#[derive(Debug, Clone)]
pub struct ObservedTree {
    pub app: String,
    pub root: AccessibilityNode,
}

impl ObservedTree {
    pub fn new(app: impl Into<String>, root: AccessibilityNode) -> Self {
        Self {
            app: app.into(),
            root,
        }
    }

    /// How many nodes the tree holds.
    pub fn node_count(&self) -> usize {
        self.root.flatten().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tree::identity::NodeIdentity;
    use crate::tree::presentation::NodePresentation;

    fn node(role: &str) -> AccessibilityNode {
        AccessibilityNode {
            ref_id: None,
            role: role.to_string(),
            identity: NodeIdentity::default(),
            presentation: NodePresentation::default(),
            handle: None,
            children_count: None,
            subtree_truncated: false,
            children: Vec::new(),
        }
    }

    #[test]
    fn a_tree_counts_every_node() {
        let mut root = node("window");
        root.children.push(node("push button"));
        let tree = ObservedTree::new("Calculator", root);
        assert_eq!(tree.node_count(), 2);
        assert_eq!(tree.app, "Calculator");
    }
}
