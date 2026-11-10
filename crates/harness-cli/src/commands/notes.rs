//! `luna notes` — build a knowledge graph over the Markdown notes store.
//!
//! The graph itself lives in `harness-note-graph`; this command reads the notes
//! through the configured engine, writes the artifacts, and renders the report.
//! All user-visible prose belongs here, per `agents.md` §1.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use harness_core::config::Config;
use harness_core::{HarnessError, NotesEngine};
use harness_note_graph::{
    analyze::{QuestionKind, SurpriseReason},
    export, graph_notes, validate, BuiltGraph,
};
use harness_notes::FileNotesEngine;

/// Default folder name under `paths.notes_dir` for generated artifacts.
const OUTPUT_DIR_NAME: &str = "graph";

/// Report file written beside `graph.json`.
const REPORT_FILE: &str = "GRAPH_REPORT.md";

#[derive(Debug, Subcommand)]
pub enum NotesCommand {
    /// Build the notes knowledge graph and write graph.json, the wiki and a report
    Graph {
        /// Output directory (default: `<notes_dir>/graph`)
        #[arg(long)]
        out: Option<PathBuf>,
        /// One JSON summary on stdout
        #[arg(long)]
        json: bool,
        /// Skip the wiki Markdown export
        #[arg(long)]
        no_wiki: bool,
        /// Skip the graph.html and graph.svg visual exports
        #[arg(long)]
        no_visual: bool,
    },
}

pub async fn handle_notes_command(
    command: NotesCommand,
    config: &Config,
) -> Result<(), HarnessError> {
    match command {
        NotesCommand::Graph {
            out,
            json,
            no_wiki,
            no_visual,
        } => run_graph(config, out, json, no_wiki, no_visual).await,
    }
}

async fn run_graph(
    config: &Config,
    out: Option<PathBuf>,
    json: bool,
    no_wiki: bool,
    no_visual: bool,
) -> Result<(), HarnessError> {
    let engine = FileNotesEngine::new(
        config.paths.notes_dir.clone(),
        Some(config.paths.notes_db.clone()),
    )?;
    let notes = engine.list_notes(usize::MAX, 0).await?;
    if notes.is_empty() {
        return Err(HarnessError::NotesError(format!(
            "no notes found in '{}'",
            config.paths.notes_dir.display()
        )));
    }

    let built = graph_notes(&notes);
    let issues = validate(&built.graph);
    let out_dir = out.unwrap_or_else(|| config.paths.notes_dir.join(OUTPUT_DIR_NAME));
    std::fs::create_dir_all(&out_dir).map_err(|error| {
        HarnessError::Io(format!(
            "cannot create output directory '{}': {error}",
            out_dir.display()
        ))
    })?;

    let graph_path = out_dir.join("graph.json");
    export::to_json(&built.graph, &built.communities, &built.labels, &graph_path)?;

    let html_path = out_dir.join("graph.html");
    let svg_path = out_dir.join("graph.svg");
    if !no_visual {
        export::to_html(&built.graph, &built.communities, &built.labels, &html_path)?;
        export::to_svg(&built.graph, &built.communities, &built.labels, &svg_path)?;
    }

    let wiki_path = out_dir.join("wiki");
    let wiki_written = if no_wiki {
        0
    } else {
        export::to_wiki(
            &built.graph,
            &built.communities,
            &built.labels,
            &built.analysis,
            &wiki_path,
        )?
    };

    let report = render_report(
        &built,
        &issues,
        &graph_path,
        &html_path,
        &svg_path,
        &wiki_path,
        no_wiki,
        no_visual,
    );
    let report_path = out_dir.join(REPORT_FILE);
    std::fs::write(&report_path, report).map_err(|error| {
        HarnessError::Io(format!(
            "cannot write report '{}': {error}",
            report_path.display()
        ))
    })?;

    if json {
        let summary = serde_json::json!({
            "output": out_dir,
            "graph": graph_path,
            "html": if no_visual { serde_json::Value::Null } else { serde_json::json!(html_path) },
            "svg": if no_visual { serde_json::Value::Null } else { serde_json::json!(svg_path) },
            "wiki": if no_wiki { serde_json::Value::Null } else { serde_json::json!(wiki_path) },
            "notes": built.analysis.notes,
            "nodes": built.graph.nodes.len(),
            "edges": built.graph.edges.len(),
            "communities": built.communities.len(),
            "dangling": built.graph.dangling,
            "hubs": built.analysis.god_nodes.iter().map(|node| serde_json::json!({
                "label": node.label,
                "kind": format!("{:?}", node.kind).to_lowercase(),
                "degree": node.degree,
            })).collect::<Vec<_>>(),
            "issues": issues.iter().map(ToString::to_string).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&summary)
                .map_err(|error| HarnessError::SerializationError(error.to_string()))?
        );
        return Ok(());
    }

    println!(
        "notes: {}  nodes: {}  edges: {}  communities: {}  dangling: {}",
        built.analysis.notes,
        built.graph.nodes.len(),
        built.graph.edges.len(),
        built.communities.len(),
        built.graph.dangling.len()
    );
    if let Some(top) = built.analysis.god_nodes.first() {
        println!(
            "top hub: {} ({} connections, {:?})",
            top.label, top.degree, top.kind
        );
    }
    if !issues.is_empty() {
        println!("warnings: {} (see {})", issues.len(), report_path.display());
    }
    println!("graph:  {}", graph_path.display());
    if !no_visual {
        println!("html:   {}", html_path.display());
        println!("svg:    {}", svg_path.display());
    }
    if !no_wiki {
        println!(
            "wiki:   {} ({} articles)",
            wiki_path.display(),
            wiki_written
        );
    }
    println!("report: {}", report_path.display());
    Ok(())
}

