//! `luna notes` — per-project notes, system documentation, and a knowledge graph.
//!
//! The graph itself lives in `harness-note-graph`; the workspace layout lives in
//! `harness_notes::projects`; this command wires them together, writes the artifacts, and renders
//! the report. All user-visible prose belongs here, per `agents.md` §1.
//!
//! Layout written under `paths.notes_dir`:
//!
//! ```text
//! <project>/notes/*.md      the notes themselves
//! <project>/docs/           generated system documentation
//! <project>/graph/          per-project graph artifacts
//! graph/                    the combined graph across every project
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use clap::Subcommand;
use harness_core::config::Config;
use harness_core::{HarnessError, Note, NotesEngine};
use harness_note_graph::{
    analyze::{QuestionKind, SurpriseReason},
    export, graph_notes, validate, BuiltGraph,
};
use harness_notes::{
    docs::{self, DocInput},
    projects, FileNotesEngine,
};
use serde_json::json;

/// Report file written beside `graph.json`.
const REPORT_FILE: &str = "GRAPH_REPORT.md";

/// Folder under `paths.notes_dir` for the graph that spans every project.
const COMBINED_DIR: &str = "graph";

#[derive(Debug, Subcommand)]
pub enum NotesCommand {
    /// Build the knowledge graph for one project, or for every project plus the combined view
    Graph {
        /// Output directory (default: `<notes_dir>/graph`, or `<notes_dir>/<project>/graph`)
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
        /// Restrict to one project (for example `inbox` or `my-app`)
        #[arg(long)]
        project: Option<String>,
    },
    /// Generate deterministic system documentation for one project or all of them
    Docs {
        /// Output directory (default: `<notes_dir>/<project>/docs`)
        #[arg(long)]
        out: Option<PathBuf>,
        /// One JSON summary on stdout
        #[arg(long)]
        json: bool,
        /// Restrict to one project (for example `inbox` or `my-app`)
        #[arg(long)]
        project: Option<String>,
        /// Source tree to document; overrides the manifest's `root`
        #[arg(long)]
        source: Option<PathBuf>,
    },
    /// Inspect and create project folders
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// List the projects in the notes workspace
    List {
        /// One JSON array on stdout
        #[arg(long)]
        json: bool,
    },
    /// Create a project folder with `notes/`, `docs/` and `graph/`
    New {
        /// Display name; the folder is its slug (`My App` -> `my-app`)
        name: String,
        /// Source tree to document; stored in the project manifest
        #[arg(long)]
        root: Option<PathBuf>,
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
            project,
        } => run_graph(config, out, json, no_wiki, no_visual, project).await,
        NotesCommand::Docs {
            out,
            json,
            project,
            source,
        } => run_docs(config, out, json, project, source).await,
        NotesCommand::Project { command } => run_project(command, config).await,
    }
}

fn open_engine(config: &Config) -> Result<FileNotesEngine, HarnessError> {
    FileNotesEngine::new(
        config.paths.notes_dir.clone(),
        Some(config.paths.notes_db.clone()),
    )
}

/// The project a note belongs to; the loader guarantees a tag, this is the fallback.
fn project_of(note: &Note) -> String {
    projects::project_from_tags(&note.tags).unwrap_or_else(|| projects::DEFAULT_PROJECT.to_string())
}

async fn run_project(command: ProjectCommand, config: &Config) -> Result<(), HarnessError> {
    let notes_dir = &config.paths.notes_dir;
    match command {
        ProjectCommand::New { name, root } => {
            let project = projects::ensure(notes_dir, &name)?;
            if let Some(root) = root.as_deref() {
                projects::link_source(&project, Some(root))?;
            }
            println!("project: {}", project.slug);
            println!("name:    {}", project.name);
            println!("notes:   {}", project.notes_dir().display());
            println!("docs:    {}", project.docs_dir().display());
            println!("graph:   {}", project.graph_dir().display());
            if let Some(root) = project.source_root() {
                println!("source:  {}", root.display());
            }
            Ok(())
        }
        ProjectCommand::List { json } => {
            let engine = open_engine(config)?;
            let notes = engine.list_notes(usize::MAX, 0).await?;
            let mut counts: BTreeMap<String, usize> = BTreeMap::new();
            for note in &notes {
                *counts.entry(project_of(note)).or_default() += 1;
            }
            let listed = projects::list(notes_dir)?;

            if json {
                let value = json!({
                    "notes_dir": notes_dir,
                    "projects": listed.iter().map(|project| json!({
                        "slug": project.slug,
                        "name": project.name,
                        "path": project.dir,
                        "notes": counts.get(&project.slug).copied().unwrap_or(0),
                    })).collect::<Vec<_>>(),
                });
                println!(
                    "{}",
                    serde_json::to_string_pretty(&value)
                        .map_err(|error| HarnessError::SerializationError(error.to_string()))?
                );
                return Ok(());
            }

            if listed.is_empty() {
                println!("no projects in '{}'", notes_dir.display());
                return Ok(());
            }
            for project in &listed {
                println!(
                    "{:<20} {:>4} notes  {}",
                    project.slug,
                    counts.get(&project.slug).copied().unwrap_or(0),
                    project.dir.display()
                );
            }
            Ok(())
        }
    }
}

