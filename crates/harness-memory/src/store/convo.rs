//! Conversation miner: mine chat export files (JSONL, JSON, Markdown/TXT) into the `conversations` domain.
//!
//! Direct implementation of MEMORY_STORE.md §6.3:
//! - Normalizes export formats to canonical exchanges (`> user` + `assistant`).
//! - Stores verbatim exchanges with `extract_mode: transcript`.
//! - Builds content indexes and compact summaries.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use chrono::Utc;
use harness_core::{HarnessError, MemoryCategory};
use walkdir::WalkDir;

use super::ids;
use super::miner::{self, MineReport, TextChunk};
use super::{MemoryStore, Record};

const CONVO_EXTENSIONS: &[&str] = &["jsonl", "json", "txt", "md"];
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
];

/// Mine a file or directory tree of conversation transcripts.
pub fn mine_convos(
    store: &MemoryStore,
    path: &Path,
    domain_override: Option<&str>,
    topic_override: Option<&str>,
) -> Result<MineReport, HarnessError> {
    if !path.exists() {
        return Err(HarnessError::MemoryError(format!(
            "Path does not exist: {}",
            path.display()
        )));
    }

    let domain = domain_override.unwrap_or("conversations").to_string();
    let mut files = Vec::new();

    if path.is_file() {
        files.push(path.to_path_buf());
    } else {
        for entry in WalkDir::new(path).into_iter().filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !SKIP_DIRS.contains(&name.as_ref())
        }) {
            let Ok(entry) = entry else {
                continue;
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let ext = entry
                .path()
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();
            if CONVO_EXTENSIONS.contains(&ext.as_str()) {
                files.push(entry.path().to_path_buf());
            }
        }
    }

    let mut report = MineReport::default();

    for file_path in files {
        let Ok(text) = fs::read_to_string(&file_path) else {
            report.skipped += 1;
            continue;
        };
        if text.trim().is_empty() {
            report.skipped += 1;
            continue;
        }

        let stem = file_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("chat");
        let topic_raw = topic_override.unwrap_or(stem);
        let topic = ids::slugify(topic_raw);
        let source_file = file_path.to_string_lossy().to_string();
        let authored_at = fs::metadata(&file_path)
            .ok()
            .and_then(|meta| meta.modified().ok())
            .map(chrono::DateTime::<Utc>::from)
            .unwrap_or_else(Utc::now);

        let exchanges = parse_exchanges(&file_path, &text);
        if exchanges.is_empty() {
            report.skipped += 1;
            continue;
        }

        let mut chunks = Vec::new();
        let mut new_ids = HashSet::new();
        let mut all_text = String::new();

        for (exchange_idx, exchange_body) in exchanges.into_iter().enumerate() {
            let id = ids::record_id_from_chunk(&domain, &topic, &source_file, exchange_idx);
            new_ids.insert(id.clone());

            let entities = miner::extract_entities(&exchange_body);
            let first = miner::first_line(&exchange_body, &source_file);

            let record = Record {
                id: id.clone(),
                domain: domain.clone(),
                topic: topic.clone(),
                source_file: Some(source_file.clone()),
                chunk_index: exchange_idx,
                line_start: 1,
                line_end: exchange_body.lines().count().max(1),
                filed_at: Utc::now(),
                entities,
                tags: vec![
                    "conversation".to_string(),
                    "transcript".to_string(),
                    topic.clone(),
                ],
                body_hash: ids::body_hash(&exchange_body),
                subject: format!("convo.{}", ids::slugify(stem)),
                predicate: format!("exchange.{}", exchange_idx),
                object: first,
                name: Some(format!("{} [exchange {}]", topic_raw, exchange_idx)),
                path: Some(source_file.clone()),
                raw_content: exchange_body.clone(),
                category: MemoryCategory::Fact,
                confidence: 1.0,
                created_at: authored_at,
                updated_at: Utc::now(),
                is_active: true,
                superseded_by: None,
                body: exchange_body.clone(),
            };

            store.write_record(record)?;
            report.records += 1;

            if !all_text.is_empty() {
                all_text.push_str("\n\n");
            }
            all_text.push_str(&exchange_body);

            chunks.push(TextChunk {
                content: exchange_body.clone(),
                chunk_index: exchange_idx,
                line_start: 1,
                line_end: exchange_body.lines().count().max(1),
            });
        }

        miner::write_content_index(store, &domain, &topic, &source_file, &chunks, &new_ids)?;
        miner::write_summary(store, &domain, &topic, &source_file, &all_text, &chunks)?;
        report.files += 1;
    }

    store.reindex()?;
    Ok(report)
}

/// Parse file content into canonical exchanges (`> user\n\nassistant`).
fn parse_exchanges(file_path: &Path, content: &str) -> Vec<String> {
    let ext = file_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "jsonl" => parse_jsonl(content),
        "json" => parse_json(content),
        _ => parse_transcript_text(content),
    }
}

