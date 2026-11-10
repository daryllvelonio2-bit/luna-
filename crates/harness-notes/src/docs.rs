//! Deterministic, capstone-style system documentation for one project.
//!
//! Nothing here asks a model anything. Every sentence is assembled from inputs that already
//! exist on disk: the project manifest, the Markdown notes, the note graph, an optional source
//! snapshot, and the CLI's own `--help`. Re-running with the same inputs produces byte-identical
//! files, which is what makes generated documentation safe to commit and diff.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use harness_core::{HarnessError, Note};
use harness_note_graph::{BuiltGraph, NodeKind};

use crate::projects::{self, Project, MANIFEST, NOTES_SUBDIR};

/// Short entry page written beside the full document.
pub const README_FILE: &str = "README.md";

/// The full system document.
pub const SYSTEM_FILE: &str = "system.md";

/// Generated note index and per-note outlines.
pub const NOTES_FILE: &str = "notes.md";

/// Directories a source scan never descends into: build output, dependencies, and editor state.
const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".idea",
    ".vscode",
    ".venv",
    "venv",
    "__pycache__",
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    "coverage",
    ".next",
    ".cache",
    ".turbo",
];

/// Depth limit for the source tree walk.
const MAX_DEPTH: usize = 8;

/// File ceiling so a mistyped `--source /` cannot stall the command.
const MAX_FILES: usize = 20_000;

/// Bound on any list rendered into the document.
const MAX_LIST: usize = 60;

/// One file produced by the renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedDoc {
    pub file: String,
    pub content: String,
}

/// Facts about the CLI surface, captured once by the command layer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CliFacts {
    pub version: String,
    pub top_help: String,
    pub notes_help: String,
    pub subcommands: Vec<(String, String)>,
}

/// A deterministic inventory of a project's source tree.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceSnapshot {
    pub root: String,
    pub files: usize,
    pub dirs: usize,
    pub bytes: u64,
    pub languages: Vec<(String, usize)>,
    pub manifests: Vec<ManifestFact>,
    pub entry_points: Vec<String>,
    pub tests: Vec<String>,
    pub top_level: Vec<String>,
    pub readme_headings: Vec<String>,
    pub readme_intro: Option<String>,
}

/// One build or package manifest found while scanning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestFact {
    pub kind: String,
    pub path: String,
    pub name: Option<String>,
    pub details: Vec<(String, String)>,
}

/// Everything the renderer needs. Borrowed so a graph build is not copied.
pub struct DocInput<'a> {
    pub project: &'a Project,
    pub created_at: &'a str,
    pub notes: &'a [Note],
    pub built: &'a BuiltGraph,
    pub source: Option<&'a SourceSnapshot>,
    pub cli: &'a CliFacts,
}

struct NoteRow {
    title: String,
    id: String,
    tags: Vec<String>,
    updated: String,
    headings: Vec<(usize, String)>,
}

struct GraphRow {
    nodes: usize,
    edges: usize,
    communities: usize,
    dangling: usize,
    notes: usize,
    headings: usize,
    tags: usize,
    concepts: usize,
    files: usize,
    symbols: usize,
    missing: usize,
    ambiguous: usize,
    hubs: Vec<(String, String, usize)>,
    community_rows: Vec<(String, usize, f32, Vec<String>)>,
}

struct Facts<'a> {
    name: String,
    slug: String,
    dir: String,
    created_at: String,
    notes: Vec<NoteRow>,
    tags: Vec<(String, usize)>,
    graph: GraphRow,
    source: Option<&'a SourceSnapshot>,
    cli: &'a CliFacts,
}

/// Render every documentation file for one project.
pub fn render_docs(input: &DocInput<'_>) -> Vec<GeneratedDoc> {
    let facts = Facts::from(input);
    vec![
        GeneratedDoc {
            file: README_FILE.to_string(),
            content: render_readme(&facts),
        },
        GeneratedDoc {
            file: SYSTEM_FILE.to_string(),
            content: render_system(&facts),
        },
        GeneratedDoc {
            file: NOTES_FILE.to_string(),
            content: render_note_doc(&facts),
        },
    ]
}

/// Write the rendered files into `dir`, returning the paths in render order.
pub fn write_docs(dir: &Path, docs: &[GeneratedDoc]) -> Result<Vec<PathBuf>, HarnessError> {
    std::fs::create_dir_all(dir).map_err(|error| {
        HarnessError::NotesError(format!(
            "Failed to create documentation directory '{}': {}",
            dir.display(),
            error
        ))
    })?;

    let mut written = Vec::new();
    for doc in docs {
        let path = dir.join(&doc.file);
        std::fs::write(&path, &doc.content).map_err(|error| {
            HarnessError::NotesError(format!(
                "Failed to write documentation '{}': {}",
                path.display(),
                error
            ))
        })?;
        written.push(path);
    }
    Ok(written)
}

