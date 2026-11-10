//! Per-project folders inside the notes directory.
//!
//! A notes directory is a workspace of projects, not a flat pile of files:
//!
//! ```text
//! ~/.luna/notes/
//!   inbox/            # notes that predate projects, and untagged captures
//!     project.json
//!     notes/<slug>.md
//!     docs/README.md
//!     graph/          # per-project knowledge graph artifacts
//!   my-app/
//!     project.json
//!     notes/...
//!     docs/...
//!     graph/...
//! ```
//!
//! The folder is the source of truth: a note lives in `<project>/notes/` and its project is
//! derived from that path, so the layout survives copying, syncing, or hand-editing. The
//! `project:<slug>` tag is injected on read when a note does not already carry one, which is what
//! makes the tag a first-class graph node without a second index.
//!
//! Everything here is deterministic and offline: the same directory always produces the same
//! project list, and no network call is made.

use std::path::{Path, PathBuf};

use chrono::Utc;
use harness_core::HarnessError;
use serde::{Deserialize, Serialize};

/// Project that owns notes with no explicit project and every note from before the layout change.
pub const DEFAULT_PROJECT: &str = "inbox";

/// Manifest file that marks a directory as a project.
pub const MANIFEST: &str = "project.json";

/// Markdown notes for the project.
pub const NOTES_SUBDIR: &str = "notes";

/// Generated system documentation for the project.
pub const DOCS_SUBDIR: &str = "docs";

/// Generated knowledge graph artifacts for the project.
pub const GRAPH_SUBDIR: &str = "graph";

/// Marker written once the flat layout has been migrated. Dot-prefixed so it is never read as a
/// note.
const MIGRATION_MARKER: &str = ".projects-v1";

/// Frontmatter prefix that binds a note to a project. `project:my-app` and `project/my-app` are
/// both accepted because frontmatter is comma separated and a user may type either.
pub const PROJECT_TAG_PREFIX: &str = "project:";

/// One project directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Project {
    pub slug: String,
    pub name: String,
    pub dir: PathBuf,
}

impl Project {
    /// Directory holding the project's Markdown notes.
    pub fn notes_dir(&self) -> PathBuf {
        self.dir.join(NOTES_SUBDIR)
    }

    /// Directory holding the project's generated documentation.
    pub fn docs_dir(&self) -> PathBuf {
        self.dir.join(DOCS_SUBDIR)
    }

    /// Directory holding the project's graph artifacts.
    pub fn graph_dir(&self) -> PathBuf {
        self.dir.join(GRAPH_SUBDIR)
    }
}

/// Manifest written to `<project>/project.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectManifest {
    pub name: String,
    pub slug: String,
    pub created_at: String,
    /// Layout version, so a future change can migrate without guessing.
    #[serde(default = "current_layout")]
    pub layout: u32,
}

fn current_layout() -> u32 {
    1
}

