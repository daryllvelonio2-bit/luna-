use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::distiller::PointerDistiller;
use harness_core::{HarnessError, Note, NotesEngine};

pub struct SqliteNotesEngine {
    #[allow(dead_code)]
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
            std::fs::create_dir_all(parent).map_err(|e| {
                HarnessError::NotesError(format!(
                    "Failed to create notes db directory '{}': {}",
                    parent.display(),
                    e
                ))
            })?;
        }

        let conn =
            Connection::open(&db_path).map_err(|e| HarnessError::NotesError(e.to_string()))?;
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
            Connection::open_in_memory().map_err(|e| HarnessError::NotesError(e.to_string()))?;
        Self::init_schema(&conn)?;
        Ok(Self {
            db_path: PathBuf::from(":memory:"),
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn init_schema(conn: &Connection) -> Result<(), HarnessError> {
        let schema_sql = include_str!("../schema.sql");
        conn.execute_batch(schema_sql)
            .map_err(|e| HarnessError::NotesError(format!("Schema init failed: {}", e)))?;
        Ok(())
    }
}

pub fn slugify(text: &str) -> String {
    let mut slug = String::new();
    let mut prev_dash = false;
    for c in text.trim().chars() {
        if c.is_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if c.is_whitespace() || c == '-' || c == '_' || c == '.' || c == '/' {
            if !slug.is_empty() && !prev_dash {
                slug.push('-');
                prev_dash = true;
            }
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "note".to_string()
    } else {
        slug
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
        let slug = slugify(title);
        let summary_pointer = PointerDistiller::distill(title, content);
        let tags_json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
        let now = Utc::now();

        let conn = self.conn.lock().await;
        let id = {
            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM notes WHERE id = ?1 LIMIT 1",
                    params![slug],
                    |_| Ok(true),
                )
                .unwrap_or(false);
            if exists {
                format!("{}-{}", slug, &uuid::Uuid::new_v4().to_string()[..4])
            } else {
                slug
            }
        };

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

    async fn list_notes(&self, limit: usize, offset: usize) -> Result<Vec<Note>, HarnessError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, title, content, summary_pointer, tags, created_at, updated_at
                 FROM notes
                 ORDER BY updated_at DESC
                 LIMIT ?1 OFFSET ?2",
            )
            .map_err(|e| HarnessError::NotesError(e.to_string()))?;

        let rows = stmt
            .query_map(params![limit as i64, offset as i64], |row| {
                let tags_str: String = row.get(4).unwrap_or_default();
                let tags: Vec<String> = serde_json::from_str(&tags_str).unwrap_or_default();
                let created_str: String = row.get(5).unwrap_or_default();
                let updated_str: String = row.get(6).unwrap_or_default();
                let created_at = DateTime::parse_from_rfc3339(&created_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());
                let updated_at = DateTime::parse_from_rfc3339(&updated_str)
                    .map(|dt| dt.with_timezone(&Utc))
                    .unwrap_or_else(|_| Utc::now());

                Ok(Note {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    content: row.get(2)?,
                    summary_pointer: row.get(3)?,
                    tags,
                    created_at,
                    updated_at,
                })
            })
            .map_err(|e| HarnessError::NotesError(e.to_string()))?;

        let mut notes = Vec::new();
        for r in rows.filter_map(Result::ok) {
            notes.push(r);
        }
        Ok(notes)
    }

    async fn read_note(&self, id: &str) -> Result<Note, HarnessError> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare(
                "SELECT id, title, content, summary_pointer, tags, created_at, updated_at
                 FROM notes WHERE id = ?1 OR title = ?1 COLLATE NOCASE",
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
                &current.id
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

    async fn delete_note(&self, id: &str) -> Result<bool, HarnessError> {
        let conn = self.conn.lock().await;
        // The `notes_ad` trigger mirrors the delete into the FTS index, so no manual
        // `notes_fts` maintenance is needed here.
        let affected = conn
            .execute(
                "DELETE FROM notes WHERE id = ?1 OR title = ?1 COLLATE NOCASE",
                params![id],
            )
            .map_err(|e| HarnessError::NotesError(e.to_string()))?;

        if affected > 0 {
            info!("Deleted note '{}'", id);
        } else {
            warn!("Delete requested for unknown note '{}'", id);
        }

        Ok(affected > 0)
    }
}