impl<'a> From<&DocInput<'a>> for Facts<'a> {
    fn from(input: &DocInput<'a>) -> Self {
        let mut notes: Vec<NoteRow> = input
            .notes
            .iter()
            .map(|note| NoteRow {
                title: note.title.clone(),
                id: projects::split_qualified(&note.id).1.to_string(),
                tags: note
                    .tags
                    .iter()
                    .filter(|tag| projects::project_from_tags(std::slice::from_ref(tag)).is_none())
                    .map(|tag| tag.trim().to_string())
                    .filter(|tag| !tag.is_empty())
                    .collect(),
                updated: note.updated_at.format("%Y-%m-%d").to_string(),
                headings: note_headings(&note.content),
            })
            .collect();
        notes.sort_by(|left, right| {
            left.title
                .to_lowercase()
                .cmp(&right.title.to_lowercase())
                .then_with(|| left.id.cmp(&right.id))
        });

        let mut tag_counts: BTreeMap<String, usize> = BTreeMap::new();
        for note in &notes {
            for tag in &note.tags {
                *tag_counts.entry(tag.clone()).or_default() += 1;
            }
        }
        let mut tags: Vec<(String, usize)> = tag_counts.into_iter().collect();
        tags.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

        let analysis = &input.built.analysis;
        let community_rows = analysis
            .communities
            .iter()
            .map(|community| {
                let members: Vec<String> = community
                    .members
                    .iter()
                    .filter_map(|id| input.built.graph.node(id))
                    .filter(|node| node.kind == NodeKind::Note)
                    .map(|node| node.label.clone())
                    .collect();
                (
                    community.label.clone(),
                    community.size,
                    community.cohesion,
                    members,
                )
            })
            .collect();

        Facts {
            name: input.project.name.clone(),
            slug: input.project.slug.clone(),
            dir: input.project.dir.display().to_string(),
            created_at: input.created_at.to_string(),
            notes,
            tags,
            graph: GraphRow {
                nodes: input.built.graph.nodes.len(),
                edges: input.built.graph.edges.len(),
                communities: analysis.communities.len(),
                dangling: analysis.dangling,
                notes: analysis.notes,
                headings: analysis.headings,
                tags: analysis.tags,
                concepts: analysis.concepts,
                files: analysis.files,
                symbols: analysis.symbols,
                missing: analysis.missing,
                ambiguous: analysis.ambiguous_edges,
                hubs: analysis
                    .god_nodes
                    .iter()
                    .map(|hub| {
                        (
                            hub.label.clone(),
                            format!("{:?}", hub.kind).to_lowercase(),
                            hub.degree,
                        )
                    })
                    .collect(),
                community_rows,
            },
            source: input.source,
            cli: input.cli,
        }
    }
}

fn render_readme(facts: &Facts<'_>) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} — documentation\n\n", facts.name));
    out.push_str(&format!("{}\n\n", abstract_text(facts)));

    out.push_str("## Start here\n\n");
    out.push_str(&format!(
        "- [System documentation]({SYSTEM_FILE}) — architecture, system flow, and user flows.\n\
         - [Note index]({NOTES_FILE}) — every captured note and its outline.\n\
         - [Graph report](../graph/GRAPH_REPORT.md) — hubs, communities, and review prompts.\n\
         - [Interactive graph](../graph/graph.html) · [image](../graph/graph.svg)\n\n"
    ));

    out.push_str("## Workspace\n\n");
    out.push_str("| Path | Contents |\n| --- | --- |\n");
    out.push_str(&format!(
        "| `{slug}/notes/` | The project's Markdown notes |\n\
         | `{slug}/docs/` | This documentation |\n\
         | `{slug}/graph/` | Per-project graph artifacts |\n\
         | `{slug}/{MANIFEST}` | Project manifest |\n",
        slug = facts.slug,
    ));

    out.push_str("\n## Regenerate\n\n```bash\n");
    out.push_str(&format!(
        "luna notes graph --project {slug}\nluna notes docs --project {slug}\n",
        slug = facts.slug
    ));
    out.push_str("```\n\n");
    out.push_str(
        "Both commands are deterministic: the output changes only when the notes, the source tree,\n\
         or the CLI itself change.\n",
    );
    out
}

