//! Structural checks run after build and before export.
//!
//! graphify's `validate.py` refuses an extraction whose edges point at nodes that
//! do not exist. The notes graph can be more forgiving — a dangling wikilink is a
//! useful finding, not a crash — so this module reports issues instead of
//! aborting, and the CLI decides whether to fail.

use std::collections::BTreeSet;
use std::fmt;

use crate::model::NoteGraph;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Issue {
    EmptyNodeId,
    EmptyLabel {
        id: String,
    },
    MissingEndpoint {
        edge: String,
        endpoint: String,
    },
    DuplicateEdge {
        source: String,
        target: String,
        relation: String,
    },
    DanglingNodeWithoutEdge {
        id: String,
    },
    SelfLoop {
        id: String,
    },
}

impl fmt::Display for Issue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyNodeId => write!(f, "node with an empty id"),
            Self::EmptyLabel { id } => write!(f, "node '{id}' has an empty label"),
            Self::MissingEndpoint { edge, endpoint } => {
                write!(f, "edge '{edge}' points at missing node '{endpoint}'")
            }
            Self::DuplicateEdge {
                source,
                target,
                relation,
            } => write!(f, "duplicate edge {source} -[{relation}]-> {target}"),
            Self::DanglingNodeWithoutEdge { id } => {
                write!(f, "dangling node '{id}' has no incoming link")
            }
            Self::SelfLoop { id } => write!(f, "self-loop on '{id}'"),
        }
    }
}

/// Return every structural problem in `graph`, in deterministic order.
pub fn validate(graph: &NoteGraph) -> Vec<Issue> {
    let mut issues = Vec::new();
    for node in graph.nodes.values() {
        if node.id.trim().is_empty() {
            issues.push(Issue::EmptyNodeId);
        }
        if node.label.trim().is_empty() {
            issues.push(Issue::EmptyLabel {
                id: node.id.clone(),
            });
        }
    }

    let mut seen: BTreeSet<(String, String, String)> = BTreeSet::new();
    let mut touched: BTreeSet<&str> = BTreeSet::new();
    for edge in &graph.edges {
        if edge.source == edge.target {
            issues.push(Issue::SelfLoop {
                id: edge.source.clone(),
            });
        }
        for endpoint in [&edge.source, &edge.target] {
            touched.insert(endpoint.as_str());
            if !graph.nodes.contains_key(endpoint) {
                issues.push(Issue::MissingEndpoint {
                    edge: format!("{} -> {}", edge.source, edge.target),
                    endpoint: endpoint.clone(),
                });
            }
        }
        let key = (
            edge.source.clone(),
            edge.target.clone(),
            format!("{:?}", edge.relation),
        );
        if !seen.insert(key) {
            issues.push(Issue::DuplicateEdge {
                source: edge.source.clone(),
                target: edge.target.clone(),
                relation: format!("{:?}", edge.relation),
            });
        }
    }

    for node in graph.nodes.values() {
        if node.kind == crate::model::NodeKind::Missing && !touched.contains(node.id.as_str()) {
            issues.push(Issue::DanglingNodeWithoutEdge {
                id: node.id.clone(),
            });
        }
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{note_id, Confidence, Edge, Node, NodeKind, Relation};

    #[test]
    fn a_clean_graph_has_no_issues() {
        let mut graph = NoteGraph::default();
        for id in ["a", "b"] {
            graph
                .nodes
                .insert(note_id(id), Node::new(note_id(id), id, NodeKind::Note));
        }
        graph.edges.push(Edge::new(
            note_id("a"),
            note_id("b"),
            Relation::References,
            Confidence::Extracted,
        ));
        assert!(validate(&graph).is_empty());
    }

    #[test]
    fn a_missing_endpoint_is_reported() {
        let mut graph = NoteGraph::default();
        graph
            .nodes
            .insert(note_id("a"), Node::new(note_id("a"), "a", NodeKind::Note));
        graph.edges.push(Edge::new(
            note_id("a"),
            note_id("ghost"),
            Relation::References,
            Confidence::Extracted,
        ));
        assert!(validate(&graph)
            .iter()
            .any(|issue| matches!(issue, Issue::MissingEndpoint { .. })));
    }

    #[test]
    fn a_duplicate_edge_is_reported() {
        let mut graph = NoteGraph::default();
        for id in ["a", "b"] {
            graph
                .nodes
                .insert(note_id(id), Node::new(note_id(id), id, NodeKind::Note));
        }
        let edge = Edge::new(
            note_id("a"),
            note_id("b"),
            Relation::References,
            Confidence::Extracted,
        );
        graph.edges.push(edge.clone());
        graph.edges.push(edge);
        assert!(validate(&graph)
            .iter()
            .any(|issue| matches!(issue, Issue::DuplicateEdge { .. })));
    }
}
