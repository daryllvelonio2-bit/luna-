//! `MemoryEngine` adapter over the Markdown store.
//!
//! This is the replacement for `SqliteMemoryEngine`: same trait, same
//! `MemoryRecord` contract, but every durable byte is a Markdown file and the
//! search structure is derived.

use async_trait::async_trait;
use chrono::Utc;
use std::path::PathBuf;

use harness_core::{HarnessError, MemoryCategory, MemoryEngine, MemoryRecord};

use super::{MemoryStore, Record, SearchFilter, SearchResult, StoreConfig};

pub struct MarkdownMemoryEngine {
    store: MemoryStore,
}

impl MarkdownMemoryEngine {
    pub fn new(config: StoreConfig) -> Result<Self, HarnessError> {
        Ok(Self {
            store: MemoryStore::open(config)?,
        })
    }

    pub fn from_root(root: PathBuf) -> Result<Self, HarnessError> {
        Self::new(StoreConfig::new(root))
    }

    pub fn store(&self) -> &MemoryStore {
        &self.store
    }

    /// Write a raw record (used by the miner and the CLI, not the trait).
    pub fn write_record(&self, record: Record) -> Result<(), HarnessError> {
        self.store.write_record(record)
    }

    pub fn search(
        &self,
        query: &str,
        filter: &SearchFilter,
        limit: usize,
    ) -> Result<Vec<SearchResult>, HarnessError> {
        self.store.search(query, filter, limit)
    }
}

#[async_trait]
impl MemoryEngine for MarkdownMemoryEngine {
    async fn retrieve(&self, query: &str, limit: usize) -> Result<Vec<MemoryRecord>, HarnessError> {
        self.store.retrieve(query, limit)
    }

    async fn save(&self, record: &MemoryRecord) -> Result<(), HarnessError> {
        if record.id.trim().is_empty() {
            return Err(HarnessError::MemoryError(
                "Cannot save a memory record with an empty id".to_string(),
            ));
        }

        let incoming = Record::from_memory(record);

        // Auto-supersede conflicting active records, matching the SQLite
        // engine: one active record per (subject, predicate), case-insensitive.
        // Note pointers match exactly because their predicate embeds the note id.
        let conflicts: Vec<Record> = self
            .store
            .all_records()?
            .into_iter()
            .filter(|existing| existing.is_active && existing.id != incoming.id)
            .filter(|existing| {
                if incoming.category == MemoryCategory::NotePointer
                    || existing.category == MemoryCategory::NotePointer
                {
                    existing.subject == incoming.subject && existing.predicate == incoming.predicate
                } else {
                    existing.subject.eq_ignore_ascii_case(&incoming.subject)
                        && existing.predicate.eq_ignore_ascii_case(&incoming.predicate)
                }
            })
            .collect();

        self.store.write_record(incoming.clone())?;

        for mut old in conflicts {
            old.is_active = false;
            old.superseded_by = Some(incoming.id.clone());
            old.updated_at = Utc::now();
            self.store.write_record(old)?;
        }
        Ok(())
    }

    async fn update(&self, id: &str, updated: &MemoryRecord) -> Result<(), HarnessError> {
        let Some(target) = self.store.get_record(id)? else {
            return Err(HarnessError::MemoryError(format!(
                "Cannot update memory '{}': no such record (pass the record id shown in CURRENT ACTIVE MEMORIES)",
                id
            )));
        };

        if updated.id == target.id {
            // Same identity: revise in place and keep the taxonomy placement.
            let mut record = Record::from_memory(updated);
            record.domain = target.domain.clone();
            record.topic = target.topic.clone();
            record.created_at = target.created_at;
            record.is_active = true;
            record.superseded_by = None;
            self.store.write_record(record)?;
        } else {
            // A changed identity writes a replacement and retires the old file.
            self.save(updated).await?;
            let mut old = target;
            old.is_active = false;
            old.superseded_by = Some(updated.id.clone());
            old.updated_at = Utc::now();
            self.store.write_record(old)?;
        }

        // Retire any other active duplicate, but not for note pointers.
        if updated.category != MemoryCategory::NotePointer {
            for mut duplicate in self.store.all_records()?.into_iter().filter(|record| {
                record.is_active
                    && record.id != updated.id
                    && record.category != MemoryCategory::NotePointer
                    && record.subject.eq_ignore_ascii_case(&updated.subject)
                    && record.predicate.eq_ignore_ascii_case(&updated.predicate)
            }) {
                duplicate.is_active = false;
                duplicate.superseded_by = Some(updated.id.clone());
                duplicate.updated_at = Utc::now();
                self.store.write_record(duplicate)?;
            }
        }
        Ok(())
    }

