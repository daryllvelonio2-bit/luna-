//! The Markdown memory store: canonical files, placement, indexing, search,
//! mining, audit, and export.
//!
//! Markdown is the source of truth. Everything under `search_index/` is
//! derived and can be deleted at any time; the in-process cache is rebuilt
//! from the `.md` files and `reindex` simply drops it.

pub mod atomic;
pub mod context;
pub mod convo;
pub mod crosslinks;
pub mod daemon;
pub mod embed;
pub mod engine;
pub mod events;
pub mod frontmatter;
pub mod graph;
pub mod hooks;
pub mod ids;
pub mod journal;
pub mod kg;
pub mod links;
pub mod mcp;
pub mod miner;
pub mod pointer_index;
pub mod search;
pub mod sessions;
pub mod summary;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use chrono::{DateTime, Utc};
use harness_core::{HarnessError, MemoryCategory, MemoryRecord};
use serde::{Deserialize, Serialize};
use serde_yaml::{Mapping, Value};

pub use context::{Layer0, Layer1, Layer2, Layer3, MemoryStack};
pub use convo::mine_convos;
pub use crosslinks::{CrossLink, CrossLinksStore};
pub use daemon::{
    choose_write_route, parse_write_routing_policy, DaemonEndpointInfo, DaemonPaths, DaemonRuntime,
    Job, NativeDaemonClient, QueueStore, WriteRoutingDecision, WriteRoutingPolicy,
    WriteRoutingTarget, LOCK_REFUSAL_ERROR_CLASS, MAX_ATTEMPTS,
};
pub use engine::{DaemonWriteEngine, MarkdownMemoryEngine};
pub use events::{ArtifactMeta, Event, EventsStore};
pub use graph::{CrossLinkBridge, GraphData, GraphEdge, GraphIndex, GraphNode, TraversalStep};
pub use hooks::{render_shared_brain_rules, run_hook, run_instructions};
pub use journal::{JournalEntry, JournalStore};
pub use kg::{
    Entity, EntityAttribute, EntityCandidate, EntityQueryResult, KnowledgeGraph,
    KnowledgeGraphStats, Triple,
};
pub use links::{DomainLink, LinksStore};
pub use mcp::{all_mcp_tools, dispatch_mcp_tool, handle_mcp_jsonrpc, run_stdio_server, McpToolDef};
pub use miner::MineReport;
pub use pointer_index::{ContentIndex, IndexBoost, PointerLine};
pub use sessions::mine_sessions;
pub use summary::{
    CompactSummary, SummaryArc, SummaryCross, SummaryEntry, SummaryHeader, SUMMARY_SPEC,
};

pub const MANIFEST_SCHEMA: u32 = 1;

/// Runtime settings for the Markdown store.
#[derive(Debug, Clone)]
pub struct StoreConfig {
    pub root: PathBuf,
    pub embedder: String,
    pub chunk_chars: usize,
    pub index_chars: usize,
    pub index_scan_chars: usize,
    pub vector_weight: f32,
    pub bm25_weight: f32,
    pub index_boost: f32,
    pub auto_mine: bool,
    pub shard_after: usize,
    pub overfetch: usize,
    /// Save every N human messages when hooks are enabled.
    pub hooks_save_interval: usize,
    /// Let lifecycle hooks write memory automatically.
    pub hooks_auto_save: bool,
    /// Legacy SQLite database to import when the Markdown store is empty.
    pub legacy_db: Option<PathBuf>,
}

impl StoreConfig {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            embedder: embed::identity().to_string(),
            chunk_chars: 800,
            index_chars: 1_500,
            index_scan_chars: 5_000,
            vector_weight: 0.6,
            bm25_weight: 0.4,
            index_boost: 0.1,
            auto_mine: false,
            shard_after: 5_000,
            overfetch: 4,
            hooks_save_interval: hooks::DEFAULT_SAVE_INTERVAL,
            hooks_auto_save: true,
            legacy_db: None,
        }
    }
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self::new(default_root())
    }
}

pub fn default_root() -> PathBuf {
    harness_core::config::home_dir()
        .join(".luna")
        .join("memory")
}

/// One canonical record: a verbatim Markdown file with typed front matter.
///
/// The `subject`/`predicate`/`object` and timestamps mirror Luna's existing
/// `MemoryRecord`, so the old `save_memory` / `update_memory` / `forget_memory`
/// tools keep their contract while the file store replaces SQLite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub domain: String,
    pub topic: String,
    pub source_file: Option<String>,
    pub chunk_index: usize,
    pub line_start: usize,
    pub line_end: usize,
    pub filed_at: DateTime<Utc>,
    pub entities: Vec<String>,
    pub tags: Vec<String>,
    pub body_hash: String,
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub name: Option<String>,
    pub path: Option<String>,
    pub raw_content: String,
    pub category: MemoryCategory,
    pub confidence: f32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub is_active: bool,
    pub superseded_by: Option<String>,
    pub body: String,
}

impl Record {
    pub fn from_memory(record: &MemoryRecord) -> Self {
        let (domain, topic) = placement(record);
        let body = if record.raw_content.trim().is_empty() {
            record.object.clone()
        } else {
            record.raw_content.clone()
        };
        Self {
            id: record.id.clone(),
            domain,
            topic,
            source_file: record.path.clone(),
            chunk_index: 0,
            line_start: 0,
            line_end: 0,
            filed_at: Utc::now(),
            entities: Vec::new(),
            tags: Vec::new(),
            body_hash: ids::body_hash(&body),
            subject: record.subject.clone(),
            predicate: record.predicate.clone(),
            object: record.object.clone(),
            name: record.name.clone(),
            path: record.path.clone(),
            raw_content: record.raw_content.clone(),
            category: record.category,
            confidence: record.confidence,
            created_at: record.created_at,
            updated_at: record.updated_at,
            is_active: record.is_active,
            superseded_by: record.superseded_by.clone(),
            body,
        }
    }

    pub fn to_memory(&self) -> MemoryRecord {
        MemoryRecord {
            id: self.id.clone(),
            subject: self.subject.clone(),
            predicate: self.predicate.clone(),
            object: self.object.clone(),
            name: self.name.clone(),
            path: self.path.clone(),
            raw_content: if self.raw_content.trim().is_empty() {
                self.body.clone()
            } else {
                self.raw_content.clone()
            },
            category: self.category,
            confidence: self.confidence,
            created_at: self.created_at,
            updated_at: self.updated_at,
            is_active: self.is_active,
            superseded_by: self.superseded_by.clone(),
        }
    }

