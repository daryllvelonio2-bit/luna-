//! File-backed notes: one markdown file per note, in a folder the user owns.
//!
//! A note is not a row. It is a document the user can open, edit, grep, or commit like any other
//! file, which is the whole point of this engine: `paths.notes_dir` holds `<slug>.md`, front
//! matter carries the metadata the harness needs (title, pointer, tags, timestamps), and the body
//! below the fence is verbatim note content.
//!
//! The legacy `notes.db` is read exactly once, to import the notes an older install left behind.
//! After a successful import the database is renamed to `notes.db.imported`, so it is never read
//! twice and the old data stays on disk as a backup.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

use crate::distiller::PointerDistiller;
use harness_core::{HarnessError, Note, NotesEngine};

/// Marks the end of the front matter block.
const FRONT_MATTER_FENCE: &str = "---";

/// Turns a title into a filename-safe slug (`Stripe Spec 2026` → `stripe-spec-2026`).
pub fn slugify(text: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for c in text.trim().chars() {
        if c.is_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if (c.is_whitespace() || c == '-' || c == '_' || c == '.' || c == '/')
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
        "note".to_string()
    } else {
        slug
    }
}

/// Validates that an identifier is safe to use as a file stem inside the notes directory.
///
/// Refuses empty strings, traversal sequences, path separators (`/`, `\`), leading dots,
/// and absolute paths.
pub fn is_safe_note_id(id: &str) -> bool {
    let trimmed = id.trim();
    if trimmed.is_empty()
        || trimmed.starts_with('.')
        || trimmed.contains('/')
        || trimmed.contains('\\')
    {
        return false;
    }
    let p = Path::new(trimmed);
    if p.is_absolute() {
        return false;
    }
    let mut components = p.components();
    matches!(
        (components.next(), components.next()),
        (Some(std::path::Component::Normal(_)), None)
    )
}

pub struct FileNotesEngine {
    dir: PathBuf,
    cache: std::sync::RwLock<std::collections::HashMap<PathBuf, (std::time::SystemTime, Note)>>,
}

impl FileNotesEngine {
    /// Opens the folder, creating it if needed, and imports a legacy `notes_db` when one exists.
    pub fn new(dir: PathBuf, legacy_db: Option<PathBuf>) -> Result<Self, HarnessError> {
        std::fs::create_dir_all(&dir).map_err(|e| {
            HarnessError::NotesError(format!(
                "Failed to create notes directory '{}': {}",
                dir.display(),
                e
            ))
        })?;

        let dir = dir.canonicalize().map_err(|e| {
            HarnessError::NotesError(format!(
                "Failed to canonicalize notes directory '{}': {}",
                dir.display(),
                e
            ))
        })?;

        if let Some(db) = legacy_db {
            Self::import_legacy(&dir, &db);
        }

        Ok(Self {
            dir,
            cache: std::sync::RwLock::new(std::collections::HashMap::new()),
        })
    }