fn render_system(facts: &Facts<'_>) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} — system documentation\n\n", facts.name));
    out.push_str(&format!("> {}\n\n", abstract_text(facts)));

    out.push_str("## Document control\n\n");
    out.push_str("| Field | Value |\n| --- | --- |\n");
    out.push_str(&format!(
        "| Project | {} (`{}`) |\n",
        facts.name, facts.slug
    ));
    out.push_str(&format!("| Project folder | `{}` |\n", facts.dir));
    out.push_str(&format!("| Created | {} |\n", facts.created_at));
    out.push_str(&format!(
        "| Notes | {} |\n| Graph | {} nodes, {} edges, {} communities |\n",
        facts.notes.len(),
        facts.graph.nodes,
        facts.graph.edges,
        facts.graph.communities
    ));
    out.push_str(&format!(
        "| Source root | {} |\n",
        facts
            .source
            .map(|source| format!("`{}`", source.root))
            .unwrap_or_else(|| "not linked".to_string())
    ));
    let generator = if facts.cli.version.is_empty() {
        format!("`luna notes docs --project {}`", facts.slug)
    } else {
        format!(
            "`luna notes docs --project {}` on Luna {}",
            facts.slug, facts.cli.version
        )
    };
    out.push_str(&format!("| Generated by | {generator} |\n\n"));

    out.push_str("## Contents\n\n");
    for (number, title) in [
        (1, "Introduction"),
        (2, "System overview"),
        (3, "Architecture"),
        (4, "System flow"),
        (5, "User flows"),
        (6, "Commands and interfaces"),
        (7, "Data and storage"),
        (8, "Operations"),
        (9, "Knowledge graph"),
        (10, "Decisions"),
    ] {
        out.push_str(&format!(
            "{number}. [{title}](#{number}-{anchor})\n",
            anchor = slug_for_anchor(title)
        ));
    }
    out.push_str(&format!(
        "- [Appendix A — Source inventory](#appendix-a--source-inventory)\n\
         - [Appendix B — Glossary](#appendix-b--glossary)\n\
         - [Note index]({NOTES_FILE})\n\n"
    ));

    out.push_str("## 1. Introduction\n\n");
    out.push_str(&format!(
        "This document describes **{}** as it exists in the Luna notes workspace. It is generated\n\
         from the project's Markdown notes, the note knowledge graph, {}and the Luna CLI surface.\n\
         Nothing is inferred that the inputs do not support: a section with no evidence says so\n\
         instead of inventing detail.\n\n",
        facts.name,
        if facts.source.is_some() {
            "a snapshot of the linked source tree, "
        } else {
            ""
        }
    ));

    out.push_str("### 1.1 Purpose\n\n");
    out.push_str(
        "The purpose of this document is to give a new maintainer one place to answer three\n\
         questions: what the system is, how a user moves through it, and where its state lives.\n\
         It doubles as the project's record of decisions, so a later change can see what was\n\
         rejected and why.\n\n",
    );

    out.push_str("### 1.2 Scope\n\n");
    out.push_str(&format!(
        "In scope: the `{}` project folder, its notes, its generated graph, and the Luna commands\n\
         that act on them. Out of scope: notes belonging to other projects, which are documented\n\
         in their own folders. Cross-project links remain visible in the combined graph at\n\
         `<notes_dir>/graph/`.\n\n",
        facts.slug
    ));

    out.push_str("### 1.3 Intended audience\n\n");
    out.push_str(
        "Maintainers who need to change the system, reviewers who need to verify it, and anyone\n\
         resuming the work after a pause. No prior knowledge of this project is assumed.\n\n",
    );

    out.push_str("## 2. System overview\n\n");
    out.push_str("### 2.1 What the system is\n\n");
    if let Some(source) = facts.source {
        out.push_str(&format!(
            "The linked source root is `{}`. It contains {} files in {} directories ({}).\n",
            source.root,
            source.files,
            source.dirs,
            human_bytes(source.bytes)
        ));
        if let Some(intro) = &source.readme_intro {
            out.push_str(&format!(
                "\nThe project's own README describes it as:\n\n> {intro}\n"
            ));
        }
        out.push('\n');
    } else {
        out.push_str(
            "No source root is linked to this project, so this document covers the knowledge base\n\
             rather than a code tree. Link one with\n\
             `luna notes project new <name> --root <path>` or pass `--source <path>` to\n\
             `luna notes docs`.\n\n",
        );
    }

    out.push_str("### 2.2 Who it serves\n\n");
    out.push_str(
        "The primary user is the person capturing knowledge: they write and link notes, then read\n\
         the graph and this document back. Secondary users are teammates who inherit the folder\n\
         and need the system explained without a handover call.\n\n",
    );

    out.push_str("### 2.3 Operating environment\n\n");
    out.push_str(&format!(
        "Notes are plain Markdown on the local filesystem, so the workspace works without a\n\
         database or network. The CLI is Luna {} and the notes root is `{}`.\n\n",
        if facts.cli.version.is_empty() {
            "(version unknown)"
        } else {
            &facts.cli.version
        },
        facts.dir
    ));

    out.push_str("## 3. Architecture\n\n");
    out.push_str("### 3.1 Components\n\n");
    out.push_str("| Component | Responsibility | Evidence |\n| --- | --- | --- |\n");
    out.push_str(&format!(
        "| Notes store | Markdown files with YAML frontmatter | `{slug}/notes/` |\n\
         | Project manifest | Identity, creation date, layout version | `{slug}/{MANIFEST}` |\n\
         | Graph builder | Extracts headings, tags, links, and shared terms | `{slug}/graph/graph.json` |\n\
         | Documentation generator | Renders this file from the inputs above | `luna notes docs` |\n",
        slug = facts.slug
    ));
    if let Some(source) = facts.source {
        for manifest in &source.manifests {
            out.push_str(&format!(
                "| {} manifest | Build and dependency definition | `{}` |\n",
                manifest.kind, manifest.path
            ));
        }
    }
    out.push('\n');

    out.push_str("### 3.2 Repository layout\n\n");
    if let Some(source) = facts.source {
        out.push_str("```text\n");
        out.push_str(&format!("{}/\n", source.root));
        for entry in &source.top_level {
            out.push_str(&format!("  {entry}\n"));
        }
        out.push_str("```\n\n");
    } else {
        out.push_str(&format!(
            "```text\n{}\n  notes/    # Markdown notes\n  docs/     # generated documentation\n  graph/    # graph artifacts\n  {MANIFEST}  # manifest\n```\n\n",
            facts.slug
        ));
    }

    out.push_str("### 3.3 Data flow\n\n");
    out.push_str(
        "1. A note is written to `<project>/notes/<slug>.md` with YAML frontmatter.\n\
         2. `luna notes graph` reads every note and extracts headings, tags, links, files, symbols,\n\
            and shared terms into nodes and edges.\n\
         3. Communities are detected deterministically and hubs are ranked by degree.\n\
         4. The graph is exported as JSON, HTML, SVG, and a Markdown wiki.\n\
         5. `luna notes docs` reads the notes, the graph, the source snapshot, and the CLI help and\n\
            renders this document.\n\n",
    );

    out.push_str("## 4. System flow\n\n");
    out.push_str(
        "The diagram below is the path from stored text to the document you are reading. Every\n\
         stage is a pure function of the stage before it.\n\n",
    );
    out.push_str(
        "```text\n\
         Markdown notes  (project/notes/*.md)\n\
                │  read by FileNotesEngine\n\
                ▼\n\
         Note model  (title, tags, body, timestamps)\n\
                │  extract: headings · tags · links · code spans · terms\n\
                ▼\n\
         Knowledge graph  (project/graph/graph.json)\n\
                │  cluster · analyze · validate\n\
                ▼\n\
         Documentation generator  (luna notes docs)\n\
                │  notes + graph + source snapshot + CLI help\n\
                ▼\n\
         docs/system.md  (this document)\n\
         ```\n\n",
    );

    out.push_str("## 5. User flows\n\n");
    out.push_str(
        "Each flow is a numbered path from a terminal prompt to an observable result. Commands\n\
         are the ones this CLI actually exposes; nothing is invented.\n\n",
    );

    out.push_str("### 5.1 First run\n\n");
    out.push_str(
        "1. Run `luna` with no arguments. The CLI starts and opens the interactive chat.\n\
         2. Luna resolves configuration from `$LUNA_CONFIG` or `~/.luna/config.yaml`, creating\n\
            defaults on first use.\n\
         3. The notes engine creates `inbox/` with `notes/`, `docs/`, and `graph/` if it is missing.\n\
         4. Run `luna notes project list` to confirm the workspace and see note counts.\n\
         5. Run `luna --help` to see the full command surface.\n\n",
    );

    out.push_str("### 5.2 Capture and structure notes\n\n");
    out.push_str(
        "1. In chat, ask Luna to remember something; the agent saves it through the note tool.\n\
         2. Untagged notes land in `inbox/notes/`. Add a `project:<name>` tag to route a note to a\n\
            project folder.\n\
         3. Run `luna notes project new <name>` to create a project before capturing into it.\n\
         4. Run `luna notes graph --project <name>` to refresh that project's graph.\n\n",
    );

    out.push_str("### 5.3 Generate documentation\n\n");
    out.push_str(&format!(
        "1. Run `luna notes docs --project {}`.\n\
         2. The generator reads the project's notes and graph, the linked source tree, and the CLI\n\
            help.\n\
         3. It writes `docs/{README_FILE}`, `docs/{SYSTEM_FILE}`, and `docs/{NOTES_FILE}`.\n\
         4. Re-running is idempotent: files change only when an input changes.\n\n",
        facts.slug
    ));

    out.push_str("### 5.4 Recover from a bad change\n\n");
    out.push_str(
        "1. Notes are plain files, so `git checkout` or a filesystem restore is the first recovery\n\
            step.\n\
         2. Deleted notes are moved to `<notes_dir>/.archive/<project>/` rather than erased.\n\
         3. Regenerate derived state with `luna notes graph` and `luna notes docs`; both can be\n\
            rebuilt from the Markdown source at any time.\n\n",
    );

    out.push_str("## 6. Commands and interfaces\n\n");
    out.push_str("### 6.1 Top-level commands\n\n");
    out.push_str("| Command | Purpose |\n| --- | --- |\n");
    for (name, about) in &facts.cli.subcommands {
        out.push_str(&format!("| `luna {name}` | {} |\n", clean_cell(about)));
    }
    out.push('\n');

    out.push_str("### 6.2 Notes commands\n\n```text\n");
    out.push_str(facts.cli.notes_help.trim_end());
    out.push_str("\n```\n\n");

    out.push_str("### 6.3 Full help\n\n```text\n");
    out.push_str(facts.cli.top_help.trim_end());
    out.push_str("\n```\n\n");

    out.push_str("## 7. Data and storage\n\n");
    out.push_str("| Path | Contents |\n| --- | --- |\n");
    out.push_str(&format!(
        "| `<notes_dir>/{slug}/notes/<slug>.md` | Notes with YAML frontmatter |\n\
         | `<notes_dir>/{slug}/docs/` | Generated documentation |\n\
         | `<notes_dir>/{slug}/graph/graph.json` | Graph nodes and edges |\n\
         | `<notes_dir>/{slug}/graph/graph.html` | Interactive graph |\n\
         | `<notes_dir>/{slug}/graph/graph.svg` | Static graph image |\n\
         | `<notes_dir>/{slug}/graph/wiki/` | Markdown wiki export |\n\
         | `<notes_dir>/{slug}/{MANIFEST}` | Project manifest |\n\
         | `<notes_dir>/.archive/{slug}/` | Archived notes |\n\
         | `<notes_dir>/graph/` | Combined graph across projects |\n",
        slug = facts.slug
    ));
    out.push_str("\n### 7.1 Note frontmatter\n\n");
    out.push_str("```yaml\n---\nid: example-note\ntitle: Example note\ntags: [project:");
    out.push_str(&facts.slug);
    out.push_str("]\n---\n```\n\n");
    out.push_str(
        "The `project:<slug>` tag is routing metadata. It is injected on read when a note lacks it,\n\
         so the graph always has a project node without a second index.\n\n",
    );

    out.push_str("## 8. Operations\n\n");
    out.push_str("| Command | Purpose |\n| --- | --- |\n");
    for (command, purpose) in operation_commands(facts) {
        out.push_str(&format!("| `{command}` | {purpose} |\n"));
    }
    out.push('\n');
    out.push_str(
        "Regeneration is safe to run in CI: the graph and docs are derived artifacts, and a dirty\n\
         diff means the inputs changed.\n\n",
    );

    out.push_str("## 9. Knowledge graph\n\n");
    out.push_str("| Measure | Value |\n| --- | --- |\n");
    for (label, value) in [
        ("Notes", facts.graph.notes),
        ("Headings", facts.graph.headings),
        ("Tags", facts.graph.tags),
        ("Concepts", facts.graph.concepts),
        ("Files", facts.graph.files),
        ("Symbols", facts.graph.symbols),
        ("Nodes", facts.graph.nodes),
        ("Edges", facts.graph.edges),
        ("Communities", facts.graph.communities),
        ("Dangling links", facts.graph.dangling),
        ("Missing nodes", facts.graph.missing),
        ("Ambiguous edges", facts.graph.ambiguous),
    ] {
        out.push_str(&format!("| {label} | {value} |\n"));
    }
    out.push('\n');

    out.push_str("### 9.1 Hubs\n\n");
    if facts.graph.hubs.is_empty() {
        out.push_str("- none yet\n");
    } else {
        for (label, kind, degree) in &facts.graph.hubs {
            out.push_str(&format!(
                "- {} — `{kind}` · {} connections\n",
                label, degree
            ));
        }
    }
    out.push('\n');

    out.push_str("### 9.2 Communities\n\n");
    if facts.graph.community_rows.is_empty() {
        out.push_str("- none yet\n");
    } else {
        out.push_str("| Community | Nodes | Cohesion | Notes |\n| --- | --- | --- | --- |\n");
        for (label, size, cohesion, members) in &facts.graph.community_rows {
            out.push_str(&format!(
                "| {} | {} | {:.2} | {} |\n",
                clean_cell(label),
                size,
                cohesion,
                if members.is_empty() {
                    "—".to_string()
                } else {
                    clean_cell(&members.join(", "))
                }
            ));
        }
    }
    out.push('\n');

    out.push_str("## 10. Decisions\n\n");
    for (index, (title, decision, alternative)) in DECISIONS.iter().enumerate() {
        out.push_str(&format!(
            "### 10.{} {}\n\n{}\n\n*Rejected alternative:* {}\n\n",
            index + 1,
            title,
            decision,
            alternative
        ));
    }

    out.push_str("## Appendix A — Source inventory\n\n");
    if let Some(source) = facts.source {
        out.push_str(&format!(
            "Scanned `{}`: {} files, {} directories, {}.\n\n",
            source.root,
            source.files,
            source.dirs,
            human_bytes(source.bytes)
        ));

        out.push_str("### Languages\n\n| Language | Files |\n| --- | --- |\n");
        if source.languages.is_empty() {
            out.push_str("| — | 0 |\n");
        } else {
            for (language, count) in &source.languages {
                out.push_str(&format!("| {language} | {count} |\n"));
            }
        }
        out.push('\n');

        out.push_str("### Manifests\n\n");
        if source.manifests.is_empty() {
            out.push_str("- none found\n");
        } else {
            for manifest in &source.manifests {
                let name = manifest
                    .name
                    .as_deref()
                    .map(|name| format!(" (`{name}`)"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "- `{}` — {}{name}\n",
                    manifest.path, manifest.kind
                ));
                for (key, value) in &manifest.details {
                    out.push_str(&format!("  - {key}: {value}\n"));
                }
            }
        }
        out.push('\n');

        out.push_str("### Entry points\n\n");
        if source.entry_points.is_empty() {
            out.push_str("- none detected\n");
        } else {
            for path in &source.entry_points {
                out.push_str(&format!("- `{path}`\n"));
            }
        }
        out.push('\n');

        out.push_str("### Tests\n\n");
        if source.tests.is_empty() {
            out.push_str("- none detected\n");
        } else {
            out.push_str(&format!("{} test files detected:\n\n", source.tests.len()));
            for path in source.tests.iter().take(MAX_LIST) {
                out.push_str(&format!("- `{path}`\n"));
            }
            if source.tests.len() > MAX_LIST {
                out.push_str(&format!("- … {} more\n", source.tests.len() - MAX_LIST));
            }
        }
        out.push('\n');

        out.push_str("### README headings\n\n");
        if source.readme_headings.is_empty() {
            out.push_str("- none found\n");
        } else {
            for heading in &source.readme_headings {
                out.push_str(&format!("- {}\n", clean_cell(heading)));
            }
        }
        out.push('\n');
    } else {
        out.push_str(
            "No source root is linked, so there is no code inventory to report. Pass `--source`\n\
             or set `root` in the project manifest to include one.\n\n",
        );
    }

    out.push_str("## Appendix B — Glossary\n\n");
    if facts.tags.is_empty() {
        out.push_str("No tags have been captured in this project yet.\n");
    } else {
        out.push_str("| Tag | Notes |\n| --- | --- |\n");
        for (tag, count) in &facts.tags {
            out.push_str(&format!("| `{tag}` | {count} |\n"));
        }
        out.push('\n');
    }

    out
}

