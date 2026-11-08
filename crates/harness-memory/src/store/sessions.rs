//! Session miner: mine SQLite `sessions.db` transcripts into the `conversations` domain.
//!
//! Direct implementation of MEMORY_STORE.md §6.4:
//! - Connects to `sessions.db` (reads `sessions` and `messages` tables).
//! - Pairs user messages (`> user`) with assistant responses (`assistant`) into canonical exchanges.
//! - Files each exchange as a verbatim record with `extract_mode: transcript`.
//! - Builds content indexes and compact summaries for each session.
//! - Reindexes store so mined conversations are immediately retrievable via hybrid search.

use std::collections::HashSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use harness_core::{HarnessError, MemoryCategory};

use super::ids;
use super::miner::{self, MineReport, TextChunk};
use super::{MemoryStore, Record};

/// Mine `sessions.db` transcripts into the store's `conversations` domain.
pub fn mine_sessions(
    store: &MemoryStore,
    sessions_db_path: &Path,
    domain_override: Option<&str>,
    topic_override: Option<&str>,
) -> Result<MineReport, HarnessError> {
    if !sessions_db_path.exists() {
        return Err(HarnessError::MemoryError(format!(
            "Sessions database does not exist: {}",
            sessions_db_path.display()
        )));
    }

    let conn = rusqlite::Connection::open(sessions_db_path)
        .map_err(|e| HarnessError::MemoryError(format!("Failed to open sessions.db: {e}")))?;

    let mut session_stmt = conn
        .prepare(
            "SELECT id, created_at, updated_at, title, name, model FROM sessions ORDER BY created_at ASC",
        )
        .map_err(|e| HarnessError::MemoryError(format!("Failed to prepare sessions query: {e}")))?;

    let session_rows = session_stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(|e| HarnessError::MemoryError(format!("Failed to query sessions: {e}")))?;

    let mut sessions = Vec::new();
    for r in session_rows {
        if let Ok(s) = r {
            sessions.push(s);
        }
    }

    let domain = domain_override.unwrap_or("conversations").to_string();
    let mut report = MineReport::default();

    for (session_id, _created_at_str, _updated_at_str, title, name, _model) in sessions {
        let topic_raw = if let Some(top) = topic_override {
            top.to_string()
        } else if !name.trim().is_empty() {
            name.trim().to_string()
        } else if !title.trim().is_empty() {
            title.trim().to_string()
        } else {
            let short_id = if session_id.len() > 8 {
                &session_id[..8]
            } else {
                &session_id
            };
            format!("session_{short_id}")
        };
        let topic = ids::slugify(&topic_raw);
        let source_file = format!("sessions.db::{}", session_id);

        let mut msg_stmt = conn
            .prepare(
                "SELECT seq, role, content, is_summary, created_at FROM messages WHERE session_id = ? ORDER BY seq ASC",
            )
            .map_err(|e| HarnessError::MemoryError(format!("Failed to prepare messages query: {e}")))?;

        let msg_rows = msg_stmt
            .query_map([&session_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)? != 0,
                    row.get::<_, String>(4)?,
                ))
            })
            .map_err(|e| HarnessError::MemoryError(format!("Failed to query messages: {e}")))?;

        let mut messages = Vec::new();
        for r in msg_rows {
            if let Ok(m) = r {
                messages.push(m);
            }
        }

        if messages.is_empty() {
            report.skipped += 1;
            continue;
        }

        // Pair messages into user+assistant exchanges
        let mut exchanges: Vec<(String, DateTime<Utc>, usize, usize)> = Vec::new();
        let mut current_user: Option<(String, DateTime<Utc>, usize)> = None;
        let mut current_assistant: Vec<(String, usize)> = Vec::new();

        for (seq, role, content_opt, is_summary, created_at_raw) in messages {
            if is_summary {
                continue;
            }
            let text = content_opt.unwrap_or_default().trim().to_string();
            if text.is_empty() {
                continue;
            }
            let ts = DateTime::parse_from_rfc3339(&created_at_raw)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now());

            let role_lower = role.to_lowercase();
            if role_lower == "user" {
                if let Some((user_text, user_ts, user_seq)) = current_user.take() {
                    let ai_text = current_assistant
                        .iter()
                        .map(|(t, _)| t.as_str())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    let end_seq = current_assistant
                        .last()
                        .map(|(_, s)| *s)
                        .unwrap_or(user_seq);
                    let body = if ai_text.is_empty() {
                        format!("> {user_text}")
                    } else {
                        format!("> {user_text}\n\n{ai_text}")
                    };
                    exchanges.push((body, user_ts, user_seq, end_seq));
                    current_assistant.clear();
                }
                current_user = Some((text, ts, seq as usize));
            } else if role_lower == "assistant" {
                current_assistant.push((text, seq as usize));
            }
        }

        // Emit final pending exchange
        if let Some((user_text, user_ts, user_seq)) = current_user.take() {
            let ai_text = current_assistant
                .iter()
                .map(|(t, _)| t.as_str())
                .collect::<Vec<_>>()
                .join("\n\n");
            let end_seq = current_assistant
                .last()
                .map(|(_, s)| *s)
                .unwrap_or(user_seq);
            let body = if ai_text.is_empty() {
                format!("> {user_text}")
            } else {
                format!("> {user_text}\n\n{ai_text}")
            };
            exchanges.push((body, user_ts, user_seq, end_seq));
        }

        if exchanges.is_empty() {
            report.skipped += 1;
            continue;
        }

        let mut chunks = Vec::new();
        let mut new_ids = HashSet::new();
        let mut all_text = String::new();

        for (exchange_idx, (exchange_body, authored_at, line_start, line_end)) in
            exchanges.into_iter().enumerate()
        {
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
                line_start,
                line_end,
                filed_at: Utc::now(),
                entities,
                tags: vec![
                    "session".to_string(),
                    "conversation".to_string(),
                    topic.clone(),
                ],
                body_hash: ids::body_hash(&exchange_body),
                subject: format!("session.{}", session_id),
                predicate: format!("exchange.{}", exchange_idx),
                object: first,
                name: Some(format!("{} [exchange {}]", topic_raw, exchange_idx)),
                path: Some(source_file.clone()),
                raw_content: exchange_body.clone(),
                category: MemoryCategory::Fact,
                confidence: 1.0,
                created_at: authored_at,
                updated_at: authored_at,
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
                content: exchange_body,
                chunk_index: exchange_idx,
                line_start,
                line_end,
            });
        }

        miner::write_content_index(store, &domain, &topic, &source_file, &chunks, &new_ids)?;
        miner::write_summary(store, &domain, &topic, &source_file, &all_text, &chunks)?;
        report.files += 1;
    }

    store.reindex()?;
    Ok(report)
}