    fn to_document(&self) -> String {
        let mut map = Mapping::new();
        let mut put = |key: &str, value: String| {
            map.insert(Value::String(key.to_string()), Value::String(value));
        };
        put("id", self.id.clone());
        put("domain", self.domain.clone());
        put("topic", self.topic.clone());
        if let Some(source) = &self.source_file {
            put("source_file", source.clone());
        }
        put("chunk_index", self.chunk_index.to_string());
        put("line_start", self.line_start.to_string());
        put("line_end", self.line_end.to_string());
        put("filed_at", self.filed_at.to_rfc3339());
        put("entities", self.entities.join(", "));
        put("tags", self.tags.join(", "));
        put("body_hash", self.body_hash.clone());
        put("subject", self.subject.clone());
        put("predicate", self.predicate.clone());
        put("object", self.object.clone());
        if let Some(name) = &self.name {
            put("name", name.clone());
        }
        if let Some(path) = &self.path {
            put("path", path.clone());
        }
        put("category", self.category.to_string());
        put("confidence", format!("{:.4}", self.confidence));
        put("created_at", self.created_at.to_rfc3339());
        put("updated_at", self.updated_at.to_rfc3339());
        put("is_active", self.is_active.to_string());
        if let Some(superseded) = &self.superseded_by {
            put("superseded_by", superseded.clone());
        }
        let yaml = serde_yaml::to_string(&map).unwrap_or_default();
        frontmatter::render(&yaml, &self.body)
    }

    fn from_document(text: &str) -> Result<Self, String> {
        let (yaml, body) =
            frontmatter::split(text).ok_or_else(|| "missing front matter".to_string())?;
        let value = frontmatter::parse(yaml).map_err(|e| e.to_string())?;
        let id = frontmatter::string(&value, "id").ok_or_else(|| "missing id".to_string())?;
        let category = match frontmatter::string(&value, "category").as_deref() {
            Some("user_profile") => MemoryCategory::UserProfile,
            Some("code_pattern") => MemoryCategory::CodePattern,
            Some("workflow_rule") => MemoryCategory::WorkflowRule,
            Some("note_pointer") => MemoryCategory::NotePointer,
            Some("transcript") => MemoryCategory::Transcript,
            _ => MemoryCategory::Fact,
        };
        let domain = frontmatter::string(&value, "domain").unwrap_or_else(|| "memory".to_string());
        let topic = frontmatter::string(&value, "topic").unwrap_or_else(|| "general".to_string());
        let created_at =
            parse_time(frontmatter::string(&value, "created_at")).unwrap_or_else(Utc::now);
        let updated_at =
            parse_time(frontmatter::string(&value, "updated_at")).unwrap_or(created_at);
        let filed_at = parse_time(frontmatter::string(&value, "filed_at")).unwrap_or(created_at);
        let raw_content = body.to_string();
        Ok(Self {
            id,
            domain,
            topic,
            source_file: frontmatter::opt_string(&value, "source_file"),
            chunk_index: frontmatter::u64(&value, "chunk_index").unwrap_or(0) as usize,
            line_start: frontmatter::u64(&value, "line_start").unwrap_or(0) as usize,
            line_end: frontmatter::u64(&value, "line_end").unwrap_or(0) as usize,
            filed_at,
            entities: frontmatter::list(&value, "entities"),
            tags: frontmatter::list(&value, "tags"),
            body_hash: frontmatter::string(&value, "body_hash")
                .unwrap_or_else(|| ids::body_hash(body)),
            subject: frontmatter::string(&value, "subject").unwrap_or_default(),
            predicate: frontmatter::string(&value, "predicate").unwrap_or_default(),
            object: frontmatter::string(&value, "object").unwrap_or_default(),
            name: frontmatter::opt_string(&value, "name"),
            path: frontmatter::opt_string(&value, "path"),
            raw_content,
            category,
            confidence: frontmatter::f32(&value, "confidence").unwrap_or(1.0),
            created_at,
            updated_at,
            is_active: frontmatter::bool(&value, "is_active", true),
            superseded_by: frontmatter::opt_string(&value, "superseded_by"),
            body: body.to_string(),
        })
    }
}

fn parse_time(value: Option<String>) -> Option<DateTime<Utc>> {
    value.and_then(|text| {
        DateTime::parse_from_rfc3339(&text)
            .map(|time| time.with_timezone(&Utc))
            .ok()
    })
}

/// Where a Luna `MemoryRecord` lives in the taxonomy.
fn placement(record: &MemoryRecord) -> (String, String) {
    if record.category == MemoryCategory::NotePointer {
        return ("notes".to_string(), "pointers".to_string());
    }
    if let Some((domain, rest)) = record.id.split_once('.') {
        if !domain.is_empty() && !rest.is_empty() {
            return (ids::slugify(domain), ids::slugify(rest));
        }
    }
    (
        "memory".to_string(),
        ids::slugify(&record.category.to_string()),
    )
}

/// Optional filters for a search.
#[derive(Debug, Clone, Default)]
pub struct SearchFilter {
    pub domain: Option<String>,
    pub topic: Option<String>,
    pub since: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
    /// Drop records of this category before ranking. The prompt block uses it to keep raw
    /// transcripts out of the always-loaded list while leaving them searchable.
    pub exclude_category: Option<MemoryCategory>,
}

/// One search result, carrying the record and its score components.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub record: Record,
    pub path: PathBuf,
    pub score: f32,
    pub lexical: f32,
    pub vector: f32,
    pub index_boost: f32,
    pub matched_via: String,
    pub index_preview: Option<String>,
    pub excerpt: String,
}

struct Indexed {
    path: PathBuf,
    record: Record,
    vector: Vec<f32>,
}

#[derive(Default)]
struct Cache {
    loaded: bool,
    docs: Vec<Indexed>,
}

/// The store handle. Cheap to clone? No: shared behind an `Arc` by the engine.
pub struct MemoryStore {
    pub config: StoreConfig,
    cache: RwLock<Cache>,
}

