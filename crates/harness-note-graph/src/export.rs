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

/// Palette shared by the HTML and SVG exports. The order is stable so a
/// community keeps its colour between the two files and across rebuilds.
pub const COMMUNITY_COLORS: &[&str] = &[
    "#4e79a7", "#f28e2b", "#e15759", "#76b7b2", "#59a14f", "#edc948", "#b07aa1", "#ff9da7",
    "#9c755f", "#bab0ac", "#86bcb6", "#d37295",
];

/// The fill a community is drawn with. Communities past the palette wrap
/// around it, so the colour is a quick cue rather than a unique identifier.
pub fn community_color(cid: usize) -> &'static str {
    COMMUNITY_COLORS[cid % COMMUNITY_COLORS.len()]
}

/// Radius in the static SVG. Notes are the anchors, tags and headings are
/// texture, and a missing link is drawn large because it is a problem to fix.
fn svg_radius(kind: NodeKind) -> f32 {
    match kind {
        NodeKind::Note => 6.0,
        NodeKind::Heading => 2.6,
        NodeKind::Tag => 3.2,
        NodeKind::Concept => 4.2,
        NodeKind::File => 3.0,
        NodeKind::Symbol => 3.0,
        NodeKind::Missing => 5.0,
    }
}

fn node_community_map(communities: &BTreeMap<usize, Vec<String>>) -> BTreeMap<&str, usize> {
    communities
        .iter()
        .flat_map(|(cid, members)| members.iter().map(move |id| (id.as_str(), *cid)))
        .collect()
}

