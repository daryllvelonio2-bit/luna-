//! Knowledge graph over the Markdown notes store.
//!
//! The pipeline mirrors the one used by `graphify` for corpora, reduced to what a
//! notes folder actually contains:
//!
//! ```text
//! extract  ->  build  ->  cluster  ->  analyze  ->  export
//! ```
//!
//! A note becomes a *page* node; its headings become `contains` edges; its
//! frontmatter tags become shared tag nodes; `[[wikilinks]]`, Markdown links and
//! inline code spans become citation edges; and terms shared by two or more
//! notes become concept nodes. Every edge carries a provenance label so a
//! consumer can tell an explicit link (`EXTRACTED`) from a heuristic
//! co-occurrence (`INFERRED`) or an uncertain guess (`AMBIGUOUS`).
//!
//! The crate is deliberately self-contained: it owns its graph type, its
//! deterministic community detection, and its exporters, so it can be tested and
//! reused without pulling in a graph database or an external layout engine.

pub mod analyze;
pub mod build;
pub mod cluster;
pub mod export;
pub mod extract;
pub mod model;
pub mod validate;

pub use analyze::{Analysis, CommunitySummary, GodNode, QuestionKind, Surprise};
pub use build::{build, GraphBuilder};
pub use cluster::{cluster, cohesion, community_labels};
pub use export::{to_json, to_wiki};
pub use extract::extract_note;
pub use model::{Confidence, Edge, Extraction, Node, NodeKind, NoteGraph, Relation};
pub use validate::validate;

/// Everything the CLI needs from one pass, without repeating the pipeline.
#[derive(Debug, Clone)]
pub struct BuiltGraph {
    pub graph: NoteGraph,
    pub communities: std::collections::BTreeMap<usize, Vec<String>>,
    pub labels: std::collections::BTreeMap<usize, String>,
    pub analysis: Analysis,
}

/// Run extract -> build -> cluster -> analyze for a set of notes.
pub fn graph_notes(notes: &[harness_core::Note]) -> BuiltGraph {
    let extractions: Vec<Extraction> = notes.iter().map(extract_note).collect();
    let graph = build(extractions);
    let communities = cluster(&graph);
    let labels = community_labels(&graph, &communities);
    let analysis = analyze::analyze(&graph, &communities, &labels);
    BuiltGraph {
        graph,
        communities,
        labels,
        analysis,
    }
}
