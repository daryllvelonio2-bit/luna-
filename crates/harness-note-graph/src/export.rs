//! Export the graph to the formats graphify produces: a node-link `graph.json`
//! and a wiki directory of Markdown articles joined by `[[wikilinks]]`.
//!
//! The wiki is written so the output directory can be opened directly as an
//! Obsidian vault; that is the same convention graphify's `--obsidian` export
//! follows, without the extra per-node boilerplate.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use harness_core::HarnessError;
use serde_json::{json, Value};

use crate::analyze::Analysis;
use crate::model::{slug, Confidence, NodeKind, NoteGraph};

/// Confidence serialized in the same shape graphify writes.
fn confidence_label(confidence: Confidence) -> &'static str {
    match confidence {
        Confidence::Extracted => "EXTRACTED",
        Confidence::Inferred => "INFERRED",
        Confidence::Ambiguous => "AMBIGUOUS",
    }
}

fn kind_label(kind: NodeKind) -> &'static str {
    match kind {
        NodeKind::Note => "note",
        NodeKind::Heading => "heading",
        NodeKind::Tag => "tag",
        NodeKind::Concept => "concept",
        NodeKind::File => "file",
        NodeKind::Symbol => "symbol",
        NodeKind::Missing => "missing",
    }
}

/// Build the node-link document without touching the filesystem.
pub fn graph_json(
    graph: &NoteGraph,
    communities: &BTreeMap<usize, Vec<String>>,
    labels: &BTreeMap<usize, String>,
) -> Value {
    let node_community: BTreeMap<&str, usize> = communities
        .iter()
        .flat_map(|(cid, members)| members.iter().map(move |id| (id.as_str(), *cid)))
        .collect();

    let nodes: Vec<Value> = graph
        .nodes
        .values()
        .map(|node| {
            let mut value = json!({
                "id": node.id,
                "label": node.label,
                "kind": kind_label(node.kind),
                "norm_label": node.label.to_lowercase(),
            });
            if let Some(source) = &node.source_file {
                value["source_file"] = json!(source);
            }
            if let Some(location) = &node.source_location {
                value["source_location"] = json!(location);
            }
            if !node.metadata.is_empty() {
                value["metadata"] = json!(node.metadata);
            }
            if let Some(cid) = node_community.get(node.id.as_str()) {
                value["community"] = json!(cid);
                if let Some(label) = labels.get(cid) {
                    value["community_name"] = json!(label);
                }
            }
            value
        })
        .collect();

    let links: Vec<Value> = graph
        .edges
        .iter()
        .map(|edge| {
            json!({
                "source": edge.source,
                "target": edge.target,
                "relation": match edge.relation {
                    crate::model::Relation::Contains => "contains",
                    crate::model::Relation::References => "references",
                    crate::model::Relation::Tagged => "tagged",
                    crate::model::Relation::Mentions => "mentions",
                    crate::model::Relation::PartOf => "part_of",
                },
                "confidence": confidence_label(edge.confidence),
                "confidence_score": edge.weight,
                "weight": edge.weight,
            })
        })
        .collect();

    json!({
        "directed": false,
        "multigraph": false,
        "graph": {
            "nodes": graph.nodes.len(),
            "links": graph.edges.len(),
            "dangling": graph.dangling,
        },
        "nodes": nodes,
        "links": links,
    })
}

/// Write `<path>` as pretty JSON, leaving an identical file untouched.
pub fn to_json(
    graph: &NoteGraph,
    communities: &BTreeMap<usize, Vec<String>>,
    labels: &BTreeMap<usize, String>,
    path: &Path,
) -> Result<(), HarnessError> {
    let value = graph_json(graph, communities, labels);
    let text = serde_json::to_string_pretty(&value)
        .map_err(|error| HarnessError::SerializationError(error.to_string()))?;
    write_changed(path, &format!("{text}\n"))
}

/// Write `index.md` plus one article per community and per god node.
///
/// Returns the number of articles written, excluding `index.md`.
pub fn to_wiki(
    graph: &NoteGraph,
    communities: &BTreeMap<usize, Vec<String>>,
    labels: &BTreeMap<usize, String>,
    analysis: &Analysis,
    dir: &Path,
) -> Result<usize, HarnessError> {
    std::fs::create_dir_all(dir).map_err(|error| {
        HarnessError::Io(format!(
            "cannot create wiki directory '{}': {error}",
            dir.display()
        ))
    })?;

    let mut used: BTreeSet<String> = BTreeSet::new();
    used.insert("index".to_string());

    let mut community_files: BTreeMap<usize, String> = BTreeMap::new();
    for (cid, members) in communities {
        let base = labels
            .get(cid)
            .map(|label| slug(label))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| format!("community-{cid}"));
        let stem = unique_stem(&base, &mut used);
        community_files.insert(*cid, stem.clone());
        let article = community_article(graph, *cid, members, labels);
        write_changed(&dir.join(format!("{stem}.md")), &article)?;
    }

    let mut written = communities.len();
    for god in &analysis.god_nodes {
        let base = slug(&god.label);
        if base.is_empty() {
            continue;
        }
        let stem = unique_stem(&format!("hub-{base}"), &mut used);
        write_changed(
            &dir.join(format!("{stem}.md")),
            &hub_article(graph, communities, labels, god),
        )?;
        written += 1;
    }

    let index = index_article(graph, analysis, &community_files);
    write_changed(&dir.join("index.md"), &index)?;
    Ok(written)
}

