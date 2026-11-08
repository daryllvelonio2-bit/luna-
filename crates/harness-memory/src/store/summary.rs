//! Compact summary (AAAK Dialect): structured symbolic summary layer.
//!
//! Direct port of MemPalace's `dialect.py`:
//! - Grammar:
//!     Header:  FILE_NUM|DOMAIN/TOPIC|DATE|TITLE
//!     Entry:   S<id>:ENTITIES|topic_keywords|"key_quote"|WEIGHT|FLAGS
//!     Cross:   X:S<id><->S<id>|label
//!     Arc:     ARC:state->state->state
//! - Flags: ORIGIN, CORE, SENSITIVE, PIVOT, DECISION, TECHNICAL
//! - Lossy: explicitly lossy; preserves pointers to verbatim records.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use chrono::{DateTime, NaiveDate, Utc};
use regex::Regex;
use serde_yaml::Mapping;

use crate::store::frontmatter;
use crate::store::miner::TextChunk;
use crate::store::search::tokenize;

pub const SUMMARY_SPEC: &str = r#"# Compact Summary Specification (AAAK Dialect)

A lossy structured symbolic summary format for fast topical scanning and cross-linking.

## Format Grammar
Header:  FILE_NUM|DOMAIN/TOPIC|DATE|TITLE
Entry:   S<id>:ENTITIES|topic_keywords|"key_quote"|WEIGHT|FLAGS
Cross:   X:S<id><->S<id>|label
Arc:     ARC:state->state->state

## Flags
ORIGIN    - origin moment (birth of something)
CORE      - core belief or identity pillar
SENSITIVE - handle with absolute care
PIVOT     - emotional or structural turning point
DECISION  - explicit architectural or design choice
TECHNICAL - technical architecture or implementation detail

## State / Emotion Signals
neutral, focused, determ, convict, anx, excite, frust, confuse,
love, rage, hope, fear, trust, joy, grief, surprise, grat, curious,
wonder, relief, satis
"#;

static QUOTE_RE: OnceLock<Regex> = OnceLock::new();
static ENTITY_WORD_RE: OnceLock<Regex> = OnceLock::new();

fn quote_regex() -> &'static Regex {
    QUOTE_RE.get_or_init(|| Regex::new(r#""([^"]{8,120})""#).unwrap())
}

fn entity_word_regex() -> &'static Regex {
    ENTITY_WORD_RE.get_or_init(|| Regex::new(r"\b[A-Z][a-z]+(?:[A-Z][a-z]+|[A-Z]{2,})*\b").unwrap())
}

/// Port of MemPalace's `_FLAG_SIGNALS` in dialect.py.
const FLAG_SIGNALS: &[(&str, &str)] = &[
    ("decided", "DECISION"),
    ("chose", "DECISION"),
    ("switched", "DECISION"),
    ("migrated", "DECISION"),
    ("replaced", "DECISION"),
    ("instead of", "DECISION"),
    ("because", "DECISION"),
    ("founded", "ORIGIN"),
    ("created", "ORIGIN"),
    ("started", "ORIGIN"),
    ("born", "ORIGIN"),
    ("launched", "ORIGIN"),
    ("first time", "ORIGIN"),
    ("core", "CORE"),
    ("fundamental", "CORE"),
    ("essential", "CORE"),
    ("principle", "CORE"),
    ("belief", "CORE"),
    ("always", "CORE"),
    ("never forget", "CORE"),
    ("turning point", "PIVOT"),
    ("changed everything", "PIVOT"),
    ("realized", "PIVOT"),
    ("breakthrough", "PIVOT"),
    ("epiphany", "PIVOT"),
    ("api", "TECHNICAL"),
    ("database", "TECHNICAL"),
    ("architecture", "TECHNICAL"),
    ("deploy", "TECHNICAL"),
    ("infrastructure", "TECHNICAL"),
    ("algorithm", "TECHNICAL"),
    ("framework", "TECHNICAL"),
    ("server", "TECHNICAL"),
    ("config", "TECHNICAL"),
];