fn render_note_doc(facts: &Facts<'_>) -> String {
    let mut out = String::new();
    out.push_str(&format!("# {} — note index\n\n", facts.name));
    out.push_str(&format!(
        "{} notes are captured in this project. This file is generated; edit the notes, not this\n\
         index.\n\n",
        facts.notes.len()
    ));

    out.push_str("## Index\n\n");
    out.push_str("| Note | Tags | Updated | Headings |\n| --- | --- | --- | --- |\n");
    for note in &facts.notes {
        out.push_str(&format!(
            "| [{}]({}) | {} | {} | {} |\n",
            clean_cell(&note.title),
            note_file(&note.id),
            if note.tags.is_empty() {
                "—".to_string()
            } else {
                clean_cell(&note.tags.join(", "))
            },
            note.updated,
            note.headings.len()
        ));
    }
    out.push('\n');

    out.push_str("## Outlines\n\n");
    if facts.notes.is_empty() {
        out.push_str(
            "No notes yet. Capture one through the agent, then regenerate this document.\n",
        );
    } else {
        for note in &facts.notes {
            out.push_str(&format!("### {}\n\n", clean_cell(&note.title)));
            if note.headings.is_empty() {
                out.push_str("- no headings\n\n");
                continue;
            }
            for (level, heading) in &note.headings {
                let indent = "  ".repeat(level.saturating_sub(1));
                out.push_str(&format!("{indent}- {}\n", clean_cell(heading)));
            }
            out.push('\n');
        }
    }
    out
}

