use serde::{Deserialize, Serialize};

use super::identity::ElementIdentity;

/// How much of the tree to read, and how to print it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TreeOptions {
    /// Keep only the nodes the agent can act on.
    #[serde(default)]
    pub interactive_only: bool,
    /// Stop descending at this depth.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// Map the tree shallowly: every node is kept, but branches are cut.
    #[serde(default)]
    pub skeleton: bool,
    /// Print the tree compactly.
    #[serde(default)]
    pub compact: bool,
    /// Drill into this ref instead of reading the whole window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<String>,
    /// The element the drill-down starts at.
    ///
    /// The core fills this in from `root`: a ref names an element by identity,
    /// and only the core knows what the ref stood for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_identity: Option<ElementIdentity>,
}

impl TreeOptions {
    pub fn interactive() -> Self {
        Self {
            interactive_only: true,
            ..Default::default()
        }
    }

    pub fn with_root(mut self, root: impl Into<String>) -> Self {
        self.root = Some(root.into());
        self
    }

    pub fn with_max_depth(mut self, depth: u32) -> Self {
        self.max_depth = Some(depth);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_interactive_profile_keeps_only_actionable_nodes() {
        let options = TreeOptions::interactive();
        assert!(options.interactive_only);
        assert!(!options.skeleton);
    }

    #[test]
    fn a_root_and_a_depth_can_be_set() {
        let options = TreeOptions::interactive()
            .with_root("@s1:e3")
            .with_max_depth(4);
        assert_eq!(options.root.as_deref(), Some("@s1:e3"));
        assert_eq!(options.max_depth, Some(4));
    }
}
