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

/// Words too common to identify a note. Without this, a natural-language query would match every
/// note on its articles and prepositions; the exact-phrase check still uses the whole query.
const SEARCH_STOPWORDS: &[&str] = &[
    "the", "and", "for", "with", "that", "this", "from", "are", "was", "were", "has", "have",
    "had", "but", "not", "you", "your", "can", "how", "what", "when", "where", "which", "who",
    "why", "does", "did", "doing", "into", "about", "over", "under", "then", "than", "them",
    "they", "its", "our", "out", "use", "using", "would", "should", "could", "there",
];

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
                            ..Default::default()
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
        match harness_core::fsutil::replace_file(db_path, &retired) {
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
            let part1_id = format!("{}-part-1", id);
            if let Ok(part1_path) = self.note_path(&part1_id) {
                if part1_path.is_file() {
                    let mut parts = Vec::new();
                    let mut p = 1;
                    while let Ok(part_path) = self.note_path(&format!("{}-part-{}", id, p)) {
                        if part_path.is_file() {
                            if let Ok(part_note) = parse_note_file(&part_path) {
                                parts.push(part_note);
                            }
                            p += 1;
                        } else {
                            break;
                        }
                    }
                    if !parts.is_empty() {
                        let first = &parts[0];
                        let full_content = parts
                            .iter()
                            .map(|p| p.content.as_str())
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        let title = first
                            .title
                            .split(" (Part 1 of ")
                            .next()
                            .unwrap_or(&first.title)
                            .to_string();
                        return Ok(Note {
                            id: id.to_string(),
                            title,
                            content: full_content,
                            summary_pointer: first.summary_pointer.clone(),
                            tags: first.tags.clone(),
                            created_at: first.created_at,
                            updated_at: parts
                                .last()
                                .map(|p| p.updated_at)
                                .unwrap_or(first.updated_at),
                            parent_id: None,
                            part_number: None,
                            part_count: Some(parts.len()),
                            content_hash: None,
                        });
                    }
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

/// Target chunk size for long notes (4-8 KB). Target is ~6 KB.
pub const NOTE_CHUNK_TARGET_BYTES: usize = 6_000;
/// Maximum chunk size before splitting a part.
pub const NOTE_CHUNK_MAX_BYTES: usize = 8_192;
/// Minimum chunk size before creating a new split.
pub const NOTE_CHUNK_MIN_BYTES: usize = 4_096;

/// Computes a stable hex digest for note content deduplication.
pub fn compute_content_hash(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())[..16].to_string()
}

/// Splits long note content into 4-8 KB parts, preserving paragraph and line boundaries.
pub fn chunk_note_content(content: &str, _target_size: usize, max_size: usize) -> Vec<String> {
    if content.len() <= max_size {
        return vec![content.to_string()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();

    for block in content.split("\n\n") {
        if block.len() > max_size {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            for line in block.lines() {
                if current.is_empty() {
                    current.push_str(line);
                } else if current.len() + 1 + line.len() <= max_size {
                    current.push('\n');
                    current.push_str(line);
                } else {
                    chunks.push(std::mem::take(&mut current));
                    current.push_str(line);
                }
            }
            if current.len() >= NOTE_CHUNK_MIN_BYTES {
                chunks.push(std::mem::take(&mut current));
            }
            continue;
        }

        if current.is_empty() {
            current.push_str(block);
        } else if current.len() + 2 + block.len() <= max_size {
            current.push_str("\n\n");
            current.push_str(block);
        } else {
            chunks.push(std::mem::take(&mut current));
            current.push_str(block);
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Extracts significant words from a note title for similarity comparison.
fn title_words(title: &str) -> std::collections::HashSet<String> {
    title
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| {
            !w.is_empty() && !SEARCH_STOPWORDS.contains(w) && *w != "note" && *w != "document"
        })
        .map(str::to_string)
        .collect()
}

/// Whether two titles refer to the same document topic.
fn titles_near_match(a: &str, b: &str) -> bool {
    let slug_a = slugify(a);
    let slug_b = slugify(b);
    if slug_a == slug_b {
        return true;
    }
    let words_a = title_words(a);
    let words_b = title_words(b);
    if words_a.len() < 2 || words_b.len() < 2 {
        return false;
    }
    let intersection = words_a.intersection(&words_b).count();
    let union = words_a.union(&words_b).count();
    if union == 0 {
        return false;
    }
    let jaccard = intersection as f32 / union as f32;
    jaccard >= 0.70
        || (intersection >= 2 && (intersection == words_a.len() || intersection == words_b.len()))
}

/// Whether two sets of tags strongly overlap on a specific domain.
fn tags_near_match(tags_a: &[String], tags_b: &[String]) -> bool {
    if tags_a.is_empty() || tags_b.is_empty() {
        return false;
    }
    let set_a: std::collections::HashSet<String> =
        tags_a.iter().map(|t| t.trim().to_lowercase()).collect();
    let set_b: std::collections::HashSet<String> =
        tags_b.iter().map(|t| t.trim().to_lowercase()).collect();
    let shared = set_a.intersection(&set_b).count();
    shared >= 2 || (shared >= 1 && (set_a.len() == 1 || set_b.len() == 1))
}

fn write_note_with_chunks(dir: &Path, note: &Note) -> Result<PathBuf, HarnessError> {
    if note.content.len() <= NOTE_CHUNK_MAX_BYTES {
        let mut single = note.clone();
        single.content_hash = Some(compute_content_hash(&single.content));
        return write_note(dir, &single, false);
    }

    let chunks = chunk_note_content(&note.content, NOTE_CHUNK_TARGET_BYTES, NOTE_CHUNK_MAX_BYTES);
    let total_parts = chunks.len();

    // Write chunk files with stable parent id, part number, and hash dedupe
    for (i, chunk_body) in chunks.iter().enumerate() {
        let part_number = i + 1;
        let part_id = format!("{}-part-{}", note.id, part_number);
        let part_hash = compute_content_hash(chunk_body);
        let part_path = dir.join(format!("{}.md", part_id));

        // Hash dedupe: if file exists and has identical hash, skip writing
        if part_path.is_file() {
            if let Ok(existing_part) = parse_note_file(&part_path) {
                if existing_part.content_hash.as_deref() == Some(&part_hash) {
                    continue;
                }
            }
        }

        let part_note = Note {
            id: part_id,
            title: format!("{} (Part {} of {})", note.title, part_number, total_parts),
            content: chunk_body.clone(),
            summary_pointer: format!("{} (Part {})", note.summary_pointer, part_number),
            tags: note.tags.clone(),
            created_at: note.created_at,
            updated_at: note.updated_at,
            parent_id: Some(note.id.clone()),
            part_number: Some(part_number),
            part_count: Some(total_parts),
            content_hash: Some(part_hash),
        };
        write_note(dir, &part_note, false)?;
    }

    // Clean up any stale old parts if previous count was higher
    let mut check_part = total_parts + 1;
    while dir
        .join(format!("{}-part-{}.md", note.id, check_part))
        .is_file()
    {
        let _ = std::fs::remove_file(dir.join(format!("{}-part-{}.md", note.id, check_part)));
        check_part += 1;
    }

    // Also write the parent note file containing the full document
    let mut parent = note.clone();
    parent.part_count = Some(total_parts);
    parent.content_hash = Some(compute_content_hash(&note.content));
    write_note(dir, &parent, false)
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

        // Near-match title and tag reconciliation:
        let all_notes = self.load_all()?;
        let existing = all_notes.into_iter().find(|n| {
            if n.parent_id.is_some() {
                return false;
            }
            n.id == slug
                || n.id == title
                || n.title.eq_ignore_ascii_case(title)
                || (titles_near_match(&n.title, title)
                    && (tags.is_empty()
                        || tags_near_match(&n.tags, tags)
                        || title_words(&n.title) == title_words(title)))
        });

        if let Some(existing) = existing {
            info!(
                "Consolidating note for '{}' onto existing note '{}'",
                title, existing.id
            );
            // Reconcile by appending new content if not already present
            let target_content = if existing.content.contains(content.trim()) {
                existing.content.clone()
            } else if content.trim().contains(existing.content.trim()) {
                content.trim().to_string()
            } else {
                let mut combined = existing.content.trim_end().to_string();
                combined.push_str("\n\n");
                combined.push_str(content.trim());
                combined
            };

            // Merge tags
            let mut merged_tags = existing.tags.clone();
            for tag in tags {
                if !merged_tags.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
                    merged_tags.push(tag.clone());
                }
            }

            return self
                .update_note(
                    &existing.id,
                    Some(title),
                    Some(&target_content),
                    Some(&merged_tags),
                )
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
            parent_id: None,
            part_number: None,
            part_count: None,
            content_hash: Some(compute_content_hash(content)),
        };

        let path = write_note_with_chunks(&self.dir, &note)?;
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

        // Substring matching only ever found a note when the caller guessed its exact wording.
        // Score terms as well, so "build command" reaches a note that says
        // "cargo build --release": an exact phrase is still the strongest signal, then how many
        // query terms appear, then how often they appear.
        let terms: Vec<String> = needle
            .split(|c: char| !c.is_alphanumeric())
            .filter(|term| term.chars().count() >= 2)
            .filter(|term| !SEARCH_STOPWORDS.contains(term))
            .map(str::to_string)
            .collect();

        let mut scored: Vec<(usize, Note)> = Vec::new();
        for note in self.load_all()? {
            let haystack =
                format!("{}\n{}\n{}", note.title, note.content, note.tags.join(" ")).to_lowercase();
            let exact = haystack.matches(&needle).count();
            let distinct = terms
                .iter()
                .filter(|term| haystack.contains(term.as_str()))
                .count();
            if exact == 0 && distinct == 0 {
                continue;
            }
            let occurrences: usize = terms
                .iter()
                .map(|term| haystack.matches(term.as_str()).count())
                .sum();
            let score = exact * 10_000 + distinct * 100 + occurrences;
            scored.push((score, note));
        }

        // Strongest match first. The sort is stable, so equal scores keep the newest-update
        // ordering `load_all` already provides.
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
            parent_id: current.parent_id,
            part_number: current.part_number,
            part_count: current.part_count,
            content_hash: None,
        };
        let path = write_note_with_chunks(&self.dir, &note)?;

        info!("Updated note '{}' ('{}')", note.id, path.display());
        Ok(note)
    }

    async fn delete_note(&self, id: &str) -> Result<bool, HarnessError> {
        let Ok(note) = self.find(id) else {
            warn!("Delete requested for unknown note '{}'", id);
            return Ok(false);
        };

        let path = self.note_path(&note.id)?;
        let mut deleted = false;
        if path.is_file() {
            if std::fs::remove_file(&path).is_ok() {
                deleted = true;
                if let Ok(mut cache) = self.cache.write() {
                    cache.remove(&path);
                }
            }
        }
        let mut p = 1;
        while let Ok(part_path) = self.note_path(&format!("{}-part-{}", note.id, p)) {
            if part_path.is_file() {
                let _ = std::fs::remove_file(&part_path);
                if let Ok(mut cache) = self.cache.write() {
                    cache.remove(&part_path);
                }
                deleted = true;
                p += 1;
            } else {
                break;
            }
        }

        if deleted {
            info!("Deleted note '{}'", note.id);
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn archive_note(&self, id: &str) -> Result<bool, HarnessError> {
        let Ok(note) = self.find(id) else {
            return Ok(false);
        };

        let path = self.note_path(&note.id)?;
        let archive_dir = self.dir.join(ARCHIVE_DIR);
        std::fs::create_dir_all(&archive_dir).map_err(|e| {
            HarnessError::NotesError(format!(
                "Failed to create the notes archive '{}': {}",
                archive_dir.display(),
                e
            ))
        })?;

        let mut archived_any = false;
        if path.is_file() {
            let target = archive_dir.join(format!("{}.md", note.id));
            if harness_core::fsutil::replace_file(&path, &target).is_ok() {
                archived_any = true;
                if let Ok(mut cache) = self.cache.write() {
                    cache.remove(&path);
                }
            }
        }
        let mut p = 1;
        while let Ok(part_path) = self.note_path(&format!("{}-part-{}", note.id, p)) {
            if part_path.is_file() {
                let part_target = archive_dir.join(format!("{}-part-{}.md", note.id, p));
                let _ = harness_core::fsutil::replace_file(&part_path, &part_target);
                if let Ok(mut cache) = self.cache.write() {
                    cache.remove(&part_path);
                }
                archived_any = true;
                p += 1;
            } else {
                break;
            }
        }

        if archived_any {
            info!("Archived note '{}' to '{}'", note.id, archive_dir.display());
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// Subdirectory that holds soft-deleted notes. It is a dot directory so `load_all` (which only
/// reads top-level `.md` files) never lists it as a note.
const ARCHIVE_DIR: &str = ".archive";

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
    if let Some(parent_id) = &note.parent_id {
        body.push_str(&format!("parent_id: {}\n", single_line(parent_id)));
    }
    if let Some(part_number) = note.part_number {
        body.push_str(&format!("part_number: {}\n", part_number));
    }
    if let Some(part_count) = note.part_count {
        body.push_str(&format!("part_count: {}\n", part_count));
    }
    if let Some(content_hash) = &note.content_hash {
        body.push_str(&format!("content_hash: {}\n", single_line(content_hash)));
    }
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
        HarnessError::NotesError(format!(
            "Failed to write temp note '{}': {}",
            tmp.display(),
            e
        ))
    })?;
    harness_core::fsutil::replace_file(&tmp, &path).map_err(|e| {
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
    let mut parent_id: Option<String> = None;
    let mut part_number: Option<usize> = None;
    let mut part_count: Option<usize> = None;
    let mut content_hash: Option<String> = None;

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
                if ((value.starts_with('"') && value.ends_with('"'))
                    || (value.starts_with('\'') && value.ends_with('\'')))
                    && value.len() >= 2
                {
                    value = value[1..value.len() - 1].to_string();
                }
                match key.trim() {
                    "id" => id = value,
                    "parent_id" => parent_id = Some(value),
                    "part_number" => part_number = value.parse().ok(),
                    "part_count" => part_count = value.parse().ok(),
                    "content_hash" => content_hash = Some(value),
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
        parent_id,
        part_number,
        part_count,
        content_hash,
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