fn unique_stem(base: &str, used: &mut BTreeSet<String>) -> String {
    let mut stem = base.to_string();
    let mut suffix = 2usize;
    while used.contains(&stem) {
        stem = format!("{base}-{suffix}");
        suffix += 1;
    }
    used.insert(stem.clone());
    stem
}

fn community_article(
    graph: &NoteGraph,
    cid: usize,
    members: &[String],
    labels: &BTreeMap<usize, String>,
) -> String {
    let label = labels
        .get(&cid)
        .cloned()
        .unwrap_or_else(|| format!("Community {cid}"));
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", heading(&label)));
    out.push_str(&format!(
        "Community {cid} · {} node(s) · {:.2} cohesion\n\n",
        members.len(),
        crate::cluster::cohesion(graph, members)
    ));
    out.push_str("## Members\n\n");
    for id in members {
        let Some(node) = graph.node(id) else {
            continue;
        };
        let link = link_for(node.kind, &node.label);
        out.push_str(&format!(
            "- {link} — `{}` · {} connection(s)\n",
            kind_label(node.kind),
            graph.degree(id)
        ));
    }
    out.push('\n');

    let inside: BTreeSet<&str> = members.iter().map(String::as_str).collect();
    let mut outside: Vec<String> = Vec::new();
    for edge in &graph.edges {
        let (Some(source), Some(target)) = (graph.node(&edge.source), graph.node(&edge.target))
        else {
            continue;
        };
        let crosses =
            inside.contains(edge.source.as_str()) != inside.contains(edge.target.as_str());
        if !crosses {
            continue;
        }
        let relation = format!(
            "{} -> {} ({}, {})",
            source.label,
            target.label,
            relation_label(edge.relation),
            confidence_label(edge.confidence)
        );
        if !outside.contains(&relation) {
            outside.push(relation);
        }
    }
    if !outside.is_empty() {
        outside.sort();
        outside.truncate(20);
        out.push_str("## Cross-community links\n\n");
        for line in outside {
            out.push_str(&format!("- {line}\n"));
        }
        out.push('\n');
    }

    out
}

fn hub_article(
    graph: &NoteGraph,
    communities: &BTreeMap<usize, Vec<String>>,
    labels: &BTreeMap<usize, String>,
    god: &crate::analyze::GodNode,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {}\n\n", heading(&god.label)));
    out.push_str(&format!(
        "Hub · `{}` · {} connection(s)\n\n",
        kind_label(god.kind),
        god.degree
    ));
    out.push_str("## Neighbours\n\n");
    let mut neighbours: Vec<&str> = graph.neighbours(&god.id);
    neighbours.sort();
    for id in neighbours {
        if let Some(node) = graph.node(id) {
            out.push_str(&format!("- {}\n", link_for(node.kind, &node.label)));
        }
    }
    out.push('\n');

    let mut seen: BTreeSet<usize> = BTreeSet::new();
    for edge in &graph.edges {
        let other = if edge.source == god.id {
            &edge.target
        } else if edge.target == god.id {
            &edge.source
        } else {
            continue;
        };
        for (cid, members) in communities {
            if members.contains(other) && seen.insert(*cid) {
                let label = labels
                    .get(cid)
                    .cloned()
                    .unwrap_or_else(|| format!("Community {cid}"));
                out.push_str(&format!("- Community: {}\n", heading(&label)));
            }
        }
    }
    out
}

fn index_article(
    graph: &NoteGraph,
    analysis: &Analysis,
    community_files: &BTreeMap<usize, String>,
) -> String {
    let mut out = String::new();
    out.push_str("# Notes graph\n\n");
    out.push_str(&format!(
        "{} notes · {} tags · {} concepts · {} edges · {} dangling link(s)\n\n",
        analysis.notes, analysis.tags, analysis.concepts, analysis.edges, analysis.dangling
    ));

    out.push_str("## Communities\n\n");
    for summary in &analysis.communities {
        let file = community_files
            .get(&summary.id)
            .cloned()
            .unwrap_or_else(|| format!("community-{}", summary.id));
        out.push_str(&format!(
            "- [{}]({}.md) — {} node(s), {:.2} cohesion\n",
            heading(&summary.label),
            file,
            summary.size,
            summary.cohesion
        ));
    }
    out.push('\n');

    if !analysis.god_nodes.is_empty() {
        out.push_str("## Hubs\n\n");
        for god in &analysis.god_nodes {
            out.push_str(&format!(
                "- {} — `{}` · {} connection(s)\n",
                heading(&god.label),
                kind_label(god.kind),
                god.degree
            ));
        }
        out.push('\n');
    }

    if !analysis.surprises.is_empty() {
        out.push_str("## Surprising connections\n\n");
        for surprise in &analysis.surprises {
            out.push_str(&format!(
                "- {} -> {} ({}, {})\n",
                heading(&surprise.source_label),
                heading(&surprise.target_label),
                relation_label(surprise.relation),
                confidence_label(surprise.confidence)
            ));
        }
        out.push('\n');
    }

    if !graph.dangling.is_empty() {
        out.push_str("## Broken links\n\n");
        for target in &graph.dangling {
            out.push_str(&format!("- `{target}`\n"));
        }
        out.push('\n');
    }

    out
}