fn abstract_text(facts: &Facts<'_>) -> String {
    let source_phrase = facts
        .source
        .map(|source| format!(", {} files in the linked source tree", source.files))
        .unwrap_or_default();
    format!(
        "{} is documented from {} Markdown notes{}, a knowledge graph of {} nodes and {} edges, \
         and the Luna CLI surface. It records the system's architecture, its system flow, the \
         user flows that produce and consume it, and the decisions behind the current layout.",
        facts.name,
        facts.notes.len(),
        source_phrase,
        facts.graph.nodes,
        facts.graph.edges
    )
}

fn operation_commands(facts: &Facts<'_>) -> Vec<(String, String)> {
    let mut commands = vec![
        (
            format!("luna notes graph --project {}", facts.slug),
            "Rebuild this project's knowledge graph.".to_string(),
        ),
        (
            format!("luna notes docs --project {}", facts.slug),
            "Regenerate this documentation.".to_string(),
        ),
    ];

    let Some(source) = facts.source else {
        return commands;
    };

    for manifest in &source.manifests {
        match manifest.kind.as_str() {
            "Cargo" => {
                commands.push((
                    "cargo build --release".to_string(),
                    "Build the workspace in release mode.".to_string(),
                ));
                commands.push(("cargo test".to_string(), "Run the test suite.".to_string()));
                commands.push((
                    "cargo clippy --all-targets -- -D warnings".to_string(),
                    "Lint every target, failing on warnings.".to_string(),
                ));
            }
            "npm" => {
                commands.push((
                    "npm ci".to_string(),
                    "Install dependencies from the lockfile.".to_string(),
                ));
                let has_test = manifest
                    .details
                    .iter()
                    .any(|(key, value)| key == "scripts" && value.contains("test"));
                commands.push((
                    if has_test {
                        "npm test".to_string()
                    } else {
                        "npm run build".to_string()
                    },
                    if has_test {
                        "Run the package's test script.".to_string()
                    } else {
                        "Build the package.".to_string()
                    },
                ));
            }
            "Python" => {
                commands.push((
                    "python -m pytest".to_string(),
                    "Run the test suite.".to_string(),
                ));
            }
            "Go" => {
                commands.push((
                    "go build ./...".to_string(),
                    "Build every package.".to_string(),
                ));
                commands.push((
                    "go test ./...".to_string(),
                    "Run the test suite.".to_string(),
                ));
            }
            _ => {}
        }
    }

    commands.sort_by(|left, right| left.0.cmp(&right.0));
    commands.dedup_by(|left, right| left.0 == right.0);
    commands
}