/// The CLI surface, captured once so every project's document describes the same binary.
fn cli_facts() -> docs::CliFacts {
    use clap::CommandFactory;
    let command = crate::Cli::command();
    let mut subcommands: Vec<(String, String)> = command
        .get_subcommands()
        .filter(|sub| sub.get_name() != "help")
        .map(|sub| {
            (
                sub.get_name().to_string(),
                sub.get_about()
                    .map(|about| about.to_string())
                    .unwrap_or_default(),
            )
        })
        .collect();
    subcommands.sort_by(|left, right| left.0.cmp(&right.0));

    let mut top = command.clone();
    let top_help = top.render_long_help().to_string();
    let notes_help = command
        .find_subcommand("notes")
        .map(|notes| {
            let mut notes = notes.clone();
            notes.render_long_help().to_string()
        })
        .unwrap_or_default();

    docs::CliFacts {
        version: crate::VERSION.to_string(),
        top_help,
        notes_help,
        subcommands,
    }
}

async fn run_docs(
    config: &Config,
    out: Option<PathBuf>,
    json: bool,
    project: Option<String>,
    source: Option<PathBuf>,
) -> Result<(), HarnessError> {
    let notes_dir = &config.paths.notes_dir;
    if project.is_none() && (out.is_some() || source.is_some()) {
        return Err(HarnessError::NotesError(
            "`--out` and `--source` apply to one project; pass `--project <name>`".to_string(),
        ));
    }

    let engine = open_engine(config)?;
    let notes = engine.list_notes(usize::MAX, 0).await?;
    let mut groups: BTreeMap<String, Vec<Note>> = BTreeMap::new();
    for note in notes {
        groups.entry(project_of(&note)).or_default().push(note);
    }

    let available = projects::list(notes_dir)?;
    if available.is_empty() {
        return Err(HarnessError::NotesError(format!(
            "no projects found in '{}'; run `luna notes project new <name>` first",
            notes_dir.display()
        )));
    }

    let targets = if let Some(slug) = project.as_deref() {
        let slug = projects::slugify_project(slug);
        let known: Vec<String> = available.iter().map(|entry| entry.slug.clone()).collect();
        match available
            .into_iter()
            .find(|candidate| candidate.slug == slug)
        {
            Some(target) => vec![target],
            None => {
                return Err(HarnessError::NotesError(format!(
                    "project '{}' does not exist (known: {})",
                    slug,
                    known.join(", ")
                )))
            }
        }
    } else {
        available
    };

    let cli = cli_facts();
    let mut summaries = Vec::new();

    for target in targets {
        let subset = groups.get(&target.slug).cloned().unwrap_or_default();
        let built = graph_notes(&subset);
        let root = source.clone().or_else(|| target.source_root());
        let snapshot = match root.as_deref() {
            Some(root) => Some(docs::scan_source(root).map_err(|error| {
                HarnessError::NotesError(format!(
                    "cannot document source root '{}': {}",
                    root.display(),
                    error
                ))
            })?),
            None => None,
        };

        let created = target.created_at();
        let input = DocInput {
            project: &target,
            created_at: &created,
            notes: &subset,
            built: &built,
            source: snapshot.as_ref(),
            cli: &cli,
        };
        let rendered = docs::render_docs(&input);
        let dir = out.clone().unwrap_or_else(|| target.docs_dir());
        let written = docs::write_docs(&dir, &rendered)?;

        if json {
            summaries.push(json!({
                "project": target.slug,
                "name": target.name,
                "docs": dir,
                "files": written,
                "notes": subset.len(),
                "nodes": built.graph.nodes.len(),
                "edges": built.graph.edges.len(),
                "source": snapshot.as_ref().map(|snapshot| snapshot.root.clone()),
            }));
        } else {
            println!(
                "{:<18} docs: {} files  notes: {}  nodes: {}  source: {}",
                target.slug,
                rendered.len(),
                subset.len(),
                built.graph.nodes.len(),
                snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.root.as_str())
                    .unwrap_or("not linked")
            );
            println!("{:<18} path: {}", "", dir.display());
        }
    }

    if json {
        let value = json!({ "notes_dir": notes_dir, "docs": summaries });
        println!(
            "{}",
            serde_json::to_string_pretty(&value)
                .map_err(|error| HarnessError::SerializationError(error.to_string()))?
        );
    }
    Ok(())
}