impl MemoryStore {
    pub fn open(config: StoreConfig) -> Result<Self, HarnessError> {
        let store = Self {
            config,
            cache: RwLock::new(Cache::default()),
        };
        store.ensure_layout()?;
        // One-time upgrade path: if the legacy SQLite database exists and the
        // Markdown store is empty, import it automatically so an upgrade does
        // not silently start with no recall.
        if let Some(db) = store.config.legacy_db.clone() {
            if db.exists() {
                let empty = store
                    .all_record_paths()
                    .map(|paths| paths.is_empty())
                    .unwrap_or(false);
                if empty {
                    match store.migrate_from_sqlite(&db) {
                        Ok(count) if count > 0 => tracing::info!(
                            "memory store: imported {count} record(s) from {}",
                            db.display()
                        ),
                        Ok(_) => {}
                        Err(error) => tracing::warn!(
                            "memory store: could not import {}: {error}",
                            db.display()
                        ),
                    }
                }
            }
        }
        Ok(store)
    }

    pub fn root(&self) -> &Path {
        &self.config.root
    }

    fn ensure_layout(&self) -> Result<(), HarnessError> {
        let root = &self.config.root;
        for dir in [
            root.to_path_buf(),
            root.join("domains"),
            root.join("links"),
            root.join("crosslinks"),
            root.join("graph").join("entities"),
            root.join("graph").join("triples"),
            root.join("journal"),
            root.join("events"),
            root.join("artifacts"),
            root.join("summaries"),
            root.join("search_index"),
        ] {
            fs::create_dir_all(&dir).map_err(io_err)?;
        }
        let store_md = root.join("store.md");
        if !store_md.exists() {
            let text = frontmatter::render(
                &format!(
                    "schema: {}\ncreated_at: {}\nembedder: {}",
                    MANIFEST_SCHEMA,
                    Utc::now().to_rfc3339(),
                    self.config.embedder
                ),
                "# Luna memory store\n\nMarkdown is canonical. `search_index/` is derived and safe to delete.\n",
            );
            atomic::write_str(&store_md, &text).map_err(io_err)?;
        }
        let identity = root.join("identity.md");
        if !identity.exists() {
            atomic::write_str(
                &identity,
                "# Identity\n\nLuna is the user's long-running engineering agent. This file is the always-injected identity layer; keep it short.\n",
            )
            .map_err(io_err)?;
        }
        let summary_md = root.join("SUMMARY.md");
        if !summary_md.exists() {
            atomic::write_str(&summary_md, summary::SUMMARY_SPEC).map_err(io_err)?;
        }
        Ok(())
    }

    pub fn record_path(&self, domain: &str, topic: &str, id: &str) -> PathBuf {
        let domain = ids::slugify(domain);
        let topic = ids::slugify(topic);
        let stem = ids::safe_id(id);
        self.config
            .root
            .join("domains")
            .join(domain)
            .join("topics")
            .join(topic)
            .join("records")
            .join(format!("{stem}.md"))
    }

    fn ensure_topic(&self, domain: &str, topic: &str) -> Result<(), HarnessError> {
        let domain_dir = self.config.root.join("domains").join(ids::slugify(domain));
        let topic_dir = domain_dir.join("topics").join(ids::slugify(topic));
        fs::create_dir_all(topic_dir.join("records")).map_err(io_err)?;
        fs::create_dir_all(topic_dir.join("indexes")).map_err(io_err)?;
        fs::create_dir_all(topic_dir.join("summaries")).map_err(io_err)?;
        let domain_md = domain_dir.join("domain.md");
        if !domain_md.exists() {
            let text = frontmatter::render(
                &format!(
                    "domain: {}\ncreated_at: {}",
                    ids::slugify(domain),
                    Utc::now().to_rfc3339()
                ),
                "",
            );
            atomic::write_str(&domain_md, &text).map_err(io_err)?;
        }
        let topic_md = topic_dir.join("topic.md");
        if !topic_md.exists() {
            let text = frontmatter::render(
                &format!(
                    "topic: {}\ndomain: {}\ncreated_at: {}",
                    ids::slugify(topic),
                    ids::slugify(domain),
                    Utc::now().to_rfc3339()
                ),
                "",
            );
            atomic::write_str(&topic_md, &text).map_err(io_err)?;
        }
        Ok(())
    }

    /// Write a record. This is the only mutation path for record files.
    pub fn write_record(&self, record: Record) -> Result<(), HarnessError> {
        let _lock = atomic::lock(&self.config.root.join("search_index").join(".lock"))?;
        self.ensure_topic(&record.domain, &record.topic)?;
        let path = self.record_path(&record.domain, &record.topic, &record.id);
        atomic::cleanup_temp(&path);
        atomic::write_str(&path, &record.to_document()).map_err(io_err)?;

        // If the id moved domains/topics, remove the old file.
        if let Some(old) = self.find_path(&record.id) {
            if old != path {
                let _ = fs::remove_file(&old);
            }
        }

        let vector = embed::embed(&search_text(&record));
        let mut cache = self.cache.write().unwrap_or_else(|e| e.into_inner());
        cache.loaded = true;
        cache.docs.retain(|doc| doc.record.id != record.id);
        cache.docs.push(Indexed {
            path,
            record,
            vector,
        });
        cache
            .docs
            .sort_by_key(|a| std::cmp::Reverse(a.record.updated_at));
        self.write_manifest(cache.docs.len());
        Ok(())
    }

    /// Delete one record file by id, if it exists.
    pub fn delete_record(&self, id: &str) -> Result<bool, HarnessError> {
        let _lock = atomic::lock(&self.config.root.join("search_index").join(".lock"))?;
        let Some(path) = self.find_path(id) else {
            return Ok(false);
        };
        fs::remove_file(&path).map_err(io_err)?;
        let mut cache = self.cache.write().unwrap_or_else(|e| e.into_inner());
        cache.docs.retain(|doc| doc.record.id != id);
        Ok(true)
    }

    pub fn delete_all_records(&self) -> Result<usize, HarnessError> {
        let paths = self.all_record_paths()?;
        let mut removed = 0;
        for path in paths {
            if fs::remove_file(&path).is_ok() {
                removed += 1;
            }
        }
        let mut cache = self.cache.write().unwrap_or_else(|e| e.into_inner());
        cache.docs.clear();
        cache.loaded = true;
        self.write_manifest(0);
        Ok(removed)
    }