const DECISIONS: &[(&str, &str, &str)] = &[
    (
        "Project folders are the source of truth",
        "A note belongs to the project whose folder contains it. The folder is the index; there is \
         no project table to drift out of sync, and copying or syncing a folder carries its \
         project with it.",
        "A database column for the project, which would need a migration, a repair path, and a \
         second source of truth next to the files.",
    ),
    (
        "Save routing is tag-driven",
        "A `project:<slug>` tag chooses the destination folder. Untagged notes fall back to \
         `inbox`, so the save API stays unchanged for callers that never learned about projects.",
        "A required project argument on every save call, which would break existing tools and \
         make capture slower than the thought being captured.",
    ),
    (
        "One combined graph plus per-project graphs",
        "Each project graph answers questions inside its scope, while the combined graph keeps \
         cross-project links resolvable. Both are written from one read of the notes.",
        "Only per-project graphs, which leave citations between projects permanently dangling.",
    ),
    (
        "Deterministic documentation instead of model-written prose",
        "Every section is assembled from notes, graph data, a source snapshot, and `--help`. The \
         output is reproducible, diffable, and costs no tokens or network calls.",
        "Prompting a model on every run, which gives nicer prose but drifts, costs tokens, and \
         cannot be verified in CI.",
    ),
    (
        "Scaffold documentation when a project is created",
        "A new project gets `docs/README.md` and `docs/system.md` immediately, so the folder is \
         useful before the first generation and the expected sections are visible.",
        "Creating empty directories, which hides what the documentation is supposed to contain.",
    ),
];

fn note_headings(content: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut fenced = false;
    for line in content.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if !(1..=3).contains(&hashes) {
            continue;
        }
        let rest = &trimmed[hashes..];
        if !rest.starts_with(' ') && !rest.starts_with('\t') {
            continue;
        }
        let title = rest.trim().trim_end_matches('#').trim();
        if !title.is_empty() {
            out.push((hashes, title.to_string()));
        }
    }
    out
}

fn note_file(id: &str) -> String {
    format!("../{NOTES_SUBDIR}/{id}.md")
}

fn clean_cell(text: &str) -> String {
    text.replace('|', "\\|")
        .replace(['\n', '\r'], " ")
        .trim()
        .to_string()
}

