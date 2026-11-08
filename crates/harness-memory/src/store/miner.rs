//! Project miner: walk a tree, chunk text, and file verbatim records with
//! content indexes and compact summaries.
//!
//! Direct port of MemPalace's `miner.py`, `room_detector_local.py`, and `palace/closets.py`:
//! - Paragraph-aware sliding window chunking (`chunk_text`)
//! - Folder-to-room mapping (70+ patterns) and topic detection
//! - Compact pointer lines with action verbs, markdown headers, and quotes

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

use chrono::Utc;
use harness_core::{HarnessError, MemoryCategory};
use regex::Regex;
use walkdir::WalkDir;

use super::atomic;
use super::ids;
use super::Record;

pub const DEFAULT_CHUNK_SIZE: usize = 800;
pub const DEFAULT_CHUNK_OVERLAP: usize = 100;
pub const DEFAULT_MIN_CHUNK_SIZE: usize = 50;

const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

const SKIP_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "node_modules",
    "target",
    "dist",
    "build",
    ".next",
    ".venv",
    "venv",
    "__pycache__",
    ".luna",
    ".cache",
    "vendor",
    "coverage",
];

const TEXT_EXTENSIONS: &[&str] = &[
    "rs", "md", "markdown", "toml", "yaml", "yml", "json", "jsonl", "ts", "tsx", "js", "jsx", "py",
    "go", "java", "c", "h", "cc", "cpp", "hpp", "sh", "bash", "zsh", "sql", "css", "scss", "html",
    "htm", "txt", "lua", "rb", "swift", "kt", "kts", "php", "ex", "exs", "erl", "hs", "ml", "clj",
    "zig", "nim", "cs", "fs", "vue", "svelte",
];

/// 70+ folder-to-room mappings ported verbatim from MemPalace's `room_detector_local.py:FOLDER_ROOM_MAP`.
const FOLDER_ROOM_MAP: &[(&str, &str)] = &[
    ("frontend", "frontend"),
    ("front-end", "frontend"),
    ("front_end", "frontend"),
    ("client", "frontend"),
    ("ui", "frontend"),
    ("views", "frontend"),
    ("components", "frontend"),
    ("pages", "frontend"),
    ("backend", "backend"),
    ("back-end", "backend"),
    ("back_end", "backend"),
    ("server", "backend"),
    ("api", "backend"),
    ("routes", "backend"),
    ("services", "backend"),
    ("controllers", "backend"),
    ("models", "backend"),
    ("database", "backend"),
    ("db", "backend"),
    ("docs", "documentation"),
    ("doc", "documentation"),
    ("documentation", "documentation"),
    ("wiki", "documentation"),
    ("readme", "documentation"),
    ("notes", "documentation"),
    ("design", "design"),
    ("designs", "design"),
    ("mockups", "design"),
    ("wireframes", "design"),
    ("assets", "design"),
    ("storyboard", "design"),
    ("costs", "costs"),
    ("cost", "costs"),
    ("budget", "costs"),
    ("finance", "costs"),
    ("financial", "costs"),
    ("pricing", "costs"),
    ("invoices", "costs"),
    ("accounting", "costs"),
    ("meetings", "meetings"),
    ("meeting", "meetings"),
    ("calls", "meetings"),
    ("meeting_notes", "meetings"),
    ("standup", "meetings"),
    ("minutes", "meetings"),
    ("team", "team"),
    ("staff", "team"),
    ("hr", "team"),
    ("hiring", "team"),
    ("employees", "team"),
    ("people", "team"),
    ("research", "research"),
    ("references", "research"),
    ("reading", "research"),
    ("papers", "research"),
    ("planning", "planning"),
    ("roadmap", "planning"),
    ("strategy", "planning"),
    ("specs", "planning"),
    ("requirements", "planning"),
    ("tests", "testing"),
    ("test", "testing"),
    ("testing", "testing"),
    ("qa", "testing"),
    ("scripts", "scripts"),
    ("tools", "scripts"),
    ("utils", "scripts"),
    ("config", "configuration"),
    ("configs", "configuration"),
    ("settings", "configuration"),
    ("infrastructure", "configuration"),
    ("infra", "configuration"),
    ("deploy", "configuration"),
];

#[derive(Debug, Default)]
pub struct MineReport {
    pub files: usize,
    pub records: usize,
    pub skipped: usize,
    pub removed: usize,
}

#[derive(Debug, Clone)]
pub struct TextChunk {
    pub content: String,
    pub chunk_index: usize,
    pub line_start: usize,
    pub line_end: usize,
}

