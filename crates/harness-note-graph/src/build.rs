//! Assemble per-note extractions into one deterministic graph.
//!
//! graphify's build stage performs three passes: merge nodes by id, resolve
//! semantic twins, and prune edges that no longer have both endpoints. The notes
//! store needs the same shape, plus one extra pass that only makes sense for
//! text: promote terms shared by several notes into concept nodes.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::{concept_id, Edge, Extraction, Node, NodeKind, NoteGraph, Relation};

/// A term used by a single note says nothing connective; two is the minimum.
const MIN_CONCEPT_NOTES: usize = 2;

/// A term used by more than this fraction of notes is vocabulary, not signal.
/// Mirrors graphify's hub suppression: a word in every note would otherwise
/// dominate the graph as a fake hub.
const MAX_CONCEPT_FRACTION: f32 = 0.6;

/// Hard ceiling on concept nodes so a large vault cannot explode the graph.
const MAX_CONCEPTS: usize = 200;

/// Accumulates extractions before the final graph is built.
#[derive(Debug, Default)]
pub struct GraphBuilder {
    nodes: BTreeMap<String, Node>,
    edges: BTreeMap<(String, String, Relation), Edge>,
    placeholders: BTreeSet<String>,
    terms: BTreeMap<String, BTreeSet<String>>,
    note_count: usize,
}

impl GraphBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Merge one note's extraction. Later definitions win for a node id, which is
    /// how a real note replaces the placeholder its incoming links created.
    pub fn add(&mut self, extraction: Extraction) {
        // The page node is the extraction's own note: the first real `Note`
        // node. Link placeholders and notes merely cited by this page are not
        // the author of its terms, so only the page gets the term set.
        let page = extraction
            .nodes
            .iter()
            .find(|node| node.kind == NodeKind::Note && !node.metadata.contains_key("placeholder"))
            .map(|node| node.id.clone());

        for node in extraction.nodes {
            if node.kind == NodeKind::Note && node.metadata.contains_key("placeholder") {
                let id = node.id.clone();
                self.placeholders.insert(id.clone());
                self.nodes.entry(id).or_insert(node);
                continue;
            }
            if node.kind == NodeKind::Note {
                self.placeholders.remove(&node.id);
            }
            self.nodes.insert(node.id.clone(), node);
        }
        for edge in extraction.edges {
            self.merge_edge(edge);
        }
        if let Some(note) = page {
            self.note_count += 1;
            self.terms.entry(note).or_default().extend(extraction.terms);
        }
    }

    fn merge_edge(&mut self, edge: Edge) {
        let key = (edge.source.clone(), edge.target.clone(), edge.relation);
        match self.edges.get_mut(&key) {
            Some(existing) if existing.confidence >= edge.confidence => {
                existing.weight = existing.weight.max(edge.weight);
            }
            _ => {
                self.edges.insert(key, edge);
            }
        }
    }

    /// Finish the graph: resolve placeholders, add shared concepts, drop
    /// half-edges, and sort everything for a byte-stable export.
    pub fn build(mut self) -> NoteGraph {
        for id in std::mem::take(&mut self.placeholders) {
            if let Some(node) = self.nodes.get_mut(&id) {
                node.kind = NodeKind::Missing;
                node.metadata.remove("placeholder");
                node.metadata.insert("dangling".into(), "true".into());
            }
        }

        self.add_concepts();

        let mut dangling: Vec<String> = self
            .nodes
            .values()
            .filter(|node| node.kind == NodeKind::Missing)
            .map(|node| node.label.clone())
            .collect();
        dangling.sort();
        dangling.dedup();

        let edges: Vec<Edge> = self
            .edges
            .into_values()
            .filter(|edge| {
                self.nodes.contains_key(&edge.source) && self.nodes.contains_key(&edge.target)
            })
            .collect();

        NoteGraph {
            nodes: self.nodes,
            edges,
            dangling,
        }
    }

    fn add_concepts(&mut self) {
        if self.note_count < MIN_CONCEPT_NOTES {
            return;
        }
        let max_documents = ((self.note_count as f32) * MAX_CONCEPT_FRACTION).ceil() as usize;

        let tag_labels: BTreeSet<String> = self
            .nodes
            .values()
            .filter(|node| node.kind == NodeKind::Tag)
            .map(|node| crate::model::slug(&node.label))
            .collect();

        let mut candidates: Vec<(String, Vec<String>)> = self
            .terms
            .iter()
            .map(|(note, terms)| (note.clone(), terms.iter().cloned().collect::<Vec<_>>()))
            .fold(
                BTreeMap::<String, Vec<String>>::new(),
                |mut acc, (note, terms)| {
                    for term in terms {
                        acc.entry(term).or_default().push(note.clone());
                    }
                    acc
                },
            )
            .into_iter()
            .filter(|(term, notes)| {
                notes.len() >= MIN_CONCEPT_NOTES
                    && notes.len() <= max_documents.max(MIN_CONCEPT_NOTES)
                    && !tag_labels.contains(&crate::model::slug(term))
            })
            .collect();

        candidates.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
        candidates.truncate(MAX_CONCEPTS);

        for (term, notes) in candidates {
            let id = concept_id(&term);
            self.nodes
                .entry(id.clone())
                .or_insert_with(|| Node::new(id.clone(), term.clone(), NodeKind::Concept));
            for note in notes {
                self.merge_edge(Edge::new(
                    note,
                    id.clone(),
                    Relation::Mentions,
                    crate::model::Confidence::Inferred,
                ));
            }
        }
    }
}