/// Turns a display name into a folder-safe slug. Mirrors [`crate::file_engine::slugify`] but is
/// kept separate so project naming can diverge from note naming without coupling the two.
pub fn slugify_project(name: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for c in name.trim().chars() {
        if c.is_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if (c.is_whitespace() || matches!(c, '-' | '_' | '.' | '/' | '\\'))
            && !slug.is_empty()
            && !prev_dash
        {
            slug.push('-');
            prev_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        DEFAULT_PROJECT.to_string()
    } else {
        slug
    }
}

/// Whether `slug` is safe to use as a single path component.
fn is_safe_slug(slug: &str) -> bool {
    !slug.is_empty()
        && !slug.starts_with('.')
        && !slug.contains('/')
        && !slug.contains('\\')
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

/// The project a note path belongs to, if it sits in `<project>/notes/`.
pub fn project_of_path(notes_dir: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(notes_dir).ok()?;
    let mut components = relative.components();
    let first = components.next()?.as_os_str().to_str()?;
    let second = components.next()?.as_os_str().to_str()?;
    if second == NOTES_SUBDIR && is_safe_slug(first) {
        Some(first.to_string())
    } else {
        None
    }
}

/// Reads the project name from `<dir>/project.json`, falling back to the folder slug.
fn manifest_name(dir: &Path, slug: &str) -> String {
    std::fs::read_to_string(dir.join(MANIFEST))
        .ok()
        .and_then(|raw| serde_json::from_str::<ProjectManifest>(&raw).ok())
        .map(|manifest| manifest.name)
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| slug.replace('-', " "))
}

/// Every project under `notes_dir`, sorted by slug.
///
/// A directory counts as a project when it has a manifest or a `notes/` folder, which keeps
/// unrelated folders (a hand-created `drafts/`, an editor's `.obsidian/`) out of the list. The
/// default project is included even before it is created so callers always have a target.
pub fn list(notes_dir: &Path) -> Result<Vec<Project>, HarnessError> {
    if !notes_dir.is_dir() {
        return Ok(Vec::new());
    }

    let entries = std::fs::read_dir(notes_dir).map_err(|error| {
        HarnessError::NotesError(format!(
            "Failed to read notes directory '{}': {}",
            notes_dir.display(),
            error
        ))
    })?;

    let mut projects = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(slug) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_safe_slug(slug) {
            continue;
        }
        let looks_like_project =
            path.join(MANIFEST).is_file() || path.join(NOTES_SUBDIR).is_dir();
        if !looks_like_project {
            continue;
        }
        projects.push(Project {
            name: manifest_name(&path, slug),
            slug: slug.to_string(),
            dir: path,
        });
    }

    projects.sort_by(|left, right| left.slug.cmp(&right.slug));
    Ok(projects)
}

/// Creates the folder skeleton for `name` (idempotent) and returns it.
pub fn ensure(notes_dir: &Path, name: &str) -> Result<Project, HarnessError> {
    let slug = slugify_project(name);
    let dir = notes_dir.join(&slug);
    let project = Project {
        slug,
        name: name.trim().to_string(),
        dir,
    };

    for path in [
        project.notes_dir(),
        project.docs_dir(),
        project.graph_dir(),
    ] {
        std::fs::create_dir_all(&path).map_err(|error| {
            HarnessError::NotesError(format!(
                "Failed to create project directory '{}': {}",
                path.display(),
                error
            ))
        })?;
    }

    let manifest_path = project.dir.join(MANIFEST);
    if !manifest_path.is_file() {
        let manifest = ProjectManifest {
            name: project.name.clone(),
            slug: project.slug.clone(),
            created_at: Utc::now().to_rfc3339(),
            layout: current_layout(),
        };
        let json = serde_json::to_string_pretty(&manifest)
            .map_err(|error| HarnessError::SerializationError(error.to_string()))?;
        std::fs::write(&manifest_path, format!("{json}\n")).map_err(|error| {
            HarnessError::NotesError(format!(
                "Failed to write project manifest '{}': {}",
                manifest_path.display(),
                error
            ))
        })?;
    }

    write_docs_scaffold(&project)?;
    Ok(project)
}

