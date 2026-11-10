//! Graph data model shared by every pipeline stage.
//!
//! The shapes follow graphify's extraction schema (`nodes`/`edges` with a
//! `confidence` label) so an exported `graph.json` stays familiar to anyone who
//! has used that tool.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Provenance of one edge, in decreasing order of certainty.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Confidence {
    /// The relationship is written in the source: a wikilink, a tag, a heading.
    Extracted,
    /// The relationship is a reasonable deduction: shared terms, a symbol mention.
    Inferred,
    /// The relationship is uncertain and should be reviewed by a human.
    Ambiguous,
}

impl Confidence {
    /// Numeric weight used by clustering and surprise scoring.
    pub fn score(self) -> f32 {
        match self {
            Self::Extracted => 1.0,
            Self::Inferred => 0.6,
            Self::Ambiguous => 0.25,
        }
    }
}

/// What a node represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    /// A stored note: the page node.
    Note,
    /// A Markdown heading inside a note.
    Heading,
    /// A frontmatter tag.
    Tag,
    /// A term shared by several notes.
    Concept,
    /// A file path cited by a note.
    File,
    /// A code symbol cited by a note.
    Symbol,
    /// A `[[wikilink]]` target that no stored note resolves to.
    Missing,
}

/// What an edge means.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Relation {
    /// Structural containment: note -> heading, heading -> heading.
    Contains,
    /// Note -> note citation written in the body.
    References,
    /// Note -> tag from explicit frontmatter.
    Tagged,
    /// Note -> file, symbol, or concept named in the body.
    Mentions,
    /// Note -> parent note for a chunked note (`part_of`).
    PartOf,
}

/// One graph node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub label: String,
    pub kind: NodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_location: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

impl Node {
    pub fn new(id: impl Into<String>, label: impl Into<String>, kind: NodeKind) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            kind,
            source_file: None,
            source_location: None,
            metadata: BTreeMap::new(),
        }
    }

    pub fn at(mut self, source_file: impl Into<String>, line: usize) -> Self {
        self.source_file = Some(source_file.into());
        self.source_location = Some(format!("L{line}"));
        self
    }

    pub fn with(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
}

/// One graph edge. `weight` lets a later stage down-rank weaker relations
/// without losing the edge, which is how graphify treats `AMBIGUOUS` links.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub source: String,
    pub target: String,
    pub relation: Relation,
    pub confidence: Confidence,
    #[serde(default = "default_weight")]
    pub weight: f32,
}

fn default_weight() -> f32 {
    1.0
}

impl Edge {
    pub fn new(
        source: impl Into<String>,
        target: impl Into<String>,
        relation: Relation,
        confidence: Confidence,
    ) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            relation,
            confidence,
            weight: confidence.score(),
        }
    }
}

/// The per-file output of `extract`, before nodes and edges are merged.
#[derive(Debug, Clone, Default)]
pub struct Extraction {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// Lowercase content terms used to mine shared concepts across notes. Kept
    /// out of the node schema because a single note cannot know which terms are
    /// shared: only the builder can.
    pub terms: Vec<String>,
}

/// A merged, deterministic graph. `nodes` is a map so lookups stay O(log n) and
/// export order is stable across runs.
#[derive(Debug, Clone, Default)]
pub struct NoteGraph {
    pub nodes: BTreeMap<String, Node>,
    pub edges: Vec<Edge>,
    /// Link targets that no stored note resolved to. Reported, never silently
    /// dropped: a broken `[[link]]` is worth fixing.
    pub dangling: Vec<String>,
}

impl NoteGraph {
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.get(id)
    }

    /// Distinct neighbours of `id`, ignoring self-loops.
    pub fn neighbours(&self, id: &str) -> Vec<&str> {
        let mut seen = std::collections::BTreeSet::new();
        for edge in &self.edges {
            if edge.source == id && edge.target != id {
                seen.insert(edge.target.as_str());
            } else if edge.target == id && edge.source != id {
                seen.insert(edge.source.as_str());
            }
        }
        seen.into_iter().collect()
    }

    pub fn degree(&self, id: &str) -> usize {
        self.neighbours(id).len()
    }

    /// Nodes of one kind, in id order.
    pub fn nodes_of_kind(&self, kind: NodeKind) -> impl Iterator<Item = &Node> {
        self.nodes.values().filter(move |node| node.kind == kind)
    }
}

/// Normalize a label into a stable, filesystem-safe identifier fragment.
pub fn slug(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_dash = false;
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

/// Canonical id for a stored note. `slug` is also what wikilink resolution
/// compares against, so `[[Memory vs notes]]` finds `memory-vs-notes`.
pub fn note_id(note_slug: &str) -> String {
    format!("note:{note_slug}")
}

pub fn heading_id(note_slug: &str, heading_slug: &str, line: usize) -> String {
    format!("heading:{note_slug}:{heading_slug}:{line}")
}

pub fn tag_id(tag: &str) -> String {
    format!("tag:{}", slug(tag))
}

pub fn concept_id(term: &str) -> String {
    format!("concept:{}", slug(term))
}

pub fn file_id(path: &str) -> String {
    format!("file:{}", slug(path))
}

pub fn symbol_id(symbol: &str) -> String {
    format!("symbol:{}", slug(symbol))
}

pub fn missing_id(target: &str) -> String {
    format!("missing:{}", slug(target))
}
