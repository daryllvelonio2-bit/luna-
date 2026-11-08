//! Agent daily journal: per-agent, per-day, append-only Markdown journals.
//!
//! Direct implementation of MEMORY_STORE.md §4.7:
//!
//! ```markdown
//! ---
//! agent: luna
//! date: 2026-11-08
//! ---
//! ## 21:59
//! Investigated the memory store. Confirmed the retrieval pipeline is hybrid.
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use harness_core::HarnessError;
use serde::{Deserialize, Serialize};

use super::atomic;
use super::frontmatter;
use super::ids;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JournalEntry {
    pub agent: String,
    pub date: String,
    pub time: String,
    pub content: String,
    pub timestamp: DateTime<Utc>,
}

pub struct JournalStore {
    root: PathBuf,
}

impl JournalStore {
    pub fn new(store_root: PathBuf) -> Self {
        Self {
            root: store_root.join("journal"),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn agent_dir(&self, agent: &str) -> PathBuf {
        let slug = ids::slugify(agent);
        let safe_slug = if slug.is_empty() { "luna" } else { &slug };
        self.root.join(safe_slug)
    }

    /// Write an entry to the agent's journal for today (or specified timestamp).
    pub fn write_entry(
        &self,
        agent: &str,
        content: &str,
        timestamp: Option<DateTime<Utc>>,
    ) -> Result<JournalEntry, HarnessError> {
        let ts = timestamp.unwrap_or_else(Utc::now);
        let date_str = ts.format("%Y-%m-%d").to_string();
        let time_str = ts.format("%H:%M").to_string();

        let slug = ids::slugify(agent);
        let safe_slug = if slug.is_empty() {
            "luna".to_string()
        } else {
            slug
        };

        let dir = self.root.join(&safe_slug);
        fs::create_dir_all(&dir)
            .map_err(|e| HarnessError::MemoryError(format!("Failed to create journal dir: {e}")))?;

        let file_path = dir.join(format!("{date_str}.md"));
        let body = content.trim();

        if file_path.exists() {
            let mut existing = fs::read_to_string(&file_path).map_err(|e| {
                HarnessError::MemoryError(format!("Failed to read journal file: {e}"))
            })?;
            if !existing.ends_with('\n') {
                existing.push('\n');
            }
            existing.push_str(&format!("\n## {time_str}\n{body}\n"));
            atomic::write_str(&file_path, &existing).map_err(|e| {
                HarnessError::MemoryError(format!("Failed to write journal entry: {e}"))
            })?;
        } else {
            let front = format!("agent: {}\ndate: {}", safe_slug, date_str);
            let doc_body = format!("## {time_str}\n{body}\n");
            let rendered = frontmatter::render(&front, &doc_body);
            atomic::write_str(&file_path, &rendered).map_err(|e| {
                HarnessError::MemoryError(format!("Failed to write journal entry: {e}"))
            })?;
        }

        Ok(JournalEntry {
            agent: safe_slug,
            date: date_str,
            time: time_str,
            content: body.to_string(),
            timestamp: ts,
        })
    }

    /// Read journal entries for an agent, optionally bounded by date or limit.
    pub fn read_entries(
        &self,
        agent: &str,
        date: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<JournalEntry>, HarnessError> {
        let dir = self.agent_dir(agent);
        if !dir.exists() {
            return Ok(Vec::new());
        }

        let mut files: Vec<PathBuf> = Vec::new();
        if let Some(target_date) = date {
            let path = dir.join(format!("{target_date}.md"));
            if path.exists() {
                files.push(path);
            }
        } else {
            let Ok(entries) = fs::read_dir(&dir) else {
                return Ok(Vec::new());
            };
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_file() && p.extension().and_then(|s| s.to_str()).unwrap_or("") == "md" {
                    files.push(p);
                }
            }
            // Sort newest date first
            files.sort_by(|a, b| b.file_name().cmp(&a.file_name()));
        }

        let mut entries = Vec::new();
        for file in files {
            let Ok(text) = fs::read_to_string(&file) else {
                continue;
            };
            let (yaml_part, body_part) = match frontmatter::split(&text) {
                Some((y, b)) => (y, b),
                None => ("", text.as_str()),
            };

            let date_from_front = if !yaml_part.is_empty() {
                frontmatter::parse(yaml_part)
                    .ok()
                    .and_then(|val| frontmatter::string(&val, "date"))
            } else {
                None
            };

            let date_str = date_from_front.unwrap_or_else(|| {
                file.file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("2026-01-01")
                    .to_string()
            });

            let mut file_entries = parse_day_body(agent, &date_str, body_part);
            file_entries.reverse();
            entries.extend(file_entries);
        }

        // Stable sort descending by timestamp: later days and later times first
        entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        if let Some(lim) = limit {
            entries.truncate(lim);
        }

        Ok(entries)
    }
}

fn parse_day_body(agent: &str, date_str: &str, body: &str) -> Vec<JournalEntry> {
    let mut entries = Vec::new();
    let mut current_time: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## ") {
            if let Some(time) = current_time.take() {
                let content = current_lines.join("\n").trim().to_string();
                if !content.is_empty() {
                    let ts = make_timestamp(date_str, &time);
                    entries.push(JournalEntry {
                        agent: agent.to_string(),
                        date: date_str.to_string(),
                        time,
                        content,
                        timestamp: ts,
                    });
                }
                current_lines.clear();
            }
            let time_str = trimmed[3..].trim().to_string();
            current_time = Some(time_str);
        } else if current_time.is_some() {
            current_lines.push(line);
        }
    }

    if let Some(time) = current_time.take() {
        let content = current_lines.join("\n").trim().to_string();
        if !content.is_empty() {
            let ts = make_timestamp(date_str, &time);
            entries.push(JournalEntry {
                agent: agent.to_string(),
                date: date_str.to_string(),
                time,
                content,
                timestamp: ts,
            });
        }
    }

    entries
}

fn make_timestamp(date_str: &str, time_str: &str) -> DateTime<Utc> {
    let date = NaiveDate::parse_from_str(date_str, "%Y-%m-%d")
        .unwrap_or_else(|_| NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());

    let clean_time = time_str.split_whitespace().next().unwrap_or("00:00");
    let time = if clean_time.len() == 5 {
        NaiveTime::parse_from_str(clean_time, "%H:%M")
    } else {
        NaiveTime::parse_from_str(clean_time, "%H:%M:%S")
    }
    .unwrap_or_else(|_| NaiveTime::from_hms_opt(0, 0, 0).unwrap());

    let ndt = NaiveDateTime::new(date, time);
    DateTime::<Utc>::from_naive_utc_and_offset(ndt, Utc)
}
