use serde::{Deserialize, Serialize};

use crate::tree::identity::ElementIdentity;

/// What a ref remembers about the element it names.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RefEntry {
    /// The qualified ref, `@<snapshot_id>:e<N>`.
    pub ref_id: String,
    /// The snapshot that minted this ref.
    pub snapshot_id: String,
    /// The element number within that snapshot.
    pub local_ref: String,
    /// The application the element lives in.
    pub app: String,
    /// The key the element is found by when the tree is read again.
    pub identity: ElementIdentity,
}

impl RefEntry {
    pub fn new(
        snapshot_id: &str,
        local_ref: &str,
        app: &str,
        identity: ElementIdentity,
    ) -> Self {
        Self {
            ref_id: crate::refs::token::qualify_ref_id(snapshot_id, local_ref),
            snapshot_id: snapshot_id.to_string(),
            local_ref: local_ref.to_string(),
            app: app.to_string(),
            identity,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_entry_remembers_the_app_and_the_identity() {
        let entry = RefEntry::new(
            "s1",
            "@e2",
            "Calculator",
            ElementIdentity {
                role: "push button".to_string(),
                name: Some("7".to_string()),
                ..Default::default()
            },
        );
        assert_eq!(entry.ref_id, "@s1:e2");
        assert_eq!(entry.app, "Calculator");
        assert_eq!(entry.identity.role, "push button");
    }
}