    async fn forget(&self, id: &str) -> Result<(), HarnessError> {
        let trimmed = id.trim();
        if trimmed.is_empty() {
            return Ok(());
        }

        if is_forget_all(trimmed) {
            self.store.delete_all_records()?;
            return Ok(());
        }

        let tokens: Vec<&str> = if trimmed.contains([';', ',', '\n']) {
            trimmed
                .split([';', ',', '\n'])
                .map(str::trim)
                .filter(|token| !token.is_empty())
                .collect()
        } else {
            vec![trimmed]
        };

        let all = self.store.all_records()?;
        for token in tokens {
            let mut targets: Vec<String> = Vec::new();
            for record in &all {
                let exact_id = record.id.eq_ignore_ascii_case(token);
                let note_pointer = record.id.eq_ignore_ascii_case(&format!("mem_ptr_{token}"));
                let subject_match = !token.starts_with("mem_ptr_")
                    && record.category != MemoryCategory::NotePointer
                    && record.subject.eq_ignore_ascii_case(token);
                if exact_id || note_pointer || subject_match {
                    targets.push(record.id.clone());
                }
            }
            for target in targets {
                self.store.delete_record(&target)?;
                // Clear references from records that pointed at the deleted one.
                for mut referrer in self
                    .store
                    .all_records()?
                    .into_iter()
                    .filter(|record| record.superseded_by.as_deref() == Some(target.as_str()))
                {
                    referrer.superseded_by = None;
                    self.store.write_record(referrer)?;
                }
            }
        }
        Ok(())
    }

    async fn add_note_pointer(&self, note_id: &str, pointer: &str) -> Result<(), HarnessError> {
        let now = Utc::now();
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
            created_at: now,
            updated_at: now,
            is_active: true,
            superseded_by: None,
        };
        self.save(&record).await
    }

    async fn get_record(&self, id: &str) -> Result<Option<MemoryRecord>, HarnessError> {
        Ok(self.store.get_record(id)?.map(|record| record.to_memory()))
    }

    fn format_as_system_prompt(&self, memories: &[MemoryRecord]) -> String {
        self.store.format_as_system_prompt(memories)
    }
}

fn is_forget_all(trimmed: &str) -> bool {
    matches!(
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
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_engine() -> MarkdownMemoryEngine {
        let root = std::env::temp_dir().join(format!(
            "luna-engine-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        MarkdownMemoryEngine::from_root(root).unwrap()
    }

    fn memory(id: &str, object: &str) -> MemoryRecord {
        MemoryRecord {
            id: id.to_string(),
            subject: id.split('.').next().unwrap_or(id).to_string(),
            predicate: id.split('.').nth(1).unwrap_or("fact").to_string(),
            object: object.to_string(),
            name: None,
            path: None,
            raw_content: object.to_string(),
            category: MemoryCategory::Fact,
            confidence: 1.0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            is_active: true,
            superseded_by: None,
        }
    }

    #[tokio::test]
    async fn save_then_retrieve_round_trips() {
        let engine = temp_engine();
        engine
            .save(&memory("user.profile", "Rust engineer"))
            .await
            .unwrap();
        let records = engine.retrieve("Rust engineer", 5).await.unwrap();
        assert!(records.iter().any(|record| record.id == "user.profile"));
        let _ = std::fs::remove_dir_all(engine.store().root());
    }

    #[tokio::test]
    async fn save_supersedes_the_previous_active_record() {
        let engine = temp_engine();
        engine
            .save(&memory("env.database", "Postgres"))
            .await
            .unwrap();
        engine
            .save(&memory("env.database", "SQLite"))
            .await
            .unwrap();
        let record = engine.get_record("env.database").await.unwrap().unwrap();
        assert_eq!(record.object, "SQLite");
        let active = engine
            .store()
            .active_records()
            .unwrap()
            .into_iter()
            .filter(|record| record.subject == "env" && record.predicate == "database")
            .count();
        assert_eq!(active, 1);
        let _ = std::fs::remove_dir_all(engine.store().root());
    }

    #[tokio::test]
    async fn update_changes_in_place() {
        let engine = temp_engine();
        engine
            .save(&memory("user.profile", "Rust engineer"))
            .await
            .unwrap();
        let mut updated = memory("user.profile", "Rust engineer and writer");
        updated.updated_at = Utc::now();
        engine.update("user.profile", &updated).await.unwrap();
        let record = engine.get_record("user.profile").await.unwrap().unwrap();
        assert!(record.object.contains("writer"));
        let _ = std::fs::remove_dir_all(engine.store().root());
    }

    #[tokio::test]
    async fn forget_all_removes_every_record() {
        let engine = temp_engine();
        engine.save(&memory("user.profile", "one")).await.unwrap();
        engine.save(&memory("env.os", "linux")).await.unwrap();
        engine.forget("all").await.unwrap();
        assert!(engine.retrieve("anything", 10).await.unwrap().is_empty());
        let _ = std::fs::remove_dir_all(engine.store().root());
    }
}
