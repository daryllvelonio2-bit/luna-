//! Project miner: walk a tree, chunk text, and file verbatim records with
//! content indexes and compact summaries.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use chrono::Utc;
use harness_core::{HarnessError, MemoryCategory};
use walkdir::WalkDir;

use super::atomic;
use super::ids;
use super::Record;

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
    "rs", "md", "markdown", "toml", "yaml", "yml", "json", "jsonl", "ts", "tsx", "js", "jsx",
    "py", "go", "java", "c", "h", "cc", "cpp", "hpp", "sh", "bash", "zsh", "sql", "css",
    "scss", "html", "htm", "txt", "lua", "rb", "swift", "kt", "kts", "php", "ex", "exs", "erl",
    "hs", "ml", "clj", "zig", "nim", "cs", "fs", "vue", "svelte",
];

#[derive(Debug, Default)]
pub struct MineReport {
    pub files: usize,
    pub records: usize,
    pub skipped: usize,
    pub removed: usize,
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
        .or_else(|| dir.file_name().and_then(|name| name.to_str()).map(ids::slugify))
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
        let topic = topic_for(relative);
        let chunks = chunk(&text, store.config.chunk_chars);
        let mut new_ids = HashSet::new();

        for (index, (start, end, chunk_text)) in chunks.iter().enumerate() {
            let id = ids::record_id(&domain, &topic, &source_file, index, chunk_text);
            new_ids.insert(id.clone());
            if existing.iter().any(|record| record.id == id) {
                continue;
            }
            let entities = extract_entities(chunk_text);
            let record = Record {
                id,
                domain: domain.clone(),
                topic: topic.clone(),
                source_file: Some(source_file.clone()),
                chunk_index: index,
                line_start: *start,
                line_end: *end,
                filed_at: Utc::now(),
                entities: entities.clone(),
                tags: vec![Path::new(&source_file)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .unwrap_or("text")
                    .to_string()],
                body_hash: ids::body_hash(chunk_text),
                subject: topic.clone(),
                predicate: "source".to_string(),
                object: first_line(chunk_text, &source_file),
                name: None,
                path: Some(path.to_string_lossy().to_string()),
                raw_content: chunk_text.to_string(),
                category: MemoryCategory::Fact,
                confidence: 1.0,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                is_active: true,
                superseded_by: None,
                body: chunk_text.to_string(),
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

fn topic_for(relative: &Path) -> String {
    let components: Vec<String> = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str().map(str::to_string))
        .collect();
    if components.len() >= 2 {
        let first = components[0].to_ascii_lowercase();
        if matches!(first.as_str(), "crates" | "packages" | "libs" | "apps" | "src") {
            return ids::slugify(&components[1]);
        }
        return ids::slugify(&components[0]);
    }
    relative
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(ids::slugify)
        .unwrap_or_else(|| "general".to_string())
}

/// Split text on line boundaries into chunks of roughly `chunk_chars`.
/// Returns `(start_line, end_line, text)`, with 1-based inclusive lines.
fn chunk(text: &str, chunk_chars: usize) -> Vec<(usize, usize, String)> {
    let chunk_chars = chunk_chars.max(200);
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut start_line = 1usize;
    let mut line_number = 0usize;

    for line in text.split_inclusive('\n') {
        line_number += 1;
        if current.is_empty() {
            start_line = line_number;
        }
        current.push_str(line);
        if current.chars().count() >= chunk_chars {
            let trimmed = current.trim_end().to_string();
            if !trimmed.is_empty() {
                chunks.push((start_line, line_number, trimmed));
            }
            current.clear();
        }
    }
    if !current.trim().is_empty() {
        chunks.push((start_line, line_number, current.trim_end().to_string()));
    }
    if chunks.is_empty() {
        chunks.push((1, 1, text.trim().to_string()));
    }
    chunks
}

fn first_line(body: &str, fallback: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.chars().take(120).collect())
        .unwrap_or_else(|| fallback.to_string())
}

fn extract_entities(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut entities = Vec::new();
    for word in text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':')) {
        let word = word.trim_matches(':');
        if word.len() < 3 || word.len() > 64 {
            continue;
        }
        let looks_named = word
            .chars()
            .next()
            .map(|c| c.is_uppercase())
            .unwrap_or(false)
            || word.contains('_')
            || word.contains("::");
        if !looks_named {
            continue;
        }
        let key = word.to_ascii_lowercase();
        if seen.insert(key) {
            entities.push(word.to_string());
        }
        if entities.len() >= 16 {
            break;
        }
    }
    entities
}

