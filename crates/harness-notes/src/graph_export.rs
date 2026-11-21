//! Writing a project's knowledge-graph artifacts without the CLI.
//!
//! `luna notes graph` is not the only writer: the background saver keeps a project's graph in
//! step after it writes a note, so the artifacts exist the first time a project is mentioned and
//! stay current afterwards. Both callers use the same export functions from `harness-note-graph`;
//! this module only decides which files a project folder gets.

use std::path::Path;

use harness_core::{HarnessError, Note};
use harness_note_graph::{export, graph_notes};

/// The files [`write`] produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenGraph {
    pub graph: std::path::PathBuf,
    pub html: std::path::PathBuf,
    pub svg: std::path::PathBuf,
    pub wiki: std::path::PathBuf,
    pub nodes: usize,
    pub edges: usize,
}

/// Build `notes` into `out_dir`: `graph.json`, the interactive HTML, the SVG, and the wiki.
///
/// The folder is created when missing. Existing artifacts are replaced, so the graph always
/// describes the notes as they are now.
pub fn write(notes: &[Note], out_dir: &Path) -> Result<WrittenGraph, HarnessError> {
    std::fs::create_dir_all(out_dir).map_err(|error| {
        HarnessError::Io(format!(
            "cannot create graph directory '{}': {error}",
            out_dir.display()
        ))
    })?;

    let built = graph_notes(notes);
    let graph = out_dir.join("graph.json");
    let html = out_dir.join("graph.html");
    let svg = out_dir.join("graph.svg");
    let wiki = out_dir.join("wiki");

    export::to_json(&built.graph, &built.communities, &built.labels, &graph)?;
    export::to_html(&built.graph, &built.communities, &built.labels, &html)?;
    export::to_svg(&built.graph, &built.communities, &built.labels, &svg)?;
    export::to_wiki(
        &built.graph,
        &built.communities,
        &built.labels,
        &built.analysis,
        &wiki,
    )?;

    Ok(WrittenGraph {
        graph,
        html,
        svg,
        wiki,
        nodes: built.graph.nodes.len(),
        edges: built.graph.edges.len(),
    })
}