/// Split content into drawer-sized chunks.
/// Direct port of MemPalace's `miner.py:chunk_text`.
pub fn chunk_text(
    content: &str,
    chunk_size: usize,
    chunk_overlap: usize,
    min_chunk_size: usize,
) -> Vec<TextChunk> {
    assert!(
        chunk_overlap <= chunk_size / 2,
        "chunk_overlap ({}) must be at most chunk_size / 2 ({})",
        chunk_overlap,
        chunk_size / 2
    );

    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut start = 0;
    let mut chunk_index = 0;

    let mut nl_before_start = 0;
    let mut start_anchor = 0;
    let mut nl_before_end = 0;
    let mut end_anchor = 0;

    while start < content.len() {
        // UTF-8 char boundary guard for start
        while start < content.len() && !content.is_char_boundary(start) {
            start += 1;
        }
        if start >= content.len() {
            break;
        }

        let mut end = (start + chunk_size).min(content.len());
        while end > start && !content.is_char_boundary(end) {
            end -= 1;
        }

        // Try to break at paragraph boundary, then newline boundary
        if end < content.len() {
            let slice = &content[start..end];
            let midpoint = chunk_size / 2;
            if let Some(pos) = slice.rfind("\n\n") {
                if pos > midpoint {
                    end = start + pos;
                } else if let Some(pos_nl) = slice.rfind('\n') {
                    if pos_nl > midpoint {
                        end = start + pos_nl;
                    }
                }
            } else if let Some(pos_nl) = slice.rfind('\n') {
                if pos_nl > midpoint {
                    end = start + pos_nl;
                }
            }
        }

        let chunk = content[start..end].trim();
        if chunk.len() >= min_chunk_size {
            let line_start = if start >= start_anchor {
                nl_before_start += content[start_anchor..start]
                    .chars()
                    .filter(|&c| c == '\n')
                    .count();
                start_anchor = start;
                nl_before_start + 1
            } else {
                content[..start].chars().filter(|&c| c == '\n').count() + 1
            };

            let line_end = if end >= end_anchor {
                nl_before_end += content[end_anchor..end]
                    .chars()
                    .filter(|&c| c == '\n')
                    .count();
                end_anchor = end;
                nl_before_end + 1
            } else {
                content[..end].chars().filter(|&c| c == '\n').count() + 1
            };

            chunks.push(TextChunk {
                content: chunk.to_string(),
                chunk_index,
                line_start,
                line_end,
            });
            chunk_index += 1;
        }

        let next_start = if end < content.len() {
            end.saturating_sub(chunk_overlap)
        } else {
            end
        };
        // Avoid infinite loop if start does not advance
        if next_start <= start {
            start = end;
        } else {
            start = next_start;
        }
    }

    if chunks.is_empty() && !trimmed.is_empty() {
        let lines = trimmed.chars().filter(|&c| c == '\n').count() + 1;
        chunks.push(TextChunk {
            content: trimmed.to_string(),
            chunk_index: 0,
            line_start: 1,
            line_end: lines,
        });
    }

    chunks
}

/// Tokenize a name into lowercased tokens split by '-', '_', '.', '/'.
/// Matches MemPalace `miner.py:_tokens`.
fn name_tokens(val: &str) -> HashSet<String> {
    val.split(|c: char| c == '-' || c == '_' || c == '.' || c == '/')
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Returns true when `a` and `b` match as equal strings or as separator-bounded tokens.
/// Matches MemPalace `miner.py:_name_matches`.
fn name_matches(a: &str, b: &str) -> bool {
    let a_low = a.to_lowercase();
    let b_low = b.to_lowercase();
    if a_low == b_low {
        return true;
    }
    let tok_a = name_tokens(a);
    let tok_b = name_tokens(b);
    tok_a.contains(&b_low) || tok_b.contains(&a_low)
}

/// Route a file to the right topic.
/// Direct port of MemPalace's `detect_room` (miner.py:679).
fn detect_topic(relative: &Path, content: &str) -> String {
    let components: Vec<String> = relative
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(str::to_string))
        .collect();

    // Priority 1: folder path matches FOLDER_ROOM_MAP
    if components.len() >= 2 {
        for part in &components[..components.len() - 1] {
            let part_lower = part.to_lowercase().replace('-', "_");
            for &(pattern, room) in FOLDER_ROOM_MAP {
                if name_matches(&part_lower, pattern) {
                    return room.to_string();
                }
            }
        }
        // Sub-container mapping for crates/packages/libs/apps
        let first = components[0].to_lowercase().replace('-', "_");
        if matches!(first.as_str(), "crates" | "packages" | "libs" | "apps") {
            if let Some(next) = components.get(1) {
                return ids::slugify(next);
            }
        }
    }

    // Priority 2: filename matches known room/topic
    let stem = relative
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    for &(pattern, room) in FOLDER_ROOM_MAP {
        if name_matches(&stem, pattern) {
            return room.to_string();
        }
    }

    // Priority 3: keyword scoring from content window
    let content_window = content
        .chars()
        .take(2000)
        .collect::<String>()
        .to_lowercase();
    let mut scores: HashMap<&'static str, usize> = HashMap::new();
    for &(pattern, room) in FOLDER_ROOM_MAP {
        let count = content_window.matches(pattern).count();
        if count > 0 {
            *scores.entry(room).or_insert(0) += count;
        }
    }
    if let Some((&best_room, &score)) = scores.iter().max_by_key(|&(_, count)| count) {
        if score > 0 {
            return best_room.to_string();
        }
    }

    // Priority 4: fallback to first component or "general"
    if let Some(first) = components.first() {
        if components.len() > 1 {
            return ids::slugify(first);
        }
    }
    "general".to_string()
}