fn write_content_index(
    store: &super::MemoryStore,
    domain: &str,
    topic: &str,
    source_file: &str,
    chunks: &[(usize, usize, String)],
    ids: &HashSet<String>,
) -> Result<(), HarnessError> {
    let now = Utc::now();
    let mut lines = Vec::new();
    let mut ordered: Vec<&String> = ids.iter().collect();
    ordered.sort();
    for (index, (start, end, text)) in chunks.iter().enumerate() {
        let Some(id) = ordered.get(index) else {
            continue;
        };
        let entities = extract_entities(text);
        let entity_cell = entities.iter().take(6).cloned().collect::<Vec<_>>().join(";");
        let label = first_line(text, source_file);
        lines.push(format!(
            "{} | {} | {}:L{}-L{} | →{}",
            escape(&label),
            escape(&entity_cell),
            now.date_naive(),
            start,
            end,
            id
        ));
    }
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
    atomic::write_str(&path, &super::frontmatter::render(&yaml, &body)).map_err(|error| {
        HarnessError::MemoryError(format!("{}: {error}", path.display()))
    })
}

fn write_summary(
    store: &super::MemoryStore,
    domain: &str,
    topic: &str,
    source_file: &str,
    text: &str,
    chunks: &[(usize, usize, String)],
) -> Result<(), HarnessError> {
    let now = Utc::now();
    let title = first_line(text, source_file);
    let mut body = format!(
        "FILE 1|{}/{}|{}|{}\n",
        domain,
        topic,
        now.date_naive(),
        escape(&title)
    );
    for (index, (_, _, chunk_text)) in chunks.iter().enumerate() {
        let entities = extract_entities(chunk_text);
        let keywords = super::search::tokenize(chunk_text)
            .into_iter()
            .take(6)
            .collect::<Vec<_>>()
            .join(";");
        let quote: String = first_line(chunk_text, "")
            .chars()
            .take(160)
            .collect();
        body.push_str(&format!(
            "S{}:{}|{}|\"{}\"|CORE|TECHNICAL\n",
            index + 1,
            escape(&entities.join(";")),
            escape(&keywords),
            escape(&quote)
        ));
    }
    let yaml = format!(
        "source_file: {}\nbuilt_at: {}\nlossy: true",
        yaml_string(source_file),
        now.to_rfc3339()
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
        .join("summaries")
        .join(format!("{stem}.summary.md"));
    atomic::write_str(&path, &super::frontmatter::render(&yaml, &body)).map_err(|error| {
        HarnessError::MemoryError(format!("{}: {error}", path.display()))
    })
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
}

fn yaml_string(value: &str) -> String {
    serde_yaml::to_string(&value)
        .unwrap_or_default()
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{MemoryStore, StoreConfig};

    #[test]
    fn chunks_preserve_lines() {
        let text = "one\ntwo\nthree\nfour\n";
        let chunks = chunk(text, 200);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].0, 1);
        assert_eq!(chunks[0].1, 4);
    }

    #[test]
    fn mines_a_directory_into_markdown_records() {
        let dir = std::env::temp_dir().join(format!("luna-mine-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("src/lib.rs"),
            "pub struct MemoryStore;\n// the store keeps Markdown records\n",
        )
        .unwrap();
        let store = MemoryStore::open(StoreConfig::new(dir.join(".memory"))).unwrap();
        let report = mine(&store, &dir, Some("demo")).unwrap();
        assert_eq!(report.files, 1);
        assert!(report.records >= 1);
        let records = store.active_records().unwrap();
        assert!(records.iter().any(|record| record.body.contains("MemoryStore")));
        let _ = fs::remove_dir_all(&dir);
    }
}
