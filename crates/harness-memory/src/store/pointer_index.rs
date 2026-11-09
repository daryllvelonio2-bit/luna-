//! Content index: compact pointer lines mapping topics and
//! entities to record IDs.
//!
//! Content-index builder:
//! - Pointer line grammar: `topic | entity;entity | [YYYY-MM-DD:Lstart-Lend] | →record_id[,record_id]`
//! - Front matter tracking `source_file`, `records`, `built_at`, `chunk_indexes`
//! - Size limits: `index_chars` (default 1,500), at most 3 record pointers per line
//! - Index boost: rank-based boosting (0.40 / 0.25 / 0.15 / 0.08 / 0.04) for hybrid search

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde_yaml::Mapping;

use crate::store::frontmatter;
use crate::store::search::LexicalIndex;

pub const CLOSET_RANK_BOOSTS: &[f32] = &[0.40, 0.25, 0.15, 0.08, 0.04];
pub const MAX_RECORDS_PER_LINE: usize = 3;
pub const DEFAULT_INDEX_CHARS: usize = 1_500;

/// One parsed pointer line in a content index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PointerLine {
    pub topic: String,
    pub entities: Vec<String>,
    pub span: Option<String>,
    pub record_ids: Vec<String>,
}

impl PointerLine {
    pub fn new(
        topic: impl Into<String>,
        entities: Vec<String>,
        span: Option<String>,
        record_ids: Vec<String>,
    ) -> Self {
        Self {
            topic: topic.into(),
            entities,
            span,
            record_ids,
        }
    }

    /// Render pointer line according to specification grammar.
    pub fn render(&self) -> String {
        let topic_escaped = escape(&self.topic);
        let entities_escaped = self
            .entities
            .iter()
            .map(|e| escape(e))
            .collect::<Vec<_>>()
            .join(";");
        let ids: Vec<&str> = self
            .record_ids
            .iter()
            .take(MAX_RECORDS_PER_LINE)
            .map(String::as_str)
            .collect();
        let ids_str = ids.join(",");

        if let Some(span) = &self.span {
            format!(
                "{}|{}|{}|→{}",
                topic_escaped, entities_escaped, span, ids_str
            )
        } else {
            format!("{}|{}|→{}", topic_escaped, entities_escaped, ids_str)
        }
    }

    /// Parse a single pointer line (with or without leading "- ").
    pub fn parse(raw: &str) -> Option<Self> {
        let line = raw.trim().strip_prefix("- ").unwrap_or(raw.trim());
        if line.is_empty() {
            return None;
        }

        let segments = split_unescaped(line);
        if segments.len() < 3 {
            return None;
        }

        let topic = unescape(&segments[0]);
        let entities = if segments[1].is_empty() {
            Vec::new()
        } else {
            segments[1]
                .split(';')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(unescape)
                .collect()
        };

        let (span, id_part) = if segments.len() >= 4 {
            let span_candidate = segments[2].trim();
            (Some(span_candidate.to_string()), segments[3..].join("|"))
        } else {
            (None, segments[2].clone())
        };

        let id_str = id_part
            .trim()
            .strip_prefix('→')
            .or_else(|| id_part.trim().strip_prefix("->"))
            .unwrap_or(id_part.trim());

        let record_ids: Vec<String> = id_str
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();

        if record_ids.is_empty() {
            return None;
        }

        Some(Self {
            topic,
            entities,
            span,
            record_ids,
        })
    }
}

/// A parsed content index file (`.index.md`).
#[derive(Debug, Clone, PartialEq)]
pub struct ContentIndex {
    pub source_file: String,
    pub records: Vec<String>,
    pub built_at: DateTime<Utc>,
    pub chunk_indexes: Vec<usize>,
    pub lines: Vec<PointerLine>,
}

