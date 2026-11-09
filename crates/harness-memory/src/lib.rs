pub mod engine;
pub mod saver;
pub mod store;
pub mod tools;

pub use engine::SqliteMemoryEngine;
pub use saver::{MemorySaver, SaverPolicy, SAVER_SYSTEM_PROMPT};
pub use store::{
    all_mcp_tools, choose_write_route, default_root, dispatch_mcp_tool, handle_mcp_jsonrpc,
    mine_convos, mine_sessions, parse_write_routing_policy, render_shared_brain_rules, run_hook,
    run_instructions, run_stdio_server, ArtifactMeta, AuditReport, CompactSummary, ContentIndex,
    CrossLink, CrossLinkBridge, CrossLinksStore, DaemonEndpointInfo, DaemonPaths, DaemonRuntime,
    DaemonWriteEngine, DomainLink, Entity, EntityAttribute, EntityCandidate, EntityQueryResult,
    Event, EventsStore, GraphData, GraphEdge, GraphIndex, GraphNode, IndexBoost, Job, JournalEntry,
    JournalStore, KnowledgeGraph, KnowledgeGraphStats, Layer0, Layer1, Layer2, Layer3, LinksStore,
    MarkdownMemoryEngine, McpToolDef, MemoryStack, MemoryStore, MineReport, NativeDaemonClient,
    PointerLine, QueueStore, Record, SearchFilter, SearchResult, StatusReport, StoreConfig,
    TraversalStep, Triple, WriteRoutingDecision, WriteRoutingPolicy, WriteRoutingTarget,
    LOCK_REFUSAL_ERROR_CLASS, MAX_ATTEMPTS, SUMMARY_SPEC,
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
    Daemon(DaemonWriteEngine),
}

impl MemoryEngineKind {
    pub fn markdown(&self) -> Option<&MarkdownMemoryEngine> {
        match self {
            Self::Markdown(engine) => Some(engine),
            Self::Daemon(engine) => Some(engine.direct()),
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
            Self::Daemon(engine) => engine.retrieve(query, limit).await,
        }
    }

    async fn retrieve_cards(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>, HarnessError> {
        match self {
            Self::Sqlite(engine) => engine.retrieve_cards(query, limit).await,
            Self::Markdown(engine) => engine.retrieve_cards(query, limit).await,
            Self::Daemon(engine) => engine.retrieve_cards(query, limit).await,
        }
    }

    async fn save(&self, record: &MemoryRecord) -> Result<(), HarnessError> {
        match self {
            Self::Sqlite(engine) => engine.save(record).await,
            Self::Markdown(engine) => engine.save(record).await,
            Self::Daemon(engine) => engine.save(record).await,
        }
    }

    async fn update(&self, id: &str, updated: &MemoryRecord) -> Result<(), HarnessError> {
        match self {
            Self::Sqlite(engine) => engine.update(id, updated).await,
            Self::Markdown(engine) => engine.update(id, updated).await,
            Self::Daemon(engine) => engine.update(id, updated).await,
        }
    }

    async fn forget(&self, id: &str) -> Result<(), HarnessError> {
        match self {
            Self::Sqlite(engine) => engine.forget(id).await,
            Self::Markdown(engine) => engine.forget(id).await,
            Self::Daemon(engine) => engine.forget(id).await,
        }
    }

    async fn add_note_pointer(&self, note_id: &str, pointer: &str) -> Result<(), HarnessError> {
        match self {
            Self::Sqlite(engine) => engine.add_note_pointer(note_id, pointer).await,
            Self::Markdown(engine) => engine.add_note_pointer(note_id, pointer).await,
            Self::Daemon(engine) => engine.add_note_pointer(note_id, pointer).await,
        }
    }

    async fn remove_note_pointer(&self, note_id: &str) -> Result<(), HarnessError> {
        match self {
            Self::Sqlite(engine) => engine.remove_note_pointer(note_id).await,
            Self::Markdown(engine) => engine.remove_note_pointer(note_id).await,
            Self::Daemon(engine) => engine.remove_note_pointer(note_id).await,
        }
    }

    async fn get_record(&self, id: &str) -> Result<Option<MemoryRecord>, HarnessError> {
        match self {
            Self::Sqlite(engine) => engine.get_record(id).await,
            Self::Markdown(engine) => engine.get_record(id).await,
            Self::Daemon(engine) => engine.get_record(id).await,
        }
    }

    fn format_as_system_prompt(&self, memories: &[MemoryRecord]) -> String {
        match self {
            Self::Sqlite(engine) => engine.format_as_system_prompt(memories),
            Self::Markdown(engine) => engine.format_as_system_prompt(memories),
            Self::Daemon(engine) => engine.format_as_system_prompt(memories),
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
    store.hooks_save_interval = settings.hooks_save_interval.max(1);
    store.hooks_auto_save = settings.hooks_auto_save;
    store.legacy_db = Some(config.paths.memory_db.clone());
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
    let policy = store::parse_write_routing_policy(&config.memory.store.write_routing, false)?;
    let direct = MarkdownMemoryEngine::new(store_config(config))?;
    match policy {
        store::WriteRoutingPolicy::Direct => {
            Ok(Arc::new(MemoryEngineKind::Markdown(direct)))
        }
        store::WriteRoutingPolicy::Prefer | store::WriteRoutingPolicy::Require => {
            match NativeDaemonClient::connect_root(&config.paths.memory_dir) {
                Ok(client) => Ok(Arc::new(MemoryEngineKind::Daemon(
                    DaemonWriteEngine::new(direct, client, policy),
                ))),
                Err(error) if policy == store::WriteRoutingPolicy::Require => {
                    Err(HarnessError::ConfigurationError(format!(
                        "memory.store.write_routing = 'require' but the daemon is not running: {error}. Start it with `luna memory daemon start --bg`."
                    )))
                }
                Err(_) => Ok(Arc::new(MemoryEngineKind::Markdown(direct))),
            }
        }
    }
}