    fn find_path(&self, id: &str) -> Option<PathBuf> {
        {
            let cache = self.cache.read().unwrap_or_else(|e| e.into_inner());
            if cache.loaded {
                if let Some(doc) = cache.docs.iter().find(|doc| doc.record.id == id) {
                    return Some(doc.path.clone());
                }
            }
        }
        self.all_record_paths()
            .ok()?
            .into_iter()
            .find(|path| path.file_stem().and_then(|s| s.to_str()) == Some(&ids::safe_id(id)))
    }

    fn all_record_paths(&self) -> Result<Vec<PathBuf>, HarnessError> {
        let domains = self.config.root.join("domains");
        if !domains.exists() {
            return Ok(Vec::new());
        }
        let mut paths = Vec::new();
        collect_record_paths(&domains, &mut paths)?;
        Ok(paths)
    }

    fn load_all(&self) -> Result<Vec<Indexed>, HarnessError> {
        let mut docs = Vec::new();
        let mut invalid = Vec::new();
        for path in self.all_record_paths()? {
            match fs::read_to_string(&path) {
                Ok(text) => match Record::from_document(&text) {
                    Ok(record) => {
                        let vector = embed::embed(&search_text(&record));
                        docs.push(Indexed {
                            path,
                            record,
                            vector,
                        });
                    }
                    Err(error) => invalid.push((path, error)),
                },
                Err(error) => invalid.push((path, error.to_string())),
            }
        }
        for (path, error) in invalid {
            tracing::warn!(
                "memory store: skipping unreadable record {}: {}",
                path.display(),
                error
            );
        }
        docs.sort_by_key(|a| std::cmp::Reverse(a.record.updated_at));
        Ok(docs)
    }

    fn ensure_loaded(&self) -> Result<(), HarnessError> {
        {
            let cache = self.cache.read().unwrap_or_else(|e| e.into_inner());
            if cache.loaded {
                return Ok(());
            }
        }
        let docs = self.load_all()?;
        let mut cache = self.cache.write().unwrap_or_else(|e| e.into_inner());
        cache.docs = docs;
        cache.loaded = true;
        Ok(())
    }

    pub fn all_records(&self) -> Result<Vec<Record>, HarnessError> {
        self.ensure_loaded()?;
        let cache = self.cache.read().unwrap_or_else(|e| e.into_inner());
        Ok(cache.docs.iter().map(|doc| doc.record.clone()).collect())
    }

    pub fn active_records(&self) -> Result<Vec<Record>, HarnessError> {
        Ok(self
            .all_records()?
            .into_iter()
            .filter(|record| record.is_active)
            .collect())
    }

    pub fn get_record(&self, id_or_subject: &str) -> Result<Option<Record>, HarnessError> {
        self.ensure_loaded()?;
        let needle = id_or_subject.trim().to_ascii_lowercase();
        let cache = self.cache.read().unwrap_or_else(|e| e.into_inner());
        let exact = cache
            .docs
            .iter()
            .find(|doc| doc.record.id.eq_ignore_ascii_case(id_or_subject.trim()));
        if let Some(doc) = exact {
            return Ok(Some(doc.record.clone()));
        }
        Ok(cache
            .docs
            .iter()
            .find(|doc| {
                doc.record.is_active
                    && (doc.record.subject.eq_ignore_ascii_case(&needle)
                        || format!("{}.{}", doc.record.subject, doc.record.predicate)
                            .eq_ignore_ascii_case(&needle))
            })
            .map(|doc| doc.record.clone()))
    }

    /// Hybrid search over active records, with a recency fallback.
    pub fn search(
        &self,
        query: &str,
        filter: &SearchFilter,
        limit: usize,
    ) -> Result<Vec<SearchResult>, HarnessError> {
        self.ensure_loaded()?;
        let cache = self.cache.read().unwrap_or_else(|e| e.into_inner());
        let now = Utc::now();
        let filtered: Vec<&Indexed> = cache
            .docs
            .iter()
            .filter(|doc| doc.record.is_active)
            .filter(|doc| {
                filter
                    .domain
                    .as_ref()
                    .map(|domain| ids::slugify(domain) == ids::slugify(&doc.record.domain))
                    .unwrap_or(true)
            })
            .filter(|doc| {
                filter
                    .topic
                    .as_ref()
                    .map(|topic| ids::slugify(topic) == ids::slugify(&doc.record.topic))
                    .unwrap_or(true)
            })
            // Time windows use the content's authored time (`created_at`),
            // which miners populate from the source. `updated_at` only tracks
            // the last write, so using it would hide older memories that were
            // re-filed or superseded recently.
            .filter(|doc| {
                filter
                    .since
                    .map(|since| doc.record.created_at >= since)
                    .unwrap_or(true)
            })
            .filter(|doc| {
                filter
                    .before
                    .map(|before| doc.record.created_at < before)
                    .unwrap_or(true)
            })
            .filter(|doc| {
                filter
                    .exclude_category
                    .map(|category| doc.record.category != category)
                    .unwrap_or(true)
            })
            .collect();

        let documents: Vec<(String, String, Vec<f32>)> = filtered
            .iter()
            .map(|doc| {
                (
                    doc.record.id.clone(),
                    search_text(&doc.record),
                    doc.vector.clone(),
                )
            })
            .collect();
        let overfetch = limit
            .saturating_mul(self.config.overfetch.max(1))
            .max(limit);

        // Load content indexes and compute content-index rank boosts per source file
        let indexes = self.content_indexes().unwrap_or_default();
        let index_boosts = pointer_index::calculate_index_boosts(&indexes, query);
        let mut boost_by_id: HashMap<String, f32> = HashMap::new();
        for doc in &filtered {
            if let Some(src) = &doc.record.source_file {
                if let Some(cb) = index_boosts.get(src) {
                    boost_by_id.insert(doc.record.id.clone(), cb.boost);
                }
            }
        }

        let hits = search::hybrid_boosted(
            &documents,
            &boost_by_id,
            query,
            self.config.vector_weight,
            self.config.bm25_weight,
            overfetch,
        );
        let by_id: HashMap<&str, &Indexed> = filtered
            .iter()
            .map(|doc| (doc.record.id.as_str(), *doc))
            .collect();
        let mut results: Vec<SearchResult> = hits
            .into_iter()
            .filter_map(|hit| {
                let doc = by_id.get(hit.id.as_str())?;
                let preview = doc
                    .record
                    .source_file
                    .as_ref()
                    .and_then(|src| index_boosts.get(src))
                    .map(|b| b.preview.clone());
                Some(SearchResult {
                    record: doc.record.clone(),
                    path: doc.path.clone(),
                    score: hit.score,
                    lexical: hit.lexical,
                    vector: hit.vector,
                    index_boost: hit.index_boost,
                    matched_via: hit.matched_via,
                    index_preview: preview,
                    excerpt: excerpt(&doc.record.body, query),
                })
            })
            .collect();
        results.truncate(limit);

        if results.is_empty() && !filtered.is_empty() {
            // Recency fallback mirrors the SQLite engine: a query that matches
            // nothing still gets the most recent active records instead of an
            // empty prompt block.
            results = filtered
                .iter()
                .take(limit)
                .map(|doc| SearchResult {
                    record: doc.record.clone(),
                    path: doc.path.clone(),
                    score: 0.0,
                    lexical: 0.0,
                    vector: 0.0,
                    index_boost: 0.0,
                    matched_via: "record".to_string(),
                    index_preview: None,
                    excerpt: excerpt(&doc.record.body, query),
                })
                .collect();
        }
        let _ = now;
        Ok(results)
    }