impl ContentIndex {
    pub fn parse(text: &str) -> Result<Self, String> {
        let (yaml, body) = frontmatter::split(text)
            .ok_or_else(|| "missing front matter in content index".to_string())?;
        let value = frontmatter::parse(yaml).map_err(|e| e.to_string())?;

        let source_file = frontmatter::string(&value, "source_file")
            .ok_or_else(|| "missing source_file in index front matter".to_string())?;
        let records = frontmatter::list(&value, "records");
        let built_at = frontmatter::string(&value, "built_at")
            .and_then(|t| DateTime::parse_from_rfc3339(&t).ok())
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let chunk_indexes = match value.get("chunk_indexes") {
            Some(serde_yaml::Value::Sequence(seq)) => seq
                .iter()
                .filter_map(|v| match v {
                    serde_yaml::Value::Number(n) => n.as_u64().map(|u| u as usize),
                    serde_yaml::Value::String(s) => s.parse().ok(),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };

        let lines: Vec<PointerLine> = body
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .filter_map(PointerLine::parse)
            .collect();

        Ok(Self {
            source_file,
            records,
            built_at,
            chunk_indexes,
            lines,
        })
    }

    /// Render content index into full Markdown document with front matter.
    pub fn render(&self, max_chars: usize) -> String {
        let mut map = Mapping::new();
        map.insert(
            serde_yaml::Value::String("source_file".to_string()),
            serde_yaml::Value::String(self.source_file.clone()),
        );
        map.insert(
            serde_yaml::Value::String("records".to_string()),
            serde_yaml::Value::Sequence(
                self.records
                    .iter()
                    .map(|r| serde_yaml::Value::String(r.clone()))
                    .collect(),
            ),
        );
        map.insert(
            serde_yaml::Value::String("built_at".to_string()),
            serde_yaml::Value::String(self.built_at.to_rfc3339()),
        );
        map.insert(
            serde_yaml::Value::String("chunk_indexes".to_string()),
            serde_yaml::Value::Sequence(
                self.chunk_indexes
                    .iter()
                    .map(|i| serde_yaml::Value::Number((*i).into()))
                    .collect(),
            ),
        );
        let yaml = serde_yaml::to_string(&map).unwrap_or_default();

        let mut body = String::new();
        let mut budget = max_chars;
        for line in &self.lines {
            let rendered = line.render();
            let cost = rendered.chars().count() + 3; // "- \n"
            if cost > budget && !body.is_empty() {
                break;
            }
            budget = budget.saturating_sub(cost);
            body.push_str("- ");
            body.push_str(&rendered);
            body.push('\n');
        }

        frontmatter::render(&yaml, &body)
    }
}

/// The content-index rank boost calculated for a source file.
#[derive(Debug, Clone)]
pub struct IndexBoost {
    pub source_file: String,
    pub rank: usize,
    pub boost: f32,
    pub preview: String,
}

/// Search content indexes using BM25 and calculate rank boosts per source file.
/// Direct port of the reference design's `_index_boosts` (query.py:106, query.py:368).
pub fn calculate_index_boosts(
    indexes: &[ContentIndex],
    query: &str,
) -> HashMap<String, IndexBoost> {
    if indexes.is_empty() || query.trim().is_empty() {
        return HashMap::new();
    }

    // Build documents for lexical index: one doc per content index combining all lines
    let mut docs = Vec::new();
    for (i, index) in indexes.iter().enumerate() {
        let mut text = String::new();
        for line in &index.lines {
            text.push_str(&line.topic);
            text.push(' ');
            text.push_str(&line.entities.join(" "));
            text.push(' ');
        }
        text.push_str(&index.source_file);
        docs.push((i.to_string(), text));
    }

    let lexical = LexicalIndex::build(docs);
    let hits = lexical.search(query, indexes.len());

    let mut boosts = HashMap::new();
    let mut rank = 0;

    for (idx_str, score) in hits {
        if score <= 0.0 || rank >= CLOSET_RANK_BOOSTS.len() {
            break;
        }
        let Ok(idx) = idx_str.parse::<usize>() else {
            continue;
        };
        let Some(index) = indexes.get(idx) else {
            continue;
        };

        if boosts.contains_key(&index.source_file) {
            continue;
        }

        let boost_val = CLOSET_RANK_BOOSTS[rank];
        let preview = index
            .lines
            .first()
            .map(|l| l.render())
            .unwrap_or_else(|| index.source_file.clone());

        boosts.insert(
            index.source_file.clone(),
            IndexBoost {
                source_file: index.source_file.clone(),
                rank,
                boost: boost_val,
                preview,
            },
        );
        rank += 1;
    }

    boosts
}

/// Escape `\` and `|` and convert newlines to space.
pub fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
}

/// Unescape `\|` and `\\`.
pub fn unescape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                if next == '|' || next == '\\' {
                    out.push(next);
                    chars.next();
                    continue;
                }
            }
        }
        out.push(c);
    }
    out.trim().to_string()
}