fn render_report(
    built: &BuiltGraph,
    issues: &[harness_note_graph::validate::Issue],
    graph_path: &Path,
    html_path: &Path,
    svg_path: &Path,
    wiki_path: &Path,
    no_wiki: bool,
    no_visual: bool,
) -> String {
    let analysis = &built.analysis;
    let mut out = String::new();
    out.push_str("# Notes graph report\n\n");
    out.push_str(&format!(
        "{} notes · {} headings · {} tags · {} concepts · {} files · {} symbols\n\n",
        analysis.notes,
        analysis.headings,
        analysis.tags,
        analysis.concepts,
        analysis.files,
        analysis.symbols
    ));
    out.push_str(&format!(
        "{} edges · {} communities · {} dangling links · {} ambiguous edges\n\n",
        analysis.edges,
        analysis.communities.len(),
        analysis.dangling,
        analysis.ambiguous_edges
    ));
    let mut artifacts = format!("`{}`", graph_path.display());
    if !no_visual {
        artifacts.push_str(&format!(
            ", interactive `{}`, image `{}`",
            html_path.display(),
            svg_path.display()
        ));
    }
    if !no_wiki {
        artifacts.push_str(&format!(", wiki `{}`", wiki_path.display()));
    }
    out.push_str(&format!("Artifacts: {artifacts} and `GRAPH_REPORT.md`.\n\n"));

    out.push_str("## Hubs\n\n");
    if analysis.god_nodes.is_empty() {
        out.push_str("- none\n");
    } else {
        for god in &analysis.god_nodes {
            out.push_str(&format!(
                "- {} — `{:?}` · {} connections\n",
                god.label, god.kind, god.degree
            ));
        }
    }
    out.push('\n');

    out.push_str("## Surprising connections\n\n");
    if analysis.surprises.is_empty() {
        out.push_str("- none\n");
    } else {
        for surprise in &analysis.surprises {
            out.push_str(&format!(
                "- {} -> {} ({}, {}){}\n",
                surprise.source_label,
                surprise.target_label,
                relation_label(surprise.relation),
                confidence_label(surprise.confidence),
                reason_suffix(&surprise.reasons)
            ));
        }
    }
    out.push('\n');

    out.push_str("## Suggested questions\n\n");
    if analysis.questions.is_empty() {
        out.push_str("- none\n");
    } else {
        for question in &analysis.questions {
            out.push_str(&format!("- {}\n", question_text(question)));
        }
    }
    out.push('\n');

    out.push_str("## Communities\n\n");
    for summary in &analysis.communities {
        let members: Vec<&str> = summary
            .members
            .iter()
            .filter_map(|id| built.graph.node(id))
            .filter(|node| node.kind == harness_note_graph::NodeKind::Note)
            .map(|node| node.label.as_str())
            .collect();
        out.push_str(&format!(
            "- {} ({} nodes, {:.2} cohesion): {}\n",
            summary.label,
            summary.size,
            summary.cohesion,
            members.join(", ")
        ));
    }
    out.push('\n');

    if !built.graph.dangling.is_empty() {
        out.push_str("## Broken links\n\n");
        for target in &built.graph.dangling {
            out.push_str(&format!("- `{}`\n", target));
        }
        out.push('\n');
    }

    out.push_str("## Validation\n\n");
    if issues.is_empty() {
        out.push_str("- clean\n");
    } else {
        for issue in issues {
            out.push_str(&format!("- {issue}\n"));
        }
    }
    out
}

fn question_text(question: &QuestionKind) -> String {
    match question {
        QuestionKind::CommonGround { nodes } => {
            format!("What do these hubs have in common: {}?", nodes.join(", "))
        }
        QuestionKind::Bridge { left, right } => {
            format!("How are \"{left}\" and \"{right}\" connected?")
        }
        QuestionKind::Isolated { note } => {
            format!("\"{note}\" has no links. Should it be linked or archived?")
        }
        QuestionKind::Ambiguous { left, right } => {
            format!("Is the link between \"{left}\" and \"{right}\" intended?")
        }
        QuestionKind::Community { label } => {
            format!("Is \"{label}\" one topic, or should it be split?")
        }
    }
}

fn reason_suffix(reasons: &[SurpriseReason]) -> String {
    if reasons.is_empty() {
        return String::new();
    }
    let parts: Vec<&str> = reasons
        .iter()
        .map(|reason| match reason {
            SurpriseReason::CrossesCommunities => "crosses communities",
            SurpriseReason::LowDegree => "low degree",
            SurpriseReason::Ambiguous => "uncertain",
            SurpriseReason::CrossKind => "different kinds",
        })
        .collect();
    format!(" — {}", parts.join(", "))
}

fn relation_label(relation: harness_note_graph::Relation) -> &'static str {
    match relation {
        harness_note_graph::Relation::Contains => "contains",
        harness_note_graph::Relation::References => "references",
        harness_note_graph::Relation::Tagged => "tagged",
        harness_note_graph::Relation::Mentions => "mentions",
        harness_note_graph::Relation::PartOf => "part_of",
    }
}

fn confidence_label(confidence: harness_note_graph::Confidence) -> &'static str {
    match confidence {
        harness_note_graph::Confidence::Extracted => "EXTRACTED",
        harness_note_graph::Confidence::Inferred => "INFERRED",
        harness_note_graph::Confidence::Ambiguous => "AMBIGUOUS",
    }
}