/// Counts and paths for one graph build, so printing and `--json` share the same source.
struct GraphSummary {
    scope: String,
    output: PathBuf,
    graph: PathBuf,
    html: Option<PathBuf>,
    svg: Option<PathBuf>,
    wiki: Option<PathBuf>,
    notes: usize,
    nodes: usize,
    edges: usize,
    communities: usize,
    dangling: usize,
    hubs: Vec<(String, String, usize)>,
    issues: Vec<String>,
}

impl GraphSummary {
    fn to_json(&self) -> serde_json::Value {
        json!({
            "scope": self.scope,
            "output": self.output,
            "graph": self.graph,
            "html": self.html,
            "svg": self.svg,
            "wiki": self.wiki,
            "notes": self.notes,
            "nodes": self.nodes,
            "edges": self.edges,
            "communities": self.communities,
            "dangling": self.dangling,
            "hubs": self.hubs.iter().map(|(label, kind, degree)| json!({
                "label": label,
                "kind": kind,
                "degree": degree,
            })).collect::<Vec<_>>(),
            "issues": self.issues,
        })
    }

    fn print(&self) {
        println!(
            "{:<18} notes: {}  nodes: {}  edges: {}  communities: {}  dangling: {}",
            self.scope, self.notes, self.nodes, self.edges, self.communities, self.dangling
        );
        if let Some((label, kind, degree)) = self.hubs.first() {
            println!(
                "{:<18} top hub: {} ({} connections, {})",
                "", label, degree, kind
            );
        }
        println!("{:<18} graph: {}", "", self.graph.display());
        if let Some(path) = &self.html {
            println!("{:<18} html:  {}", "", path.display());
        }
        if let Some(path) = &self.svg {
            println!("{:<18} svg:   {}", "", path.display());
        }
        if let Some(path) = &self.wiki {
            println!("{:<18} wiki:  {}", "", path.display());
        }
        if !self.issues.is_empty() {
            println!(
                "{:<18} warnings: {} (see {})",
                "",
                self.issues.len(),
                self.output.join(REPORT_FILE).display()
            );
        }
    }
}