    pub fn retrieve(&self, query: &str, limit: usize) -> Result<Vec<MemoryRecord>, HarnessError> {
        Ok(self
            .search(query, &SearchFilter::default(), limit)?
            .into_iter()
            .map(|result| result.record.to_memory())
            .collect())
    }

    /// Retrieve records for the prompt block, excluding raw transcripts.
    pub fn retrieve_cards(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryRecord>, HarnessError> {
        let mut memories: Vec<MemoryRecord> = Vec::new();

        // 1. L0/L1 Priority: Top project cards, canonical commands, and user profile
        if let Ok(active) = self.active_records() {
            let mut l0_l1: Vec<&Record> = active
                .iter()
                .filter(|r| r.category != MemoryCategory::Transcript)
                .filter(|r| {
                    r.category == MemoryCategory::UserProfile
                        || r.domain.eq_ignore_ascii_case("project")
                        || r.topic.eq_ignore_ascii_case("project")
                        || r.subject.starts_with("project.")
                        || matches!(
                            r.predicate.as_str(),
                            "build-command"
                                | "build_command"
                                | "test-command"
                                | "test_command"
                                | "run-command"
                                | "run_command"
                                | "root"
                                | "stack"
                                | "runbook"
                        )
                })
                .collect();
            l0_l1.sort_by(|a, b| {
                b.confidence
                    .partial_cmp(&a.confidence)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| b.filed_at.cmp(&a.filed_at))
            });
            for r in l0_l1.into_iter().take(limit.min(6)) {
                memories.push(r.to_memory());
            }
        }

        // 2. Query search
        if !query.trim().is_empty() && memories.len() < limit {
            let filter = SearchFilter {
                exclude_category: Some(MemoryCategory::Transcript),
                ..SearchFilter::default()
            };
            if let Ok(results) = self.search(query, &filter, limit - memories.len()) {
                for res in results {
                    let mem = res.record.to_memory();
                    if !memories.iter().any(|m| m.id == mem.id) {
                        memories.push(mem);
                    }
                }
            }
        }

        // 3. Fallback to active records if still under limit
        if memories.len() < limit {
            if let Ok(mut active) = self.active_records() {
                active.retain(|r| r.category != MemoryCategory::Transcript);
                active.sort_by_key(|r| std::cmp::Reverse(r.filed_at));
                for r in active {
                    let mem = r.to_memory();
                    if !memories.iter().any(|m| m.id == mem.id) {
                        memories.push(mem);
                        if memories.len() >= limit {
                            break;
                        }
                    }
                }
            }
        }

        Ok(memories)
    }

    /// Records rendered into the system-prompt block. Kept byte-compatible
    /// with the SQLite engine so prompt caching and model behavior do not shift.
    pub fn format_as_system_prompt(&self, memories: &[MemoryRecord]) -> String {
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

    /// 4-layer context stack handle.
    pub fn context_stack(&self) -> context::MemoryStack {
        context::MemoryStack::new(self.config.root.clone())
    }

    /// Wake-up context: L0 (identity) + L1 (essential story), bounded under 900 tokens.
    pub fn wake_up(&self, domain: Option<&str>) -> Result<String, HarnessError> {
        let records = self.active_records()?;
        Ok(self.context_stack().wake_up(&records, domain))
    }

    /// On-demand L2 recall filtered by domain and/or topic.
    pub fn recall(
        &self,
        domain: Option<&str>,
        topic: Option<&str>,
        limit: usize,
    ) -> Result<String, HarnessError> {
        let records = self.active_records()?;
        Ok(self.context_stack().recall(&records, domain, topic, limit))
    }

    /// L0 identity plus a bounded L1 recent-context block. `limit` caps how
    /// many records L1 may include; the character/token budget still applies.
    pub fn context_block(&self, limit: usize) -> Result<String, HarnessError> {
        let records = self.active_records()?;
        let mut stack = self.context_stack();
        stack.l1 = context::Layer1::new(limit.max(1), context::L1_MAX_CHARS);
        Ok(stack.wake_up(&records, None))
    }

    /// Load all parsed content indexes from disk.
    pub fn content_indexes(&self) -> Result<Vec<pointer_index::ContentIndex>, HarnessError> {
        let domains = self.config.root.join("domains");
        if !domains.exists() {
            return Ok(Vec::new());
        }
        let mut paths = Vec::new();
        collect_files_with_parent(&domains, "indexes", &mut paths)?;
        let mut indexes = Vec::new();
        for path in paths {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(index) = pointer_index::ContentIndex::parse(&content) {
                    indexes.push(index);
                }
            }
        }
        Ok(indexes)
    }

    /// Load all parsed compact summaries from disk.
    pub fn summaries(&self) -> Result<Vec<summary::CompactSummary>, HarnessError> {
        let domains = self.config.root.join("domains");
        if !domains.exists() {
            return Ok(Vec::new());
        }
        let mut paths = Vec::new();
        collect_files_with_parent(&domains, "summaries", &mut paths)?;
        let mut summaries = Vec::new();
        for path in paths {
            if let Ok(content) = fs::read_to_string(&path) {
                if let Ok(sum) = summary::CompactSummary::parse(&content) {
                    summaries.push(sum);
                }
            }
        }
        Ok(summaries)
    }