    /// An engine over a scratch directory; used by tests.
    pub fn in_memory() -> Result<Self, HarnessError> {
        let dir = std::env::temp_dir().join(format!("luna-notes-{}", uuid::Uuid::new_v4()));
        Self::new(dir, None)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Copies every row of a legacy `notes.db` into a markdown file, then retires the database.
    ///
    /// Best effort by design: a missing, unreadable, or already-migrated database must never stop
    /// the harness from starting, so every failure is a warning and the folder stays usable.
    fn import_legacy(dir: &Path, db_path: &Path) {
        if !db_path.exists() {
            return;
        }

        let conn = match rusqlite::Connection::open_with_flags(
            db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) {
            Ok(conn) => conn,
            Err(error) => {
                warn!(
                    "could not open the legacy note store '{}': {}",
                    db_path.display(),
                    error
                );
                return;
            }
        };

        let mut imported = 0usize;
        let rows = conn.prepare(
            "SELECT id, title, content, summary_pointer, tags, created_at, updated_at FROM notes",
        );
        match rows {
            Ok(mut stmt) => {
                let mapped = stmt.query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0).unwrap_or_default(),
                        row.get::<_, String>(1).unwrap_or_default(),
                        row.get::<_, String>(2).unwrap_or_default(),
                        row.get::<_, String>(3).unwrap_or_default(),
                        row.get::<_, String>(4).unwrap_or_default(),
                        row.get::<_, String>(5).unwrap_or_default(),
                        row.get::<_, String>(6).unwrap_or_default(),
                    ))
                });
                if let Ok(mapped) = mapped {
                    for row in mapped.flatten() {
                        let (id, title, content, pointer, tags_json, created, updated) = row;
                        if title.trim().is_empty() {
                            continue;
                        }
                        let note = Note {
                            id: if id.trim().is_empty() {
                                slugify(&title)
                            } else {
                                id
                            },
                            title,
                            content,
                            summary_pointer: if pointer.trim().is_empty() {
                                String::new()
                            } else {
                                pointer
                            },
                            tags: serde_json::from_str(&tags_json).unwrap_or_default(),
                            created_at: parse_timestamp(&created),
                            updated_at: parse_timestamp(&updated),
                        };
                        match write_note(dir, &note, true) {
                            Ok(_) => imported += 1,
                            Err(error) => warn!("could not import note '{}': {}", note.id, error),
                        }
                    }
                }
            }
            Err(error) => {
                warn!("legacy note store has no usable notes table: {}", error);
                return;
            }
        }

        let retired = db_path.with_extension("db.imported");
        match std::fs::rename(db_path, &retired) {
            Ok(()) => info!(
                "imported {} note(s) from '{}' into '{}' (legacy store retired as '{}')",
                imported,
                db_path.display(),
                dir.display(),
                retired.display()
            ),
            Err(error) => warn!(
                "imported {} note(s) but could not retire '{}': {}",
                imported,
                db_path.display(),
                error
            ),
        }
    }

    /// Every note on disk, newest update first.
    fn load_all(&self) -> Result<Vec<Note>, HarnessError> {
        let entries = std::fs::read_dir(&self.dir).map_err(|e| {
            HarnessError::NotesError(format!(
                "Failed to read notes directory '{}': {}",
                self.dir.display(),
                e
            ))
        })?;

        let mut current_paths = std::collections::HashSet::new();
        let mut notes = Vec::new();
        let mut cache_updates = Vec::new();

        {
            let cache = self.cache.read().unwrap();
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("md") {
                    continue;
                }
                current_paths.insert(path.clone());
                let mtime = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

                if let Some((cached_mtime, cached_note)) = cache.get(&path) {
                    if *cached_mtime == mtime {
                        notes.push(cached_note.clone());
                        continue;
                    }
                }

                match parse_note_file(&path) {
                    Ok(note) => {
                        notes.push(note.clone());
                        cache_updates.push((path, mtime, note));
                    }
                    Err(error) => warn!("skipping unreadable note '{}': {}", path.display(), error),
                }
            }
        }

        if !cache_updates.is_empty() {
            let mut cache = self.cache.write().unwrap();
            for (path, mtime, note) in cache_updates {
                cache.insert(path, (mtime, note));
            }
            cache.retain(|p, _| current_paths.contains(p));
        }

        notes.sort_by_key(|a| std::cmp::Reverse(a.updated_at));
        Ok(notes)
    }

    /// Resolves the filesystem path for a note id, ensuring the resulting path is strictly
    /// inside `self.dir`.
    pub fn note_path(&self, id: &str) -> Result<PathBuf, HarnessError> {
        let trimmed = id.trim();
        if !is_safe_note_id(trimmed) {
            return Err(HarnessError::NotesError(format!(
                "Invalid note id '{}': must not contain path separators, start with '.', or escape notes directory",
                id
            )));
        }
        let target = self.dir.join(format!("{}.md", trimmed));
        if target.exists() {
            let canonical = target.canonicalize().map_err(|e| {
                HarnessError::NotesError(format!(
                    "Failed to canonicalize note path '{}': {}",
                    target.display(),
                    e
                ))
            })?;
            if !canonical.starts_with(&self.dir) {
                return Err(HarnessError::NotesError(format!(
                    "Note path escapes notes directory '{}': {}",
                    self.dir.display(),
                    target.display()
                )));
            }
            Ok(canonical)
        } else {
            Ok(target)
        }
    }

    /// Resolves a note by file stem (its id) or, failing that, by title.
    fn find(&self, id: &str) -> Result<Note, HarnessError> {
        if is_safe_note_id(id) {
            if let Ok(direct) = self.note_path(id) {
                if direct.is_file() {
                    return parse_note_file(&direct);
                }
            }
        }

        self.load_all()?
            .into_iter()
            .find(|n| n.id == id || n.title.eq_ignore_ascii_case(id))
            .ok_or_else(|| {
                HarnessError::NotesError(format!(
                    "Note '{}' not found in '{}'",
                    id,
                    self.dir.display()
                ))
            })
    }
}