async fn run_graph(
    config: &Config,
    out: Option<PathBuf>,
    json: bool,
    no_wiki: bool,
    no_visual: bool,
    project: Option<String>,
) -> Result<(), HarnessError> {
    let notes_dir = &config.paths.notes_dir;
    let engine = open_engine(config)?;
    let notes = engine.list_notes(usize::MAX, 0).await?;
    if notes.is_empty() {
        return Err(HarnessError::NotesError(format!(
            "no notes found in '{}'",
            notes_dir.display()
        )));
    }

    // Group once: the combined graph and every per-project graph come from the same read.
    let mut groups: BTreeMap<String, Vec<Note>> = BTreeMap::new();
    for note in notes {
        groups.entry(project_of(&note)).or_default().push(note);
    }

    let mut summaries = Vec::new();

    if let Some(slug) = project.as_deref() {
        let slug = projects::slugify_project(slug);
        let Some(selected) = groups.remove(&slug) else {
            let known: Vec<&str> = groups.keys().map(String::as_str).collect();
            return Err(HarnessError::NotesError(format!(
                "project '{}' has no notes (known: {})",
                slug,
                known.join(", ")
            )));
        };
        let output = out.unwrap_or_else(|| notes_dir.join(&slug).join(projects::GRAPH_SUBDIR));
        summaries.push(write_graph(&selected, &output, &slug, no_wiki, no_visual)?);
    } else {
        // The combined view keeps cross-project links intact: a note in one project can reference
        // a note in another, and only this graph can resolve that edge.
        let all: Vec<Note> = groups.values().flatten().cloned().collect();
        let combined_dir = out.unwrap_or_else(|| notes_dir.join(COMBINED_DIR));
        summaries.push(write_graph(
            &all,
            &combined_dir,
            "all projects",
            no_wiki,
            no_visual,
        )?);

        for (slug, subset) in &groups {
            let output = notes_dir.join(slug).join(projects::GRAPH_SUBDIR);
            summaries.push(write_graph(subset, &output, slug, no_wiki, no_visual)?);
        }
    }

    if json {
        let value = json!({
            "notes_dir": notes_dir,
            "mode": if project.is_some() { "project" } else { "all" },
            "graphs": summaries.iter().map(GraphSummary::to_json).collect::<Vec<_>>(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&value)
                .map_err(|error| HarnessError::SerializationError(error.to_string()))?
        );
        return Ok(());
    }

    for summary in &summaries {
        summary.print();
        println!();
    }
    Ok(())
}

/// Build and write one graph into `out_dir`.
fn write_graph(
    notes: &[Note],
    out_dir: &Path,
    scope: &str,
    no_wiki: bool,
    no_visual: bool,
) -> Result<GraphSummary, HarnessError> {
    std::fs::create_dir_all(out_dir).map_err(|error| {
        HarnessError::Io(format!(
            "cannot create output directory '{}': {error}",
            out_dir.display()
        ))
    })?;

    let built = graph_notes(notes);
    let issues = validate(&built.graph);

    let graph_path = out_dir.join("graph.json");
    export::to_json(&built.graph, &built.communities, &built.labels, &graph_path)?;

    let html_path = out_dir.join("graph.html");
    let svg_path = out_dir.join("graph.svg");
    if !no_visual {
        export::to_html(&built.graph, &built.communities, &built.labels, &html_path)?;
        export::to_svg(&built.graph, &built.communities, &built.labels, &svg_path)?;
    }

    let wiki_path = out_dir.join("wiki");
    if !no_wiki {
        export::to_wiki(
            &built.graph,
            &built.communities,
            &built.labels,
            &built.analysis,
            &wiki_path,
        )?;
    }

    let report = render_report(
        &built,
        &issues,
        scope,
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

    Ok(GraphSummary {
        scope: scope.to_string(),
        output: out_dir.to_path_buf(),
        graph: graph_path,
        html: if no_visual { None } else { Some(html_path) },
        svg: if no_visual { None } else { Some(svg_path) },
        wiki: if no_wiki { None } else { Some(wiki_path) },
        notes: built.analysis.notes,
        nodes: built.graph.nodes.len(),
        edges: built.graph.edges.len(),
        communities: built.communities.len(),
        dangling: built.graph.dangling.len(),
        hubs: built
            .analysis
            .god_nodes
            .iter()
            .map(|node| {
                (
                    node.label.clone(),
                    format!("{:?}", node.kind).to_lowercase(),
                    node.degree,
                )
            })
            .collect(),
        issues: issues.iter().map(ToString::to_string).collect(),
    })
}

#[allow(clippy::too_many_arguments)]
fn render_report(
    built: &BuiltGraph,
    issues: &[harness_note_graph::validate::Issue],
    scope: &str,
    graph_path: &Path,
    html_path: &Path,
    svg_path: &Path,
    wiki_path: &Path,
    no_wiki: bool,
    no_visual: bool,
) -> String {
    let analysis = &built.analysis;
    let mut out = String::new();
    out.push_str(&format!("# Notes graph report — {scope}\n\n"));
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
    out.push_str(&format!(
        "Artifacts: {artifacts} and `GRAPH_REPORT.md`.\n\n"
    ));

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
        for missing in &built.graph.dangling {
            out.push_str(&format!("- `{}`\n", missing));
        }
        out.push('\n');
    }

    if !issues.is_empty() {
        out.push_str("## Validation\n\n");
        for issue in issues {
            out.push_str(&format!("- {issue}\n"));
        }
        out.push('\n');
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