/// Mine `dir` into `store`. Existing records for a source are replaced when
/// the source's content changes (ids are content-addressed, so a changed file
/// produces new ids and the stale ids are removed).
pub fn mine(
    store: &super::MemoryStore,
    dir: &Path,
    domain_override: Option<&str>,
) -> Result<MineReport, HarnessError> {
    let dir = dir
        .canonicalize()
        .map_err(|error| HarnessError::MemoryError(format!("{}: {error}", dir.display())))?;
    let domain = domain_override
        .map(ids::slugify)
        .or_else(|| {
            dir.file_name()
                .and_then(|name| name.to_str())
                .map(ids::slugify)
        })
        .unwrap_or_else(|| "projects".to_string());

    let mut report = MineReport::default();
    let existing = store.active_records()?;

    for entry in WalkDir::new(&dir)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            let name = entry.file_name().to_string_lossy();
            if entry.file_type().is_dir() {
                return !SKIP_DIRS.contains(&name.as_ref()) && !name.starts_with('.');
            }
            true
        })
    {
        let Ok(entry) = entry else {
            report.skipped += 1;
            continue;
        };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        if !is_text_file(path) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            report.skipped += 1;
            continue;
        };
        if metadata.len() > MAX_FILE_BYTES {
            report.skipped += 1;
            continue;
        }
        let Ok(text) = fs::read_to_string(path) else {
            report.skipped += 1;
            continue;
        };
        if text.trim().is_empty() || text.contains('\0') {
            report.skipped += 1;
            continue;
        }
        let relative = path.strip_prefix(&dir).unwrap_or(path);
        let source_file = relative.to_string_lossy().to_string();
        let topic = detect_topic(relative, &text);
        let chunks = chunk_text(
            &text,
            store.config.chunk_chars,
            DEFAULT_CHUNK_OVERLAP,
            DEFAULT_MIN_CHUNK_SIZE,
        );
        let mut new_ids = HashSet::new();

        for chunk in &chunks {
            let id = ids::record_id_from_chunk(&domain, &topic, &source_file, chunk.chunk_index);
            new_ids.insert(id.clone());
            let entities = extract_entities(&chunk.content);
            let first = first_line(&chunk.content, &source_file);
            let record = Record {
                id,
                domain: domain.clone(),
                topic: topic.clone(),
                source_file: Some(source_file.clone()),
                chunk_index: chunk.chunk_index,
                line_start: chunk.line_start,
                line_end: chunk.line_end,
                filed_at: Utc::now(),
                entities: entities.clone(),
                tags: vec![topic.clone()],
                body_hash: ids::body_hash(&chunk.content),
                subject: format!("file.{}", ids::slugify(&source_file)),
                predicate: format!("chunk.{}", chunk.chunk_index),
                object: first,
                name: Some(source_file.clone()),
                path: Some(source_file.clone()),
                raw_content: chunk.content.clone(),
                category: MemoryCategory::Fact,
                confidence: 1.0,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                is_active: true,
                superseded_by: None,
                body: chunk.content.clone(),
            };
            store.write_record(record)?;
            report.records += 1;
        }

        // Purge records from this source that no longer exist in the file.
        for stale in existing.iter().filter(|record| {
            record.domain == domain
                && record.source_file.as_deref() == Some(source_file.as_str())
                && !new_ids.contains(&record.id)
        }) {
            if store.delete_record(&stale.id)? {
                report.removed += 1;
            }
        }

        write_content_index(store, &domain, &topic, &source_file, &chunks, &new_ids)?;
        write_summary(store, &domain, &topic, &source_file, &text, &chunks)?;
        report.files += 1;
    }

    Ok(report)
}