    /// Audit the canonical tree and report structural problems.
    pub fn audit(&self) -> Result<AuditReport, HarnessError> {
        let mut report = AuditReport::default();
        let mut seen: HashMap<(String, String), String> = HashMap::new();
        let paths = self.all_record_paths()?;
        let stems: HashSet<String> = paths
            .iter()
            .filter_map(|path| {
                path.file_stem()
                    .and_then(|stem| stem.to_str())
                    .map(str::to_string)
            })
            .collect();
        for path in paths {
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) => {
                    report
                        .unreadable
                        .push(format!("{}: {error}", path.display()));
                    continue;
                }
            };
            let record = match Record::from_document(&text) {
                Ok(record) => record,
                Err(error) => {
                    report.invalid.push(format!("{}: {error}", path.display()));
                    continue;
                }
            };
            report.records += 1;
            if !record.is_active {
                report.inactive += 1;
            }
            if record.body.trim().is_empty() {
                report.empty_bodies.push(record.id.clone());
            }
            if let Some(superseded) = &record.superseded_by {
                if !stems.contains(&ids::safe_id(superseded)) {
                    report.dangling_supersessions.push(record.id.clone());
                }
            }
            if record.is_active && record.category != MemoryCategory::NotePointer {
                let key = (
                    record.subject.trim().to_ascii_lowercase(),
                    record.predicate.trim().to_ascii_lowercase(),
                );
                if let Some(previous) = seen.insert(key, record.id.clone()) {
                    report
                        .duplicate_active
                        .push(format!("{previous} / {}", record.id));
                }
            }
        }
        Ok(report)
    }

    /// Rebuild the derived cache and manifest from Markdown.
    /// Rebuild the in-memory cache from Markdown without rebuilding the
    /// knowledge-graph or graph indexes. Used by daemon-backed engines so
    /// writes made by another process become visible.
    pub fn reload(&self) -> Result<(), HarnessError> {
        let docs = self.load_all()?;
        let mut cache = self.cache.write().unwrap_or_else(|e| e.into_inner());
        cache.docs = docs;
        cache.loaded = true;
        Ok(())
    }

    pub fn reindex(&self) -> Result<usize, HarnessError> {
        let docs = self.load_all()?;
        let count = docs.len();
        let mut cache = self.cache.write().unwrap_or_else(|e| e.into_inner());
        cache.docs = docs;
        cache.loaded = true;
        self.write_manifest(count);

        // Rebuild KG index and Graph adjacency
        let _ = self.kg().rebuild_index();
        let records: Vec<Record> = cache.docs.iter().map(|d| d.record.clone()).collect();
        let links = self.links_store().list_all_links().unwrap_or_default();
        let crosslinks = self
            .crosslinks_store()
            .list_all_crosslinks()
            .unwrap_or_default();
        let _ = self
            .graph_index()
            .build_and_save(&records, &links, &crosslinks);

        Ok(count)
    }

    pub fn kg(&self) -> kg::KnowledgeGraph {
        kg::KnowledgeGraph::new(self.config.root.clone())
    }

    pub fn links_store(&self) -> links::LinksStore {
        links::LinksStore::new(self.config.root.clone())
    }

    pub fn crosslinks_store(&self) -> crosslinks::CrossLinksStore {
        crosslinks::CrossLinksStore::new(self.config.root.clone())
    }

    pub fn graph_index(&self) -> graph::GraphIndex {
        graph::GraphIndex::new(self.config.root.clone())
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
        self.kg().add_triple(
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
        self.kg().invalidate(subject, predicate, object, ended)
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
        self.kg().supersede(
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
    ) -> Result<kg::EntityQueryResult, HarnessError> {
        self.kg().query_entity(entity, as_of, direction)
    }

    pub fn kg_query_relationship(
        &self,
        predicate: &str,
        as_of: Option<&str>,
    ) -> Result<Vec<kg::Triple>, HarnessError> {
        self.kg().query_relationship(predicate, as_of)
    }

    pub fn kg_timeline(
        &self,
        entity: Option<&str>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<kg::Triple>, HarnessError> {
        self.kg().timeline(entity, limit, offset)
    }

    pub fn kg_stats(&self) -> Result<kg::KnowledgeGraphStats, HarnessError> {
        self.kg().stats()
    }

    pub fn traverse(
        &self,
        start_node: &str,
        max_hops: usize,
    ) -> Result<Vec<graph::TraversalStep>, HarnessError> {
        // Ensure graph index is current
        let records = self.active_records().unwrap_or_default();
        let links = self.links_store().list_all_links().unwrap_or_default();
        let crosslinks = self
            .crosslinks_store()
            .list_all_crosslinks()
            .unwrap_or_default();
        let _ = self
            .graph_index()
            .build_and_save(&records, &links, &crosslinks);
        self.graph_index().traverse(start_node, max_hops)
    }

    pub fn find_cross_links(
        &self,
        domain_a: Option<&str>,
        domain_b: Option<&str>,
    ) -> Result<Vec<graph::CrossLinkBridge>, HarnessError> {
        self.graph_index().find_cross_links(domain_a, domain_b)
    }

    pub fn compute_links(&self, domain: &str) -> Result<Vec<links::DomainLink>, HarnessError> {
        let records = self.active_records()?;
        self.links_store()
            .compute_links_for_domain(domain, &records)
    }

    pub fn list_links(&self, domain: Option<&str>) -> Result<Vec<links::DomainLink>, HarnessError> {
        match domain {
            Some(d) => self.links_store().get_links_for_domain(d),
            None => self.links_store().list_all_links(),
        }
    }

    pub fn prune_links(
        &self,
        domain: Option<&str>,
        min_count: usize,
    ) -> Result<usize, HarnessError> {
        self.links_store().prune_links(domain, min_count)
    }

    pub fn crosslinks_propose(
        &self,
        max_proposals: usize,
    ) -> Result<Vec<crosslinks::CrossLink>, HarnessError> {
        let records = self.active_records()?;
        self.crosslinks_store()
            .propose_crosslinks(&records, max_proposals)
    }

    pub fn crosslinks_apply(&self) -> Result<usize, HarnessError> {
        let count = self.crosslinks_store().apply_crosslinks()?;
        if count > 0 {
            let records = self.active_records()?;
            let links = self.links_store().list_all_links().unwrap_or_default();
            let crosslinks = self
                .crosslinks_store()
                .list_all_crosslinks()
                .unwrap_or_default();
            let _ = self
                .graph_index()
                .build_and_save(&records, &links, &crosslinks);
        }
        Ok(count)
    }

    pub fn crosslinks_prune(&self) -> Result<usize, HarnessError> {
        let records = self.active_records()?;
        let endpoints: Vec<String> = records
            .into_iter()
            .map(|r| format!("{}/{}", ids::slugify(&r.domain), ids::slugify(&r.topic)))
            .collect();
        let count = self.crosslinks_store().prune_crosslinks(Some(&endpoints))?;
        if count > 0 {
            let active = self.active_records()?;
            let links = self.links_store().list_all_links().unwrap_or_default();
            let crosslinks = self
                .crosslinks_store()
                .list_all_crosslinks()
                .unwrap_or_default();
            let _ = self
                .graph_index()
                .build_and_save(&active, &links, &crosslinks);
        }
        Ok(count)
    }

    pub fn list_crosslinks(&self) -> Result<Vec<crosslinks::CrossLink>, HarnessError> {
        self.crosslinks_store().list_all_crosslinks()
    }

    fn write_manifest(&self, count: usize) {
        let manifest = serde_json::json!({
            "schema": MANIFEST_SCHEMA,
            "embedder": { "name": self.config.embedder, "dim": embed::DIM },
            "record_count": count,
            "built_at": Utc::now().to_rfc3339(),
        });
        let path = self.config.root.join("search_index").join("manifest.json");
        let _ = fs::create_dir_all(path.parent().unwrap_or(&self.config.root));
        let _ = atomic::write_str(
            &path,
            &serde_json::to_string_pretty(&manifest).unwrap_or_default(),
        );
    }

    /// Mine a directory tree into records. Returns a report of what changed.
    pub fn mine(&self, dir: &Path, domain: Option<&str>) -> Result<MineReport, HarnessError> {
        let report = miner::mine(self, dir, domain)?;
        self.reindex()?;
        Ok(report)
    }

    /// Mine a SQLite `sessions.db` file into the `conversations` domain.
    pub fn mine_sessions(
        &self,
        sessions_db_path: &Path,
        domain: Option<&str>,
        topic: Option<&str>,
    ) -> Result<MineReport, HarnessError> {
        sessions::mine_sessions(self, sessions_db_path, domain, topic)
    }

    /// Mine chat export transcripts (JSONL, JSON, TXT, MD) into the `conversations` domain.
    pub fn mine_convos(
        &self,
        path: &Path,
        domain: Option<&str>,
        topic: Option<&str>,
    ) -> Result<MineReport, HarnessError> {
        convo::mine_convos(self, path, domain, topic)
    }

    /// Get the daily journal store.
    pub fn journal(&self) -> JournalStore {
        JournalStore::new(self.config.root.clone())
    }

    /// Append a daily journal entry for an agent.
    pub fn journal_write(
        &self,
        agent: &str,
        content: &str,
        timestamp: Option<DateTime<Utc>>,
    ) -> Result<JournalEntry, HarnessError> {
        self.journal().write_entry(agent, content, timestamp)
    }

    /// Read journal entries for an agent.
    pub fn journal_read(
        &self,
        agent: &str,
        date: Option<&str>,
        limit: Option<usize>,
    ) -> Result<Vec<JournalEntry>, HarnessError> {
        self.journal().read_entries(agent, date, limit)
    }

    /// Get the coordination events and artifacts store.
    pub fn events(&self) -> EventsStore {
        EventsStore::new(self.config.root.clone())
    }

    /// Append a coordination event to a stream.
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
    ) -> Result<Event, HarnessError> {
        self.events()
            .append_event(stream, event_type, from, to, body, correlation_id, status)
    }

    /// List events from a stream.
    pub fn event_list(
        &self,
        stream: &str,
        since: Option<DateTime<Utc>>,
        limit: Option<usize>,
        unacked_by: Option<&str>,
    ) -> Result<Vec<Event>, HarnessError> {
        self.events().list_events(stream, since, limit, unacked_by)
    }

    /// Mark an event as acknowledged by an agent.
    pub fn event_ack(&self, stream: &str, agent: &str, event_id: &str) -> Result<(), HarnessError> {
        self.events().ack_event(stream, agent, event_id)
    }

    /// Wait for an event for an agent on a stream.
    pub async fn event_wait(
        &self,
        stream: &str,
        agent: &str,
        timeout_ms: u64,
    ) -> Result<Option<Event>, HarnessError> {
        self.events()
            .wait_for_event(stream, agent, timeout_ms)
            .await
    }

    /// Put an artifact byte payload with metadata.
    pub fn artifact_put(
        &self,
        id_opt: Option<&str>,
        payload: &[u8],
        extra_meta: HashMap<String, String>,
    ) -> Result<String, HarnessError> {
        self.events().put_artifact(id_opt, payload, extra_meta)
    }

    /// Get an artifact byte payload and metadata.
    pub fn artifact_get(&self, id: &str) -> Result<Option<(Vec<u8>, ArtifactMeta)>, HarnessError> {
        self.events().get_artifact(id)
    }

    /// Write a clean Markdown copy of the canonical tree.
    pub fn export(&self, out: &Path) -> Result<usize, HarnessError> {
        let mut copied = 0;
        for path in self.all_record_paths()? {
            let relative = path
                .strip_prefix(self.config.root.join("domains"))
                .unwrap_or(&path);
            let target = out.join("domains").join(relative);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(io_err)?;
            }
            fs::copy(&path, &target).map_err(io_err)?;
            copied += 1;
        }
        for file in ["identity.md", "store.md"] {
            let source = self.config.root.join(file);
            if source.exists() {
                fs::copy(&source, out.join(file)).map_err(io_err)?;
            }
        }
        Ok(copied)
    }

    /// Counts by domain and topic for `luna memory status`.
    pub fn status(&self) -> Result<StatusReport, HarnessError> {
        let records = self.active_records()?;
        let mut by_domain: BTreeMap<String, BTreeMap<String, usize>> = BTreeMap::new();
        for record in &records {
            *by_domain
                .entry(record.domain.clone())
                .or_default()
                .entry(record.topic.clone())
                .or_insert(0) += 1;
        }
        Ok(StatusReport {
            root: self.config.root.clone(),
            embedder: self.config.embedder.clone(),
            records: records.len(),
            by_domain,
        })
    }

    /// Import rows from a legacy `memory.db` if it exists.
    pub fn migrate_from_sqlite(&self, db_path: &Path) -> Result<usize, HarnessError> {
        if !db_path.exists() {
            return Err(HarnessError::MemoryError(format!(
                "No legacy memory database at {}",
                db_path.display()
            )));
        }
        let conn = rusqlite::Connection::open(db_path)
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        let mut statement = conn
            .prepare(
                "SELECT id, subject, predicate, object, raw_content, category, confidence, created_at, updated_at, is_active, superseded_by, name, path FROM memories",
            )
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        let rows = statement
            .query_map([], |row| {
                let category: String = row.get(5)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    category,
                    row.get::<_, f32>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)? != 0,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, Option<String>>(11)?,
                    row.get::<_, Option<String>>(12)?,
                ))
            })
            .map_err(|e| HarnessError::MemoryError(e.to_string()))?;
        let mut imported = 0;
        for row in rows.flatten() {
            let category = match row.5.as_str() {
                "user_profile" => MemoryCategory::UserProfile,
                "code_pattern" => MemoryCategory::CodePattern,
                "workflow_rule" => MemoryCategory::WorkflowRule,
                "note_pointer" => MemoryCategory::NotePointer,
                _ => MemoryCategory::Fact,
            };
            let created_at = parse_time(Some(row.7)).unwrap_or_else(Utc::now);
            let updated_at = parse_time(Some(row.8)).unwrap_or(created_at);
            let memory = MemoryRecord {
                id: row.0,
                subject: row.1,
                predicate: row.2,
                object: row.3,
                name: row.11,
                path: row.12,
                raw_content: row.4,
                category,
                confidence: row.6,
                created_at,
                updated_at,
                is_active: row.9,
                superseded_by: row.10,
            };
            self.write_record(Record::from_memory(&memory))?;
            imported += 1;
        }
        self.reindex()?;
        Ok(imported)
    }
}

