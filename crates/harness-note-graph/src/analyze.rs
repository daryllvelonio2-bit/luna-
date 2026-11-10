//! Graph-level analysis: hubs, cross-community bridges, and review prompts.
//!
//! The outputs are structured data, not prose. Formatting them belongs to the CLI
//! (see `agents.md` §1); this module only decides *what* is interesting, using the
//! same signals graphify reports: god nodes, surprising connections, isolated
//! notes, ambiguous edges, and low-cohesion communities.

use std::collections::{BTreeMap, BTreeSet};

use crate::cluster::cohesion;
use crate::model::{Confidence, NodeKind, NoteGraph, Relation};

/// How many hubs to report.
const TOP_GOD_NODES: usize = 10;

/// How many cross-community links to report.
const TOP_SURPRISES: usize = 5;

/// A community below this pair-density is loosely held together.
const LOW_COHESION: f32 = 0.08;

/// Hard ceiling on suggested questions; enough to guide a review, not a quiz.
const MAX_QUESTIONS: usize = 12;

#[derive(Debug, Clone, PartialEq)]
pub struct GodNode {
    pub id: String,
    pub label: String,
    pub kind: NodeKind,
    pub degree: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Surprise {
    pub source: String,
    pub target: String,
    pub source_label: String,
    pub target_label: String,
    pub relation: Relation,
    pub confidence: Confidence,
    pub score: f32,
    pub reasons: Vec<SurpriseReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurpriseReason {
    /// The endpoints sit in different communities.
    CrossesCommunities,
    /// At least one endpoint has very few other connections.
    LowDegree,
    /// The relationship itself is flagged uncertain.
    Ambiguous,
    /// The endpoints are different kinds of thing (note/tag/concept/file).
    CrossKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommunitySummary {
    pub id: usize,
    pub label: String,
    pub size: usize,
    pub cohesion: f32,
    pub members: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum QuestionKind {
    /// "What do these hubs have in common?"
    CommonGround { nodes: Vec<String> },
    /// "How is this cross-community link explained?"
    Bridge { left: String, right: String },
    /// "This note is isolated; should it be linked?"
    Isolated { note: String },
    /// "Is this uncertain link intended?"
    Ambiguous { left: String, right: String },
    /// "Is this loosely connected community one topic?"
    Community { label: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Analysis {
    pub god_nodes: Vec<GodNode>,
    pub surprises: Vec<Surprise>,
    pub questions: Vec<QuestionKind>,
    pub communities: Vec<CommunitySummary>,
    pub notes: usize,
    pub headings: usize,
    pub tags: usize,
    pub concepts: usize,
    pub files: usize,
    pub symbols: usize,
    pub missing: usize,
    pub edges: usize,
    pub dangling: usize,
    pub ambiguous_edges: usize,
}

/// Compute every analysis result in one pass over the graph.
pub fn analyze(
    graph: &NoteGraph,
    communities: &BTreeMap<usize, Vec<String>>,
    labels: &BTreeMap<usize, String>,
) -> Analysis {
    let node_community = invert(communities);
    let degree: BTreeMap<&str, usize> = graph
        .nodes
        .keys()
        .map(|id| (id.as_str(), graph.degree(id)))
        .collect();

    let god_nodes = god_nodes(graph, &degree);
    let surprises = surprises(graph, &node_community, &degree);
    let summaries = community_summaries(graph, communities, labels);

    let mut questions: Vec<Vec<QuestionKind>> = Vec::new();
    if god_nodes.len() >= 2 {
        questions.push(vec![QuestionKind::CommonGround {
            nodes: god_nodes.iter().take(3).map(|n| n.label.clone()).collect(),
        }]);
    }
    questions.push(
        surprises
            .iter()
            .take(3)
            .map(|s| QuestionKind::Bridge {
                left: s.source_label.clone(),
                right: s.target_label.clone(),
            })
            .collect(),
    );
    questions.push(ambiguous_questions(graph, &node_community));
    questions.push(isolated_questions(graph));
    questions.push(
        summaries
            .iter()
            .filter(|summary| summary.size >= 3 && summary.cohesion < LOW_COHESION)
            .take(2)
            .map(|summary| QuestionKind::Community {
                label: summary.label.clone(),
            })
            .collect(),
    );

    Analysis {
        god_nodes,
        surprises,
        questions: interleave(questions, MAX_QUESTIONS),
        communities: summaries,
        notes: graph.nodes_of_kind(NodeKind::Note).count(),
        headings: graph.nodes_of_kind(NodeKind::Heading).count(),
        tags: graph.nodes_of_kind(NodeKind::Tag).count(),
        concepts: graph.nodes_of_kind(NodeKind::Concept).count(),
        files: graph.nodes_of_kind(NodeKind::File).count(),
        symbols: graph.nodes_of_kind(NodeKind::Symbol).count(),
        missing: graph.nodes_of_kind(NodeKind::Missing).count(),
        edges: graph.edges.len(),
        dangling: graph.dangling.len(),
        ambiguous_edges: graph
            .edges
            .iter()
            .filter(|edge| edge.confidence == Confidence::Ambiguous)
            .count(),
    }
}

fn invert(communities: &BTreeMap<usize, Vec<String>>) -> BTreeMap<String, usize> {
    let mut map = BTreeMap::new();
    for (cid, members) in communities {
        for member in members {
            map.insert(member.clone(), *cid);
        }
    }
    map
}

/// Hubs are the entities everything routes through. Structural page/heading nodes
/// are excluded: a note accumulates `contains` edges mechanically and would
/// otherwise always win.
fn god_nodes(graph: &NoteGraph, degree: &BTreeMap<&str, usize>) -> Vec<GodNode> {
    let mut ranked: Vec<GodNode> = graph
        .nodes
        .values()
        .filter(|node| {
            !matches!(
                node.kind,
                NodeKind::Note | NodeKind::Heading | NodeKind::Missing
            )
        })
        .filter(|node| !node.label.trim().is_empty())
        .map(|node| GodNode {
            id: node.id.clone(),
            label: node.label.clone(),
            kind: node.kind,
            degree: degree.get(node.id.as_str()).copied().unwrap_or(0),
        })
        .filter(|node| node.degree > 0)
        .collect();
    ranked.sort_by(|a, b| {
        b.degree
            .cmp(&a.degree)
            .then_with(|| a.label.cmp(&b.label))
            .then_with(|| a.id.cmp(&b.id))
    });
    ranked.truncate(TOP_GOD_NODES);
    ranked
}

fn surprises(
    graph: &NoteGraph,
    node_community: &BTreeMap<String, usize>,
    degree: &BTreeMap<&str, usize>,
) -> Vec<Surprise> {
    let mut best: BTreeMap<(String, String), Surprise> = BTreeMap::new();
    for edge in &graph.edges {
        let Some(left) = graph.node(&edge.source) else {
            continue;
        };
        let Some(right) = graph.node(&edge.target) else {
            continue;
        };
        let (Some(left_cid), Some(right_cid)) = (
            node_community.get(&edge.source),
            node_community.get(&edge.target),
        ) else {
            continue;
        };
        if left_cid == right_cid {
            continue;
        }
        let (source, target, source_label, target_label) = if edge.source < edge.target {
            (
                edge.source.clone(),
                edge.target.clone(),
                left.label.clone(),
                right.label.clone(),
            )
        } else {
            (
                edge.target.clone(),
                edge.source.clone(),
                right.label.clone(),
                left.label.clone(),
            )
        };
        let score = surprise_score(
            edge.confidence,
            degree.get(left.id.as_str()).copied().unwrap_or(0),
            degree.get(right.id.as_str()).copied().unwrap_or(0),
            left.kind,
            right.kind,
        );
        let mut reasons = vec![SurpriseReason::CrossesCommunities];
        if edge.confidence == Confidence::Ambiguous {
            reasons.push(SurpriseReason::Ambiguous);
        }
        if degree.get(left.id.as_str()).copied().unwrap_or(0) <= 2
            || degree.get(right.id.as_str()).copied().unwrap_or(0) <= 2
        {
            reasons.push(SurpriseReason::LowDegree);
        }
        if left.kind != right.kind {
            reasons.push(SurpriseReason::CrossKind);
        }
        let candidate = Surprise {
            source,
            target,
            source_label,
            target_label,
            relation: edge.relation,
            confidence: edge.confidence,
            score,
            reasons,
        };
        let key = (edge.source.clone(), edge.target.clone());
        match best.get(&key) {
            Some(existing) if existing.score >= score => {}
            _ => {
                best.insert(key, candidate);
            }
        }
    }
    let mut ranked: Vec<Surprise> = best.into_values().collect();
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.source.cmp(&b.source))
            .then_with(|| a.target.cmp(&b.target))
    });
    ranked.truncate(TOP_SURPRISES);
    ranked
}

fn surprise_score(
    confidence: Confidence,
    left_degree: usize,
    right_degree: usize,
    left_kind: NodeKind,
    right_kind: NodeKind,
) -> f32 {
    let certainty = match confidence {
        Confidence::Ambiguous => 3.0,
        Confidence::Inferred => 2.0,
        Confidence::Extracted => 1.0,
    };
    let bridge = 1.0 / (1.0 + left_degree as f32) + 1.0 / (1.0 + right_degree as f32);
    let cross_kind = if left_kind != right_kind { 0.5 } else { 0.0 };
    certainty + bridge + cross_kind
}

fn community_summaries(
    graph: &NoteGraph,
    communities: &BTreeMap<usize, Vec<String>>,
    labels: &BTreeMap<usize, String>,
) -> Vec<CommunitySummary> {
    communities
        .iter()
        .map(|(cid, members)| CommunitySummary {
            id: *cid,
            label: labels
                .get(cid)
                .cloned()
                .unwrap_or_else(|| format!("Community {cid}")),
            size: members.len(),
            cohesion: cohesion(graph, members),
            members: members.clone(),
        })
        .collect()
}

fn isolated_questions(graph: &NoteGraph) -> Vec<QuestionKind> {
    let mut isolated: Vec<&str> = graph
        .nodes_of_kind(NodeKind::Note)
        .filter(|node| graph.degree(&node.id) == 0)
        .map(|node| node.id.as_str())
        .collect();
    isolated.sort();
    isolated
        .into_iter()
        .take(3)
        .filter_map(|id| {
            graph.node(id).map(|node| QuestionKind::Isolated {
                note: node.label.clone(),
            })
        })
        .collect()
}

fn ambiguous_questions(
    graph: &NoteGraph,
    node_community: &BTreeMap<String, usize>,
) -> Vec<QuestionKind> {
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut out = Vec::new();
    for edge in graph
        .edges
        .iter()
        .filter(|edge| edge.confidence == Confidence::Ambiguous)
    {
        let Some(left) = graph.node(&edge.source) else {
            continue;
        };
        let Some(right) = graph.node(&edge.target) else {
            continue;
        };
        if node_community.get(&edge.source) == node_community.get(&edge.target) {
            continue;
        }
        let pair = if edge.source < edge.target {
            (edge.source.clone(), edge.target.clone())
        } else {
            (edge.target.clone(), edge.source.clone())
        };
        if !seen.insert(pair) {
            continue;
        }
        out.push(QuestionKind::Ambiguous {
            left: left.label.clone(),
            right: right.label.clone(),
        });
        if out.len() >= 3 {
            break;
        }
    }
    out
}

/// Round-robin the question groups so a noisy signal cannot crowd out the rest.
fn interleave(groups: Vec<Vec<QuestionKind>>, limit: usize) -> Vec<QuestionKind> {
    let mut out = Vec::new();
    let mut index = 0;
    loop {
        let mut added = false;
        for group in &groups {
            if let Some(question) = group.get(index) {
                out.push(question.clone());
                added = true;
                if out.len() >= limit {
                    return out;
                }
            }
        }
        if !added {
            return out;
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{concept_id, note_id, tag_id, Edge, Node};

    fn graph() -> NoteGraph {
        let mut g = NoteGraph::default();
        for id in ["a", "b", "c", "d", "e"] {
            g.nodes
                .insert(note_id(id), Node::new(note_id(id), id, NodeKind::Note));
        }
        g.nodes.insert(
            tag_id("shared"),
            Node::new(tag_id("shared"), "shared", NodeKind::Tag),
        );
        g.nodes.insert(
            concept_id("compaction"),
            Node::new(concept_id("compaction"), "compaction", NodeKind::Concept),
        );
        for id in ["a", "b"] {
            g.edges.push(Edge::new(
                note_id(id),
                tag_id("shared"),
                Relation::Tagged,
                Confidence::Extracted,
            ));
        }
        g.edges.push(Edge::new(
            note_id("c"),
            concept_id("compaction"),
            Relation::Mentions,
            Confidence::Inferred,
        ));
        g.edges.push(Edge::new(
            note_id("d"),
            concept_id("compaction"),
            Relation::Mentions,
            Confidence::Inferred,
        ));
        // A thin bridge between the two clusters. Notes are excluded from god
        // nodes, so this adds a cross-community edge without changing the hub
        // ranking the other tests assert on.
        g.edges.push(Edge::new(
            note_id("a"),
            note_id("c"),
            Relation::References,
            Confidence::Inferred,
        ));
        g
    }

    #[test]
    fn god_nodes_rank_shared_entities_not_pages() {
        let g = graph();
        let communities = crate::cluster::cluster(&g);
        let labels = crate::cluster::community_labels(&g, &communities);
        let analysis = analyze(&g, &communities, &labels);
        assert_eq!(analysis.god_nodes[0].degree, 2);
        assert!(analysis.god_nodes.iter().any(|node| node.label == "shared"));
        assert!(analysis
            .god_nodes
            .iter()
            .all(|node| node.kind != NodeKind::Note));
    }

    #[test]
    fn a_cross_community_edge_is_surprising() {
        let g = graph();
        let communities = crate::cluster::cluster(&g);
        let labels = crate::cluster::community_labels(&g, &communities);
        let analysis = analyze(&g, &communities, &labels);
        assert!(!analysis.surprises.is_empty());
        assert!(analysis
            .surprises
            .iter()
            .all(|s| s.reasons.contains(&SurpriseReason::CrossesCommunities)));
    }

    #[test]
    fn an_isolated_note_gets_a_question() {
        let g = graph();
        let communities = crate::cluster::cluster(&g);
        let labels = crate::cluster::community_labels(&g, &communities);
        let analysis = analyze(&g, &communities, &labels);
        assert!(analysis
            .questions
            .iter()
            .any(|q| matches!(q, QuestionKind::Isolated { .. })));
    }

    #[test]
    fn ambiguous_edges_are_flagged() {
        let mut g = graph();
        g.edges.push(Edge::new(
            note_id("a"),
            note_id("d"),
            Relation::References,
            Confidence::Ambiguous,
        ));
        let communities = crate::cluster::cluster(&g);
        let labels = crate::cluster::community_labels(&g, &communities);
        let analysis = analyze(&g, &communities, &labels);
        assert!(analysis.ambiguous_edges > 0);
        assert!(analysis
            .surprises
            .iter()
            .any(|s| s.reasons.contains(&SurpriseReason::Ambiguous)));
    }
}
