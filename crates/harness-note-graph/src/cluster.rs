//! Deterministic community detection over the notes graph.
//!
//! graphify uses Leiden/Louvain through NetworkX. Pulling a modularity optimizer
//! into this workspace for a few hundred notes is not worth the dependency, so
//! this module implements weighted label propagation — the same family of
//! algorithm, seeded and ordered by node id so two runs on the same graph always
//! produce the same communities.

use std::collections::{BTreeMap, BTreeSet};

use crate::model::NoteGraph;

/// Label propagation converges in a handful of sweeps on note-sized graphs; the
/// cap exists so a pathological graph cannot spin forever.
const MAX_ITERATIONS: usize = 100;

/// Communities are labelled by their highest-degree member, matching graphify's
/// deterministic `label_communities_by_hub`.
pub fn community_labels(
    graph: &NoteGraph,
    communities: &BTreeMap<usize, Vec<String>>,
) -> BTreeMap<usize, String> {
    let mut labels = BTreeMap::new();
    for (cid, members) in communities {
        let hub = members
            .iter()
            .filter(|id| graph.node(id).is_some())
            .max_by(|a, b| {
                graph
                    .degree(a)
                    .cmp(&graph.degree(b))
                    .then_with(|| b.as_str().cmp(a.as_str()))
            });
        let label = hub
            .and_then(|id| graph.node(id))
            .map(|node| node.label.trim().trim_end_matches("()").to_string())
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| format!("Community {cid}"));
        labels.insert(*cid, label);
    }
    labels
}

/// Return `{community_id: [node_id, ...]}` with ids assigned by size (largest is
/// 0) and a total-order tie-break, so community ids do not churn between runs.
pub fn cluster(graph: &NoteGraph) -> BTreeMap<usize, Vec<String>> {
    if graph.nodes.is_empty() {
        return BTreeMap::new();
    }

    let adjacency = adjacency(graph);
    let order: Vec<&str> = graph.nodes.keys().map(String::as_str).collect();
    let mut labels: BTreeMap<&str, usize> = order.iter().map(|id| (*id, 0)).collect();
    for (index, id) in order.iter().enumerate() {
        labels.insert(id, index);
    }

    for _ in 0..MAX_ITERATIONS {
        let mut changed = false;
        for id in &order {
            let Some(neighbours) = adjacency.get(id) else {
                continue;
            };
            if neighbours.is_empty() {
                continue;
            }
            let mut tally: BTreeMap<usize, f32> = BTreeMap::new();
            for (neighbour, weight) in neighbours {
                if let Some(label) = labels.get(neighbour) {
                    *tally.entry(*label).or_default() += *weight;
                }
            }
            let Some((best, _)) = tally
                .iter()
                .max_by(|a, b| a.1.total_cmp(b.1).then_with(|| b.0.cmp(a.0)))
            else {
                continue;
            };
            if labels.get(id).copied() != Some(*best) {
                labels.insert(id, *best);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut groups: BTreeMap<usize, Vec<String>> = BTreeMap::new();
    for (id, label) in labels {
        groups.entry(label).or_default().push(id.to_string());
    }
    for members in groups.values_mut() {
        members.sort();
    }

    let mut ordered: Vec<Vec<String>> = groups.into_values().collect();
    ordered.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
    ordered.into_iter().enumerate().collect()
}

/// Ratio of present distinct pairs to possible distinct pairs in a community.
pub fn cohesion(graph: &NoteGraph, members: &[String]) -> f32 {
    let n = members.len();
    if n <= 1 {
        return 1.0;
    }
    let inside: BTreeSet<&str> = members.iter().map(String::as_str).collect();
    let mut pairs: BTreeSet<(&str, &str)> = BTreeSet::new();
    for edge in &graph.edges {
        if edge.source == edge.target {
            continue;
        }
        let (a, b) = (edge.source.as_str(), edge.target.as_str());
        if inside.contains(a) && inside.contains(b) {
            let pair = if a < b { (a, b) } else { (b, a) };
            pairs.insert(pair);
        }
    }
    let possible = (n * (n - 1)) as f32 / 2.0;
    if possible <= 0.0 {
        0.0
    } else {
        pairs.len() as f32 / possible
    }
}

fn adjacency(graph: &NoteGraph) -> BTreeMap<&str, BTreeMap<&str, f32>> {
    let mut adjacency: BTreeMap<&str, BTreeMap<&str, f32>> = BTreeMap::new();
    for edge in &graph.edges {
        if edge.source == edge.target {
            continue;
        }
        if !graph.nodes.contains_key(&edge.source) || !graph.nodes.contains_key(&edge.target) {
            continue;
        }
        *adjacency
            .entry(edge.source.as_str())
            .or_default()
            .entry(edge.target.as_str())
            .or_default() += edge.weight;
        *adjacency
            .entry(edge.target.as_str())
            .or_default()
            .entry(edge.source.as_str())
            .or_default() += edge.weight;
    }
    adjacency
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{note_id, Confidence, Edge, Node, NodeKind, Relation};

    fn graph() -> NoteGraph {
        let mut g = NoteGraph::default();
        for id in ["a", "b", "c", "d", "isolated"] {
            g.nodes
                .insert(note_id(id), Node::new(note_id(id), id, NodeKind::Note));
        }
        for (left, right) in [("a", "b"), ("c", "d")] {
            g.edges.push(Edge::new(
                note_id(left),
                note_id(right),
                Relation::References,
                Confidence::Extracted,
            ));
        }
        g
    }

    #[test]
    fn disconnected_components_are_separate_communities() {
        let communities = cluster(&graph());
        let groups: Vec<Vec<String>> = communities.into_values().collect();
        assert_eq!(groups.len(), 3);
        assert!(groups
            .iter()
            .any(|g| g.contains(&note_id("a")) && g.contains(&note_id("b"))));
        assert!(groups
            .iter()
            .any(|g| g.contains(&note_id("c")) && g.contains(&note_id("d"))));
        assert!(groups.iter().any(|g| g == &vec![note_id("isolated")]));
    }

    #[test]
    fn cluster_is_deterministic() {
        let first = cluster(&graph());
        let second = cluster(&graph());
        assert_eq!(first, second);
    }

    #[test]
    fn the_largest_community_gets_id_zero() {
        let communities = cluster(&graph());
        assert_eq!(communities[&0].len(), 2);
        assert!(communities.len() > 1);
    }

    #[test]
    fn labels_use_the_hub_label() {
        let g = graph();
        let communities = cluster(&g);
        let labels = community_labels(&g, &communities);
        let values: Vec<&String> = labels.values().collect();
        assert!(values.iter().any(|label| *label == "a" || *label == "b"));
    }

    #[test]
    fn cohesion_is_zero_for_an_unconnected_pair() {
        let mut g = NoteGraph::default();
        for id in ["a", "b"] {
            g.nodes
                .insert(note_id(id), Node::new(note_id(id), id, NodeKind::Note));
        }
        assert_eq!(cohesion(&g, &[note_id("a"), note_id("b")]), 0.0);
    }
}