/// A deterministic force-directed layout for the static SVG.
///
/// The simulation is seeded from a circle and run a bounded number of times,
/// so the same graph always yields the same coordinates. Very large graphs fall
/// back to a circle rather than paying a quadratic cost the exporter cannot
/// justify for a file nobody will read at that size.
fn svg_layout(graph: &NoteGraph) -> BTreeMap<String, (f32, f32)> {
    let ids: Vec<&str> = graph.nodes.keys().map(String::as_str).collect();
    let n = ids.len();
    if n == 0 {
        return BTreeMap::new();
    }

    let width = 1_200.0f32;
    let height = 900.0f32;
    if n > 700 {
        let mut placed = BTreeMap::new();
        let radius = (width.min(height) / 2.0) - 60.0;
        for (index, id) in ids.iter().enumerate() {
            let angle = std::f32::consts::TAU * index as f32 / n as f32;
            placed.insert(
                (*id).to_string(),
                (
                    width / 2.0 + radius * angle.cos(),
                    height / 2.0 + radius * angle.sin(),
                ),
            );
        }
        return placed;
    }

    let index: BTreeMap<&str, usize> = ids
        .iter()
        .enumerate()
        .map(|(position, id)| (*id, position))
        .collect();
    let mut positions: Vec<(f32, f32)> = (0..n)
        .map(|position| {
            let angle = std::f32::consts::TAU * position as f32 / n as f32;
            (
                width / 2.0 + 300.0 * angle.cos(),
                height / 2.0 + 300.0 * angle.sin(),
            )
        })
        .collect();
    let links: Vec<(usize, usize)> = graph
        .edges
        .iter()
        .filter_map(|edge| {
            let source = *index.get(edge.source.as_str())?;
            let target = *index.get(edge.target.as_str())?;
            (source != target).then_some((source, target))
        })
        .collect();

    let area = width * height;
    let k = (area / n as f32).sqrt();
    let iterations = if n > 300 { 120 } else { 300 };
    let mut temperature = width / 10.0;
    for _ in 0..iterations {
        let mut displacement = vec![(0.0f32, 0.0f32); n];

        // Repulsion between every pair.
        for left in 0..n {
            for right in (left + 1)..n {
                let dx = positions[left].0 - positions[right].0;
                let dy = positions[left].1 - positions[right].1;
                let distance = (dx * dx + dy * dy).sqrt().max(0.01);
                let force = k * k / distance;
                let ux = dx / distance;
                let uy = dy / distance;
                displacement[left].0 += ux * force;
                displacement[left].1 += uy * force;
                displacement[right].0 -= ux * force;
                displacement[right].1 -= uy * force;
            }
        }

        // Springs along the edges.
        for (source, target) in &links {
            let dx = positions[*source].0 - positions[*target].0;
            let dy = positions[*source].1 - positions[*target].1;
            let distance = (dx * dx + dy * dy).sqrt().max(0.01);
            let force = distance * distance / k;
            let ux = dx / distance;
            let uy = dy / distance;
            displacement[*source].0 -= ux * force;
            displacement[*source].1 -= uy * force;
            displacement[*target].0 += ux * force;
            displacement[*target].1 += uy * force;
        }

        for position in 0..n {
            // Gravity keeps disconnected islands on the canvas.
            displacement[position].0 += (width / 2.0 - positions[position].0) * 0.02;
            displacement[position].1 += (height / 2.0 - positions[position].1) * 0.02;
            let dx = displacement[position].0;
            let dy = displacement[position].1;
            let distance = (dx * dx + dy * dy).sqrt().max(0.01);
            let step = distance.min(temperature);
            positions[position].0 += dx / distance * step;
            positions[position].1 += dy / distance * step;
            positions[position].0 = positions[position].0.clamp(40.0, width - 40.0);
            positions[position].1 = positions[position].1.clamp(40.0, height - 40.0);
        }
        temperature *= 0.95;
    }

    ids.iter()
        .enumerate()
        .map(|(position, id)| ((*id).to_string(), positions[position]))
        .collect()
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn truncate_label(label: &str, max: usize) -> String {
    let mut out: String = label.chars().take(max).collect();
    if label.chars().count() > max {
        out.push('…');
    }
    out
}

/// Render the graph as a standalone SVG.
///
/// Edges are drawn first so nodes sit on top of them. Labels are limited to
/// notes and well-connected hubs: a label on every tag would turn the picture
/// into noise. Colours follow [`COMMUNITY_COLORS`], so the SVG and the HTML
/// view agree.
pub fn graph_svg(
    graph: &NoteGraph,
    communities: &BTreeMap<usize, Vec<String>>,
    labels: &BTreeMap<usize, String>,
) -> String {
    let width = 1_200.0f32;
    let height = 900.0f32;
    let positions = svg_layout(graph);
    let node_community = node_community_map(communities);

    let mut out = String::with_capacity(graph.nodes.len() * 120 + graph.edges.len() * 120);
    out.push_str(&format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" width="{width}" height="{height}" font-family="ui-sans-serif, system-ui, sans-serif">
<rect width="100%" height="100%" fill="#fbfbfd"/>
<text x="24" y="34" font-size="16" font-weight="600" fill="#1f2430">Notes graph</text>
<text x="24" y="54" font-size="11" fill="#6b7280">{} notes · {} edges · {} communities</text>
"##,
        graph.nodes.len(),
        graph.edges.len(),
        communities.len()
    ));

    for edge in &graph.edges {
        let (Some(source), Some(target)) =
            (positions.get(&edge.source), positions.get(&edge.target))
        else {
            continue;
        };
        let opacity = match edge.confidence {
            Confidence::Extracted => 0.55,
            Confidence::Inferred => 0.32,
            Confidence::Ambiguous => 0.2,
        };
        out.push_str(&format!(
            r##"<line x1="{:.1}" y1="{:.1}" x2="{:.1}" y2="{:.1}" stroke="#9aa3b2" stroke-opacity="{opacity}" stroke-width="0.8"/>"##,
            source.0, source.1, target.0, target.1
        ));
        out.push('\n');
    }

    for node in graph.nodes.values() {
        let Some(position) = positions.get(&node.id) else {
            continue;
        };
        let cid = node_community.get(node.id.as_str()).copied();
        let color = cid.map(community_color).unwrap_or("#8b93a3");
        let radius = svg_radius(node.kind);
        let degree = graph.degree(&node.id);
        out.push_str(&format!(
            r##"<circle cx="{:.1}" cy="{:.1}" r="{radius:.1}" fill="{color}" fill-opacity="0.85" stroke="#ffffff" stroke-width="0.9"><title>{} — {} · {} connection(s)</title></circle>"##,
            position.0,
            position.1,
            xml_escape(&node.label),
            kind_label(node.kind),
            degree
        ));
        out.push('\n');

        let label_wanted = node.kind == NodeKind::Note || degree >= 6;
        if label_wanted && !node.label.trim().is_empty() {
            out.push_str(&format!(
                r##"<text x="{:.1}" y="{:.1}" font-size="9" fill="#2b3240" text-anchor="middle">{}</text>"##,
                position.0,
                position.1 + radius + 10.0,
                xml_escape(&truncate_label(&node.label, 32))
            ));
            out.push('\n');
        }
    }

    // Community legend, bounded so a graph with a hundred tiny components does
    // not fill the corner.
    let mut legend: Vec<(usize, &str, usize)> = communities
        .iter()
        .map(|(cid, members)| {
            let name = labels.get(cid).map(String::as_str).unwrap_or("community");
            (*cid, name, members.len())
        })
        .collect();
    legend.sort_by(|left, right| right.2.cmp(&left.2).then(left.0.cmp(&right.0)));
    legend.truncate(9);
    if !legend.is_empty() {
        let legend_x = width - 300.0;
        let mut legend_y = 90.0;
        out.push_str(&format!(
            r##"<text x="{legend_x}" y="74" font-size="11" font-weight="600" fill="#1f2430">Communities</text>"##
        ));
        out.push('\n');
        for (cid, name, size) in legend {
            out.push_str(&format!(
                r##"<rect x="{legend_x}" y="{:.1}" width="10" height="10" rx="2" fill="{}"/><text x="{:.1}" y="{:.1}" font-size="10" fill="#3b4250">{} ({size})</text>"##,
                legend_y - 9.0,
                community_color(cid),
                legend_x + 16.0,
                legend_y,
                xml_escape(&truncate_label(name, 34))
            ));
            out.push('\n');
            legend_y += 18.0;
        }
    }

    out.push_str("</svg>\n");
    out
}

