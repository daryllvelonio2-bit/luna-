use serde::{Deserialize, Serialize};

/// The states a node reports, as the platform's own names.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ElementState {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub states: Vec<String>,
}

impl ElementState {
    /// Whether the node carries a state with this name, case-insensitively.
    pub fn has(&self, state: &str) -> bool {
        self.states
            .iter()
            .any(|name| name.eq_ignore_ascii_case(state))
    }

    pub fn is_enabled(&self) -> bool {
        self.has("enabled")
    }

    pub fn is_focused(&self) -> bool {
        self.has("focused")
    }

    pub fn is_selected(&self) -> bool {
        self.has("selected")
    }

    pub fn is_checked(&self) -> bool {
        self.has("checked")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_are_matched_without_regard_to_case() {
        let state = ElementState {
            states: vec!["Enabled".to_string(), "Showing".to_string()],
        };
        assert!(state.has("enabled"));
        assert!(state.has("SHOWING"));
        assert!(!state.has("focused"));
    }

    #[test]
    fn the_common_predicates_read_the_state_list() {
        let state = ElementState {
            states: vec!["enabled".to_string(), "focused".to_string(), "checked".to_string()],
        };
        assert!(state.is_enabled());
        assert!(state.is_focused());
        assert!(state.is_checked());
        assert!(!state.is_selected());
    }
}