/// Split by unescaped `|`.
fn split_unescaped(line: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut escaped = false;

    for c in line.chars() {
        if escaped {
            current.push(c);
            escaped = false;
        } else if c == '\\' {
            current.push(c);
            escaped = true;
        } else if c == '|' {
            segments.push(current.trim().to_string());
            current = String::new();
        } else {
            current.push(c);
        }
    }
    segments.push(current.trim().to_string());
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pointer_line_render_and_parse() {
        let line = PointerLine::new(
            "retrieval is LIKE-based",
            vec!["engine.rs".to_string(), "sqlite".to_string()],
            Some("2026-11-08:L363-476".to_string()),
            vec!["rec_9f2c1a7b".to_string()],
        );
        let rendered = line.render();
        assert_eq!(
            rendered,
            "retrieval is LIKE-based|engine.rs;sqlite|2026-11-08:L363-476|→rec_9f2c1a7b"
        );

        let parsed = PointerLine::parse(&format!("- {rendered}")).unwrap();
        assert_eq!(parsed.topic, "retrieval is LIKE-based");
        assert_eq!(parsed.entities, vec!["engine.rs", "sqlite"]);
        assert_eq!(parsed.span.as_deref(), Some("2026-11-08:L363-476"));
        assert_eq!(parsed.record_ids, vec!["rec_9f2c1a7b"]);
    }

    #[test]
    fn test_pointer_line_escaping() {
        let line = PointerLine::new(
            "special | characters and \\ slashes",
            vec!["a|b".to_string()],
            None,
            vec!["rec_123".to_string()],
        );
        let rendered = line.render();
        assert!(rendered.contains(r"\|"));
        let parsed = PointerLine::parse(&rendered).unwrap();
        assert_eq!(parsed.topic, "special | characters and \\ slashes");
        assert_eq!(parsed.entities, vec!["a|b"]);
    }

    #[test]
    fn test_content_index_round_trip() {
        let index = ContentIndex {
            source_file: "crates/harness-memory/src/engine.rs".to_string(),
            records: vec!["rec_1".to_string(), "rec_2".to_string()],
            built_at: Utc::now(),
            chunk_indexes: vec![0, 1],
            lines: vec![
                PointerLine::new(
                    "engine init",
                    vec!["Engine".to_string()],
                    Some("2026-11-08:L1-10".to_string()),
                    vec!["rec_1".to_string()],
                ),
                PointerLine::new(
                    "search logic",
                    vec!["Search".to_string()],
                    Some("2026-11-08:L11-20".to_string()),
                    vec!["rec_2".to_string()],
                ),
            ],
        };

        let rendered = index.render(1500);
        let parsed = ContentIndex::parse(&rendered).unwrap();
        assert_eq!(parsed.source_file, index.source_file);
        assert_eq!(parsed.records, index.records);
        assert_eq!(parsed.lines.len(), 2);
        assert_eq!(parsed.lines[0].topic, "engine init");
    }

    #[test]
    fn test_index_boost_calculation() {
        let indexes = vec![
            ContentIndex {
                source_file: "auth/login.rs".to_string(),
                records: vec!["rec_auth".to_string()],
                built_at: Utc::now(),
                chunk_indexes: vec![0],
                lines: vec![PointerLine::new(
                    "built authentication system",
                    vec!["OAuth".to_string()],
                    None,
                    vec!["rec_auth".to_string()],
                )],
            },
            ContentIndex {
                source_file: "storage/db.rs".to_string(),
                records: vec!["rec_db".to_string()],
                built_at: Utc::now(),
                chunk_indexes: vec![0],
                lines: vec![PointerLine::new(
                    "sqlite database engine",
                    vec!["SQLite".to_string()],
                    None,
                    vec!["rec_db".to_string()],
                )],
            },
        ];

        let boosts = calculate_index_boosts(&indexes, "authentication OAuth");
        assert!(boosts.contains_key("auth/login.rs"));
        let boost = &boosts["auth/login.rs"];
        assert_eq!(boost.rank, 0);
        assert_eq!(boost.boost, 0.40);
        assert!(!boosts.contains_key("storage/db.rs"));
    }
}