#[async_trait]
impl NotesEngine for FileNotesEngine {
    async fn save_note(
        &self,
        title: &str,
        content: &str,
        tags: &[String],
    ) -> Result<Note, HarnessError> {
        let slug = slugify(title);

        // Consolidate: if a note for this topic/title already exists, update it rather than creating duplicate slug-1.md
        if let Ok(existing) = self.find(title).or_else(|_| self.find(&slug)) {
            info!(
                "Consolidating note for '{}' onto existing note '{}'",
                title, existing.id
            );
            return self
                .update_note(&existing.id, Some(title), Some(content), Some(tags))
                .await;
        }

        let id = {
            let mut candidate = slug.clone();
            let mut attempt = 0u32;
            while self.dir.join(format!("{}.md", candidate)).exists() {
                attempt += 1;
                candidate = format!("{}-{}", slug, attempt);
                if attempt > 128 {
                    candidate = format!("{}-{}", slug, &uuid::Uuid::new_v4().to_string()[..4]);
                    break;
                }
            }
            candidate
        };

        let now = Utc::now();
        let note = Note {
            id,
            title: title.trim().to_string(),
            content: content.to_string(),
            summary_pointer: PointerDistiller::distill(title, content),
            tags: tags.to_vec(),
            created_at: now,
            updated_at: now,
        };

        let path = write_note(&self.dir, &note, false)?;
        info!(
            "Saved note '{}' to '{}' (Pointer: '{}')",
            note.id,
            path.display(),
            note.summary_pointer
        );
        Ok(note)
    }

    async fn search_notes(&self, query: &str, limit: usize) -> Result<Vec<Note>, HarnessError> {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return Ok(Vec::new());
        }

        let mut scored: Vec<(usize, Note)> = Vec::new();
        for note in self.load_all()? {
            let haystack =
                format!("{}\n{}\n{}", note.title, note.content, note.tags.join(" ")).to_lowercase();
            let hits = haystack.matches(&needle).count();
            if hits > 0 {
                scored.push((hits, note));
            }
        }

        // Most mentions first — a document that names the term five times is more likely the one
        // being asked about than a passing reference. Ties keep the newest-update ordering.
        scored.sort_by_key(|a| std::cmp::Reverse(a.0));
        Ok(scored.into_iter().take(limit).map(|(_, n)| n).collect())
    }

    async fn list_notes(&self, limit: usize, offset: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(self
            .load_all()?
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect())
    }

    async fn read_note(&self, id: &str) -> Result<Note, HarnessError> {
        self.find(id)
    }

    async fn update_note(
        &self,
        id: &str,
        title: Option<&str>,
        content: Option<&str>,
        tags: Option<&[String]>,
    ) -> Result<Note, HarnessError> {
        let current = self.find(id)?;
        let new_title = title.map(str::trim).unwrap_or(&current.title).to_string();
        let new_content = content.unwrap_or(&current.content).to_string();
        let new_pointer = PointerDistiller::distill(&new_title, &new_content);
        let new_tags = tags.map(|t| t.to_vec()).unwrap_or(current.tags);

        let note = Note {
            id: current.id.clone(),
            title: new_title,
            content: new_content,
            summary_pointer: new_pointer,
            tags: new_tags,
            created_at: current.created_at,
            updated_at: Utc::now(),
        };
        let path = write_note(&self.dir, &note, false)?;

        info!("Updated note '{}' ('{}')", note.id, path.display());
        Ok(note)
    }

    async fn delete_note(&self, id: &str) -> Result<bool, HarnessError> {
        let Ok(note) = self.find(id) else {
            warn!("Delete requested for unknown note '{}'", id);
            return Ok(false);
        };

        let path = self.note_path(&note.id)?;
        match std::fs::remove_file(&path) {
            Ok(()) => {
                info!("Deleted note '{}'", note.id);
                if let Ok(mut cache) = self.cache.write() {
                    cache.remove(&path);
                }
                Ok(true)
            }
            Err(error) => Err(HarnessError::NotesError(format!(
                "Failed to delete note '{}': {}",
                path.display(),
                error
            ))),
        }
    }
}

