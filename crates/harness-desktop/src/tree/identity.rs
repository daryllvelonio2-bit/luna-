use serde::{Deserialize, Serialize};

/// What a node says about itself.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NodeIdentity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The toolkit's own id for the element, when it exposes one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_id: Option<String>,
}

/// The key a ref resolves against.
///
/// The fields are the ones a platform can read again and compare: the role, the
/// toolkit id, the name and the description. Position, size and value are
/// deliberately absent — a layout change moves an element without replacing it,
/// so a key that counted position would expire every ref the moment a window
/// resized.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ElementIdentity {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handle: Option<String>,
}

impl ElementIdentity {
    /// Whether two readings of the tree describe the same element.
    pub fn matches(&self, other: &ElementIdentity) -> bool {
        self.role == other.role
            && self.native_id == other.native_id
            && self.name == other.name
            && self.description == other.description
    }

    /// A stable, readable form used in error messages and logs.
    pub fn describe(&self) -> String {
        let name = self.name.as_deref().unwrap_or("");
        if name.is_empty() {
            self.role.clone()
        } else {
            format!("{} '{}'", self.role, name)
        }
    }
}

impl NodeIdentity {
    /// The resolution key for this node under the given role.
    pub fn key(&self, role: &str) -> ElementIdentity {
        ElementIdentity {
            role: role.to_string(),
            native_id: self.native_id.clone(),
            name: self.name.clone(),
            description: self.description.clone(),
            handle: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> ElementIdentity {
        ElementIdentity {
            role: "push button".to_string(),
            native_id: None,
            name: Some("Save".to_string()),
            description: Some("Save the document".to_string()),
            handle: None,
        }
    }

    #[test]
    fn the_same_reading_matches_itself() {
        assert!(identity().matches(&identity()));
    }

    #[test]
    fn a_different_name_is_a_different_element() {
        let mut other = identity();
        other.name = Some("Open".to_string());
        assert!(!identity().matches(&other));
    }

    #[test]
    fn a_different_description_is_a_different_element() {
        let mut other = identity();
        other.description = Some("Open the document".to_string());
        assert!(!identity().matches(&other));
    }

    #[test]
    fn a_different_role_is_a_different_element() {
        let mut other = identity();
        other.role = "menu item".to_string();
        assert!(!identity().matches(&other));
    }

    #[test]
    fn an_element_is_described_by_its_name_when_it_has_one() {
        assert_eq!(identity().describe(), "push button 'Save'");
        let nameless = ElementIdentity {
            role: "separator".to_string(),
            ..Default::default()
        };
        assert_eq!(nameless.describe(), "separator");
    }
}
