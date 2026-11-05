use serde_json::json;

use crate::engine::DesktopEngine;
use crate::envelope::Response;
use crate::tree::node::AccessibilityNode;
use crate::tree::options::TreeOptions;

/// What to look for in a tree.
#[derive(Debug, Clone, Default)]
pub struct FindQuery {
    /// The role to look for, matched without regard to case.
    pub role: Option<String>,
    /// The name to look for, matched as a substring unless `exact` is set.
    pub name: Option<String>,
    /// Match the name exactly instead of as a substring.
    pub exact: bool,
}

impl FindQuery {
    fn matches(&self, node: &AccessibilityNode) -> bool {
        if let Some(role) = &self.role {
            if !node.role.eq_ignore_ascii_case(role) {
                return false;
            }
        }
        if let Some(name) = &self.name {
            let node_name = node.identity.name.as_deref().unwrap_or("");
            let hit = if self.exact {
                node_name.eq_ignore_ascii_case(name)
            } else {
                node_name.to_lowercase().contains(&name.to_lowercase())
            };
            if !hit {
                return false;
            }
        }
        true
    }
}

/// Look for elements by role or name and return them with refs.
pub fn find(engine: &DesktopEngine, app: &str, query: &FindQuery) -> Response {
    let snapshot = match engine.observe(app, &TreeOptions::default()) {
        Ok(data) => data,
        Err(error) => return Response::err("find", &error),
    };
    let tree = snapshot
        .get("tree")
        .and_then(|tree| serde_json::from_value::<AccessibilityNode>(tree.clone()).ok())
        .unwrap_or_default();
    let matches: Vec<serde_json::Value> = tree
        .flatten()
        .into_iter()
        .filter(|node| query.matches(node))
        .map(|node| serde_json::to_value(node).unwrap_or(serde_json::Value::Null))
        .collect();
    Response::ok(
        "find",
        json!({
            "app": app,
            "snapshot_id": snapshot.get("snapshot_id").cloned().unwrap_or(serde_json::Value::Null),
            "matches": matches,
            "count": matches.len(),
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(role: &str, name: &str) -> AccessibilityNode {
        AccessibilityNode {
            ref_id: None,
            role: role.to_string(),
            identity: crate::tree::identity::NodeIdentity {
                name: Some(name.to_string()),
                ..Default::default()
            },
            presentation: crate::tree::presentation::NodePresentation::default(),
            handle: None,
            children_count: None,
            subtree_truncated: false,
            children: Vec::new(),
        }
    }

    #[test]
    fn a_role_matches_without_regard_to_case() {
        let query = FindQuery {
            role: Some("PUSH BUTTON".to_string()),
            ..Default::default()
        };
        assert!(query.matches(&node("push button", "7")));
        assert!(!query.matches(&node("label", "7")));
    }

    #[test]
    fn a_name_matches_as_a_substring_by_default() {
        let query = FindQuery {
            name: Some("al".to_string()),
            ..Default::default()
        };
        assert!(query.matches(&node("label", "Calculator")));
    }

    #[test]
    fn an_exact_name_matches_only_the_whole_name() {
        let query = FindQuery {
            name: Some("al".to_string()),
            exact: true,
            ..Default::default()
        };
        assert!(!query.matches(&node("label", "Calculator")));
        assert!(query.matches(&node("label", "al")));
    }

    #[test]
    fn an_empty_query_matches_everything() {
        let query = FindQuery::default();
        assert!(query.matches(&node("label", "anything")));
    }
}
