use std::fs;
use std::path::PathBuf;

use chrono::Utc;
use harness_core::{MemoryCategory, MemoryRecord};
use harness_memory::{
    CrossLink, Entity, EntityAttribute, KnowledgeGraph, MarkdownMemoryEngine, MemoryStore, Record,
    StoreConfig, Triple,
};

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luna-phase3-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn test_phase3_entity_and_triple_file_formats() {
    let root = temp_root("formats");
    let kg = KnowledgeGraph::new(root.clone());
    kg.ensure_dirs().unwrap();

    // 1. Test Entity serialization and parsing
    let mut entity = Entity::new("SQLite", Some("technology"));
    entity.aliases = vec!["sqlite3".to_string(), "sqlite-vec".to_string()];
    entity.attributes.push(EntityAttribute {
        key: "language".to_string(),
        value: "C".to_string(),
        from: Some("2000-08-17".to_string()),
        to: None,
    });
    entity.attributes.push(EntityAttribute {
        key: "used_by".to_string(),
        value: "Luna".to_string(),
        from: Some("2026-01-01".to_string()),
        to: None,
    });
    entity.notes = "Single-file embedded database.".to_string();

    let doc = entity.to_document();
    assert!(doc.contains("entity: SQLite"));
    assert!(doc.contains("type: technology"));
    assert!(doc.contains("aliases: [\"sqlite3\",\"sqlite-vec\"]"));
    assert!(doc.contains("## Attributes"));
    assert!(doc.contains("- key: language | value: C | from: 2000-08-17 | to: null"));
    assert!(doc.contains("- key: used_by | value: Luna | from: 2026-01-01 | to: null"));
    assert!(doc.contains("## Notes\nSingle-file embedded database."));

    let parsed = Entity::from_document("sqlite", &doc).unwrap();
    assert_eq!(parsed.name, "SQLite");
    assert_eq!(parsed.entity_type, "technology");
    assert_eq!(parsed.aliases, vec!["sqlite3", "sqlite-vec"]);
    assert_eq!(parsed.attributes.len(), 2);
    assert_eq!(parsed.attributes[0].key, "language");
    assert_eq!(parsed.attributes[0].value, "C");
    assert_eq!(parsed.attributes[0].from.as_deref(), Some("2000-08-17"));
    assert_eq!(parsed.notes, "Single-file embedded database.");

    // 2. Test Triple serialization and parsing
    let triple = Triple {
        id: "t_luna_uses_sqlite_1234567890ab".to_string(),
        subject: "Luna".to_string(),
        predicate: "uses".to_string(),
        object: "SQLite".to_string(),
        valid_from: Some("2026-01-01".to_string()),
        valid_to: None,
        confidence: 1.0,
        source_record: Some("rec_9f2c".to_string()),
        source_file: Some("crates/harness-memory/src/engine.rs".to_string()),
        source_record_id: None,
        adapter_name: None,
        extracted_at: Utc::now(),
    };

    let line = triple.render_line();
    assert!(line.starts_with("- id: t_luna_uses_sqlite_1234567890ab | p: uses | o: SQLite"));
    assert!(line.contains("from: 2026-01-01 | to: null | conf: 1.0000 | src: rec_9f2c | file: crates/harness-memory/src/engine.rs"));

    let parsed_t = Triple::parse_line("Luna", &line).unwrap();
    assert_eq!(parsed_t.id, triple.id);
    assert_eq!(parsed_t.subject, "Luna");
    assert_eq!(parsed_t.predicate, "uses");
    assert_eq!(parsed_t.object, "SQLite");
    assert_eq!(parsed_t.valid_from.as_deref(), Some("2026-01-01"));
    assert!(parsed_t.valid_to.is_none());
    assert_eq!(parsed_t.source_record.as_deref(), Some("rec_9f2c"));
    assert_eq!(
        parsed_t.source_file.as_deref(),
        Some("crates/harness-memory/src/engine.rs")
    );
}