/// Port of MemPalace's `_EMOTION_SIGNALS` in dialect.py.
const EMOTION_SIGNALS: &[(&str, &str)] = &[
    ("decided", "determ"),
    ("prefer", "convict"),
    ("worried", "anx"),
    ("excited", "excite"),
    ("frustrated", "frust"),
    ("confused", "confuse"),
    ("love", "love"),
    ("hate", "rage"),
    ("hope", "hope"),
    ("fear", "fear"),
    ("trust", "trust"),
    ("happy", "joy"),
    ("sad", "grief"),
    ("curious", "curious"),
    ("anxious", "anx"),
    ("relieved", "relief"),
    ("satisf", "satis"),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryHeader {
    pub file_num: usize,
    pub domain: String,
    pub topic: String,
    pub date: NaiveDate,
    pub title: String,
}

impl SummaryHeader {
    pub fn render(&self) -> String {
        format!(
            "FILE {}|{}/{}|{}|{}",
            self.file_num,
            self.domain,
            self.topic,
            self.date,
            escape(&self.title)
        )
    }

    pub fn parse(line: &str) -> Option<Self> {
        let trimmed = line.trim();
        if !trimmed.starts_with("FILE ") {
            return None;
        }
        let parts: Vec<&str> = trimmed.split('|').collect();
        if parts.len() < 4 {
            return None;
        }

        let num_str = parts[0].strip_prefix("FILE ")?.trim();
        let file_num = num_str.parse().unwrap_or(1);

        let (domain, topic) = match parts[1].split_once('/') {
            Some((d, t)) => (d.trim().to_string(), t.trim().to_string()),
            None => ("general".to_string(), parts[1].trim().to_string()),
        };

        let date = NaiveDate::parse_from_str(parts[2].trim(), "%Y-%m-%d")
            .unwrap_or_else(|_| Utc::now().date_naive());

        let title = unescape(parts[3..].join("|").trim());

        Some(Self {
            file_num,
            domain,
            topic,
            date,
            title,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryEntry {
    pub id: usize,
    pub entities: Vec<String>,
    pub keywords: Vec<String>,
    pub quote: String,
    pub weight: String,
    pub flags: Vec<String>,
}

impl SummaryEntry {
    pub fn render(&self) -> String {
        let ent = escape(&self.entities.join(";"));
        let kw = escape(&self.keywords.join(";"));
        let q = escape(&self.quote);
        let flags = if self.flags.is_empty() {
            String::new()
        } else {
            format!("|{}", self.flags.join("|"))
        };
        format!(
            "S{}:{}|{}|\"{}\"|{}{}",
            self.id, ent, kw, q, self.weight, flags
        )
    }

    pub fn parse(line: &str) -> Option<Self> {
        let trimmed = line.trim();
        if !trimmed.starts_with('S') {
            return None;
        }
        let (id_part, rest) = trimmed.split_once(':')?;
        let id: usize = id_part[1..].parse().ok()?;

        let parts: Vec<&str> = rest.split('|').collect();
        if parts.len() < 4 {
            return None;
        }

        let entities = parts[0]
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(unescape)
            .collect();

        let keywords = parts[1]
            .split(';')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(unescape)
            .collect();

        let raw_quote = parts[2].trim().trim_matches('"');
        let quote = unescape(raw_quote);

        let weight = parts[3].trim().to_string();

        let flags = if parts.len() > 4 {
            parts[4..]
                .iter()
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect()
        } else {
            Vec::new()
        };

        Some(Self {
            id,
            entities,
            keywords,
            quote,
            weight,
            flags,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryCross {
    pub from_id: usize,
    pub to_id: usize,
    pub label: String,
}

impl SummaryCross {
    pub fn render(&self) -> String {
        format!(
            "X:S{}<->S{}|{}",
            self.from_id,
            self.to_id,
            escape(&self.label)
        )
    }

    pub fn parse(line: &str) -> Option<Self> {
        let trimmed = line.trim();
        if !trimmed.starts_with("X:S") {
            return None;
        }
        let (arrow_part, label_part) = trimmed.split_once('|')?;
        let rest = arrow_part.strip_prefix("X:S")?;
        let (from_str, to_str) = rest.split_once("<->S")?;
        let from_id: usize = from_str.trim().parse().ok()?;
        let to_id: usize = to_str.trim().parse().ok()?;
        let label = unescape(label_part.trim());
        Some(Self {
            from_id,
            to_id,
            label,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryArc {
    pub states: Vec<String>,
}

impl SummaryArc {
    pub fn render(&self) -> String {
        format!("ARC:{}", self.states.join("->"))
    }

    pub fn parse(line: &str) -> Option<Self> {
        let trimmed = line.trim();
        let rest = trimmed.strip_prefix("ARC:")?;
        let states = rest
            .split("->")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();
        Some(Self { states })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompactSummary {
    pub source_file: String,
    pub built_at: DateTime<Utc>,
    pub lossy: bool,
    pub header: Option<SummaryHeader>,
    pub entries: Vec<SummaryEntry>,
    pub crosses: Vec<SummaryCross>,
    pub arc: Option<SummaryArc>,
}

impl CompactSummary {
    pub fn parse(text: &str) -> Result<Self, String> {
        let (yaml, body) = frontmatter::split(text)
            .ok_or_else(|| "missing front matter in compact summary".to_string())?;
        let value = frontmatter::parse(yaml).map_err(|e| e.to_string())?;

        let source_file = frontmatter::string(&value, "source_file")
            .ok_or_else(|| "missing source_file in summary front matter".to_string())?;
        let built_at = frontmatter::string(&value, "built_at")
            .and_then(|t| DateTime::parse_from_rfc3339(&t).ok())
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);
        let lossy = frontmatter::bool(&value, "lossy", true);

        let mut header = None;
        let mut entries = Vec::new();
        let mut crosses = Vec::new();
        let mut arc = None;

        for line in body.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed.starts_with("FILE ") {
                header = SummaryHeader::parse(trimmed);
            } else if trimmed.starts_with('S') && trimmed.contains(':') {
                if let Some(entry) = SummaryEntry::parse(trimmed) {
                    entries.push(entry);
                }
            } else if trimmed.starts_with("X:") {
                if let Some(cross) = SummaryCross::parse(trimmed) {
                    crosses.push(cross);
                }
            } else if trimmed.starts_with("ARC:") {
                arc = SummaryArc::parse(trimmed);
            }
        }

        Ok(Self {
            source_file,
            built_at,
            lossy,
            header,
            entries,
            crosses,
            arc,
        })
    }

    pub fn render(&self) -> String {
        let mut map = Mapping::new();
        map.insert(
            serde_yaml::Value::String("source_file".to_string()),
            serde_yaml::Value::String(self.source_file.clone()),
        );
        map.insert(
            serde_yaml::Value::String("built_at".to_string()),
            serde_yaml::Value::String(self.built_at.to_rfc3339()),
        );
        map.insert(
            serde_yaml::Value::String("lossy".to_string()),
            serde_yaml::Value::Bool(self.lossy),
        );
        let yaml = serde_yaml::to_string(&map).unwrap_or_default();

        let mut body = String::new();
        if let Some(h) = &self.header {
            body.push_str(&h.render());
            body.push('\n');
        }
        for entry in &self.entries {
            body.push_str(&entry.render());
            body.push('\n');
        }
        for cross in &self.crosses {
            body.push_str(&cross.render());
            body.push('\n');
        }
        if let Some(arc) = &self.arc {
            body.push_str(&arc.render());
            body.push('\n');
        }

        frontmatter::render(&yaml, &body)
    }
}

/// Generate a structured symbolic summary from file text and chunks.
/// Direct port of MemPalace's `dialect.py:encode_text`.
pub fn generate_summary(
    domain: &str,
    topic: &str,
    source_file: &str,
    file_num: usize,
    text: &str,
    chunks: &[TextChunk],
) -> CompactSummary {
    let now = Utc::now();
    let title = first_meaningful_line(text, source_file);

    let header = SummaryHeader {
        file_num,
        domain: domain.to_string(),
        topic: topic.to_string(),
        date: now.date_naive(),
        title,
    };

    let mut entries = Vec::new();
    let mut state_sequence = Vec::new();

    for (i, chunk) in chunks.iter().enumerate() {
        let window = if chunk.content.len() > 2000 {
            &chunk.content[..2000]
        } else {
            &chunk.content
        };
        let lower = window.to_lowercase();

        // 1. Entities
        let entities = extract_entities(window);

        // 2. Keywords
        let keywords: Vec<String> = tokenize(window).into_iter().take(5).collect();

        // 3. Key quote
        let quote = extract_quote(window);

        // 4. Flags
        let mut flags = Vec::new();
        for &(pattern, flag) in FLAG_SIGNALS {
            if lower.contains(pattern) && !flags.contains(&flag.to_string()) {
                flags.push(flag.to_string());
            }
        }
        if flags.is_empty() {
            flags.push("TECHNICAL".to_string());
        }

        // 5. Weight
        let weight = if flags.contains(&"CORE".to_string()) {
            "CORE".to_string()
        } else if flags.contains(&"DECISION".to_string()) {
            "DECISION".to_string()
        } else {
            "NORMAL".to_string()
        };

        // 6. Emotion/state signal
        let mut state = "neutral";
        for &(pattern, signal) in EMOTION_SIGNALS {
            if lower.contains(pattern) {
                state = signal;
                break;
            }
        }
        if state_sequence.last().copied() != Some(state) {
            state_sequence.push(state);
        }

        entries.push(SummaryEntry {
            id: i + 1,
            entities,
            keywords,
            quote,
            weight,
            flags,
        });
    }

    // Cross links: pairs of entries that share 2+ entities
    let mut crosses = Vec::new();
    for i in 0..entries.len() {
        for j in (i + 1)..entries.len() {
            let set_a: HashSet<&String> = entries[i].entities.iter().collect();
            let common: Vec<&String> = entries[j]
                .entities
                .iter()
                .filter(|e| set_a.contains(e))
                .collect();
            if common.len() >= 2 {
                let label = common
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(";");
                crosses.push(SummaryCross {
                    from_id: entries[i].id,
                    to_id: entries[j].id,
                    label,
                });
            }
        }
    }

    let arc = if state_sequence.is_empty() {
        Some(SummaryArc {
            states: vec!["neutral".to_string(), "focused".to_string()],
        })
    } else if state_sequence.len() == 1 {
        Some(SummaryArc {
            states: vec![state_sequence[0].to_string(), "focused".to_string()],
        })
    } else {
        Some(SummaryArc {
            states: state_sequence.into_iter().map(str::to_string).collect(),
        })
    };

    CompactSummary {
        source_file: source_file.to_string(),
        built_at: now,
        lossy: true,
        header: Some(header),
        entries,
        crosses,
        arc,
    }
}

fn extract_entities(text: &str) -> Vec<String> {
    let re = entity_word_regex();
    let mut counts: HashMap<String, usize> = HashMap::new();
    let stop_words: HashSet<&'static str> = crate::store::search::MEMPALACE_STOPWORDS
        .iter()
        .copied()
        .collect();

    for m in re.find_iter(text) {
        let word = m.as_str();
        let lower = word.to_lowercase();
        if stop_words.contains(lower.as_str()) || word.len() < 3 {
            continue;
        }
        *counts.entry(word.to_string()).or_insert(0) += 1;
    }

    let mut frequent: Vec<(String, usize)> = counts.into_iter().collect();
    frequent.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    frequent.into_iter().take(5).map(|(w, _)| w).collect()
}

fn extract_quote(text: &str) -> String {
    if let Some(cap) = quote_regex().captures(text) {
        if let Some(q) = cap.get(1) {
            return q.as_str().trim().to_string();
        }
    }
    // Fallback: take first sentence/line up to 60 chars
    first_meaningful_line(text, "").chars().take(60).collect()
}

fn first_meaningful_line(body: &str, fallback: &str) -> String {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.chars().take(80).collect())
        .unwrap_or_else(|| fallback.to_string())
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('|', "\\|")
        .replace(['\r', '\n'], " ")
}

fn unescape(value: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_summary_spec_constant() {
        assert!(SUMMARY_SPEC.contains("FILE_NUM|DOMAIN/TOPIC|DATE|TITLE"));
        assert!(SUMMARY_SPEC.contains("TECHNICAL"));
    }

    #[test]
    fn test_summary_header_parse_render() {
        let header = SummaryHeader {
            file_num: 7,
            domain: "luna".to_string(),
            topic: "memory".to_string(),
            date: NaiveDate::from_ymd_opt(2026, 11, 8).unwrap(),
            title: "Memory engine".to_string(),
        };
        let line = header.render();
        assert_eq!(line, "FILE 7|luna/memory|2026-11-08|Memory engine");
        let parsed = SummaryHeader::parse(&line).unwrap();
        assert_eq!(parsed, header);
    }

    #[test]
    fn test_summary_entry_parse_render() {
        let entry = SummaryEntry {
            id: 1,
            entities: vec!["engine".to_string(), "sqlite".to_string()],
            keywords: vec!["retrieval".to_string(), "fallback".to_string()],
            quote: "LIKE over six columns".to_string(),
            weight: "CORE".to_string(),
            flags: vec!["TECHNICAL".to_string()],
        };
        let line = entry.render();
        assert_eq!(
            line,
            "S1:engine;sqlite|retrieval;fallback|\"LIKE over six columns\"|CORE|TECHNICAL"
        );
        let parsed = SummaryEntry::parse(&line).unwrap();
        assert_eq!(parsed, entry);
    }

    #[test]
    fn test_summary_cross_and_arc() {
        let cross = SummaryCross {
            from_id: 1,
            to_id: 2,
            label: "retrieval feeds ranking".to_string(),
        };
        let cross_line = cross.render();
        assert_eq!(cross_line, "X:S1<->S2|retrieval feeds ranking");
        assert_eq!(SummaryCross::parse(&cross_line).unwrap(), cross);

        let arc = SummaryArc {
            states: vec!["neutral".to_string(), "focused".to_string()],
        };
        let arc_line = arc.render();
        assert_eq!(arc_line, "ARC:neutral->focused");
        assert_eq!(SummaryArc::parse(&arc_line).unwrap(), arc);
    }

    #[test]
    fn test_compact_summary_round_trip() {
        let summary = CompactSummary {
            source_file: "crates/harness-memory/src/engine.rs".to_string(),
            built_at: Utc::now(),
            lossy: true,
            header: Some(SummaryHeader {
                file_num: 1,
                domain: "luna".to_string(),
                topic: "memory".to_string(),
                date: Utc::now().date_naive(),
                title: "Engine Implementation".to_string(),
            }),
            entries: vec![SummaryEntry {
                id: 1,
                entities: vec!["Engine".to_string()],
                keywords: vec!["memory".to_string()],
                quote: "test quote".to_string(),
                weight: "CORE".to_string(),
                flags: vec!["TECHNICAL".to_string()],
            }],
            crosses: vec![],
            arc: Some(SummaryArc {
                states: vec!["neutral".to_string(), "focused".to_string()],
            }),
        };

        let rendered = summary.render();
        let parsed = CompactSummary::parse(&rendered).unwrap();
        assert_eq!(parsed.source_file, summary.source_file);
        assert_eq!(parsed.entries.len(), 1);
        assert_eq!(parsed.entries[0].quote, "test quote");
        assert_eq!(parsed.arc.unwrap().states, vec!["neutral", "focused"]);
    }
}