#[derive(Debug, Default)]
pub struct AuditReport {
    pub records: usize,
    pub inactive: usize,
    pub invalid: Vec<String>,
    pub unreadable: Vec<String>,
    pub empty_bodies: Vec<String>,
    pub dangling_supersessions: Vec<String>,
    pub duplicate_active: Vec<String>,
}

#[derive(Debug)]
pub struct StatusReport {
    pub root: PathBuf,
    pub embedder: String,
    pub records: usize,
    pub by_domain: BTreeMap<String, BTreeMap<String, usize>>,
}

fn search_text(record: &Record) -> String {
    format!(
        "{} {} {} {} {} {}",
        record.subject,
        record.predicate,
        record.object,
        record.name.as_deref().unwrap_or(""),
        record.path.as_deref().unwrap_or(""),
        record.body
    )
}

fn excerpt(body: &str, query: &str) -> String {
    let query_terms = search::tokenize(query);
    let lower = body.to_lowercase();
    let anchor = query_terms
        .iter()
        .filter_map(|term| lower.find(term))
        .min()
        .unwrap_or(0);
    let start = body[..anchor.min(body.len())]
        .char_indices()
        .rev()
        .nth(80)
        .map(|(index, _)| index)
        .unwrap_or(0);
    let end = body[start..]
        .char_indices()
        .nth(320)
        .map(|(index, _)| start + index)
        .unwrap_or(body.len());
    let mut text = body[start..end].trim().to_string();
    if start > 0 {
        text.insert(0, '…');
    }
    if end < body.len() {
        text.push('…');
    }
    text
}