/// Write `<path>` as an SVG, leaving an identical file untouched.
pub fn to_svg(
    graph: &NoteGraph,
    communities: &BTreeMap<usize, Vec<String>>,
    labels: &BTreeMap<usize, String>,
    path: &Path,
) -> Result<(), HarnessError> {
    write_changed(path, &graph_svg(graph, communities, labels))
}

/// The interactive export: one HTML file with the graph data embedded and a
/// small canvas renderer, so the notes can be browsed by opening a file. No
/// network access and no build step: the artifacts stay local and this crate
/// gains no dependency.
pub fn graph_html(
    graph: &NoteGraph,
    communities: &BTreeMap<usize, Vec<String>>,
    labels: &BTreeMap<usize, String>,
) -> String {
    let value = graph_json(graph, communities, labels);
    let data = serde_json::to_string(&value)
        .unwrap_or_else(|_| "{}".to_string())
        // A label containing `</script>` must not close the embedding tag.
        .replace("</", "<\\/");
    HTML_TEMPLATE
        .replace("__GRAPH_DATA__", &data)
        .replace("__COMMUNITY_COLORS__", &colors_json())
        .replace("__NOTE_COUNT__", &graph.nodes.len().to_string())
        .replace("__EDGE_COUNT__", &graph.edges.len().to_string())
}

/// The palette as a JSON array, so the page and the SVG cannot drift apart.
fn colors_json() -> String {
    let mut out = String::from("[");
    for (index, color) in COMMUNITY_COLORS.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(color);
        out.push('"');
    }
    out.push(']');
    out
}

/// Write `<path>` as the interactive HTML page, leaving an identical file alone.
pub fn to_html(
    graph: &NoteGraph,
    communities: &BTreeMap<usize, Vec<String>>,
    labels: &BTreeMap<usize, String>,
    path: &Path,
) -> Result<(), HarnessError> {
    write_changed(path, &graph_html(graph, communities, labels))
}

