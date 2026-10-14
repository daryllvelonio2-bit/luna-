use std::path::PathBuf;
use std::sync::Arc;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use tokio::sync::Mutex;
use tracing::{info, warn};

use harness_core::{HarnessError, Note, NotesEngine};
use crate::distiller::PointerDistiller;

pub struct SqliteNotesEngine {
    db_path: PathBuf,
    conn: Arc<Mutex<Connection>>,
}

impl SqliteNotesEngine {
    pub fn new(path: Option<PathBuf>) -> Result<Self, HarnessError> {
        let db_path = path.unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".luna").join("notes.db")
        });

        if let Some(parent) = db_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let conn = Connection::open(&db_path).map_err(|e| HarnessError::NotesError(e.to_string()))?;
        let engine = Self {
            db_path,
            conn: Arc::new(Mutex::new(conn)),
        };

        engine.init_schema_sync()?;
        Ok(engine)
    }

    pub fn in_memory() -> Result<Self, HarnessError> {
        let conn = Connection::open_in_memory().map_err(|e| HarnessError::NotesError(e.to_string()))?;
        let engine = Self {
            db_path: PathBuf::from(":memory:"),
            conn: Arc::new(Mutex::new(conn)),
        };
        engine.init_schema_sync()?;
        Ok(engine)
    }

    fn init_schema_sync(&self) -> Result<(), HarnessError> {
        let schema_sql = include_str!("../schema.sql");
        let conn = self.conn.blocking_lock();
        conn.execute_batch(schema_sql)
            .map_err(|e| HarnessError::NotesError(format!("Schema init failed: {}", e)))?;
        Ok(())
    }
}

#[async_trait]
impl NotesEngine for SqliteNotesEngine {
    async fn save_note(
        &self,
        title: &str,
        content: &str,
        tags: &[String],
    ) -> Result<Note, HarnessError> {
        let id = format!("note_{}", uuid::Uuid::new_v4());
        let summary_pointer = PointerDistiller::distill(title, content);
        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
        let now = Utc::now();

        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO notes (id, title, content, summary_pointer, tags, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                title,
                content,
                summary_pointer,
                tags_json,
                now.to_rfc3339(),
                now.to_rfc3339()
            ],
        )
        .map_err(|e| HarnessError::NotesError(e.to_string()))?;

        info!("Saved note '{}' (Pointer: '{}')", id, summary_pointer);

        Ok(Note {
            id,
            title: title.to_string(),
            content: content.to_string(),
            summary_pointer,
            tags: tags.to_vec(),
            created_at: now,
            updated_at: now,
        })
    }

    async fn search_notes(&self, query: &str, limit: usize) -> Result<Vec<Note>, HarnessError> {
        let conn = self.conn.lock().await;

        // FTS5 MATCH query with fallback to LIKE if query has special syntax or no FTS hits
        let mut notes = Vec::new();
        let sanitized_query = query.replace('"', "").trim().to_string();

        let fts_res = conn.prepare(
            "SELECT n.id, n.title, n.content, n.summary_pointer, n.tags, n.created_at, n.updated_at
             FROM notes n
             JOIN notes_fts f ON n.rowid = f.rowid
             WHERE notes_fts MATCH ?1
             ORDER BY rank
             LIMIT ?2",
        );

        if let Ok(mut stmt) = fts_res {
            let rows = stmt.query_map(params![sanitized_query, limit as i64], |row| {
                let tags_str: String = row.get(4).unwrap_or_default();
                let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();
                Ok(Note {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    content: row.get(2)?,
                    summary_pointer: row.get(3)?,
                    tags,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                })
            });

            if let Ok(mapped) = rows {
                for r in mapped.filter_map(Result::ok) {
                    notes.push(r);
                }
            }
        }

        // Fallback to LIKE if FTS returned nothing
        if notes.is_empty() {
            let like_pattern = format!("%{}%", query);
            let mut fallback_stmt = conn
                .prepare(
                    "SELECT id, title, content, summary_pointer, tags, created_at, updated_at
                     FROM notes
                     WHERE title LIKE ?1 OR content LIKE ?1
                     ORDER BY updated_at DESC
                     LIMIT ?2",
                )
                .map_err(|e| HarnessError::NotesError(e.to_string()))?;

            let rows = fallback_stmt
                .query_map(params![like_pattern, limit as i64], |row| {
                    let tags_str: String = row.get(4).unwrap_or_default();
                    let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();
                    Ok(Note {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        content: row.get(2)?,
                        summary_pointer: row.get(3)?,
                        tags,
                        created_at: Utc::now(),
                        updated_at: Utc::now(),
                    })
                })
                .map_err(|e| HarnessError::NotesError(e.to_string()))?;

            for r in rows.filter_map(Result::ok) {
                notes.push(r);
            }
        }

        Ok(notes)
    }

    async fn read_note(&self, id: &str) -> Result<Note, HarnessError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, title, content, summary_pointer, tags, created_at, updated_at
                 FROM notes WHERE id = ?1",
            )
            .map_err(|e| HarnessError::NotesError(e.to_string()))?;

        let note = stmt
            .query_row(params![id], |row| {
                let tags_str: String = row.get(4).unwrap_or_default();
                let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();
                Ok(Note {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    content: row.get(2)?,
                    summary_pointer: row.get(3)?,
                    tags,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                })
            })
            .map_err(|e| HarnessError::NotesError(format!("Note '{}' not found: {}", id, e)))?;

        Ok(note)
    }

    async fn update_note(
        &self,
        id: &str,
        title: Option<&str>,
        content: Option<&str>,
        tags: Option<&[String]>,
    ) -> Result<Note, HarnessError> {
        let current = self.read_note(id).await?;
        let new_title = title.unwrap_or(&current.title);
        let new_content = content.unwrap_or(&current.content);
        let new_pointer = PointerDistiller::distill(new_title, new_content);
        let new_tags = tags.map(|t| t.to_vec()).unwrap_or(current.tags);
        let tags_json = serde_json::to_string(&new_tags).unwrap_or_else(|_| "[]".to_string());
        let now = Utc::now();

        let conn = self.conn.lock().await;
        conn.execute(
            "UPDATE notes
             SET title = ?1, content = ?2, summary_pointer = ?3, tags = ?4, updated_at = ?5
             WHERE id = ?6",
            params![
                new_title,
                new_content,
                new_pointer,
                tags_json,
                now.to_rfc3339(),
                id
            ],
        )
        .map_err(|e| HarnessError::NotesError(e.to_string()))?;

        info!("Updated note '{}' (Pointer: '{}')", id, new_pointer);

        Ok(Note {
            id: id.to_string(),
            title: new_title.to_string(),
            content: new_content.to_string(),
            summary_pointer: new_pointer,
            tags: new_tags,
            created_at: current.created_at,
            updated_at: now,
        })
    }
}