fn heading(label: &str) -> String {
    label
        .replace(['[', ']', '|'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn link_for(kind: NodeKind, label: &str) -> String {
    match kind {
        NodeKind::Note => format!("[[{}]]", heading(label)),
        _ => format!("`{}`", heading(label)),
    }
}

fn relation_label(relation: crate::model::Relation) -> &'static str {
    match relation {
        crate::model::Relation::Contains => "contains",
        crate::model::Relation::References => "references",
        crate::model::Relation::Tagged => "tagged",
        crate::model::Relation::Mentions => "mentions",
        crate::model::Relation::PartOf => "part_of",
    }
}

/// Write only when the bytes differ. Re-exporting an unchanged graph should not
/// churn mtimes and trigger watchers.
fn write_changed(path: &Path, content: &str) -> Result<(), HarnessError> {
    match std::fs::read_to_string(path) {
        Ok(existing) if existing == content => return Ok(()),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(HarnessError::Io(format!(
                "cannot read '{}': {error}",
                path.display()
            )))
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            HarnessError::Io(format!("cannot create '{}': {error}", parent.display()))
        })?;
    }
    std::fs::write(path, content)
        .map_err(|error| HarnessError::Io(format!("cannot write '{}': {error}", path.display())))
}

/// Default output directory next to the notes folder.
pub fn default_output(notes_dir: &Path) -> PathBuf {
    notes_dir.join("graph")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{note_id, Confidence, Edge, Node, Relation};

    fn fixture() -> (
        NoteGraph,
        BTreeMap<usize, Vec<String>>,
        BTreeMap<usize, String>,
    ) {
        let mut graph = NoteGraph::default();
        for id in ["alpha", "beta"] {
            graph
                .nodes
                .insert(note_id(id), Node::new(note_id(id), id, NodeKind::Note));
        }
        graph.edges.push(Edge::new(
            note_id("alpha"),
            note_id("beta"),
            Relation::References,
            Confidence::Extracted,
        ));
        let mut communities = BTreeMap::new();
        communities.insert(0, vec![note_id("alpha"), note_id("beta")]);
        let mut labels = BTreeMap::new();
        labels.insert(0, "alpha".to_string());
        (graph, communities, labels)
    }

    #[test]
    fn json_has_node_link_shape() {
        let (graph, communities, labels) = fixture();
        let value = graph_json(&graph, &communities, &labels);
        assert_eq!(value["directed"], false);
        assert_eq!(value["nodes"].as_array().map(Vec::len), Some(2));
        assert_eq!(value["links"].as_array().map(Vec::len), Some(1));
        assert_eq!(value["links"][0]["confidence"], "EXTRACTED");
        assert_eq!(value["nodes"][0]["community"], 0);
    }

    #[test]
    fn wiki_writes_an_index_and_community_article() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (graph, communities, labels) = fixture();
        let analysis = crate::analyze::analyze(&graph, &communities, &labels);
        let written = to_wiki(&graph, &communities, &labels, &analysis, dir.path()).expect("wiki");
        assert_eq!(written, 1);
        let index = std::fs::read_to_string(dir.path().join("index.md")).expect("index");
        assert!(index.contains("Notes graph"));
        assert!(index.contains("alpha.md"));
        let article = std::fs::read_to_string(dir.path().join("alpha.md")).expect("article");
        assert!(article.contains("[[alpha]]"));
        assert!(article.contains("[[beta]]"));
    }

    #[test]
    fn writing_twice_is_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (graph, communities, labels) = fixture();
        let analysis = crate::analyze::analyze(&graph, &communities, &labels);
        to_wiki(&graph, &communities, &labels, &analysis, dir.path()).expect("first");
        let before = std::fs::metadata(dir.path().join("index.md")).and_then(|m| m.modified());
        to_wiki(&graph, &communities, &labels, &analysis, dir.path()).expect("second");
        let after = std::fs::metadata(dir.path().join("index.md")).and_then(|m| m.modified());
        assert_eq!(before.ok(), after.ok());
    }
}