/// The documentation a project starts with.
///
/// This is a scaffold, not a summary: it names the sections the capstone-style document needs and
/// marks what is still missing, so the file is useful before the generator fills it in and stays
/// honest afterwards.
fn write_docs_scaffold(project: &Project) -> Result<(), HarnessError> {
    let readme_path = project.docs_dir().join("README.md");
    if readme_path.exists() {
        return Ok(());
    }

    let readme = format!(
        "# {} — system documentation\n\n\
        Generated and hand-written documentation for this project lives here. The note graph for\n\
        the project is in `../{GRAPH_SUBDIR}/`, and the Markdown notes are in `../{NOTES_SUBDIR}/`.\n\n\
        ## Contents\n\n\
        1. [System overview](system.md#system-overview) — what the system is and who it serves.\n\
        2. [Architecture](system.md#architecture) — components, boundaries, and data flow.\n\
        3. [User flows](system.md#user-flows) — terminal entry points and every step a user takes.\n\
        4. [Commands and interfaces](system.md#commands-and-interfaces) — the CLI surface.\n\
        5. [Data and storage](system.md#data-and-storage) — where state lives and how it moves.\n\
        6. [Operations](system.md#operations) — build, test, deploy, recover.\n\
        7. [Decisions](system.md#decisions) — what was chosen, and what was rejected.\n\n\
        Regenerate the graph with `luna notes graph --project {slug}`.\n",
        project.name,
        slug = project.slug,
        GRAPH_SUBDIR = GRAPH_SUBDIR,
        NOTES_SUBDIR = NOTES_SUBDIR,
    );

    let system = format!(
        "# {name} — system documentation\n\n\
        > Status: scaffold. Sections marked TODO are filled from code, `--help` output, tests and\n\
        > the note graph; nothing here is invented by a model at runtime.\n\n\
        ## System overview\n\n\
        TODO: one paragraph on the problem, the users, and the outcome.\n\n\
        ## Architecture\n\n\
        TODO: components and their responsibilities.\n\n\
        | Component | Responsibility | Source |\n\
        | --- | --- | --- |\n\
        | | | |\n\n\
        ## User flows\n\n\
        Each flow is a numbered path from a terminal prompt to an observable result.\n\n\
        ### First run\n\n\
        1. TODO\n\n\
        ### Daily use\n\n\
        1. TODO\n\n\
        ## Commands and interfaces\n\n\
        TODO: the CLI surface, one line per command, sourced from `--help`.\n\n\
        ```text\n\
        $ luna notes graph --project {slug}\n\
        ```\n\n\
        ## Data and storage\n\n\
        - Notes: `../{notes}/<slug>.md`\n\
        - Graph artifacts: `../{graph}/`\n\
        - Manifest: `../{manifest}`\n\n\
        ## Operations\n\n\
        TODO: build, test, release, rollback.\n\n\
        ## Decisions\n\n\
        TODO: one entry per decision, with the alternative that was rejected.\n",
        name = project.name,
        slug = project.slug,
        notes = NOTES_SUBDIR,
        graph = GRAPH_SUBDIR,
        manifest = MANIFEST,
    );

    for (path, contents) in [(&readme_path, readme), (&project.docs_dir().join("system.md"), system)]
    {
        std::fs::write(path, contents).map_err(|error| {
            HarnessError::NotesError(format!(
                "Failed to write documentation '{}': {}",
                path.display(),
                error
            ))
        })?;
    }
    Ok(())
}

/// Moves top-level `*.md` files into `inbox/notes/`, once.
///
/// Returns how many notes moved. The marker makes this a no-op forever after, so a user who later
/// drops a loose `.md` into the notes root keeps it there deliberately instead of having it
/// silently relocated on the next start.
pub fn migrate_flat(notes_dir: &Path) -> Result<usize, HarnessError> {
    let marker = notes_dir.join(MIGRATION_MARKER);
    if marker.exists() {
        return Ok(0);
    }

    let inbox = ensure(notes_dir, DEFAULT_PROJECT)?;
    let mut moved = 0usize;

    if let Ok(entries) = std::fs::read_dir(notes_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Some(name) = path.file_name() else {
                continue;
            };
            let target = inbox.notes_dir().join(name);
            if target.exists() {
                continue;
            }
            if std::fs::rename(&path, &target).is_ok() {
                moved += 1;
            }
        }
    }

    std::fs::write(&marker, b"1\n").map_err(|error| {
        HarnessError::NotesError(format!(
            "Failed to write migration marker '{}': {}",
            marker.display(),
            error
        ))
    })?;
    Ok(moved)
}

