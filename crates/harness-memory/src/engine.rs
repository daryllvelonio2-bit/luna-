use std::path::PathBuf;
use std::sync::Arc;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
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

        let conn = Connection::open(&db_path).map_err(|e| HarnessError::MemoryError(e.to_string()))?;
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
        let conn = Connection::open_in_memory().map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        Self::init_schema(&conn)?;
        Ok(Self {
            db_path: PathBuf::from(":memory:"),
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), HarnessError> {
        let schema_sql = include_str!("../schema.sql");
        conn.execute_batch(schema_sql)
            .map_err(|e| HarnessError::MemoryError(format!("Schema initialization failed: {}", e)))?;
        Ok(())
    }
}

#[async_trait]
impl MemoryEngine for SqliteMemoryEngine {
    async fn retrieve(&self, query: &str, limit: usize) -> Result<Vec<MemoryRecord>, HarnessError> {
        let conn = self.conn.lock().await;
        let query_pattern = format!("%{}%", query.trim());

        let mut stmt = conn
            .prepare(
                "SELECT id, subject, predicate, object, raw_content, category, confidence, created_at, updated_at, is_active, superseded_by
                 FROM memories
                 WHERE is_active = 1
                   AND (raw_content LIKE ?1 OR subject LIKE ?1 OR predicate LIKE ?1 OR object LIKE ?1)
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
                    "harness_improvement" => MemoryCategory::HarnessImprovement,
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
                    "SELECT id, subject, predicate, object, raw_content, category, confidence, created_at, updated_at, is_active, superseded_by
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
                        "harness_improvement" => MemoryCategory::HarnessImprovement,
                        _ => MemoryCategory::Fact,
                    };
                    Ok(MemoryRecord {
                        id: row.get(0)?,
                        subject: row.get(1)?,
                        predicate: row.get(2)?,
                        object: row.get(3)?,
                        raw_content: row.get(4)?,
                        category,
                        confidence: row.get(6)?,
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
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

        // Auto-supersede conflicting active memories with matching subject & predicate.
        // The conflict set is collected before the insert so the new row is never its own
        // predecessor; the insert then has to land first because `superseded_by` is a foreign
        // key onto `memories(id)` (bundled SQLite enables foreign keys by default).
        let old_ids: Vec<String> = {
            let mut check_stmt = tx
                .prepare("SELECT id FROM memories WHERE subject = ?1 AND predicate = ?2 AND is_active = 1")
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

            let rows = check_stmt
                .query_map(params![record.subject, record.predicate], |row| row.get(0))
                .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
            let mut ids = Vec::new();
            for r in rows {
                if let Ok(id) = r {
                    ids.push(id);
                }
            }
            ids
        };

        tx.execute(
            "INSERT INTO memories (id, subject, predicate, object, raw_content, category, confidence, created_at, updated_at, is_active, superseded_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
            ],
        )
        .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        for old_id in old_ids {
            debug!("Superseding prior memory '{}' with new memory '{}'", old_id, record.id);
            tx.execute(
                "UPDATE memories SET is_active = 0, superseded_by = ?1, updated_at = ?2 WHERE id = ?3",
                params![record.id, Utc::now().to_rfc3339(), old_id],
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        }

        tx.commit()
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        info!("Saved memory '{}' [{}: {} -> {}]", record.id, record.category, record.subject, record.object);
        Ok(())
    }

    async fn update(&self, id: &str, updated: &MemoryRecord) -> Result<(), HarnessError> {
        let mut conn = self.conn.lock().await;

        let tx = conn
            .transaction()
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        // Same foreign-key ordering as `save`: the replacement row must exist before the row it
        // supersedes can point at it.
        tx.execute(
            "INSERT INTO memories (id, subject, predicate, object, raw_content, category, confidence, created_at, updated_at, is_active, superseded_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                updated.id,
                updated.subject,
                updated.predicate,
                updated.object,
                updated.raw_content,
                updated.category.to_string(),
                updated.confidence,
                updated.created_at.to_rfc3339(),
                updated.updated_at.to_rfc3339(),
                if updated.is_active { 1 } else { 0 },
                updated.superseded_by,
            ],
        )
        .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

        tx.execute(
            "UPDATE memories SET is_active = 0, superseded_by = ?1, updated_at = ?2 WHERE id = ?3",
            params![updated.id, Utc::now().to_rfc3339(), id],
        )
        .map_err(|e| HarnessError::MemoryError(e.to_string()))?;

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
            raw_content: format!("Note \"{}\" (id: {})", pointer, note_id),
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
                lines.push(format!("- [Note] {}", m.raw_content));
            } else {
                lines.push(format!(
                    "- [{}] {}: {} (ID: {})",
                    m.category, m.subject, m.raw_content, m.id
                ));
            }
        }
        lines.join("\n")
    }
}