fn parse_jsonl(content: &str) -> Vec<String> {
    let mut messages: Vec<(String, String)> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };

        // Case 1: exchange object {"user": "...", "assistant": "..."}
        if let (Some(u), Some(a)) = (
            val.get("user").and_then(|v| v.as_str()),
            val.get("assistant").and_then(|v| v.as_str()),
        ) {
            messages.push(("user".to_string(), u.to_string()));
            messages.push(("assistant".to_string(), a.to_string()));
            continue;
        }

        // Case 2: role/content message {"role": "user", "content": "..."}
        if let (Some(role), Some(text)) = (
            val.get("role").and_then(|v| v.as_str()),
            val.get("content").and_then(|v| v.as_str()),
        ) {
            messages.push((role.to_string(), text.to_string()));
            continue;
        }

        // Case 3: Claude Code JSONL {"type": "user"|"assistant", "message": {"content": ...}}
        if let Some(msg_type) = val.get("type").and_then(|v| v.as_str()) {
            if let Some(text) = val
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_str())
            {
                messages.push((msg_type.to_string(), text.to_string()));
            }
        }
    }

    pair_messages(messages)
}

fn parse_json(content: &str) -> Vec<String> {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(content) else {
        return parse_transcript_text(content);
    };

    if let Some(arr) = val.as_array() {
        let mut messages = Vec::new();
        for item in arr {
            if let (Some(u), Some(a)) = (
                item.get("user").and_then(|v| v.as_str()),
                item.get("assistant").and_then(|v| v.as_str()),
            ) {
                messages.push(("user".to_string(), u.to_string()));
                messages.push(("assistant".to_string(), a.to_string()));
                continue;
            }
            if let (Some(role), Some(text)) = (
                item.get("role").and_then(|v| v.as_str()),
                item.get("content").and_then(|v| v.as_str()),
            ) {
                messages.push((role.to_string(), text.to_string()));
            }
        }
        if !messages.is_empty() {
            return pair_messages(messages);
        }
    }

    parse_transcript_text(content)
}

fn parse_transcript_text(content: &str) -> Vec<String> {
    let mut normalized_lines = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(user_text) = trimmed.strip_prefix("User:") {
            normalized_lines.push(format!("> {}", user_text.trim()));
        } else if let Some(user_text) = trimmed.strip_prefix("Human:") {
            normalized_lines.push(format!("> {}", user_text.trim()));
        } else if let Some(ai_text) = trimmed.strip_prefix("Assistant:") {
            normalized_lines.push(ai_text.trim().to_string());
        } else if let Some(ai_text) = trimmed.strip_prefix("AI:") {
            normalized_lines.push(ai_text.trim().to_string());
        } else {
            normalized_lines.push(line.to_string());
        }
    }

    let mut exchanges = Vec::new();
    let mut current_user: Option<String> = None;
    let mut current_assistant: Vec<String> = Vec::new();

    for line in normalized_lines {
        let trimmed = line.trim();
        if trimmed.starts_with('>') {
            if let Some(user) = current_user.take() {
                let ai = current_assistant.join("\n").trim().to_string();
                if ai.is_empty() {
                    exchanges.push(format!("> {user}"));
                } else {
                    exchanges.push(format!("> {user}\n\n{ai}"));
                }
                current_assistant.clear();
            }
            let user_part = trimmed.trim_start_matches('>').trim().to_string();
            current_user = Some(user_part);
        } else if current_user.is_some() && (!trimmed.is_empty() || !current_assistant.is_empty()) {
            current_assistant.push(line);
        }
    }

    if let Some(user) = current_user.take() {
        let ai = current_assistant.join("\n").trim().to_string();
        if ai.is_empty() {
            exchanges.push(format!("> {user}"));
        } else {
            exchanges.push(format!("> {user}\n\n{ai}"));
        }
    }

    if exchanges.is_empty() {
        let chunks = miner::chunk_text(content, 800, 100, 50);
        chunks.into_iter().map(|c| c.content).collect()
    } else {
        exchanges
    }
}

fn pair_messages(messages: Vec<(String, String)>) -> Vec<String> {
    let mut exchanges = Vec::new();
    let mut current_user: Option<String> = None;
    let mut current_assistant: Vec<String> = Vec::new();

    for (role, text) in messages {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            continue;
        }
        let role_low = role.to_lowercase();
        if role_low == "user" || role_low == "human" {
            if let Some(u) = current_user.take() {
                let a = current_assistant.join("\n\n").trim().to_string();
                if a.is_empty() {
                    exchanges.push(format!("> {u}"));
                } else {
                    exchanges.push(format!("> {u}\n\n{a}"));
                }
                current_assistant.clear();
            }
            current_user = Some(trimmed.to_string());
        } else if role_low == "assistant" || role_low == "ai" {
            current_assistant.push(trimmed.to_string());
        }
    }

    if let Some(u) = current_user.take() {
        let a = current_assistant.join("\n\n").trim().to_string();
        if a.is_empty() {
            exchanges.push(format!("> {u}"));
        } else {
            exchanges.push(format!("> {u}\n\n{a}"));
        }
    }

    exchanges
}