/// The self-contained page. Kept as a raw string so the CSS/JS braces need no
/// escaping; `__GRAPH_DATA__` is replaced with the serialized node-link JSON,
/// `__COMMUNITY_COLORS__` with [`COMMUNITY_COLORS`] as a JSON array, and the
/// two count placeholders with the graph totals.
const HTML_TEMPLATE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Notes graph</title>
<style>
  :root { color-scheme: light; }
  * { box-sizing: border-box; }
  html, body { margin: 0; height: 100%; }
  body {
    font: 13px/1.4 ui-sans-serif, system-ui, -apple-system, "Segoe UI", sans-serif;
    color: #1f2430; background: #fbfbfd; overflow: hidden;
  }
  #stage { position: fixed; inset: 0; cursor: grab; }
  #stage.dragging { cursor: grabbing; }
  canvas { display: block; }

  .panel {
    position: fixed; background: rgba(255,255,255,.92); border: 1px solid #e1e4ea;
    border-radius: 10px; box-shadow: 0 4px 18px rgba(20,25,40,.08);
    backdrop-filter: blur(6px);
  }
  #head { top: 16px; left: 16px; padding: 12px 14px; min-width: 240px; }
  #head h1 { margin: 0 0 2px; font-size: 15px; font-weight: 650; letter-spacing: -.01em; }
  #head p { margin: 0 0 10px; color: #6b7280; font-size: 11px; }
  #search {
    width: 100%; padding: 7px 9px; border: 1px solid #d8dce4; border-radius: 7px;
    font: inherit; font-size: 12px; outline: none; background: #fff;
  }
  #search:focus { border-color: #7c9cc4; box-shadow: 0 0 0 3px rgba(124,156,196,.18); }

  #legend { bottom: 16px; left: 16px; padding: 10px 12px; max-width: 320px; }
  #legend h2 { margin: 0 0 6px; font-size: 10px; text-transform: uppercase; letter-spacing: .08em; color: #6b7280; }
  .legend-row { display: flex; align-items: center; gap: 7px; padding: 1px 0; font-size: 11px; color: #3b4250; cursor: pointer; }
  .legend-row:hover { color: #111827; }
  .dot { width: 9px; height: 9px; border-radius: 50%; flex: none; }
  .count { color: #9aa1b0; margin-left: auto; }

  #tip {
    position: fixed; pointer-events: none; z-index: 10; max-width: 320px;
    padding: 7px 9px; border-radius: 7px; background: #1f2430; color: #fff;
    font-size: 11px; line-height: 1.35; opacity: 0; transition: opacity .08s;
  }
  #tip strong { display: block; font-size: 12px; margin-bottom: 1px; }
  #tip span { color: #b9c0cd; }
  #hint { position: fixed; right: 16px; bottom: 16px; color: #8b93a3; font-size: 11px; }
</style>
</head>
<body>
<div id="stage"><canvas id="canvas"></canvas></div>

<div id="head" class="panel">
  <h1>Notes graph</h1>
  <p>__NOTE_COUNT__ nodes · __EDGE_COUNT__ edges</p>
  <input id="search" type="search" placeholder="Filter by name, tag or kind…" autocomplete="off" spellcheck="false">
</div>

<div id="legend" class="panel"><h2>Communities</h2><div id="legend-rows"></div></div>
<div id="tip"></div>
<div id="hint">scroll to zoom · drag to pan · click a node to focus</div>

<script id="graph-data" type="application/json">__GRAPH_DATA__</script>
<script>
(() => {
  "use strict";
  const DATA = JSON.parse(document.getElementById("graph-data").textContent);
  const COLORS = __COMMUNITY_COLORS__;
  const KIND_RADIUS = { note: 7, heading: 3, tag: 3.5, concept: 5, file: 3.5, symbol: 3.5, missing: 6 };

  const canvas = document.getElementById("canvas");
  const stage = document.getElementById("stage");
  const ctx = canvas.getContext("2d");
  const tip = document.getElementById("tip");
  const search = document.getElementById("search");

  const nodes = DATA.nodes.map((node, index) => ({
    ...node,
    x: Math.cos(index * 2.399963) * 240 + (Math.random() - 0.5) * 40,
    y: Math.sin(index * 2.399963) * 240 + (Math.random() - 0.5) * 40,
    vx: 0, vy: 0,
    degree: 0,
    r: KIND_RADIUS[node.kind] || 4,
    color: COLORS[(node.community ?? 0) % COLORS.length],
  }));
  const byId = new Map(nodes.map(node => [node.id, node]));
  const links = DATA.links
    .map(link => ({ source: byId.get(link.source), target: byId.get(link.target), confidence: link.confidence }))
    .filter(link => link.source && link.target && link.source !== link.target);
  const adjacency = new Map(nodes.map(node => [node.id, new Set()]));
  for (const link of links) {
    link.source.degree++;
    link.target.degree++;
    adjacency.get(link.source.id).add(link.target.id);
    adjacency.get(link.target.id).add(link.source.id);
  }

  let width = 0, height = 0, dpr = 1;
  let scale = 1, offsetX = 0, offsetY = 0;
  let hovered = null, selected = null, dragging = null;
  let panning = false, panStart = null, moved = false;
  let query = "";
  let alpha = 0.35;

  function resize() {
    dpr = window.devicePixelRatio || 1;
    width = window.innerWidth;
    height = window.innerHeight;
    canvas.width = Math.round(width * dpr);
    canvas.height = Math.round(height * dpr);
    canvas.style.width = width + "px";
    canvas.style.height = height + "px";
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  }

  function matches(node) {
    if (!query) return false;
    const haystack = (node.label + " " + node.kind + " " + (node.community_name || "")).toLowerCase();
    return haystack.includes(query);
  }

  function alphaFor(node) {
    if (query) return matches(node) ? 1 : 0.12;
    if (selected) return selected === node.id || adjacency.get(selected).has(node.id) ? 1 : 0.14;
    return 1;
  }

  // Force simulation: repulsion between all nodes, springs along links,
  // and a light pull to the centre. Runs until it settles, then idles.
  function simulate(alpha) {
    const count = nodes.length;
    const repulsion = 1400;
    const spring = 0.012;
    const ideal = 90;

    for (let i = 0; i < count; i++) {
      const a = nodes[i];
      for (let j = i + 1; j < count; j++) {
        const b = nodes[j];
        let dx = a.x - b.x, dy = a.y - b.y;
        let dist2 = dx * dx + dy * dy;
        if (dist2 < 1) { dist2 = 1; dx = 0.5; dy = 0.5; }
        if (dist2 > 90000) continue;
        const dist = Math.sqrt(dist2);
        const force = (repulsion / dist2) * alpha;
        const ux = dx / dist, uy = dy / dist;
        a.vx += ux * force; a.vy += uy * force;
        b.vx -= ux * force; b.vy -= uy * force;
      }
    }
    for (const link of links) {
      const a = link.source, b = link.target;
      const dx = b.x - a.x, dy = b.y - a.y;
      const dist = Math.max(1, Math.sqrt(dx * dx + dy * dy));
      const force = (dist - ideal) * spring * alpha;
      const ux = dx / dist, uy = dy / dist;
      a.vx += ux * force; a.vy += uy * force;
      b.vx -= ux * force; b.vy -= uy * force;
    }
    for (const node of nodes) {
      if (node === dragging) continue;
      node.vx -= node.x * 0.0009 * alpha;
      node.vy -= node.y * 0.0009 * alpha;
      node.vx *= 0.86; node.vy *= 0.86;
      node.x += node.vx; node.y += node.vy;
    }
  }

  function toWorld(px, py) {
    return { x: (px - width / 2 - offsetX) / scale, y: (py - height / 2 - offsetY) / scale };
  }

  function draw() {
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, width, height);
    ctx.save();
    ctx.translate(width / 2 + offsetX, height / 2 + offsetY);
    ctx.scale(scale, scale);

    ctx.lineWidth = 1 / scale;
    for (const link of links) {
      const a = link.source, b = link.target;
      const active = !selected || selected === a.id || selected === b.id;
      const searchHit = query && (matches(a) || matches(b));
      let opacity = link.confidence === "EXTRACTED" ? 0.5 : link.confidence === "INFERRED" ? 0.3 : 0.18;
      if (selected) opacity = active ? 0.65 : 0.05;
      if (query) opacity = searchHit ? 0.7 : 0.04;
      ctx.strokeStyle = "rgba(120,132,150," + opacity + ")";
      ctx.beginPath();
      ctx.moveTo(a.x, a.y);
      ctx.lineTo(b.x, b.y);
      ctx.stroke();
    }

    for (const node of nodes) {
      const alpha = alphaFor(node);
      ctx.globalAlpha = alpha;
      ctx.beginPath();
      ctx.arc(node.x, node.y, node.r, 0, Math.PI * 2);
      ctx.fillStyle = node.color;
      ctx.fill();
      if (node === hovered || node.id === selected) {
        ctx.lineWidth = 2 / scale;
        ctx.strokeStyle = "#1f2430";
        ctx.stroke();
      }
      const labelled = node.kind === "note" || node.degree >= 6 || matches(node);
      if (labelled) {
        ctx.globalAlpha = Math.min(1, alpha + 0.05);
        ctx.fillStyle = "#2b3240";
        ctx.font = `${10 / scale}px ui-sans-serif, system-ui, sans-serif`;
        ctx.textAlign = "center";
        ctx.textBaseline = "top";
        const label = node.label.length > 30 ? node.label.slice(0, 29) + "…" : node.label;
        ctx.fillText(label, node.x, node.y + node.r + 3 / scale);
      }
    }
    ctx.globalAlpha = 1;
    ctx.restore();
  }

  function animate() {
    simulate(alpha);
    alpha *= 0.99;
    draw();
    // Cool down instead of blocking the first paint on a long loop: big
    // graphs stay responsive and the layout settles a few frames in.
    if (alpha > 0.02) {
      requestAnimationFrame(animate);
    }
  }

  function nodeAt(px, py) {
    const point = toWorld(px, py);
    let best = null, bestDist = Infinity;
    for (const node of nodes) {
      const dx = node.x - point.x, dy = node.y - point.y;
      const dist = Math.sqrt(dx * dx + dy * dy);
      if (dist < node.r + 6 / scale && dist < bestDist) { best = node; bestDist = dist; }
    }
    return best;
  }

  function showTip(node, px, py) {
    if (!node) { tip.style.opacity = "0"; return; }
    tip.innerHTML = "<strong>" + escapeHtml(node.label) + "</strong>" +
      "<span>" + escapeHtml(node.kind) + (node.community_name ? " · " + escapeHtml(node.community_name) : "") +
      " · " + node.degree + " connection(s)</span>";
    tip.style.left = Math.min(px + 14, width - 340) + "px";
    tip.style.top = Math.min(py + 14, height - 90) + "px";
    tip.style.opacity = "1";
  }

  function escapeHtml(text) {
    return String(text).replace(/[&<>"']/g, ch => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[ch]));
  }

  function renderLegend() {
    const groups = new Map();
    for (const node of nodes) {
      const key = node.community ?? 0;
      const entry = groups.get(key) || { name: node.community_name || "community " + key, count: 0, color: node.color };
      entry.count++;
      groups.set(key, entry);
    }
    const rows = [...groups.entries()].sort((a, b) => b[1].count - a[1].count).slice(0, 10);
    const host = document.getElementById("legend-rows");
    host.textContent = "";
    for (const [key, entry] of rows) {
      const row = document.createElement("div");
      row.className = "legend-row";
      row.dataset.community = key;
      row.innerHTML = '<span class="dot" style="background:' + entry.color + '"></span>' +
        "<span>" + escapeHtml(entry.name) + '</span><span class="count">' + entry.count + "</span>";
      row.addEventListener("click", () => {
        const inGroup = nodes.filter(node => (node.community ?? 0) === key);
        selected = selected && inGroup.some(node => node.id === selected) ? null : (inGroup[0] ? inGroup[0].id : null);
        if (selected) { offsetX = -inGroup[0].x * scale; offsetY = -inGroup[0].y * scale; }
        draw();
      });
      host.appendChild(row);
    }
  }

  canvas.addEventListener("mousemove", event => {
    if (panning && panStart) {
      offsetX = panStart.ox + (event.clientX - panStart.x);
      offsetY = panStart.oy + (event.clientY - panStart.y);
      moved = true;
      draw();
      return;
    }
    if (dragging) {
      const point = toWorld(event.clientX, event.clientY);
      dragging.x = point.x;
      dragging.y = point.y;
      moved = true;
      draw();
      return;
    }
    const node = nodeAt(event.clientX, event.clientY);
    hovered = node;
    showTip(node, event.clientX, event.clientY);
    stage.style.cursor = node ? "pointer" : "grab";
    draw();
  });

  canvas.addEventListener("mousedown", event => {
    const node = nodeAt(event.clientX, event.clientY);
    moved = false;
    if (node) {
      dragging = node;
    } else {
      panning = true;
      panStart = { x: event.clientX, y: event.clientY, ox: offsetX, oy: offsetY };
      stage.classList.add("dragging");
    }
  });

  window.addEventListener("mouseup", event => {
    if (dragging) { dragging = null; }
    if (panning) {
      panning = false;
      stage.classList.remove("dragging");
    }
  });

  canvas.addEventListener("mouseleave", () => {
    hovered = null;
    showTip(null);
    draw();
  });

  canvas.addEventListener("click", event => {
    // A pan or a node drag ends in a click; do not treat it as a selection.
    if (moved) { moved = false; return; }
    const node = nodeAt(event.clientX, event.clientY);
    selected = node ? (selected === node.id ? null : node.id) : null;
    draw();
  });

  canvas.addEventListener("wheel", event => {
    event.preventDefault();
    const before = toWorld(event.clientX, event.clientY);
    const factor = Math.exp(-event.deltaY * 0.0012);
    scale = Math.min(4, Math.max(0.25, scale * factor));
    const after = toWorld(event.clientX, event.clientY);
    offsetX += (after.x - before.x) * scale;
    offsetY += (after.y - before.y) * scale;
    draw();
  }, { passive: false });

  search.addEventListener("input", () => {
    query = search.value.trim().toLowerCase();
    draw();
  });

  window.addEventListener("resize", () => { resize(); draw(); });

  resize();
  renderLegend();
  animate();
})();
</script>
</body>
</html>
"##;

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
    fn svg_draws_edges_and_community_coloured_nodes() {
        let (graph, communities, labels) = fixture();
        let svg = graph_svg(&graph, &communities, &labels);
        assert!(svg.starts_with("<svg"));
        assert!(svg.ends_with("</svg>\n"));
        assert!(svg.contains("<line "));
        assert_eq!(svg.matches("<circle ").count(), 2);
        assert!(svg.contains(COMMUNITY_COLORS[0]));
        // Notes get a label, so the picture is readable without a tooltip.
        assert!(svg.contains(">alpha</text>"));
        assert!(svg.contains(">beta</text>"));
    }

    #[test]
    fn svg_escapes_labels() {
        let mut graph = NoteGraph::default();
        let id = note_id("risky");
        graph.nodes.insert(
            id.clone(),
            Node::new(id.clone(), "a & b <c>", NodeKind::Note),
        );
        let svg = graph_svg(&graph, &BTreeMap::new(), &BTreeMap::new());
        assert!(svg.contains("a &amp; b &lt;c&gt;"));
        assert!(!svg.contains("a & b <c>"));
    }

    #[test]
    fn html_embeds_the_graph_and_survives_script_labels() {
        let mut graph = NoteGraph::default();
        let id = note_id("risky");
        graph.nodes.insert(
            id.clone(),
            Node::new(id.clone(), "closing </script> tag", NodeKind::Note),
        );
        let html = graph_html(&graph, &BTreeMap::new(), &BTreeMap::new());
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("const DATA = JSON.parse"));
        // One closing tag for the data block, one for the renderer.
        assert_eq!(html.matches("</script>").count(), 2);
        assert!(html.contains("<\\/script>"));
        // Placeholders are gone, so a broken replace cannot ship silently.
        assert!(!html.contains("__GRAPH_DATA__"));
        assert!(!html.contains("__COMMUNITY_COLORS__"));
    }

    #[test]
    fn visual_exports_are_idempotent() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (graph, communities, labels) = fixture();
        to_html(
            &graph,
            &communities,
            &labels,
            &dir.path().join("graph.html"),
        )
        .expect("html");
        to_svg(&graph, &communities, &labels, &dir.path().join("graph.svg")).expect("svg");
        let html = std::fs::read_to_string(dir.path().join("graph.html")).expect("html read");
        to_html(
            &graph,
            &communities,
            &labels,
            &dir.path().join("graph.html"),
        )
        .expect("html again");
        let again = std::fs::read_to_string(dir.path().join("graph.html")).expect("html read");
        assert_eq!(html, again);
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