fn collect_record_paths(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), HarnessError> {
    collect_files_with_parent(dir, "records", out)
}

fn collect_files_with_parent(
    dir: &Path,
    parent_name: &str,
    out: &mut Vec<PathBuf>,
) -> Result<(), HarnessError> {
    let entries = fs::read_dir(dir).map_err(io_err)?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_files_with_parent(&path, parent_name, out)?;
        } else if path.extension().and_then(|e| e.to_str()) == Some("md")
            && path
                .parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                == Some(parent_name)
        {
            out.push(path);
        }
    }
    Ok(())
}

fn io_err(error: std::io::Error) -> HarnessError {
    HarnessError::MemoryError(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_store() -> MemoryStore {
        let root = std::env::temp_dir().join(format!(
            "luna-store-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or(0)
        ));
        MemoryStore::open(StoreConfig::new(root)).unwrap()
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

    #[test]
    fn record_round_trips_through_markdown() {
        let store = temp_store();
        let record = Record::from_memory(&memory("user.profile", "Rust engineer"));
        store.write_record(record.clone()).unwrap();
        let loaded = store.get_record("user.profile").unwrap().unwrap();
        assert_eq!(loaded.subject, "user");
        assert_eq!(loaded.object, "Rust engineer");
        assert_eq!(loaded.body, "Rust engineer");
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn search_returns_the_matching_record() {
        let store = temp_store();
        store
            .write_record(Record::from_memory(&memory(
                "user.profile",
                "Rust engineer building Luna",
            )))
            .unwrap();
        store
            .write_record(Record::from_memory(&memory(
                "env.database",
                "Postgres on port 5432",
            )))
            .unwrap();
        let results = store
            .search("Rust engineer", &SearchFilter::default(), 5)
            .unwrap();
        assert_eq!(
            results.first().map(|r| r.record.id.as_str()),
            Some("user.profile")
        );
        let _ = fs::remove_dir_all(store.root());
    }

    #[test]
    fn audit_flags_duplicate_active_records() {
        let store = temp_store();
        let mut first = Record::from_memory(&memory("user.profile", "one"));
        first.id = "a".to_string();
        let mut second = Record::from_memory(&memory("user.profile", "two"));
        second.id = "b".to_string();
        store.write_record(first).unwrap();
        store.write_record(second).unwrap();
        let report = store.audit().unwrap();
        assert_eq!(report.duplicate_active.len(), 1);
        let _ = fs::remove_dir_all(store.root());
    }
}
