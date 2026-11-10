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

    pub fn wake_up(&self, domain: Option<&str>) -> Result<String, HarnessError> {
        self.store.wake_up(domain)
    }

    pub fn recall(
        &self,
        domain: Option<&str>,
        topic: Option<&str>,
        limit: usize,
    ) -> Result<String, HarnessError> {
        self.store.recall(domain, topic, limit)
    }

    pub fn context_stack(&self) -> super::context::MemoryStack {
        self.store.context_stack()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn kg_add_triple(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        valid_from: Option<&str>,
        valid_to: Option<&str>,
        confidence: Option<f32>,
        source_record: Option<&str>,
        source_file: Option<&str>,
        source_record_id: Option<&str>,
        adapter_name: Option<&str>,
    ) -> Result<String, HarnessError> {
        self.store.kg_add_triple(
            subject,
            predicate,
            object,
            valid_from,
            valid_to,
            confidence,
            source_record,
            source_file,
            source_record_id,
            adapter_name,
        )
    }

    pub fn kg_invalidate(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        ended: Option<&str>,
    ) -> Result<usize, HarnessError> {
        self.store.kg_invalidate(subject, predicate, object, ended)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn kg_supersede(
        &self,
        subject: &str,
        predicate: &str,
        old_obj: &str,
        new_obj: &str,
        at: Option<&str>,
        confidence: Option<f32>,
        source_record: Option<&str>,
        source_file: Option<&str>,
        source_record_id: Option<&str>,
        adapter_name: Option<&str>,
    ) -> Result<String, HarnessError> {
        self.store.kg_supersede(
            subject,
            predicate,
            old_obj,
            new_obj,
            at,
            confidence,
            source_record,
            source_file,
            source_record_id,
            adapter_name,
        )
    }

    pub fn kg_query(
        &self,
        entity: &str,
        as_of: Option<&str>,
        direction: Option<&str>,
    ) -> Result<super::kg::EntityQueryResult, HarnessError> {
        self.store.kg_query(entity, as_of, direction)
    }

    pub fn kg_query_relationship(
        &self,
        predicate: &str,
        as_of: Option<&str>,
    ) -> Result<Vec<super::kg::Triple>, HarnessError> {
        self.store.kg_query_relationship(predicate, as_of)
    }

    pub fn kg_timeline(
        &self,
        entity: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<super::kg::Triple>, HarnessError> {
        self.store.kg_timeline(entity, limit, offset)
    }

    pub fn kg_stats(&self) -> Result<super::kg::KnowledgeGraphStats, HarnessError> {
        self.store.kg_stats()
    }

    pub fn traverse(
        &self,
        start_node: &str,
        max_hops: usize,
    ) -> Result<Vec<super::graph::TraversalStep>, HarnessError> {
        self.store.traverse(start_node, max_hops)
    }

    pub fn find_cross_links(
        &self,
        domain_a: Option<&str>,
        domain_b: Option<&str>,
    ) -> Result<Vec<super::graph::CrossLinkBridge>, HarnessError> {
        self.store.find_cross_links(domain_a, domain_b)
    }

    pub fn mine_sessions(
        &self,
        sessions_db_path: &std::path::Path,
        domain: Option<&str>,
        topic: Option<&str>,
    ) -> Result<super::MineReport, HarnessError> {
        self.store.mine_sessions(sessions_db_path, domain, topic)
    }

    pub fn mine_convos(
        &self,
        path: &std::path::Path,
        domain: Option<&str>,
        topic: Option<&str>,
    ) -> Result<super::MineReport, HarnessError> {
        self.store.mine_convos(path, domain, topic)
    }

    pub fn journal_write(
        &self,
        agent: &str,
        content: &str,
        timestamp: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<super::JournalEntry, HarnessError> {
        self.store.journal_write(agent, content, timestamp)
    }

    pub fn journal_read(
        &self,
        agent: &str,
        date: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<super::JournalEntry>, HarnessError> {
        self.store.journal_read(agent, date, limit)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn event_append(
        &self,
        stream: &str,
        event_type: &str,
        from: &str,
        to: Option<&str>,
        body: &str,
        correlation_id: Option<&str>,
        status: Option<&str>,
    ) -> Result<super::Event, HarnessError> {
        self.store
            .event_append(stream, event_type, from, to, body, correlation_id, status)
    }

    pub fn event_list(
        &self,
        stream: &str,
        since: Option<chrono::DateTime<chrono::Utc>>,
        limit: Option<usize>,
        unacked_by: Option<&str>,
    ) -> Result<Vec<super::Event>, HarnessError> {
        self.store.event_list(stream, since, limit, unacked_by)
    }

    pub fn event_ack(&self, stream: &str, agent: &str, event_id: &str) -> Result<(), HarnessError> {
        self.store.event_ack(stream, agent, event_id)
    }

    pub async fn event_wait(
        &self,
        stream: &str,
        agent: &str,
        timeout_ms: u64,
    ) -> Result<Option<super::Event>, HarnessError> {
        self.store.event_wait(stream, agent, timeout_ms).await
    }

    pub fn artifact_put(
        &self,
        id_opt: Option<&str>,
        payload: &[u8],
        extra_meta: std::collections::HashMap<String, String>,
    ) -> Result<String, HarnessError> {
        self.store.artifact_put(id_opt, payload, extra_meta)
    }

    pub fn artifact_get(
        &self,
        id: &str,
    ) -> Result<Option<(Vec<u8>, super::ArtifactMeta)>, HarnessError> {
        self.store.artifact_get(id)
    }
}

/// Save a `MemoryRecord`: write the incoming record and retire any active
/// record with the same subject/predicate. Shared by the direct engine and the
/// daemon job dispatcher so both write paths behave identically.
pub fn save_record(store: &MemoryStore, record: &MemoryRecord) -> Result<(), HarnessError> {
    if record.id.trim().is_empty() {
        return Err(HarnessError::MemoryError(
            "Cannot save a memory record with an empty id".to_string(),
        ));
    }

    let incoming = Record::from_memory(record);

    // Auto-supersede conflicting active records, matching the SQLite engine:
    // one active record per (subject, predicate), case-insensitive. Note
    // pointers match exactly because their predicate embeds the note id.
    let conflicts: Vec<Record> = store
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

    store.write_record(incoming.clone())?;

    for mut old in conflicts {
        old.is_active = false;
        old.superseded_by = Some(incoming.id.clone());
        old.updated_at = Utc::now();
        store.write_record(old)?;
    }
    Ok(())
}

/// Update a record in place or replace its identity. See `save_record`.
pub fn update_record(
    store: &MemoryStore,
    id: &str,
    updated: &MemoryRecord,
) -> Result<(), HarnessError> {
    let Some(target) = store.get_record(id)? else {
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
        store.write_record(record)?;
    } else {
        // A changed identity writes a replacement and retires the old file.
        save_record(store, updated)?;
        let mut old = target;
        old.is_active = false;
        old.superseded_by = Some(updated.id.clone());
        old.updated_at = Utc::now();
        store.write_record(old)?;
    }

    // Retire any other active duplicate, but not for note pointers.
    if updated.category != MemoryCategory::NotePointer {
        for mut duplicate in store.all_records()?.into_iter().filter(|record| {
            record.is_active
                && record.id != updated.id
                && record.category != MemoryCategory::NotePointer
                && record.subject.eq_ignore_ascii_case(&updated.subject)
                && record.predicate.eq_ignore_ascii_case(&updated.predicate)
        }) {
            duplicate.is_active = false;
            duplicate.superseded_by = Some(updated.id.clone());
            duplicate.updated_at = Utc::now();
            store.write_record(duplicate)?;
        }
    }
    Ok(())
}

/// Delete one or more records by id, subject, or note pointer. See `save_record`.
pub fn forget_records(store: &MemoryStore, id: &str) -> Result<(), HarnessError> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return Ok(());
    }

    if is_forget_all(trimmed) {
        store.delete_all_records()?;
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

    let all = store.all_records()?;
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
            store.delete_record(&target)?;
            // Clear references from records that pointed at the deleted one.
            for mut referrer in store
                .all_records()?
                .into_iter()
                .filter(|record| record.superseded_by.as_deref() == Some(target.as_str()))
            {
                referrer.superseded_by = None;
                store.write_record(referrer)?;
            }
        }
    }
    Ok(())
}

#[async_trait]
impl MemoryEngine for MarkdownMemoryEngine {
    async fn retrieve(&self, query: &str, limit: usize) -> Result<Vec<MemoryRecord>, HarnessError> {
        self.store.retrieve(query, limit)
    }

    async fn retrieve_cards(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>, HarnessError> {
        self.store.retrieve_cards(query, limit)
    }

    async fn save(&self, record: &MemoryRecord) -> Result<(), HarnessError> {
        save_record(&self.store, record)
    }

    async fn update(&self, id: &str, updated: &MemoryRecord) -> Result<(), HarnessError> {
        update_record(&self.store, id, updated)
    }

    async fn forget(&self, id: &str) -> Result<(), HarnessError> {
        forget_records(&self.store, id)
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

/// Engine that forwards writes to a running daemon, falling back to the direct
/// store when the policy is `prefer` and the daemon is unreachable. Reads are
/// served from Markdown after a light reload so daemon writes become visible.
pub struct DaemonWriteEngine {
    direct: MarkdownMemoryEngine,
    client: super::daemon::NativeDaemonClient,
    policy: super::daemon::WriteRoutingPolicy,
}

impl DaemonWriteEngine {
    pub fn new(
        direct: MarkdownMemoryEngine,
        client: super::daemon::NativeDaemonClient,
        policy: super::daemon::WriteRoutingPolicy,
    ) -> Self {
        Self {
            direct,
            client,
            policy,
        }
    }

    pub fn direct(&self) -> &MarkdownMemoryEngine {
        &self.direct
    }

    fn may_fallback(&self) -> bool {
        self.policy == super::daemon::WriteRoutingPolicy::Prefer
    }

    async fn forward(&self, kind: &str, payload: serde_json::Value) -> Result<(), HarnessError> {
        self.client.execute(kind, payload).await.map(|_| ())
    }
}

#[async_trait]
impl MemoryEngine for DaemonWriteEngine {
    async fn retrieve(&self, query: &str, limit: usize) -> Result<Vec<MemoryRecord>, HarnessError> {
        self.direct.store().reload()?;
        self.direct.retrieve(query, limit).await
    }

    async fn retrieve_cards(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>, HarnessError> {
        self.direct.store().reload()?;
        self.direct.retrieve_cards(query, limit).await
    }

    async fn save(&self, record: &MemoryRecord) -> Result<(), HarnessError> {
        let payload = serde_json::to_value(record)
            .map_err(|e| HarnessError::SerializationError(e.to_string()))?;
        match self.forward("save_memory", payload).await {
            Ok(()) => Ok(()),
            Err(_) if self.may_fallback() => self.direct.save(record).await,
            Err(error) => Err(error),
        }
    }

    async fn update(&self, id: &str, updated: &MemoryRecord) -> Result<(), HarnessError> {
        let payload = serde_json::json!({ "id": id, "record": updated });
        match self.forward("update_memory", payload).await {
            Ok(()) => Ok(()),
            Err(_) if self.may_fallback() => self.direct.update(id, updated).await,
            Err(error) => Err(error),
        }
    }

    async fn forget(&self, id: &str) -> Result<(), HarnessError> {
        let payload = serde_json::json!({ "id": id });
        match self.forward("forget_memory", payload).await {
            Ok(()) => Ok(()),
            Err(_) if self.may_fallback() => self.direct.forget(id).await,
            Err(error) => Err(error),
        }
    }

    async fn add_note_pointer(&self, note_id: &str, pointer: &str) -> Result<(), HarnessError> {
        self.direct.add_note_pointer(note_id, pointer).await
    }

    async fn get_record(&self, id: &str) -> Result<Option<MemoryRecord>, HarnessError> {
        self.direct.store().reload()?;
        self.direct.get_record(id).await
    }

    fn format_as_system_prompt(&self, memories: &[MemoryRecord]) -> String {
        self.direct.format_as_system_prompt(memories)
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