fn is_text_file(path: &Path) -> bool {
    let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
        return false;
    };
    let extension = extension.to_ascii_lowercase();
    TEXT_EXTENSIONS.contains(&extension.as_str())
}

static ACTION_RE: OnceLock<Regex> = OnceLock::new();
static HEADER_RE: OnceLock<Regex> = OnceLock::new();
static QUOTE_RE: OnceLock<Regex> = OnceLock::new();
static ENTITY_WORD_RE: OnceLock<Regex> = OnceLock::new();

fn action_regex() -> &'static Regex {
    ACTION_RE.get_or_init(|| {
        Regex::new(r"(?i)(?:built|fixed|wrote|added|pushed|tested|created|decided|migrated|reviewed|deployed|configured|removed|updated)\s+[\w\s]{3,40}").unwrap()
    })
}

fn header_regex() -> &'static Regex {
    HEADER_RE.get_or_init(|| Regex::new(r"(?m)^#{1,3}\s+(.{5,60})$").unwrap())
}

fn quote_regex() -> &'static Regex {
    QUOTE_RE.get_or_init(|| Regex::new(r#""([^"]{15,150})""#).unwrap())
}

fn entity_word_regex() -> &'static Regex {
    ENTITY_WORD_RE.get_or_init(|| Regex::new(r"\b[A-Z][a-z]+(?:[A-Z][a-z]+|[A-Z]{2,})*\b").unwrap())
}

/// Extract entity proper nouns (frequency >= 2, top 5).
/// Direct port of MemPalace's `palace/closets.py:build_closet_lines` entity extractor.
pub(crate) fn extract_entities(text: &str) -> Vec<String> {
    let re = entity_word_regex();
    let mut counts: HashMap<String, usize> = HashMap::new();
    let stop_words: HashSet<&'static str> =
        super::search::MEMPALACE_STOPWORDS.iter().copied().collect();

    for m in re.find_iter(text) {
        let word = m.as_str();
        let lower = word.to_lowercase();
        if stop_words.contains(lower.as_str()) || word.len() < 3 {
            continue;
        }
        *counts.entry(word.to_string()).or_insert(0) += 1;
    }

    let mut frequent: Vec<(String, usize)> = counts.into_iter().filter(|(_, c)| *c >= 2).collect();
    frequent.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    frequent.into_iter().take(5).map(|(w, _)| w).collect()
}

/// Build compact closet pointer lines from drawer chunks.
/// Direct port of MemPalace `palace/closets.py:build_closet_lines`.
pub(crate) fn build_pointer_lines(
    domain: &str,
    topic: &str,
    source_file: &str,
    chunks: &[TextChunk],
    ids: &HashSet<String>,
) -> Vec<String> {
    let now = Utc::now().date_naive();
    let mut ordered: Vec<&String> = ids.iter().collect();
    ordered.sort();
    let mut lines = Vec::new();

    for (index, chunk) in chunks.iter().enumerate() {
        let Some(record_id) = ordered.get(index) else {
            continue;
        };
        let window = if chunk.content.len() > 5000 {
            &chunk.content[..5000]
        } else {
            &chunk.content
        };

        let entities = extract_entities(window);
        let span = format!("{}:L{}-L{}", now, chunk.line_start, chunk.line_end);

        let mut topics = Vec::new();

        // 1. Action verb phrases
        for m in action_regex().find_iter(window) {
            topics.push(m.as_str().trim().to_lowercase());
        }

        // 2. Markdown headers
        for cap in header_regex().captures_iter(window) {
            if let Some(h) = cap.get(1) {
                topics.push(h.as_str().trim().to_lowercase());
            }
        }

        // Dedupe preserving order, take top 8
        let mut seen = HashSet::new();
        topics.retain(|t| seen.insert(t.clone()));
        topics.truncate(8);

        // 3. Quotes (up to 3)
        let mut quotes = Vec::new();
        for cap in quote_regex().captures_iter(window) {
            if let Some(q) = cap.get(1) {
                quotes.push(format!("\"{}\"", q.as_str().trim()));
            }
            if quotes.len() >= 3 {
                break;
            }
        }

        for t in &topics {
            lines.push(
                super::pointer_index::PointerLine::new(
                    t.clone(),
                    entities.clone(),
                    Some(span.clone()),
                    vec![(*record_id).clone()],
                )
                .render(),
            );
        }
        for q in &quotes {
            lines.push(
                super::pointer_index::PointerLine::new(
                    q.clone(),
                    entities.clone(),
                    Some(span.clone()),
                    vec![(*record_id).clone()],
                )
                .render(),
            );
        }

        // Fallback if no actions, headers, or quotes matched
        if lines.is_empty() {
            let stem = Path::new(source_file)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("source");
            lines.push(
                super::pointer_index::PointerLine::new(
                    format!("{}/{}/{}", domain, topic, stem),
                    entities.clone(),
                    Some(span.clone()),
                    vec![(*record_id).clone()],
                )
                .render(),
            );
        }
    }

    lines
}