/// Writes a note to `<dir>/<id>.md`, optionally skipping an existing file (import path).
fn write_note(dir: &Path, note: &Note, skip_existing: bool) -> Result<PathBuf, HarnessError> {
    if !is_safe_note_id(&note.id) {
        return Err(HarnessError::NotesError(format!(
            "Invalid note id '{}': must not contain path separators, start with '.', or escape notes directory",
            note.id
        )));
    }
    let canonical_dir = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let path = canonical_dir.join(format!("{}.md", note.id));
    if path.exists() {
        let canonical = path.canonicalize().map_err(|e| {
            HarnessError::NotesError(format!(
                "Failed to canonicalize note path '{}': {}",
                path.display(),
                e
            ))
        })?;
        if !canonical.starts_with(&canonical_dir) {
            return Err(HarnessError::NotesError(format!(
                "Note path escapes notes directory '{}': {}",
                canonical_dir.display(),
                path.display()
            )));
        }
    }
    if skip_existing && path.exists() {
        return Ok(path);
    }

    let tags = if note.tags.is_empty() {
        String::new()
    } else {
        note.tags
            .iter()
            .map(|t| t.replace(['\n', ','], " ").trim().to_string())
            .filter(|t| !t.is_empty())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let pointer = if note.summary_pointer.trim().is_empty() {
        PointerDistiller::distill(&note.title, &note.content)
    } else {
        note.summary_pointer.clone()
    };

    let mut body = String::new();
    body.push_str(FRONT_MATTER_FENCE);
    body.push('\n');
    body.push_str(&format!("id: {}\n", note.id));
    body.push_str(&format!("title: {}\n", single_line(&note.title)));
    body.push_str(&format!("summary_pointer: {}\n", single_line(&pointer)));
    if !tags.is_empty() {
        body.push_str(&format!("tags: {}\n", tags));
    }
    body.push_str(&format!("created_at: {}\n", note.created_at.to_rfc3339()));
    body.push_str(&format!("updated_at: {}\n", note.updated_at.to_rfc3339()));
    body.push_str(FRONT_MATTER_FENCE);
    body.push_str("\n\n");
    body.push_str(&note.content);
    if !body.ends_with('\n') {
        body.push('\n');
    }

    let tmp = path.with_extension(format!("tmp.{}", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, &body).map_err(|e| {
        HarnessError::NotesError(format!("Failed to write temp note '{}': {}", tmp.display(), e))
    })?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        HarnessError::NotesError(format!("Failed to commit note '{}': {}", path.display(), e))
    })?;
    Ok(path)
}

/// Reads a note file back. The front matter is the metadata; everything after the closing fence is
/// verbatim content, so `content: ---` or `title:` inside a note body is never mistaken for data.
fn parse_note_file(path: &Path) -> Result<Note, HarnessError> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        HarnessError::NotesError(format!("Failed to read '{}': {}", path.display(), e))
    })?;

    let fallback_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("note")
        .to_string();

    let mut lines = raw.lines();
    let mut title = String::new();
    let mut id = String::new();
    let mut pointer = String::new();
    let mut tags: Vec<String> = Vec::new();
    let mut created = String::new();
    let mut updated = String::new();

    let body_lines: Vec<&str>;

    if raw.starts_with(FRONT_MATTER_FENCE) {
        // Skip the opening fence, then read key/value pairs until the closing fence.
        lines.next();
        for line in lines.by_ref() {
            if line.trim() == FRONT_MATTER_FENCE {
                break;
            }
            if let Some((key, value)) = line.split_once(':') {
                let mut value = value.trim().to_string();
                if (value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\''))
                {
                    if value.len() >= 2 {
                        value = value[1..value.len() - 1].to_string();
                    }
                }
                match key.trim() {
                    "id" => id = value,
                    "title" => title = value,
                    "summary_pointer" => pointer = value,
                    "created_at" => created = value,
                    "updated_at" => updated = value,
                    "tags" => {
                        tags = value
                            .split(',')
                            .map(|t| t.trim().to_string())
                            .filter(|t| !t.is_empty())
                            .collect()
                    }
                    _ => {}
                }
            }
        }
        body_lines = lines.collect();
    } else {
        // A hand-written markdown file in the folder: the first heading is the title, the whole
        // file is the content. Nothing is lost just because the user did not write front matter.
        let first_heading = raw
            .lines()
            .find(|l| l.trim_start().starts_with('#'))
            .map(|l| l.trim_start_matches('#').trim().to_string());
        title = first_heading.unwrap_or_else(|| fallback_id.clone());
        body_lines = raw.lines().collect();
    }

    let content = body_lines.join("\n").trim_matches('\n').to_string();
    if title.trim().is_empty() {
        title = fallback_id.clone();
    }

    let mtime = std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .map(DateTime::<Utc>::from)
        .unwrap_or_else(Utc::now);

    let note = Note {
        id: if id.trim().is_empty() || !is_safe_note_id(&id) {
            fallback_id
        } else {
            id
        },
        title,
        content,
        summary_pointer: if pointer.trim().is_empty() {
            PointerDistiller::distill("", "")
        } else {
            pointer
        },
        tags,
        created_at: parse_timestamp(&created),
        updated_at: if updated.trim().is_empty() {
            mtime
        } else {
            parse_timestamp(&updated)
        },
    };
    Ok(note)
}

/// Front matter is one line per field: a newline in a value would forge the next field.
fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_timestamp(raw: &str) -> DateTime<Utc> {
    if raw.trim().is_empty() {
        return Utc::now();
    }
    DateTime::parse_from_rfc3339(raw)
        .map(|d| d.with_timezone(&Utc))
        // SQLite's `CURRENT_TIMESTAMP` form, which the legacy store used.
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S").map(|d| d.and_utc())
        })
        .unwrap_or_else(|_| Utc::now())
}
