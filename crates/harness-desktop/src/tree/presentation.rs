use serde::{Deserialize, Serialize};

use super::rect::Rect;

/// What a node looks like, as opposed to what it says.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodePresentation {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<String>,
    /// The actions the platform says this node supports: "click", "activate",
    /// "toggle", and so on. The list is what the adapter can actually do, not
    /// what the role suggests.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub available_actions: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Rect>,
}

impl NodePresentation {
    /// Whether the node advertises an action with this name.
    pub fn offers(&self, action: &str) -> bool {
        self.available_actions
            .iter()
            .any(|name| name.eq_ignore_ascii_case(action))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_offered_action_is_found_without_regard_to_case() {
        let presentation = NodePresentation {
            available_actions: vec!["Click".to_string()],
            ..Default::default()
        };
        assert!(presentation.offers("click"));
        assert!(!presentation.offers("toggle"));
    }
}