pub(crate) fn write_content_index(
    store: &super::MemoryStore,
    domain: &str,
    topic: &str,
    source_file: &str,
    chunks: &[TextChunk],
    ids: &HashSet<String>,
) -> Result<(), HarnessError> {
    let now = Utc::now();
    let mut ordered: Vec<&String> = ids.iter().collect();
    ordered.sort();

    let lines = build_pointer_lines(domain, topic, source_file, chunks, ids);

    let yaml = format!(
        "source_file: {}\nrecords: [{}]\nbuilt_at: {}\nchunk_indexes: [{}]",
        yaml_string(source_file),
        ordered
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        now.to_rfc3339(),
        (0..chunks.len())
            .map(|index| index.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let stem = ids::safe_id(
        Path::new(source_file)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("source"),
    );
    let path = store
        .root()
        .join("domains")
        .join(ids::slugify(domain))
        .join("topics")
        .join(ids::slugify(topic))
        .join("indexes")
        .join(format!("{stem}.index.md"));

    let mut budget = store.config.index_chars;
    let mut body = String::new();
    for line in lines {
        if line.chars().count() > budget {
            break;
        }
        budget = budget.saturating_sub(line.chars().count());
        body.push_str("- ");
        body.push_str(&line);
        body.push('\n');
    }
    atomic::write_str(&path, &super::frontmatter::render(&yaml, &body))
        .map_err(|error| HarnessError::MemoryError(format!("{}: {error}", path.display())))
}

pub(crate) fn write_summary(
    store: &super::MemoryStore,
    domain: &str,
    topic: &str,
    source_file: &str,
    text: &str,
    chunks: &[TextChunk],
) -> Result<(), HarnessError> {
    let summary = super::summary::generate_summary(domain, topic, source_file, 1, text, chunks);
    let stem = ids::safe_id(
        Path::new(source_file)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("source"),
    );
    let path = store
        .root()
        .join("domains")
        .join(ids::slugify(domain))
        .join("topics")
        .join(ids::slugify(topic))
        .join("summaries")
        .join(format!("{stem}.summary.md"));
    atomic::write_str(&path, &summary.render())
        .map_err(|error| HarnessError::MemoryError(format!("{}: {error}", path.display())))
}

pub(crate) fn yaml_string(value: &str) -> String {
    serde_yaml::to_string(&value)
        .unwrap_or_default()
        .trim()
        .to_string()
}

pub(crate) fn first_line(body: &str, fallback: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(120).collect())
        .unwrap_or_else(|| fallback.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MemoryStore, StoreConfig};

    #[test]
    fn chunks_preserve_lines_and_split_paragraphs() {
        let text = "First line\nSecond line\n\nThird line paragraph\nFourth line\n";
        let chunks = chunk_text(text, 200, 20, 10);
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].line_start, 1);
    }

    #[test]
    fn topic_detection_routes_by_folder_and_filename() {
        assert_eq!(detect_topic(Path::new("src/ui/Button.tsx"), ""), "frontend");
        assert_eq!(
            detect_topic(Path::new("docs/architecture.md"), ""),
            "documentation"
        );
        assert_eq!(
            detect_topic(Path::new("tests/search_test.rs"), ""),
            "testing"
        );
    }

    #[test]
    fn mines_a_directory_into_markdown_records() {
        let dir = std::env::temp_dir().join(format!("luna-mine-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/lib.rs"),
            "pub struct MemoryStore;\n// the store keeps Markdown records with plenty of words to pass min chunk size\n",
        )
        .unwrap();
        let store = MemoryStore::open(StoreConfig::new(dir.join(".memory"))).unwrap();
        let report = mine(&store, &dir, Some("demo")).unwrap();
        assert_eq!(report.files, 1);
        assert!(report.records >= 1);
        let records = store.active_records().unwrap();
        assert!(records
            .iter()
            .any(|record| record.body.contains("MemoryStore")));
        let _ = fs::remove_dir_all(&dir);
    }
}