fn slug_for_anchor(text: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

// ---------------------------------------------------------------------------
// Source scan
// ---------------------------------------------------------------------------

/// Inventory a source tree without following symlinks or reading file bodies twice.
///
/// The walk is bounded in depth and file count, skips build and dependency directories, and
/// sorts every list before returning, so two runs over the same tree agree byte for byte.
pub fn scan_source(root: &Path) -> Result<SourceSnapshot, HarnessError> {
    if !root.is_dir() {
        return Err(HarnessError::NotesError(format!(
            "source root '{}' is not a directory",
            root.display()
        )));
    }
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());

    let mut snapshot = SourceSnapshot {
        root: root.display().to_string(),
        ..Default::default()
    };
    let mut languages: BTreeMap<String, usize> = BTreeMap::new();
    let mut top_level = Vec::new();
    let mut stack = vec![(root.clone(), 0usize)];
    let mut seen_files = 0usize;

    while let Some((dir, depth)) = stack.pop() {
        if seen_files >= MAX_FILES {
            break;
        }
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        let mut children: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        children.sort();

        for path in children {
            let Ok(metadata) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.file_type().is_symlink() {
                continue;
            }
            let relative = path
                .strip_prefix(&root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("")
                .to_string();

            if metadata.is_dir() {
                snapshot.dirs += 1;
                if depth == 0 {
                    top_level.push(format!("{file_name}/"));
                }
                if depth < MAX_DEPTH && !SKIP_DIRS.contains(&file_name.as_str()) {
                    stack.push((path, depth + 1));
                }
                continue;
            }

            if !metadata.is_file() {
                continue;
            }
            seen_files += 1;
            snapshot.files += 1;
            snapshot.bytes += metadata.len();

            let language = path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.to_ascii_lowercase())
                .unwrap_or_else(|| "(none)".to_string());
            *languages.entry(language).or_default() += 1;

            if depth == 0 && path.is_file() {
                top_level.push(file_name.clone());
            }

            if looks_like_entry_point(&relative) {
                snapshot.entry_points.push(relative.clone());
            }
            if looks_like_test(&relative) {
                snapshot.tests.push(relative.clone());
            }

            if let Some(manifest) = read_manifest(&path, &relative) {
                snapshot.manifests.push(manifest);
            }
            if depth == 0 && file_name.eq_ignore_ascii_case("readme.md") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    snapshot.readme_headings =
                        text.lines().filter_map(atx_title).take(12).collect();
                    snapshot.readme_intro = first_paragraph(&text);
                }
            }
        }
    }

    snapshot.languages = languages.into_iter().collect();
    snapshot
        .languages
        .sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    snapshot
        .manifests
        .sort_by(|left, right| left.path.cmp(&right.path));
    snapshot.entry_points.sort();
    snapshot.entry_points.dedup();
    snapshot.entry_points.truncate(MAX_LIST);
    snapshot.tests.sort();
    snapshot.tests.dedup();
    snapshot.top_level.sort();
    snapshot.top_level.dedup();
    Ok(snapshot)
}

fn atx_title(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let hashes = trimmed.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = trimmed[hashes..].trim();
    if rest.is_empty() {
        None
    } else {
        Some(rest.trim_end_matches('#').trim().to_string())
    }
}

fn first_paragraph(text: &str) -> Option<String> {
    let mut paragraph = String::new();
    let mut fenced = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if trimmed.is_empty() {
            if !paragraph.is_empty() {
                break;
            }
            continue;
        }
        if trimmed.starts_with('#')
            || trimmed.starts_with("![")
            || trimmed.starts_with("[!")
            || trimmed.starts_with('>')
            || trimmed.starts_with('<')
        {
            continue;
        }
        if !paragraph.is_empty() {
            paragraph.push(' ');
        }
        paragraph.push_str(trimmed);
        if paragraph.chars().count() >= 400 {
            break;
        }
    }
    let paragraph = paragraph.trim().to_string();
    if paragraph.is_empty() {
        None
    } else if paragraph.chars().count() > 400 {
        Some(paragraph.chars().take(400).collect::<String>() + "…")
    } else {
        Some(paragraph)
    }
}

fn looks_like_entry_point(relative: &str) -> bool {
    const NAMES: &[&str] = &[
        "src/main.rs",
        "src/lib.rs",
        "main.rs",
        "src/index.ts",
        "src/index.js",
        "src/main.ts",
        "src/main.js",
        "index.ts",
        "index.js",
        "main.py",
        "app.py",
        "__main__.py",
        "main.go",
    ];
    NAMES.contains(&relative)
        || (relative.starts_with("src/bin/") && relative.ends_with(".rs"))
        || (relative.starts_with("cmd/") && relative.ends_with("/main.go"))
        || (relative.ends_with("/src/main.rs") && relative.matches('/').count() <= 2)
}

fn looks_like_test(relative: &str) -> bool {
    relative
        .split('/')
        .any(|part| part == "tests" || part == "test")
        || relative.rsplit('/').next().is_some_and(|name| {
            name.starts_with("test_")
                || name.contains("_test.")
                || name.contains(".test.")
                || name.contains("_spec.")
                || name.contains(".spec.")
        })
}

fn read_manifest(path: &Path, relative: &str) -> Option<ManifestFact> {
    let name = path.file_name().and_then(|name| name.to_str())?;
    let kind = match name {
        "Cargo.toml" => "Cargo",
        "package.json" => "npm",
        "pyproject.toml" | "setup.py" | "requirements.txt" => "Python",
        "go.mod" => "Go",
        _ => return None,
    };

    let text = std::fs::read_to_string(path).ok()?;
    let mut details = Vec::new();
    let package_name = match name {
        "Cargo.toml" => cargo_name(&text),
        "package.json" => {
            let value: serde_json::Value = serde_json::from_str(&text).ok()?;
            if let Some(scripts) = value.get("scripts").and_then(|scripts| scripts.as_object()) {
                let keys: Vec<&str> = scripts.keys().map(String::as_str).collect();
                if !keys.is_empty() {
                    details.push(("scripts".to_string(), keys.join(", ")));
                }
            }
            value
                .get("name")
                .and_then(|name| name.as_str())
                .map(str::to_string)
        }
        "go.mod" => text
            .lines()
            .find_map(|line| line.trim().strip_prefix("module "))
            .map(|module| module.trim().to_string()),
        _ => first_toml_name(&text),
    };

    Some(ManifestFact {
        kind: kind.to_string(),
        path: relative.to_string(),
        name: package_name,
        details,
    })
}

fn cargo_name(text: &str) -> Option<String> {
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package {
            if let Some((key, value)) = line.split_once('=') {
                if key.trim() == "name" {
                    let value = value.trim().trim_matches('"').trim_matches('\'');
                    if !value.is_empty() {
                        return Some(value.to_string());
                    }
                }
            }
        }
    }
    None
}