/// Build a graph from a batch of extractions.
pub fn build(extractions: Vec<Extraction>) -> NoteGraph {
    let mut builder = GraphBuilder::new();
    for extraction in extractions {
        builder.add(extraction);
    }
    builder.build()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{note_id, Confidence};

    fn extraction(note: &str, terms: &[&str]) -> Extraction {
        let page = Node::new(note_id(note), note, NodeKind::Note);
        let target = note_id("target");
        Extraction {
            nodes: vec![
                page.clone(),
                Node::new(target.clone(), "target", NodeKind::Note),
            ],
            edges: vec![Edge::new(
                page.id,
                target,
                Relation::References,
                Confidence::Extracted,
            )],
            terms: terms.iter().map(|t| t.to_string()).collect(),
        }
    }

    #[test]
    fn duplicate_edges_are_merged() {
        let mut e = extraction("a", &[]);
        let duplicate = e.edges[0].clone();
        let mut second = e.clone();
        e.edges.push(duplicate.clone());
        second.nodes.clear();
        second.edges = vec![duplicate];
        let graph = build(vec![e, second]);
        assert_eq!(graph.edges.len(), 1);
    }

    #[test]
    fn an_unresolved_link_becomes_a_missing_node() {
        let mut e = extraction("a", &[]);
        e.nodes
            .retain(|n| n.kind != NodeKind::Note || n.id == note_id("a"));
        e.edges[0].target = note_id("ghost");
        e.edges[0].source = note_id("a");
        e.nodes
            .push(Node::new(note_id("ghost"), "ghost", NodeKind::Note).with("placeholder", "true"));
        let graph = build(vec![e]);
        assert_eq!(
            graph.node(&note_id("ghost")).map(|n| n.kind),
            Some(NodeKind::Missing)
        );
        assert_eq!(graph.dangling, vec!["ghost".to_string()]);
    }

    #[test]
    fn a_term_shared_by_two_notes_becomes_a_concept() {
        let graph = build(vec![
            extraction("a", &["compaction", "unique-a"]),
            extraction("b", &["compaction", "unique-b"]),
        ]);
        assert!(graph.node(&concept_id("compaction")).is_some());
        assert!(graph.node(&concept_id("unique-a")).is_none());
    }

    #[test]
    fn a_term_in_every_note_is_suppressed_as_vocabulary() {
        let graph = build(vec![
            extraction("a", &["luna"]),
            extraction("b", &["luna"]),
            extraction("c", &["luna"]),
        ]);
        assert!(graph.node(&concept_id("luna")).is_none());
    }

    #[test]
    fn build_is_deterministic() {
        let a = build(vec![
            extraction("a", &["shared"]),
            extraction("b", &["shared"]),
        ]);
        let b = build(vec![
            extraction("a", &["shared"]),
            extraction("b", &["shared"]),
        ]);
        assert_eq!(
            serde_json::to_string(&a.nodes).unwrap_or_default(),
            serde_json::to_string(&b.nodes).unwrap_or_default()
        );
    }
}