/// The project named by a note's tags.
pub fn project_from_tags(tags: &[String]) -> Option<String> {
    for tag in tags {
        let trimmed = tag.trim();
        let value = trimmed
            .strip_prefix(PROJECT_TAG_PREFIX)
            .or_else(|| trimmed.strip_prefix("project/"));
        if let Some(value) = value {
            let slug = slugify_project(value);
            if is_safe_slug(&slug) {
                return Some(slug);
            }
        }
    }
    None
}

/// Adds `project:<slug>` to `tags` unless the note already names a project.
pub fn tag_for_project(tags: &mut Vec<String>, slug: &str) {
    if project_from_tags(tags).is_none() {
        tags.push(format!("{PROJECT_TAG_PREFIX}{slug}"));
    }
}

/// The slug a note id or path refers to: `my-app/build-notes` or a bare id.
pub fn split_qualified(id: &str) -> (Option<&str>, &str) {
    match id.split_once('/') {
        Some((project, leaf)) if is_safe_slug(project) && !leaf.is_empty() => {
            (Some(project), leaf)
        }
        _ => (None, id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugify_project_folds_punctuation() {
        assert_eq!(slugify_project("My App"), "my-app");
        assert_eq!(slugify_project("  Luna / Harness  "), "luna-harness");
        assert_eq!(slugify_project(""), DEFAULT_PROJECT);
    }

    #[test]
    fn ensure_creates_the_layout_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let project = ensure(dir.path(), "My App").expect("ensure");
        assert_eq!(project.slug, "my-app");
        assert!(project.notes_dir().is_dir());
        assert!(project.docs_dir().is_dir());
        assert!(project.graph_dir().is_dir());
        assert!(project.dir.join(MANIFEST).is_file());
        assert!(project.docs_dir().join("system.md").is_file());

        // Idempotent: a second call keeps the manifest and does not rewrite docs.
        let before = std::fs::metadata(project.docs_dir().join("system.md"))
            .and_then(|m| m.modified())
            .ok();
        ensure(dir.path(), "My App").expect("ensure again");
        let after = std::fs::metadata(project.docs_dir().join("system.md"))
            .and_then(|m| m.modified())
            .ok();
        assert_eq!(before, after);
    }

    #[test]
    fn list_finds_projects_and_ignores_other_directories() {
        let dir = tempfile::tempdir().expect("tempdir");
        ensure(dir.path(), "Alpha").expect("alpha");
        std::fs::create_dir_all(dir.path().join("scratch")).expect("scratch");

        let projects = list(dir.path()).expect("list");
        let slugs: Vec<&str> = projects.iter().map(|p| p.slug.as_str()).collect();
        assert_eq!(slugs, vec!["alpha"]);
    }

    #[test]
    fn migrate_flat_moves_top_level_notes_into_inbox() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("one.md"), "# one\n").expect("write");
        std::fs::write(dir.path().join("two.md"), "# two\n").expect("write");

        assert_eq!(migrate_flat(dir.path()).expect("migrate"), 2);
        assert!(dir.path().join("inbox/notes/one.md").is_file());
        assert!(dir.path().join("inbox/notes/two.md").is_file());
        assert!(!dir.path().join("one.md").exists());

        // The marker makes it a one-time move.
        std::fs::write(dir.path().join("three.md"), "# three\n").expect("write");
        assert_eq!(migrate_flat(dir.path()).expect("migrate again"), 0);
        assert!(dir.path().join("three.md").is_file());
    }

    #[test]
    fn tags_round_trip_to_a_project() {
        let mut tags = vec!["rust".to_string(), "project:My App".to_string()];
        assert_eq!(project_from_tags(&tags).as_deref(), Some("my-app"));
        tag_for_project(&mut tags, "other");
        assert_eq!(tags.len(), 2, "an existing project tag is not duplicated");
    }

    #[test]
    fn project_of_path_reads_the_layout() {
        let root = Path::new("/notes");
        assert_eq!(
            project_of_path(root, Path::new("/notes/app/notes/x.md")).as_deref(),
            Some("app")
        );
        assert_eq!(project_of_path(root, Path::new("/notes/x.md")), None);
    }
}