#[tokio::test]
async fn test_phase3_kg_crud_and_deduplication() {
    let root = temp_root("kg_crud");
    let store = MemoryStore::open(StoreConfig::new(root)).unwrap();

    // 1. add_triple auto-creates entities
    let tid1 = store
        .kg_add_triple(
            "Alice", "knows", "Bob", None, None, None, None, None, None, None,
        )
        .unwrap();
    assert!(tid1.starts_with("t_alice_knows_bob_"));
    assert!(store.kg().get_entity("Alice").unwrap().is_some());
    assert!(store.kg().get_entity("Bob").unwrap().is_some());

    // 2. Duplicate open triple returns existing ID (deduplication)
    let tid2 = store
        .kg_add_triple(
            "Alice", "knows", "Bob", None, None, None, None, None, None, None,
        )
        .unwrap();
    assert_eq!(tid1, tid2);

    // 3. Reject inverted interval (valid_to < valid_from)
    let inv = store.kg_add_triple(
        "Alice",
        "works_at",
        "Acme",
        Some("2026-05-01"),
        Some("2026-04-01"),
        None,
        None,
        None,
        None,
        None,
    );
    assert!(inv.is_err(), "Must reject inverted temporal interval");
    let err_msg = inv.unwrap_err().to_string();
    assert!(err_msg.contains("before valid_from") && err_msg.contains("inverted interval"));

    // 4. Closed assertion replay deduplication
    let tid_closed1 = store
        .kg_add_triple(
            "Alice",
            "consulted_for",
            "Globex",
            Some("2024-01-01"),
            Some("2024-12-31"),
            Some(0.9),
            Some("rec_hist1"),
            None,
            None,
            None,
        )
        .unwrap();
    let tid_closed2 = store
        .kg_add_triple(
            "Alice",
            "consulted_for",
            "Globex",
            Some("2024-01-01"),
            Some("2024-12-31"),
            Some(0.9),
            Some("rec_hist1"),
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(tid_closed1, tid_closed2);

    // 5. Invalidation closes fact and allows re-add with new ID
    let tid_job = store
        .kg_add_triple(
            "Alice",
            "works_at",
            "Acme",
            Some("2025-01-01"),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    let invalidated = store
        .kg_invalidate("Alice", "works_at", "Acme", Some("2025-12-31"))
        .unwrap();
    assert_eq!(invalidated, 1);

    let tid_job2 = store
        .kg_add_triple(
            "Alice",
            "works_at",
            "Acme",
            Some("2026-01-01"),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    assert_ne!(tid_job, tid_job2);
}

#[tokio::test]
async fn test_phase3_atomic_supersede_and_point_in_time_query() {
    let root = temp_root("supersede_as_of");
    let engine = MarkdownMemoryEngine::from_root(root).unwrap();

    // Initial state: Bot uses model A starting from May 1, 2026
    let old_tid = engine
        .kg_add_triple(
            "Bot",
            "uses_model",
            "Model-A",
            Some("2026-05-01T00:00:00Z"),
            None,
            Some(1.0),
            None,
            None,
            None,
            None,
        )
        .unwrap();

    // Atomic supersede at boundary: June 1, 2026 12:00:00Z
    let boundary = "2026-06-01T12:00:00Z";
    let new_tid = engine
        .kg_supersede(
            "Bot",
            "uses_model",
            "Model-A",
            "Model-B",
            Some(boundary),
            Some(1.0),
            None,
            None,
            None,
            None,
        )
        .unwrap();
    assert_ne!(old_tid, new_tid);

    // 1. EXIT CRITERION TEST: `kg query --as-of` before boundary returns Model-A
    let before_res = engine
        .kg_query("Bot", Some("2026-05-15T00:00:00Z"), Some("outgoing"))
        .unwrap();
    assert_eq!(before_res.facts.len(), 1);
    assert_eq!(before_res.facts[0].object, "Model-A");

    // 2. Query exactly at boundary instant returns ONLY the successor (Model-B),
    // because upper bound is strict (valid_to > as_of, interval is [valid_from, valid_to)).
    let at_res = engine
        .kg_query("Bot", Some(boundary), Some("outgoing"))
        .unwrap();
    assert_eq!(at_res.facts.len(), 1);
    assert_eq!(at_res.facts[0].object, "Model-B");

    // 3. Query after boundary returns Model-B
    let after_res = engine
        .kg_query("Bot", Some("2026-07-01T00:00:00Z"), Some("outgoing"))
        .unwrap();
    assert_eq!(after_res.facts.len(), 1);
    assert_eq!(after_res.facts[0].object, "Model-B");

    // 4. Query with no as_of returns both, categorized into active and historical
    let unconstrained = engine.kg_query("Bot", None, Some("outgoing")).unwrap();
    assert_eq!(unconstrained.facts.len(), 2);
    assert_eq!(unconstrained.active_facts.len(), 1);
    assert_eq!(unconstrained.active_facts[0].object, "Model-B");
    assert_eq!(unconstrained.historical_facts.len(), 1);
    assert_eq!(unconstrained.historical_facts[0].object, "Model-A");
}

#[tokio::test]
async fn test_phase3_entity_candidate_lookup_and_disambiguation() {
    let root = temp_root("candidates");
    let store = MemoryStore::open(StoreConfig::new(root)).unwrap();

    store
        .kg_add_triple(
            "Alice Smith",
            "works_at",
            "Acme",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    store
        .kg_add_triple(
            "Albert Einstein",
            "won",
            "Nobel",
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

    // Query for "Alice" when only "Alice Smith" exists: disambiguates and resolves
    let res = store.kg_query("Alice", None, Some("outgoing")).unwrap();
    assert_eq!(res.resolved_from.as_deref(), Some("Alice"));
    assert_eq!(res.entity, "Alice Smith");
    assert_eq!(res.facts.len(), 1);
    assert_eq!(res.facts[0].object, "Acme");

    // Candidate prefix search
    let candidates = store.kg().find_entity_candidates("Alice").unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].name, "Alice Smith");
}

#[tokio::test]
async fn test_phase3_timeline_and_stats() {
    let root = temp_root("timeline_stats");
    let store = MemoryStore::open(StoreConfig::new(root)).unwrap();

    store
        .kg_add_triple(
            "Alice",
            "born_in",
            "1990",
            Some("1990-01-01"),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    store
        .kg_add_triple(
            "Alice",
            "started_school",
            "Primary",
            Some("1996-09-01"),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
    store
        .kg_add_triple(
            "Alice",
            "graduated",
            "College",
            Some("2012-06-01"),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();

    let timeline = store.kg_timeline(Some("Alice"), 2, 0).unwrap();
    assert_eq!(timeline.len(), 2);
    assert_eq!(timeline[0].predicate, "born_in");
    assert_eq!(timeline[1].predicate, "started_school");

    let paged = store.kg_timeline(Some("Alice"), 2, 2).unwrap();
    assert_eq!(paged.len(), 1);
    assert_eq!(paged[0].predicate, "graduated");

    let stats = store.kg_stats().unwrap();
    assert!(stats.entities >= 4);
    assert_eq!(stats.triples, 3);
    assert_eq!(stats.current_facts, 3);
    assert_eq!(stats.expired_facts, 0);
    assert_eq!(
        stats.relationship_types,
        vec!["born_in", "graduated", "started_school"]
    );
}

#[tokio::test]
async fn test_phase3_within_domain_links() {
    let root = temp_root("links");
    let engine = MarkdownMemoryEngine::from_root(root.clone()).unwrap();

    // Create 3 active records in domain "luna", sharing entities
    for i in 0..3 {
        let mut rec = Record::from_memory(&MemoryRecord {
            id: format!("luna.record_{i}"),
            subject: "retrieval".to_string(),
            predicate: "core".to_string(),
            object: "Fast hybrid retrieval with Tantivy and fastembed".to_string(),
            name: Some(format!("record_{i}")),
            path: Some("crates/harness-memory/src/search.rs".to_string()),
            raw_content: "Hybrid search engine implementation.".to_string(),
            category: MemoryCategory::Fact,
            confidence: 1.0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            is_active: true,
            superseded_by: None,
        });
        rec.entities = vec![
            "tantivy".to_string(),
            "fastembed".to_string(),
            "sqlite".to_string(),
        ];
        engine.write_record(rec).unwrap();
    }

    let links = engine.store().compute_links("luna").unwrap();
    assert!(!links.is_empty());

    // Tantivy and fastembed co-occur 3 times
    let tf_link = links
        .iter()
        .find(|l| {
            (l.entity_a == "fastembed" && l.entity_b == "tantivy")
                || (l.entity_a == "tantivy" && l.entity_b == "fastembed")
        })
        .unwrap();
    assert_eq!(tf_link.count, 3);
    assert_eq!(tf_link.records.len(), 3);

    // Verify canonical links file was saved: links/luna.links.md
    let links_file = root.join("links").join("luna.links.md");
    assert!(links_file.is_file());
    let content = fs::read_to_string(&links_file).unwrap();
    assert!(content.contains("domain: luna"));
    assert!(content.contains("- a:"));
    assert!(content.contains("count: 3"));

    // Test pruning links with count > 5 (should prune all count=3 links)
    let pruned = engine.store().prune_links(Some("luna"), 5).unwrap();
    assert!(pruned > 0);
    let remaining = engine.store().list_links(Some("luna")).unwrap();
    assert!(remaining.is_empty());
}

#[tokio::test]
async fn test_phase3_crosslinks_proposal_apply_and_pruning() {
    let root = temp_root("crosslinks");
    let engine = MarkdownMemoryEngine::from_root(root.clone()).unwrap();

    // Create records spanning two domains: "luna/memory" and "luna/storage", both sharing "sqlite"
    for i in 0..4 {
        let mut rec1 = Record::from_memory(&MemoryRecord {
            id: format!("luna.mem_{i}"),
            subject: "sqlite".to_string(),
            predicate: "inproc".to_string(),
            object: "Embedded database".to_string(),
            name: Some(format!("mem_{i}")),
            path: None,
            raw_content: "Embedded database".to_string(),
            category: MemoryCategory::Fact,
            confidence: 1.0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            is_active: true,
            superseded_by: None,
        });
        rec1.domain = "luna".to_string();
        rec1.topic = "memory".to_string();
        rec1.entities = vec!["sqlite".to_string()];
        engine.write_record(rec1).unwrap();
    }

    for i in 0..2 {
        let mut rec2 = Record::from_memory(&MemoryRecord {
            id: format!("luna.stor_{i}"),
            subject: "sqlite".to_string(),
            predicate: "disk".to_string(),
            object: "Disk persistence".to_string(),
            name: Some(format!("stor_{i}")),
            path: None,
            raw_content: "Disk persistence".to_string(),
            category: MemoryCategory::Fact,
            confidence: 1.0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            is_active: true,
            superseded_by: None,
        });
        rec2.domain = "luna".to_string();
        rec2.topic = "storage".to_string();
        rec2.entities = vec!["sqlite".to_string()];
        engine.write_record(rec2).unwrap();
    }

    // 1. Propose crosslinks: Weaker side rule!
    // count in memory is 4, count in storage is 2 -> strength is min(4, 2) = 2.
    let proposals = engine.store().crosslinks_propose(10).unwrap();
    assert_eq!(proposals.len(), 1);
    assert_eq!(proposals[0].entity, "sqlite");
    assert_eq!(proposals[0].strength, 2);
    assert_eq!(proposals[0].status, "proposed");

    // Verify crosslinks/proposal.md exists
    let proposal_md = root.join("crosslinks").join("proposal.md");
    assert!(proposal_md.is_file());

    // 2. Apply proposal: materializes into crosslinks/sqlite.crosslinks.md
    let applied = engine.store().crosslinks_apply().unwrap();
    assert_eq!(applied, 1);
    assert!(
        !proposal_md.is_file(),
        "Proposal file is cleaned up after apply"
    );

    let crosslink_file = root.join("crosslinks").join("sqlite.crosslinks.md");
    assert!(crosslink_file.is_file());
    let cl_text = fs::read_to_string(&crosslink_file).unwrap();
    assert!(cl_text.contains("entity: sqlite"));
    assert!(cl_text.contains("strength: 2 | status: approved"));

    let loaded = engine.store().list_crosslinks().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].entity, "sqlite");
    assert_eq!(loaded[0].strength, 2);

    // 3. Prune crosslinks: invalidating/removing endpoints prunes dangling links
    let pruned = engine.store().crosslinks_prune().unwrap();
    assert_eq!(pruned, 0, "No dangling links to prune when endpoints exist");
}

#[tokio::test]
async fn test_phase3_exit_criterion_traverse_and_cross_links() {
    let root = temp_root("traverse");
    let engine = MarkdownMemoryEngine::from_root(root.clone()).unwrap();

    // Set up a multi-domain graph topology:
    // Topic A: "luna/memory" with entity "sqlite"
    // Topic B: "luna/storage" with entity "sqlite" and "nvme"
    // Topic C: "luna/hardware" with entity "nvme"
    let mut rec1 = Record::from_memory(&MemoryRecord {
        id: "rec_1".to_string(),
        subject: "memory_engine".to_string(),
        predicate: "uses".to_string(),
        object: "sqlite".to_string(),
        name: None,
        path: None,
        raw_content: "desc".to_string(),
        category: MemoryCategory::Fact,
        confidence: 1.0,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        is_active: true,
        superseded_by: None,
    });
    rec1.domain = "luna".to_string();
    rec1.topic = "memory".to_string();
    rec1.entities = vec!["sqlite".to_string()];
    engine.write_record(rec1).unwrap();

    let mut rec2 = Record::from_memory(&MemoryRecord {
        id: "rec_2".to_string(),
        subject: "storage_engine".to_string(),
        predicate: "uses".to_string(),
        object: "sqlite".to_string(),
        name: None,
        path: None,
        raw_content: "desc".to_string(),
        category: MemoryCategory::Fact,
        confidence: 1.0,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        is_active: true,
        superseded_by: None,
    });
    rec2.domain = "luna".to_string();
    rec2.topic = "storage".to_string();
    rec2.entities = vec!["sqlite".to_string(), "nvme".to_string()];
    engine.write_record(rec2).unwrap();

    let mut rec3 = Record::from_memory(&MemoryRecord {
        id: "rec_3".to_string(),
        subject: "hardware_specs".to_string(),
        predicate: "has".to_string(),
        object: "nvme".to_string(),
        name: None,
        path: None,
        raw_content: "desc".to_string(),
        category: MemoryCategory::Fact,
        confidence: 1.0,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        is_active: true,
        superseded_by: None,
    });
    rec3.domain = "luna".to_string();
    rec3.topic = "hardware".to_string();
    rec3.entities = vec!["nvme".to_string()];
    engine.write_record(rec3).unwrap();

    // Create crosslink cross-link for sqlite between luna/memory and luna/storage
    engine
        .store()
        .crosslinks_store()
        .save_crosslinks_for_entity(
            "sqlite",
            &[CrossLink::new(
                "sqlite",
                "luna/memory",
                "luna/storage",
                Some("sqlite"),
                5,
                "approved",
            )],
        )
        .unwrap();

    // EXIT CRITERION: `traverse` works!
    // Start traversal at "luna/memory", discover "sqlite" and "luna/storage" at hop 1, and "nvme" at hop 2
    let steps = engine.traverse("luna/memory", 2).unwrap();
    assert!(!steps.is_empty());
    assert_eq!(steps[0].node, "luna/memory");
    assert_eq!(steps[0].hop, 0);

    let hop1_nodes: Vec<&str> = steps
        .iter()
        .filter(|s| s.hop == 1)
        .map(|s| s.node.as_str())
        .collect();
    assert!(hop1_nodes.contains(&"sqlite") || hop1_nodes.contains(&"luna/storage"));

    // Find cross_links between domain "luna" and other nodes
    let cross_links = engine.find_cross_links(Some("luna"), None).unwrap();
    assert!(!cross_links.is_empty());
    assert!(cross_links.iter().any(|t| t.bridge_entity == "sqlite"));

    // Rebuild index and verify search_index/graph.json and search_index/kg.jsonl exist
    engine.store().reindex().unwrap();
    assert!(root.join("search_index").join("graph.json").is_file());
    assert!(root.join("search_index").join("kg.jsonl").is_file());
}