fn first_toml_name(text: &str) -> Option<String> {
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "name" {
                let value = value.trim().trim_matches('"').trim_matches('\'');
                if !value.is_empty() {
                    return Some(value.to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use harness_core::Note;
    use harness_note_graph::graph_notes;

    fn note(id: &str, title: &str, content: &str, tags: &[&str]) -> Note {
        Note {
            title: title.to_string(),
            id: id.to_string(),
            content: content.to_string(),
            summary_pointer: String::new(),
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            parent_id: None,
            part_number: None,
            part_count: None,
            content_hash: None,
        }
    }

    fn project(dir: &Path) -> Project {
        Project {
            slug: "demo-app".to_string(),
            name: "Demo App".to_string(),
            dir: dir.to_path_buf(),
        }
    }

    fn cli() -> CliFacts {
        CliFacts {
            version: "0.1.8".to_string(),
            top_help: "Usage: luna [OPTIONS] [COMMAND]".to_string(),
            notes_help: "Usage: luna notes <COMMAND>".to_string(),
            subcommands: vec![("notes".to_string(), "Notes workspace".to_string())],
        }
    }

    #[test]
    fn docs_are_deterministic_and_cover_the_required_sections() {
        let notes = vec![
            note(
                "release-notes",
                "Release notes",
                "# Release notes\n\nSee [[System overview]].",
                &["project:demo-app", "release"],
            ),
            note(
                "system-overview",
                "System overview",
                "# System overview\n\n## Storage\n\nMarkdown on disk.",
                &["project:demo-app", "architecture"],
            ),
        ];
        let built = graph_notes(&notes);
        let dir = PathBuf::from("/notes/demo-app");
        let input = DocInput {
            project: &project(&dir),
            created_at: "2026-01-01T00:00:00Z",
            notes: &notes,
            built: &built,
            source: None,
            cli: &cli(),
        };

        let first = render_docs(&input);
        let second = render_docs(&input);
        assert_eq!(first, second, "render must be byte-stable");

        let files: Vec<&str> = first.iter().map(|doc| doc.file.as_str()).collect();
        assert_eq!(files, vec![README_FILE, SYSTEM_FILE, NOTES_FILE]);

        let system = &first[1].content;
        for heading in [
            "## 1. Introduction",
            "## 4. System flow",
            "## 5. User flows",
            "## 6. Commands and interfaces",
            "## 7. Data and storage",
            "## 9. Knowledge graph",
            "## 10. Decisions",
            "## Appendix A — Source inventory",
            "## Appendix B — Glossary",
        ] {
            assert!(system.contains(heading), "missing {heading}");
        }
        assert!(system.contains("Demo App"));
        assert!(system.contains("Release notes"));
        assert!(system.contains("architecture"));
        assert!(system.contains("project:demo-app"));

        let index = &first[2].content;
        assert!(index.contains("Release notes"));
        assert!(index.contains("System overview"));
        assert!(index.contains("Storage"));
    }

    #[test]
    fn multiple_notes_with_the_same_title_are_ordered_by_id() {
        let notes = vec![
            note("zeta", "Same", "# Same", &[]),
            note("alpha", "Same", "# Same", &[]),
        ];
        let built = graph_notes(&notes);
        let dir = PathBuf::from("/notes/demo-app");
        let input = DocInput {
            project: &project(&dir),
            created_at: "2026-01-01T00:00:00Z",
            notes: &notes,
            built: &built,
            source: None,
            cli: &cli(),
        };
        let docs = render_docs(&input);
        let index = &docs[2].content;
        let alpha = index.find("[Same](../notes/alpha.md)").expect("alpha row");
        let zeta = index.find("[Same](../notes/zeta.md)").expect("zeta row");
        assert!(alpha < zeta);
    }

    #[test]
    fn scan_source_is_sorted_and_skips_build_output() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        std::fs::create_dir_all(root.join("src")).expect("src");
        std::fs::write(root.join("src/main.rs"), "fn main() {}\n").expect("main");
        std::fs::write(root.join("src/lib.rs"), "pub fn f() {}\n").expect("lib");
        std::fs::create_dir_all(root.join("tests")).expect("tests dir");
        std::fs::write(root.join("tests/api_test.rs"), "#[test]\nfn ok() {}\n").expect("test");
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"demo-app\"\nversion = \"0.1.0\"\n",
        )
        .expect("cargo");
        std::fs::write(
            root.join("README.md"),
            "# Demo\n\nA small demo application.\n\n## Usage\n",
        )
        .expect("readme");
        std::fs::create_dir_all(root.join("target/debug")).expect("target");
        std::fs::write(root.join("target/debug/junk.rs"), "junk").expect("junk");

        let first = scan_source(root).expect("scan");
        let second = scan_source(root).expect("scan again");
        assert_eq!(first, second);

        assert_eq!(first.root, root.canonicalize().expect("canonical"));
        assert!(first.files >= 4);
        assert!(first.languages.contains(&("rs".to_string(), 3)));
        assert!(first.entry_points.contains(&"src/main.rs".to_string()));
        assert!(first.entry_points.contains(&"src/lib.rs".to_string()));
        assert!(first.tests.contains(&"tests/api_test.rs".to_string()));
        assert!(first.manifests.iter().any(
            |manifest| manifest.kind == "Cargo" && manifest.name.as_deref() == Some("demo-app")
        ));
        assert_eq!(
            first.readme_intro.as_deref(),
            Some("A small demo application.")
        );
        assert!(first.readme_headings.contains(&"Demo".to_string()));
        assert!(!first
            .entry_points
            .iter()
            .any(|path| path.contains("target/")));
    }

    #[test]
    fn scan_source_rejects_a_non_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("file.txt");
        std::fs::write(&file, "x").expect("write");
        assert!(scan_source(&file).is_err());
    }
}
