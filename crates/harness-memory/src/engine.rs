use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, info};

use harness_core::{HarnessError, MemoryCategory, MemoryEngine, MemoryRecord};

pub struct SqliteMemoryEngine {
    #[allow(dead_code)]
    db_path: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl SqliteMemoryEngine {
    pub fn new(path: Option<PathBuf>) -> Result<Self, HarnessError> {
        let db_path = path.unwrap_or_else(|| {
            harness_core::config::home_dir()
                .join(".luna")
                .join("memory.db")
        });

        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                HarnessError::MemoryError(format!(
                    "Failed to create memory db directory '{}': {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        let conn =
            Connection::open(&db_path).map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        // Schema is applied to the owned connection before it is shared behind the mutex: the
        // constructors are reachable from async callers (`chat.rs`, `voice.rs`), and taking a
        // tokio lock there would panic inside the runtime.
        Self::init_schema(&conn)?;

        Ok(Self {
            db_path,
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn in_memory() -> Result<Self, HarnessError> {
        let conn =
            Connection::open_in_memory().map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        Self::init_schema(&conn)?;
        Ok(Self {
            db_path: PathBuf::from(":memory:"),
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), HarnessError> {
        let schema_sql = include_str!("../schema.sql");
        conn.execute_batch(schema_sql).map_err(|e| {
            HarnessError::MemoryError(format!("Schema initialization failed: {}", e))
        })?;

        // Migration for pre-existing databases: ensure lean fields exist.
        Self::add_column_if_missing(conn, "memories", "name", "TEXT")?;
        Self::add_column_if_missing(conn, "memories", "path", "TEXT")?;
        // The transcript category was added after the first schema. A CHECK constraint cannot be
        // altered in place, so rewrite the table once when the old one is found.
        Self::ensure_transcript_category(conn)?;

        // Normalize any legacy categories (e.g. from obsolete experiments) that would violate CHECK
        let _ = conn.execute(
            "UPDATE memories SET category = 'fact' WHERE category NOT IN ('user_profile', 'code_pattern', 'workflow_rule', 'fact', 'note_pointer', 'transcript')",
            [],
        );

        // Enforce topic uniqueness: collapse multiple active records for the same topic.
        Self::collapse_duplicate_active_topics(conn)?;

        Ok(())
    }

    /// Rewrite `memories` when its CHECK predates the `transcript` category.
    ///
    /// `legacy_alter_table` keeps `memory_embeddings`' foreign key pointing at the *name*
    /// `memories` instead of following the temporary rename, and disabling foreign keys keeps the
    /// move from cascading a delete through that table.
    fn ensure_transcript_category(conn: &Connection) -> Result<(), HarnessError> {
        let sql: Option<String> = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'memories'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        let Some(sql) = sql else {
            return Ok(());
        };
        if sql.contains("'transcript'") {
            return Ok(());
        }

        conn.execute_batch(
            "PRAGMA foreign_keys = OFF;
             PRAGMA legacy_alter_table = ON;
             BEGIN;
             ALTER TABLE memories RENAME TO memories_legacy;
             CREATE TABLE memories (
                 id TEXT PRIMARY KEY,
                 subject TEXT NOT NULL,
                 predicate TEXT NOT NULL,
                 object TEXT NOT NULL,
                 raw_content TEXT NOT NULL,
                 category TEXT NOT NULL CHECK(category IN ('user_profile', 'code_pattern', 'workflow_rule', 'fact', 'note_pointer', 'transcript')),
                 confidence REAL NOT NULL DEFAULT 1.0,
                 created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                 updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                 is_active INTEGER NOT NULL DEFAULT 1,
                 superseded_by TEXT REFERENCES memories(id),
                 name TEXT,
                 path TEXT
             );
             INSERT INTO memories (id, subject, predicate, object, raw_content, category, confidence, created_at, updated_at, is_active, superseded_by, name, path)
                 SELECT id, subject, predicate, object, raw_content, category, confidence, created_at, updated_at, is_active, superseded_by, name, path FROM memories_legacy;
             DROP TABLE memories_legacy;
             CREATE INDEX IF NOT EXISTS idx_memories_subject_predicate ON memories(subject, predicate);
             CREATE INDEX IF NOT EXISTS idx_memories_active ON memories(is_active);
             COMMIT;
             PRAGMA legacy_alter_table = OFF;
             PRAGMA foreign_keys = ON;",
        )
        .map_err(|e| {
            HarnessError::MemoryError(format!("Transcript category migration failed: {}", e))
        })?;
        Ok(())
    }

    fn add_column_if_missing(
        conn: &Connection,
        table: &str,
        column: &str,
        decl: &str,
    ) -> Result<(), HarnessError> {
        let mut pragma = conn
            .prepare(&format!("PRAGMA table_info({})", table))
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        let rows = pragma
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        for col_name in rows.flatten() {
            if col_name.eq_ignore_ascii_case(column) {
                return Ok(());
            }
        }
        conn.execute(
            &format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, decl),
            [],
        )
        .map_err(|e| {
            HarnessError::MemoryError(format!("Failed to add column {}: {}", column, e))
        })?;
        Ok(())
    }

    /// Guarantee that for each topic `(subject, predicate)` pair, only 1 active record remains in persistent memory.
    fn collapse_duplicate_active_topics(conn: &Connection) -> Result<(), HarnessError> {
        let mut stmt = conn
            .prepare(
                "SELECT id, subject, predicate, updated_at FROM memories
                 WHERE is_active = 1 AND category != 'note_pointer'
                 ORDER BY updated_at DESC",
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        let mut seen_keys = std::collections::HashSet::new();
        let mut stale_ids = Vec::new();

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        for r in rows.flatten() {
            let (id, subject, predicate, _) = r;
            let norm_key = (
                subject.trim().to_ascii_lowercase(),
                predicate.trim().to_ascii_lowercase(),
            );
            if !seen_keys.insert(norm_key) {
                stale_ids.push(id);
            }
        }

        for stale_id in stale_ids {
            let _ = conn.execute(
                "UPDATE memories SET is_active = 0, updated_at = ?1 WHERE id = ?2",
                params![Utc::now().to_rfc3339(), stale_id],
            );
        }

        Ok(())
    }

    /// Inserts `record`, or rewrites the row that already owns its primary key.
    ///
    /// Memory ids are derived from `subject` + `predicate`, so a revision of the same entity
    /// lands on the *same* id as its predecessor. A plain INSERT therefore dies with
    /// `UNIQUE constraint failed: memories.id` (that was the `update_memory` bug); the conflict
    /// target rewrites the row in place instead. `created_at` is deliberately left untouched so
    /// the age of the fact survives edits, and `superseded_by` is cleared because the row is
    /// active again.
    fn upsert_row(
        tx: &rusqlite::Transaction<'_>,
        record: &MemoryRecord,
    ) -> Result<(), HarnessError> {
        tx.execute(
            "INSERT INTO memories (id, subject, predicate, object, raw_content, category, confidence, created_at, updated_at, is_active, superseded_by, name, path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(id) DO UPDATE SET
                subject = excluded.subject,
                predicate = excluded.predicate,
                object = excluded.object,
                raw_content = excluded.raw_content,
                category = excluded.category,
                confidence = excluded.confidence,
                updated_at = excluded.updated_at,
                is_active = excluded.is_active,
                superseded_by = NULL,
                name = excluded.name,
                path = excluded.path",
            params![
                record.id,
                record.subject,
                record.predicate,
                record.object,
                record.raw_content,
                record.category.to_string(),
                record.confidence,
                record.created_at.to_rfc3339(),
                record.updated_at.to_rfc3339(),
                if record.is_active { 1 } else { 0 },
                record.superseded_by,
                record.name,
                record.path,
            ],
        )
        .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        Ok(())
    }

    /// Resolves one or more active memory IDs from any user/agent input:
    /// exact id, case-insensitive id, subject, subject.predicate, name, slugified form, or note pointer id.
    pub fn resolve_active_ids(
        conn: &Connection,
        raw_query: &str,
    ) -> Result<Vec<String>, HarnessError> {
        let query = raw_query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let mut matched = Vec::new();

        // 1. Exact match on ID
        let exact: Option<String> = conn
            .query_row(
                "SELECT id FROM memories WHERE id = ?1",
                params![query],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        if let Some(id) = exact {
            matched.push(id);
        }

        // 1.5 Wildcard match: e.g. "user.*", "project.*", or "*"
        if query.contains('*') {
            let pat = query.replace('*', "%");
            let mut w_stmt = conn
                .prepare(
                    "SELECT id FROM memories
                     WHERE is_active = 1 AND (
                         id LIKE ?1
                         OR subject LIKE ?1
                     )
                     ORDER BY updated_at DESC",
                )
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            let w_rows = w_stmt
                .query_map(params![pat], |row| row.get::<_, String>(0))
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            for r in w_rows.flatten() {
                if !matched.contains(&r) {
                    matched.push(r);
                }
            }
        }

        // 2. Case-insensitive match on ID, subject, subject.predicate, or name
        let mut stmt = conn
            .prepare(
                "SELECT id FROM memories
                 WHERE is_active = 1 AND (
                     LOWER(id) = LOWER(?1)
                     OR LOWER(subject) = LOWER(?1)
                     OR LOWER(subject || '.' || predicate) = LOWER(?1)
                     OR LOWER(name) = LOWER(?1)
                 )
                 ORDER BY updated_at DESC",
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        let rows = stmt
            .query_map(params![query], |row| row.get::<_, String>(0))
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        for r in rows.flatten() {
            if !matched.contains(&r) {
                matched.push(r);
            }
        }

        // 3. Note pointer specific matches: if query matches note_id or stripped mem_ptr_
        let note_candidate = query.strip_prefix("mem_ptr_").unwrap_or(query);
        let mut note_stmt = conn
            .prepare(
                "SELECT id FROM memories
                 WHERE category = 'note_pointer' AND (
                     id = ?1
                     OR id = 'mem_ptr_' || ?2
                     OR predicate = 'doc_' || ?2
                     OR LOWER(object) = LOWER(?3)
                 )",
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        let note_rows = note_stmt
            .query_map(params![query, note_candidate, query], |row| {
                row.get::<_, String>(0)
            })
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        for r in note_rows.flatten() {
            if !matched.contains(&r) {
                matched.push(r);
            }
        }

        // 4. Slugified form (matching how IDs were derived)
        let slug = query
            .chars()
            .map(|c| {
                if c.is_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
            .trim_matches('_')
            .to_string();
        if !slug.is_empty() && slug != query {
            let mut slug_stmt = conn
                .prepare(
                    "SELECT id FROM memories
                     WHERE is_active = 1 AND (
                         id = ?1
                         OR id LIKE ?1 || '.%'
                         OR LOWER(subject) = LOWER(?1)
                     )
                     ORDER BY updated_at DESC",
                )
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            let slug_rows = slug_stmt
                .query_map(params![slug], |row| row.get::<_, String>(0))
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            for r in slug_rows.flatten() {
                if !matched.contains(&r) {
                    matched.push(r);
                }
            }
        }

        // 5. Fallback substring/contains match if nothing found yet
        if matched.is_empty() {
            let pattern = format!("%{}%", query);
            let mut sub_stmt = conn
                .prepare(
                    "SELECT id FROM memories
                     WHERE is_active = 1 AND (
                         subject LIKE ?1
                         OR id LIKE ?1
                         OR name LIKE ?1
                     )
                     ORDER BY updated_at DESC
                     LIMIT 5",
                )
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            let sub_rows = sub_stmt
                .query_map(params![pattern], |row| row.get::<_, String>(0))
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            for r in sub_rows.flatten() {
                if !matched.contains(&r) {
                    matched.push(r);
                }
            }
        }

        Ok(matched)
    }

    /// Resolves the id a caller meant, falling back to flexible active lookup.
    fn resolve_id(conn: &Connection, id: &str) -> Result<Option<String>, HarnessError> {
        let ids = Self::resolve_active_ids(conn, id)?;
        Ok(ids.into_iter().next())
    }
}

/// Map the shared memory row shape to a [`MemoryRecord`].
fn memory_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MemoryRecord> {
    let cat_str: String = row.get(5)?;
    let category = match cat_str.as_str() {
        "user_profile" => MemoryCategory::UserProfile,
        "code_pattern" => MemoryCategory::CodePattern,
        "workflow_rule" => MemoryCategory::WorkflowRule,
        "note_pointer" => MemoryCategory::NotePointer,
        "transcript" => MemoryCategory::Transcript,
        _ => MemoryCategory::Fact,
    };
    let created_at = DateTime::parse_from_rfc3339(&row.get::<_, String>(7)?)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    let updated_at = DateTime::parse_from_rfc3339(&row.get::<_, String>(8)?)
        .map(|d| d.with_timezone(&Utc))
        .unwrap_or_else(|_| Utc::now());
    Ok(MemoryRecord {
        id: row.get(0)?,
        subject: row.get(1)?,
        predicate: row.get(2)?,
        object: row.get(3)?,
        name: row.get(11)?,
        path: row.get(12)?,
        raw_content: row.get(4)?,
        category,
        confidence: row.get(6)?,
        created_at,
        updated_at,
        is_active: row.get::<_, i64>(9)? != 0,
        superseded_by: row.get(10)?,
    })
}

#[async_trait]
impl MemoryEngine for SqliteMemoryEngine {
    async fn retrieve(&self, query: &str, limit: usize) -> Result<Vec<MemoryRecord>, HarnessError> {
        let conn = self.conn.lock().await;
        let query_pattern = format!("%{}%", query.trim());

        let mut stmt = conn
            .prepare(
                "SELECT id, subject, predicate, object, raw_content, category, confidence, created_at, updated_at, is_active, superseded_by, name, path
                 FROM memories
                 WHERE is_active = 1
                   AND (id LIKE ?1 OR raw_content LIKE ?1 OR subject LIKE ?1 OR predicate LIKE ?1 OR object LIKE ?1 OR name LIKE ?1 OR path LIKE ?1)
                 ORDER BY CASE WHEN category = 'transcript' THEN 1 ELSE 0 END ASC,
                          confidence DESC,
                          updated_at DESC
                 LIMIT ?2",
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        let rows = stmt
            .query_map(params![query_pattern, limit as i64], memory_from_row)
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        let mut memories: Vec<_> = rows.flatten().collect();

        // Keyword scoring fallback if exact substring search returned fewer than limit
        let stopwords = [
            "the", "and", "for", "with", "that", "this", "from", "are", "was", "were", "has",
            "have", "had", "but", "not", "you", "your", "can", "how", "what", "when", "where",
            "which", "who", "why", "does", "did", "doing", "into", "about", "over", "under",
            "then", "than", "them", "they", "its", "our", "out", "use", "using", "would", "should",
            "could", "there", "i", "do", "we", "my",
        ];
        let terms: Vec<String> = query
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 2 && !stopwords.contains(w))
            .map(str::to_string)
            .collect();

        if memories.len() < limit && !terms.is_empty() {
            let mut all_stmt = conn
                .prepare(
                    "SELECT id, subject, predicate, object, raw_content, category, confidence, created_at, updated_at, is_active, superseded_by, name, path
                     FROM memories
                     WHERE is_active = 1",
                )
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            let all_rows = all_stmt
                .query_map([], memory_from_row)
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            let mut scored: Vec<(usize, MemoryRecord)> = Vec::new();
            for m in all_rows.flatten() {
                if memories.iter().any(|existing| existing.id == m.id) {
                    continue;
                }
                let haystack = format!(
                    "{} {} {} {} {} {} {}",
                    m.id,
                    m.subject,
                    m.predicate,
                    m.object,
                    m.raw_content,
                    m.name.as_deref().unwrap_or(""),
                    m.path.as_deref().unwrap_or("")
                )
                .to_lowercase();
                let matches = terms
                    .iter()
                    .filter(|t| haystack.contains(t.as_str()))
                    .count();
                if matches > 0 {
                    scored.push((matches, m));
                }
            }
            scored.sort_by(|a, b| {
                let a_curated = a.1.category != MemoryCategory::Transcript;
                let b_curated = b.1.category != MemoryCategory::Transcript;
                b_curated
                    .cmp(&a_curated)
                    .then_with(|| b.0.cmp(&a.0))
                    .then_with(|| {
                        b.1.confidence
                            .partial_cmp(&a.1.confidence)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .then_with(|| b.1.updated_at.cmp(&a.1.updated_at))
            });
            for (_, m) in scored {
                memories.push(m);
                if memories.len() >= limit {
                    break;
                }
            }
        }

        // If query search returned fewer than limit, also fetch recent top active memories
        if memories.len() < limit {
            let remaining = limit - memories.len();
            let mut recent_stmt = conn
                .prepare(
                    "SELECT id, subject, predicate, object, raw_content, category, confidence, created_at, updated_at, is_active, superseded_by, name, path
                     FROM memories
                     WHERE is_active = 1
                     ORDER BY CASE WHEN category = 'transcript' THEN 1 ELSE 0 END ASC,
                              updated_at DESC
                     LIMIT ?1",
                )
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

            let recent_rows = recent_stmt
                .query_map(params![remaining as i64], memory_from_row)
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

            for m in recent_rows.flatten() {
                if !memories.iter().any(|existing| existing.id == m.id) {
                    memories.push(m);
                }
            }
        }

        Ok(memories)
    }

    /// The prompt-block variant: same lookup, minus raw transcripts.
    async fn retrieve_cards(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>, HarnessError> {
        let conn = self.conn.lock().await;
        let mut memories: Vec<MemoryRecord> = Vec::new();

        // 1. L0/L1 Priority: User profile, preferences, workflow rules, code patterns, and core project commands
        let mut l0_l1_stmt = conn
            .prepare(
                "SELECT id, subject, predicate, object, raw_content, category, confidence, created_at, updated_at, is_active, superseded_by, name, path
                 FROM memories
                 WHERE is_active = 1
                   AND category != 'transcript'
                   AND LOWER(predicate) NOT IN ('status', 'git_status', 'git-status', 'branch')
                   AND (
                       category IN ('user_profile', 'workflow_rule', 'code_pattern')
                       OR LOWER(subject) LIKE 'user%'
                       OR LOWER(subject) LIKE 'preference%'
                       OR LOWER(subject) LIKE 'workflow%'
                       OR LOWER(subject) LIKE 'project%'
                       OR LOWER(predicate) IN ('build-command', 'build_command', 'test-command', 'test_command', 'run-command', 'run_command', 'root', 'stack', 'runbook')
                   )
                 ORDER BY CASE
                     WHEN category IN ('user_profile', 'workflow_rule', 'code_pattern')
                          OR LOWER(subject) LIKE 'user%'
                          OR LOWER(subject) LIKE 'preference%'
                          OR LOWER(subject) LIKE 'workflow%'
                     THEN 0
                     ELSE 1
                 END ASC,
                 confidence DESC,
                 updated_at DESC
                 LIMIT ?1",
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        let l0_l1_rows = l0_l1_stmt
            .query_map(params![limit.min(8) as i64], memory_from_row)
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        for record in l0_l1_rows.flatten() {
            if !memories.iter().any(|existing| existing.id == record.id) {
                memories.push(record);
            }
        }

        // 2. Query-specific matches
        let trimmed_query = query.trim();
        if !trimmed_query.is_empty() && memories.len() < limit {
            let query_pattern = format!("%{}%", trimmed_query);
            let remaining = limit - memories.len();
            let mut stmt = conn
                .prepare(
                    "SELECT id, subject, predicate, object, raw_content, category, confidence, created_at, updated_at, is_active, superseded_by, name, path
                     FROM memories
                     WHERE is_active = 1
                       AND category != 'transcript'
                       AND (id LIKE ?1 OR raw_content LIKE ?1 OR subject LIKE ?1 OR predicate LIKE ?1 OR object LIKE ?1 OR name LIKE ?1 OR path LIKE ?1)
                     ORDER BY confidence DESC, updated_at DESC
                     LIMIT ?2",
                )
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

            let rows = stmt
                .query_map(params![query_pattern, remaining as i64], memory_from_row)
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            for record in rows.flatten() {
                if !memories.iter().any(|existing| existing.id == record.id) {
                    memories.push(record);
                }
            }

            if memories.len() < limit {
                let stopwords = [
                    "the", "and", "for", "with", "that", "this", "from", "are", "was", "were",
                    "has", "have", "had", "but", "not", "you", "your", "can", "how", "what",
                    "when", "where", "which", "who", "why", "does", "did", "doing", "into",
                    "about", "over", "under", "then", "than", "them", "they", "its", "our", "out",
                    "use", "using", "would", "should", "could", "there", "i", "do", "we", "my",
                ];
                let terms: Vec<String> = trimmed_query
                    .to_lowercase()
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| w.len() >= 2 && !stopwords.contains(w))
                    .map(str::to_string)
                    .collect();

                if !terms.is_empty() {
                    let mut all_stmt = conn
                        .prepare(
                            "SELECT id, subject, predicate, object, raw_content, category, confidence, created_at, updated_at, is_active, superseded_by, name, path
                             FROM memories
                             WHERE is_active = 1 AND category != 'transcript'",
                        )
                        .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
                    let all_rows = all_stmt
                        .query_map([], memory_from_row)
                        .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
                    let mut scored: Vec<(usize, MemoryRecord)> = Vec::new();
                    for m in all_rows.flatten() {
                        if memories.iter().any(|existing| existing.id == m.id) {
                            continue;
                        }
                        let haystack = format!(
                            "{} {} {} {} {} {} {}",
                            m.id,
                            m.subject,
                            m.predicate,
                            m.object,
                            m.raw_content,
                            m.name.as_deref().unwrap_or(""),
                            m.path.as_deref().unwrap_or("")
                        )
                        .to_lowercase();
                        let matches = terms
                            .iter()
                            .filter(|t| haystack.contains(t.as_str()))
                            .count();
                        if matches > 0 {
                            scored.push((matches, m));
                        }
                    }
                    scored.sort_by(|a, b| {
                        b.0.cmp(&a.0)
                            .then_with(|| {
                                b.1.confidence
                                    .partial_cmp(&a.1.confidence)
                                    .unwrap_or(std::cmp::Ordering::Equal)
                            })
                            .then_with(|| b.1.updated_at.cmp(&a.1.updated_at))
                    });
                    for (_, m) in scored {
                        memories.push(m);
                        if memories.len() >= limit {
                            break;
                        }
                    }
                }
            }
        }

        // 3. Recency fallback if still under limit
        if memories.len() < limit {
            let remaining = limit - memories.len();
            let mut recent_stmt = conn
                .prepare(
                    "SELECT id, subject, predicate, object, raw_content, category, confidence, created_at, updated_at, is_active, superseded_by, name, path
                     FROM memories
                     WHERE is_active = 1 AND category != 'transcript'
                     ORDER BY updated_at DESC
                     LIMIT ?1",
                )
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            let recent_rows = recent_stmt
                .query_map(params![remaining as i64], memory_from_row)
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            for record in recent_rows.flatten() {
                if !memories.iter().any(|existing| existing.id == record.id) {
                    memories.push(record);
                }
            }
        }

        Ok(memories)
    }

    async fn save(&self, record: &MemoryRecord) -> Result<(), HarnessError> {
        let mut conn = self.conn.lock().await;

        let tx = conn
            .transaction()
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        // Auto-supersede conflicting active memories:
        // For note pointers: match on subject & predicate.
        // For entity memories: strictly ONE active record per (subject, predicate), case-insensitive.
        // Keying on the predicate too is what lets `project.luna.build` and `project.luna.test`
        // coexist; keying on the subject alone silently deactivated the older attribute.
        let old_ids: Vec<String> = if record.category == MemoryCategory::NotePointer {
            let mut check_stmt = tx
                .prepare("SELECT id FROM memories WHERE subject = ?1 AND predicate = ?2 AND is_active = 1")
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

            let rows = check_stmt
                .query_map(params![record.subject, record.predicate], |row| row.get(0))
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            let mut ids = Vec::new();
            for r in rows.flatten() {
                if r != record.id {
                    ids.push(r);
                }
            }
            ids
        } else {
            let mut check_stmt = tx
                .prepare("SELECT id FROM memories WHERE LOWER(subject) = LOWER(?1) AND LOWER(predicate) = LOWER(?2) AND category != 'note_pointer' AND is_active = 1")
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

            let rows = check_stmt
                .query_map(params![record.subject, record.predicate], |row| row.get(0))
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            let mut ids = Vec::new();
            for r in rows.flatten() {
                if r != record.id {
                    ids.push(r);
                }
            }
            ids
        };

        Self::upsert_row(&tx, record)?;

        for old_id in old_ids {
            debug!(
                "Superseding prior memory '{}' with new memory '{}'",
                old_id, record.id
            );
            tx.execute(
                "UPDATE memories SET is_active = 0, superseded_by = ?1, updated_at = ?2 WHERE id = ?3",
                params![record.id, Utc::now().to_rfc3339(), old_id],
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        }

        tx.commit()
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        info!(
            "Saved memory '{}' [{}: {} -> {}]",
            record.id, record.category, record.subject, record.object
        );
        Ok(())
    }

    async fn update(&self, id: &str, updated: &MemoryRecord) -> Result<(), HarnessError> {
        let mut conn = self.conn.lock().await;

        let tx = conn
            .transaction()
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        let Some(target_id) = Self::resolve_id(&tx, id)? else {
            return Err(HarnessError::MemoryError(format!(
                "Cannot update memory '{}': no such record (pass the record id shown in CURRENT ACTIVE MEMORIES)",
                id
            )));
        };
        let id = target_id.as_str();

        if updated.id == id {
            // Same entity, same derived id: revise the row in place. Chaining a row to itself
            // would flip it to is_active = 0 and hide the update from every future prompt.
            tx.execute(
                "UPDATE memories
                 SET subject = ?1, predicate = ?2, object = ?3, raw_content = ?4,
                     category = ?5, confidence = ?6, updated_at = ?7, is_active = 1,
                     superseded_by = NULL, name = ?8, path = ?9
                 WHERE id = ?10",
                params![
                    updated.subject,
                    updated.predicate,
                    updated.object,
                    updated.raw_content,
                    updated.category.to_string(),
                    updated.confidence,
                    Utc::now().to_rfc3339(),
                    updated.name,
                    updated.path,
                    id,
                ],
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        } else {
            // Decoupled id (subject/predicate changed): write the replacement first — the old row
            // now points at it and `superseded_by` is a foreign key onto `memories(id)`.
            Self::upsert_row(&tx, updated)?;
            tx.execute(
                "UPDATE memories SET is_active = 0, superseded_by = ?1, updated_at = ?2 WHERE id = ?3",
                params![updated.id, Utc::now().to_rfc3339(), id],
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        }

        // Deactivate any other active duplicate (subject, predicate) records if this is not a note
        // pointer. A different predicate is a different attribute and must survive.
        if updated.category != MemoryCategory::NotePointer {
            tx.execute(
                "UPDATE memories SET is_active = 0, superseded_by = ?1, updated_at = ?2
                 WHERE LOWER(subject) = LOWER(?3) AND LOWER(predicate) = LOWER(?4)
                   AND id != ?1 AND is_active = 1 AND category != 'note_pointer'",
                params![
                    updated.id,
                    Utc::now().to_rfc3339(),
                    updated.subject,
                    updated.predicate
                ],
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        }

        tx.commit()
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        info!("Updated memory '{}' -> superseded by '{}'", id, updated.id);
        Ok(())
    }

    async fn forget(&self, id: &str) -> Result<(), HarnessError> {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        let is_all = matches!(
            trimmed.to_ascii_lowercase().as_str(),
            "all"
                | "*"
                | "everything"
                | "all memory"
                | "all memories"
                | "clear"
                | "clear all"
                | "delete all"
                | "delete it all"
                | "delete all memories"
                | "delete all memory"
                | "forget all"
                | "forget all memories"
        );

        let mut conn = self.conn.lock().await;
        let tx = conn
            .transaction()
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        if is_all {
            let _ = tx.execute("DELETE FROM memory_embeddings", []);
            let count = tx
                .execute("DELETE FROM memories", [])
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            tx.commit()
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            info!(
                "Hard-deleted all {} memory records from persistent storage",
                count
            );
            return Ok(());
        }

        let tokens: Vec<&str> = if trimmed.contains([';', ',', '\n']) {
            trimmed
                .split([';', ',', '\n'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect()
        } else {
            vec![trimmed]
        };

        let mut deleted_count = 0;
        for token in tokens {
            let matching_ids = Self::resolve_active_ids(&tx, token)?;
            for target_id in &matching_ids {
                tx.execute(
                    "UPDATE memories SET superseded_by = NULL WHERE superseded_by = ?1",
                    params![target_id],
                )
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

                let _ = tx.execute(
                    "DELETE FROM memory_embeddings WHERE memory_id = ?1",
                    params![target_id],
                );

                tx.execute("DELETE FROM memories WHERE id = ?1", params![target_id])
                    .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

                deleted_count += 1;
                info!("Permanently deleted memory '{}'", target_id);
            }

            if !token.starts_with("mem_ptr_") {
                let _ = tx.execute(
                    "DELETE FROM memories WHERE LOWER(subject) = LOWER(?1) AND category != 'note_pointer'",
                    params![token],
                );
            }
        }

        tx.commit()
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        info!(
            "Finished forgetting operation for '{}' (deleted {} records)",
            trimmed, deleted_count
        );
        Ok(())
    }

    async fn add_note_pointer(&self, note_id: &str, pointer: &str) -> Result<(), HarnessError> {
        let record = MemoryRecord {
            id: format!("mem_ptr_{}", note_id),
            subject: "note".to_string(),
            predicate: format!("doc_{}", note_id),
            object: pointer.to_string(),
            name: None,
            path: None,
            raw_content: format!("Note: \"{}\"", pointer),
            category: MemoryCategory::NotePointer,
            confidence: 1.0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            is_active: true,
            superseded_by: None,
        };
        self.save(&record).await
    }

    async fn get_record(&self, id_or_subject: &str) -> Result<Option<MemoryRecord>, HarnessError> {
        let conn = self.conn.lock().await;
        let ids = Self::resolve_active_ids(&conn, id_or_subject)?;
        let Some(first_id) = ids.first() else {
            return Ok(None);
        };
        let mut stmt = conn
            .prepare(
                "SELECT id, subject, predicate, object, raw_content, category, confidence, created_at, updated_at, is_active, superseded_by, name, path
                 FROM memories WHERE id = ?1",
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        let rec = stmt
            .query_row(params![first_id], |row| {
                let cat_str: String = row.get(5)?;
                let category = match cat_str.as_str() {
                    "user_profile" => MemoryCategory::UserProfile,
                    "code_pattern" => MemoryCategory::CodePattern,
                    "workflow_rule" => MemoryCategory::WorkflowRule,
                    "note_pointer" => MemoryCategory::NotePointer,
                    "transcript" => MemoryCategory::Transcript,
                    _ => MemoryCategory::Fact,
                };
                let created_at = DateTime::parse_from_rfc3339(&row.get::<_, String>(7)?)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                let updated_at = DateTime::parse_from_rfc3339(&row.get::<_, String>(8)?)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                Ok(MemoryRecord {
                    id: row.get(0)?,
                    subject: row.get(1)?,
                    predicate: row.get(2)?,
                    object: row.get(3)?,
                    raw_content: row.get(4)?,
                    category,
                    confidence: row.get(6)?,
                    created_at,
                    updated_at,
                    is_active: row.get::<_, i64>(9)? != 0,
                    superseded_by: row.get(10)?,
                    name: row.get(11)?,
                    path: row.get(12)?,
                })
            })
            .optional()
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        Ok(rec)
    }

    fn format_as_system_prompt(&self, memories: &[MemoryRecord]) -> String {
        if memories.is_empty() {
            return "(No persistent memories registered yet)".to_string();
        }

        let mut lines = Vec::new();
        for m in memories {
            if m.category == MemoryCategory::NotePointer {
                let note_id = m.id.strip_prefix("mem_ptr_").unwrap_or(&m.id);
                lines.push(format!("- [Note] {} (id: {})", m.object, note_id));
            } else {
                let display_cat = match m.category {
                    MemoryCategory::UserProfile => "User Profile",
                    MemoryCategory::CodePattern => "Code Pattern",
                    MemoryCategory::WorkflowRule => "Workflow Rule",
                    MemoryCategory::Fact => "Fact",
                    MemoryCategory::NotePointer => "Note",
                    MemoryCategory::Transcript => "Transcript",
                };

                let mut parts = Vec::new();
                if let Some(name) = &m.name {
                    let trimmed = name.trim();
                    if !trimmed.is_empty() && !trimmed.eq_ignore_ascii_case(&m.subject) {
                        parts.push(trimmed.to_string());
                    }
                }
                let obj_trimmed = m.object.trim();
                if !obj_trimmed.is_empty() {
                    parts.push(obj_trimmed.to_string());
                }
                if let Some(path) = &m.path {
                    let trimmed = path.trim();
                    if !trimmed.is_empty() {
                        parts.push(format!("path: {}", trimmed));
                    }
                }

                let summary = if parts.is_empty() {
                    m.object.clone()
                } else {
                    parts.join(" · ")
                };

                let key =
                    if m.predicate == "profile" || m.predicate == "=" || m.predicate.is_empty() {
                        m.subject.clone()
                    } else {
                        format!("{}.{}", m.subject, m.predicate)
                    };
                lines.push(format!("- [{}] {}: {}", display_cat, key, summary));
            }
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!(
            "luna-transcript-migration-{}-{}.db",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ))
    }

    #[tokio::test]
    async fn multiple_predicates_under_same_subject_survive_reopen() {
        let path = temp_db();
        {
            let engine = SqliteMemoryEngine::new(Some(path.clone())).expect("creates db");
            let editor = MemoryRecord {
                id: "user.editor".to_string(),
                subject: "user".to_string(),
                predicate: "editor".to_string(),
                object: "neovim".to_string(),
                name: None,
                path: None,
                raw_content: "user editor neovim".to_string(),
                category: MemoryCategory::UserProfile,
                confidence: 1.0,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                is_active: true,
                superseded_by: None,
            };
            let voice = MemoryRecord {
                id: "user.voice".to_string(),
                subject: "user".to_string(),
                predicate: "voice".to_string(),
                object: "am_michael".to_string(),
                name: None,
                path: None,
                raw_content: "user voice am_michael".to_string(),
                category: MemoryCategory::UserProfile,
                confidence: 1.0,
                created_at: Utc::now(),
                updated_at: Utc::now(),
                is_active: true,
                superseded_by: None,
            };
            engine.save(&editor).await.expect("saves editor");
            engine.save(&voice).await.expect("saves voice");
        }

        let reopened = SqliteMemoryEngine::new(Some(path.clone())).expect("reopens db");
        let cards = reopened.retrieve_cards("", 10).await.expect("reads cards");
        assert!(
            cards
                .iter()
                .any(|r| r.id == "user.editor" && r.object == "neovim"),
            "user.editor must remain active after reopening the database"
        );
        assert!(
            cards
                .iter()
                .any(|r| r.id == "user.voice" && r.object == "am_michael"),
            "user.voice must remain active after reopening the database"
        );

        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn a_database_with_the_old_check_keeps_its_rows_and_accepts_transcripts() {
        let path = temp_db();
        {
            let conn = Connection::open(&path).expect("creates the old db");
            conn.execute_batch(
                "CREATE TABLE memories (
                    id TEXT PRIMARY KEY,
                    subject TEXT NOT NULL,
                    predicate TEXT NOT NULL,
                    object TEXT NOT NULL,
                    raw_content TEXT NOT NULL,
                    category TEXT NOT NULL CHECK(category IN ('user_profile', 'code_pattern', 'workflow_rule', 'fact', 'note_pointer')),
                    confidence REAL NOT NULL DEFAULT 1.0,
                    created_at TIMESTAMP NOT NULL,
                    updated_at TIMESTAMP NOT NULL,
                    is_active INTEGER NOT NULL DEFAULT 1,
                    superseded_by TEXT REFERENCES memories(id),
                    name TEXT,
                    path TEXT
                );
                INSERT INTO memories (id, subject, predicate, object, raw_content, category, confidence, created_at, updated_at)
                VALUES ('user.editor', 'user', 'editor', 'neovim', 'user editor neovim', 'user_profile', 1.0,
                        '2026-01-01T00:00:00+00:00', '2026-01-01T00:00:00+00:00');",
            )
            .expect("seeds the old db");
        }

        let engine = SqliteMemoryEngine::new(Some(path.clone())).expect("migrates on open");
        let existing = engine
            .get_record("user.editor")
            .await
            .expect("reads")
            .expect("the old row survived");
        assert_eq!(existing.object, "neovim");

        let transcript = MemoryRecord {
            id: "transcript.abc123".to_string(),
            subject: "transcript.abc123".to_string(),
            predicate: "exchange".to_string(),
            object: "build with cargo".to_string(),
            name: None,
            path: None,
            raw_content: "> build with cargo\n\nUnderstood.".to_string(),
            category: MemoryCategory::Transcript,
            confidence: 1.0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            is_active: true,
            superseded_by: None,
        };
        engine.save(&transcript).await.expect("writes a transcript");

        let stored = engine
            .get_record(&transcript.id)
            .await
            .expect("reads")
            .expect("the transcript is stored");
        assert_eq!(stored.category, MemoryCategory::Transcript);
        assert!(
            engine
                .retrieve_cards("", 10)
                .await
                .expect("reads cards")
                .iter()
                .all(|record| record.category != MemoryCategory::Transcript),
            "a transcript must not enter the prompt-block retrieval"
        );

        let _ = std::fs::remove_file(path);
    }
}
