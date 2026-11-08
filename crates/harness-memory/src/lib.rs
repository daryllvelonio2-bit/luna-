pub mod engine;
pub mod saver;
pub mod store;
pub mod tools;

pub use engine::SqliteMemoryEngine;
pub use saver::{MemorySaver, SaverPolicy, SAVER_SYSTEM_PROMPT};
pub use store::{
    default_root, AuditReport, MarkdownMemoryEngine, MemoryStore, MineReport, Record, SearchFilter,
    SearchResult, StatusReport, StoreConfig,
};
pub use tools::{ForgetMemoryTool, MemorySearchTool, SaveMemoryTool, UpdateMemoryTool};

use std::sync::Arc;

use async_trait::async_trait;
use harness_core::{HarnessError, MemoryEngine, MemoryRecord};

/// Concrete engine selected at startup, so `Agent` keeps a sized `M`.
///
/// `Agent` stores `Arc<M>` with `M: MemoryEngine + Sized`; a bare
/// `Arc<dyn MemoryEngine>` cannot be that `M`. This enum is the sized
/// adapter: the rest of the CLI sees one engine type, and the Markdown store
/// is selected by config without a second agent type.
pub enum MemoryEngineKind {
    Sqlite(SqliteMemoryEngine),
    Markdown(MarkdownMemoryEngine),
}

impl MemoryEngineKind {
    pub fn markdown(&self) -> Option<&MarkdownMemoryEngine> {
        match self {
            Self::Markdown(engine) => Some(engine),
            Self::Sqlite(_) => None,
        }
    }
}

#[async_trait]
impl MemoryEngine for MemoryEngineKind {
    async fn retrieve(&self, query: &str, limit: usize) -> Result<Vec<MemoryRecord>, HarnessError> {
        match self {
            Self::Sqlite(engine) => engine.retrieve(query, limit).await,
            Self::Markdown(engine) => engine.retrieve(query, limit).await,
        }
    }

    async fn save(&self, record: &MemoryRecord) -> Result<(), HarnessError> {
        match self {
            Self::Sqlite(engine) => engine.save(record).await,
            Self::Markdown(engine) => engine.save(record).await,
        }
    }

    async fn update(&self, id: &str, updated: &MemoryRecord) -> Result<(), HarnessError> {
        match self {
            Self::Sqlite(engine) => engine.update(id, updated).await,
            Self::Markdown(engine) => engine.update(id, updated).await,
        }
    }

    async fn forget(&self, id: &str) -> Result<(), HarnessError> {
        match self {
            Self::Sqlite(engine) => engine.forget(id).await,
            Self::Markdown(engine) => engine.forget(id).await,
        }
    }

    async fn add_note_pointer(&self, note_id: &str, pointer: &str) -> Result<(), HarnessError> {
        match self {
            Self::Sqlite(engine) => engine.add_note_pointer(note_id, pointer).await,
            Self::Markdown(engine) => engine.add_note_pointer(note_id, pointer).await,
        }
    }

    async fn remove_note_pointer(&self, note_id: &str) -> Result<(), HarnessError> {
        match self {
            Self::Sqlite(engine) => engine.remove_note_pointer(note_id).await,
            Self::Markdown(engine) => engine.remove_note_pointer(note_id).await,
        }
    }

    async fn get_record(&self, id: &str) -> Result<Option<MemoryRecord>, HarnessError> {
        match self {
            Self::Sqlite(engine) => engine.get_record(id).await,
            Self::Markdown(engine) => engine.get_record(id).await,
        }
    }

    fn format_as_system_prompt(&self, memories: &[MemoryRecord]) -> String {
        match self {
            Self::Sqlite(engine) => engine.format_as_system_prompt(memories),
            Self::Markdown(engine) => engine.format_as_system_prompt(memories),
        }
    }
}

/// Build the Markdown store config from Luna's config, including the
/// zero-dependency embedder identity and the weight mapping.
pub fn store_config(config: &harness_core::config::Config) -> StoreConfig {
    let settings = &config.memory.store;
    let mut store = StoreConfig::new(config.paths.memory_dir.clone());
    store.chunk_chars = settings.chunk_chars.max(200);
    store.index_chars = settings.index_chars.max(200);
    store.index_scan_chars = settings.index_scan_chars.max(500);
    store.vector_weight = settings.hybrid_vector_weight;
    store.bm25_weight = settings.hybrid_bm25_weight;
    store.index_boost = settings.index_boost;
    store.auto_mine = settings.auto_mine;
    store.shard_after = settings.shard_after.max(100);
    // The shipped embedder is the zero-dependency feature hasher. If a model
    // backend is configured before it is implemented, the manifest must still
    // record what actually produced the vectors.
    store.embedder = store::embed::identity().to_string();
    if !settings.embedder.eq_ignore_ascii_case("hash")
        && !settings.embedder.eq_ignore_ascii_case("none")
    {
        tracing::warn!(
            "memory.store.embedder = '{}' is not available in this build; using '{}'",
            settings.embedder,
            store.embedder
        );
    }
    store
}

/// Select the memory engine from config. Markdown is the default; SQLite remains
/// available for compatibility and as a migration source.
pub fn build_memory_engine(
    config: &harness_core::config::Config,
) -> Result<Arc<MemoryEngineKind>, HarnessError> {
    if !config.memory.store.enabled {
        return Ok(Arc::new(MemoryEngineKind::Sqlite(SqliteMemoryEngine::new(
            Some(config.paths.memory_db.clone()),
        )?)));
    }
    Ok(Arc::new(MemoryEngineKind::Markdown(
        MarkdownMemoryEngine::new(store_config(config))?,
    )))
}
