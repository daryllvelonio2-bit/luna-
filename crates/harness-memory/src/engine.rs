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
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".luna").join("memory.db")
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

        // Normalize any legacy categories (e.g. from obsolete experiments) that would violate CHECK
        let _ = conn.execute(
            "UPDATE memories SET category = 'fact' WHERE category NOT IN ('user_profile', 'code_pattern', 'workflow_rule', 'fact', 'note_pointer')",
            [],
        );

        // Enforce topic uniqueness: collapse multiple active records for the same topic.
        Self::collapse_duplicate_active_topics(conn)?;

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
        for col_res in rows {
            if let Ok(col_name) = col_res {
                if col_name.eq_ignore_ascii_case(column) {
                    return Ok(());
                }
            }
        }
        conn.execute(&format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, decl), [])
            .map_err(|e| HarnessError::MemoryError(format!("Failed to add column {}: {}", column, e)))?;
        Ok(())
    }

    /// Guarantee that for each topic subject, only 1 active record remains in persistent memory.
    fn collapse_duplicate_active_topics(conn: &Connection) -> Result<(), HarnessError> {
        let mut stmt = conn
            .prepare(
                "SELECT id, subject, updated_at FROM memories
                 WHERE is_active = 1 AND category != 'note_pointer'
                 ORDER BY updated_at DESC",
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        let mut seen_subjects = std::collections::HashSet::new();
        let mut stale_ids = Vec::new();

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        for r in rows.flatten() {
            let (id, subject, _) = r;
            let norm_sub = subject.trim().to_ascii_lowercase();
            if !seen_subjects.insert(norm_sub) {
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

    /// Resolves the id a caller meant. The system prompt renders records as `subject.predicate`,
    /// which is not always the stored primary key (ids are derived from a sanitized subject), so
    /// an exact miss falls back to the rendered display name.
    fn resolve_id(
        tx: &rusqlite::Transaction<'_>,
        id: &str,
    ) -> Result<Option<String>, HarnessError> {
        let exact: Option<String> = tx
            .query_row(
                "SELECT id FROM memories WHERE id = ?1",
                params![id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        if exact.is_some() {
            return Ok(exact);
        }

        tx.query_row(
            "SELECT id FROM memories
             WHERE is_active = 1 AND subject || '.' || predicate = ?1
             ORDER BY updated_at DESC
             LIMIT 1",
            params![id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| HarnessError::MemoryError(e.to_string()))
    }
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
                   AND (raw_content LIKE ?1 OR subject LIKE ?1 OR predicate LIKE ?1 OR object LIKE ?1 OR name LIKE ?1 OR path LIKE ?1)
                 ORDER BY confidence DESC, updated_at DESC
                 LIMIT ?2",
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        let rows = stmt
            .query_map(params![query_pattern, limit as i64], |row| {
                let cat_str: String = row.get(5)?;
                let category = match cat_str.as_str() {
                    "user_profile" => MemoryCategory::UserProfile,
                    "code_pattern" => MemoryCategory::CodePattern,
                    "workflow_rule" => MemoryCategory::WorkflowRule,
                    "note_pointer" => MemoryCategory::NotePointer,
                    _ => MemoryCategory::Fact,
                };

                let created_raw: String = row.get(7)?;
                let updated_raw: String = row.get(8)?;

                let created_at = DateTime::parse_from_rfc3339(&created_raw)
                    .map(|d| d.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                let updated_at = DateTime::parse_from_rfc3339(&updated_raw)
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
            })
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        let mut memories = Vec::new();
        for r in rows {
            if let Ok(m) = r {
                memories.push(m);
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
                     ORDER BY updated_at DESC
                     LIMIT ?1",
                )
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

            let recent_rows = recent_stmt
                .query_map(params![remaining as i64], |row| {
                    let cat_str: String = row.get(5)?;
                    let category = match cat_str.as_str() {
                        "user_profile" => MemoryCategory::UserProfile,
                        "code_pattern" => MemoryCategory::CodePattern,
                        "workflow_rule" => MemoryCategory::WorkflowRule,
                        "note_pointer" => MemoryCategory::NotePointer,
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
                })
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

            for r in recent_rows {
                if let Ok(m) = r {
                    if !memories.iter().any(|existing| existing.id == m.id) {
                        memories.push(m);
                    }
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
        // For entity memories: strictly ONE active record per subject (case-insensitive)!
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
                .prepare("SELECT id FROM memories WHERE LOWER(subject) = LOWER(?1) AND category != 'note_pointer' AND is_active = 1")
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

            let rows = check_stmt
                .query_map(params![record.subject], |row| row.get(0))
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

        // Deactivate any other active duplicate topic records if this is not a note pointer
        if updated.category != MemoryCategory::NotePointer {
            tx.execute(
                "UPDATE memories SET is_active = 0, superseded_by = ?1, updated_at = ?2
                 WHERE LOWER(subject) = LOWER(?3) AND id != ?1 AND is_active = 1 AND category != 'note_pointer'",
                params![updated.id, Utc::now().to_rfc3339(), updated.subject],
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        }

        tx.commit()
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        info!("Updated memory '{}' -> superseded by '{}'", id, updated.id);
        Ok(())
    }

    async fn forget(&self, id: &str) -> Result<(), HarnessError> {
        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE memories SET is_active = 0, updated_at = ?1 WHERE id = ?2",
            params![Utc::now().to_rfc3339(), id],
        )
        .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        info!("Deactivated memory '{}'", id);
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

    fn format_as_system_prompt(&self, memories: &[MemoryRecord]) -> String {
        if memories.is_empty() {
            return "(No persistent memories registered yet)".to_string();
        }

        let mut lines = Vec::new();
        for m in memories {
            if m.category == MemoryCategory::NotePointer {
                let clean_content = if let Some(idx) = m.raw_content.find(" (id: note_") {
                    m.raw_content[..idx].to_string()
                } else {
                    m.raw_content.clone()
                };
                lines.push(format!("- [Note] {}", clean_content));
            } else {
                let display_cat = match m.category {
                    MemoryCategory::UserProfile => "User Profile",
                    MemoryCategory::CodePattern => "Code Pattern",
                    MemoryCategory::WorkflowRule => "Workflow Rule",
                    MemoryCategory::Fact => "Fact",
                    MemoryCategory::NotePointer => "Note",
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

                let key = if m.predicate == "profile" || m.predicate.is_empty() {
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
