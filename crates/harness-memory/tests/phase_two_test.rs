use std::fs;
use std::path::PathBuf;

use chrono::Utc;
use harness_core::{MemoryCategory, MemoryEngine, MemoryRecord};
use harness_memory::store::miner::TextChunk;
use harness_memory::{
    CompactSummary, ContentIndex, MarkdownMemoryEngine, MemoryStore, PointerLine, SearchFilter,
    StoreConfig, SUMMARY_SPEC,
};

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luna-phase2-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[tokio::test]
async fn test_phase2_content_indexes_and_parsing() {
    let root = temp_root("indexes");
    let store = MemoryStore::open(StoreConfig::new(root.clone())).unwrap();

    let index = ContentIndex {
        source_file: "crates/harness-memory/src/engine.rs".to_string(),
        records: vec![
            "rec_9f2c1a7b3d4e5f60718293a4".to_string(),
            "rec_a1b2c3d4e5f60718293a4b5c6".to_string(),
        ],
        built_at: Utc::now(),
        chunk_indexes: vec![3, 4],
        lines: vec![
            PointerLine::new(
                "retrieval is LIKE-based",
                vec![
                    "engine.rs".to_string(),
                    "retrieve".to_string(),
                    "sqlite".to_string(),
                ],
                Some("2026-11-08:L363-476".to_string()),
                vec!["rec_9f2c1a7b3d4e5f60718293a4".to_string()],
            ),
            PointerLine::new(
                "recent-memory fallback",
                vec!["fallback".to_string(), "recency".to_string()],
                Some("2026-11-08:L477-520".to_string()),
                vec!["rec_a1b2c3d4e5f60718293a4b5c6".to_string()],
            ),
        ],
    };

    let rendered = index.render(1500);
    assert!(rendered.contains("source_file: crates/harness-memory/src/engine.rs"));
    assert!(rendered.contains("retrieval is LIKE-based|engine.rs;retrieve;sqlite|2026-11-08:L363-476|→rec_9f2c1a7b3d4e5f60718293a4"));

    let parsed = ContentIndex::parse(&rendered).unwrap();
    assert_eq!(parsed.source_file, index.source_file);
    assert_eq!(parsed.records.len(), 2);
    assert_eq!(parsed.lines.len(), 2);
    assert_eq!(parsed.lines[0].topic, "retrieval is LIKE-based");
    assert_eq!(parsed.lines[1].entities, vec!["fallback", "recency"]);

    // Write to store index dir and verify loading
    let index_dir = root
        .join("domains")
        .join("luna")
        .join("topics")
        .join("memory")
        .join("indexes");
    fs::create_dir_all(&index_dir).unwrap();
    fs::write(index_dir.join("engine.index.md"), &rendered).unwrap();

    let loaded = store.content_indexes().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].source_file, "crates/harness-memory/src/engine.rs");
}

#[tokio::test]
async fn test_phase2_compact_summaries_and_spec() {
    let root = temp_root("summaries");
    let store = MemoryStore::open(StoreConfig::new(root.clone())).unwrap();

    // Verify SUMMARY.md was created in store root
    let summary_md = root.join("SUMMARY.md");
    assert!(summary_md.exists());
    let spec = fs::read_to_string(&summary_md).unwrap();
    assert_eq!(spec, SUMMARY_SPEC);
    assert!(spec.contains("FILE_NUM|DOMAIN/TOPIC|DATE|TITLE"));

    let chunks = vec![
        TextChunk {
            content: "We decided to switch architecture to fastembed and SQLite database engine for TECHNICAL reasons.".to_string(),
            chunk_index: 0,
            line_start: 1,
            line_end: 10,
        },
        TextChunk {
            content: "Our core belief is that SQLite database engine provides rock-solid storage.".to_string(),
            chunk_index: 1,
            line_start: 11,
            line_end: 20,
        },
    ];

    let summary = harness_memory::store::summary::generate_summary(
        "luna",
        "memory",
        "crates/harness-memory/src/engine.rs",
        7,
        "Memory Engine Implementation\nDetails follow.",
        &chunks,
    );

    let rendered = summary.render();
    assert!(rendered.contains("FILE 7|luna/memory|"));
    assert!(rendered.contains("S1:"));
    assert!(rendered.contains("S2:"));
    assert!(rendered.contains("ARC:"));

    let parsed = CompactSummary::parse(&rendered).unwrap();
    assert_eq!(parsed.source_file, "crates/harness-memory/src/engine.rs");
    assert_eq!(parsed.entries.len(), 2);
    assert_eq!(parsed.entries[0].id, 1);
    assert_eq!(parsed.entries[1].id, 2);

    // Save and load via store
    let sum_dir = root
        .join("domains")
        .join("luna")
        .join("topics")
        .join("memory")
        .join("summaries");
    fs::create_dir_all(&sum_dir).unwrap();
    fs::write(sum_dir.join("engine.summary.md"), &rendered).unwrap();

    let loaded = store.summaries().unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].source_file, "crates/harness-memory/src/engine.rs");
}

#[tokio::test]
async fn test_phase2_context_layers_and_budget() {
    let root = temp_root("context");
    fs::write(
        root.join("identity.md"),
        "# Luna Identity\nLuna is an autonomous AI systems engineer. Traits: direct, honest, competent.\n"
    ).unwrap();

    let engine = MarkdownMemoryEngine::from_root(root).unwrap();

    // Populate with 25 diverse records across multiple topics
    for i in 0..25 {
        let topic = match i % 4 {
            0 => "architecture",
            1 => "backend",
            2 => "testing",
            _ => "design",
        };
        let body =
            format!("Important engineering decision {i} about system performance and caching.");
        let record = harness_memory::Record {
            id: format!("rec_{i}"),
            domain: "luna".to_string(),
            topic: topic.to_string(),
            source_file: Some(format!("crates/core/src/{topic}.rs")),
            chunk_index: 0,
            line_start: 1,
            line_end: 10,
            filed_at: Utc::now(),
            entities: vec![],
            tags: vec![topic.to_string()],
            body_hash: harness_memory::store::ids::body_hash(&body),
            subject: format!("subject_{i}"),
            predicate: "profile".to_string(),
            object: body.clone(),
            name: Some(format!("decision_{i}")),
            path: Some(format!("crates/core/src/{topic}.rs")),
            raw_content: body.clone(),
            category: MemoryCategory::Fact,
            confidence: 1.0,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            is_active: true,
            superseded_by: None,
            body,
        };
        engine.write_record(record).unwrap();
    }

    // 1. Layer 0
    let stack = engine.context_stack();
    let l0_text = stack.l0.render();
    assert!(l0_text.contains("Luna is an autonomous AI systems engineer"));
    assert!(stack.l0.token_estimate() < 100);

    // 2. Layer 1 (Essential Story)
    let records = engine.store().active_records().unwrap();
    let l1_text = stack.l1.generate(&records, None);
    assert!(l1_text.contains("## L1 — ESSENTIAL STORY"));
    assert!(l1_text.contains("<stored-memory-"));
    assert!(l1_text.contains("Important engineering decision"));

    // 3. Combined Wake-Up block: EXIT CRITERION strictly under 900 tokens
    let wake_up = engine.wake_up(None).unwrap();
    let token_estimate = stack.wake_up_token_estimate(&records, None);
    assert!(
        token_estimate < 900,
        "Wake-up tokens ({token_estimate}) must be under 900 tokens"
    );
    assert!(wake_up.len() < 3600); // 3600 chars is ~900 tokens

    // 4. Layer 2 (On-demand recall)
    let recall_backend = engine.recall(None, Some("backend"), 5).unwrap();
    assert!(recall_backend.contains("## L2 — ON-DEMAND"));
    assert!(recall_backend.contains("[backend]"));

    // 5. Layer 3 (Deep Search)
    let results = engine
        .search("performance", &SearchFilter::default(), 5)
        .unwrap();
    let l3_formatted = stack.search("performance", &results);
    assert!(l3_formatted.contains("## L3 — SEARCH RESULTS for \"performance\""));
    assert!(l3_formatted.contains("[1]"));
}

#[tokio::test]
async fn test_phase2_search_improvement_via_closet_boost() {
    let root = temp_root("search_boost");
    let engine = MarkdownMemoryEngine::from_root(root.clone()).unwrap();

    // Create two records: one in auth/login.rs and one in notes/misc.rs
    let rec_auth = MemoryRecord {
        id: "luna.auth_jwt".to_string(),
        subject: "auth".to_string(),
        predicate: "jwt".to_string(),
        object: "Session token authorization handler".to_string(),
        name: Some("auth_jwt".to_string()),
        path: Some("crates/auth/src/jwt.rs".to_string()),
        raw_content: "Token verification and session handling implementation.".to_string(),
        category: MemoryCategory::Fact,
        confidence: 1.0,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        is_active: true,
        superseded_by: None,
    };
    let rec_other = MemoryRecord {
        id: "luna.generic_handler".to_string(),
        subject: "handler".to_string(),
        predicate: "generic".to_string(),
        object: "Generic token handler helper".to_string(),
        name: Some("generic_handler".to_string()),
        path: Some("crates/common/src/handler.rs".to_string()),
        raw_content: "Generic helper for handler tokens.".to_string(),
        category: MemoryCategory::Fact,
        confidence: 1.0,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        is_active: true,
        superseded_by: None,
    };

    engine.save(&rec_auth).await.unwrap();
    engine.save(&rec_other).await.unwrap();

    // Now write a ContentIndex (closet) for auth that highlights "OAuth authentication authorization"
    let index_dir = root
        .join("domains")
        .join("luna")
        .join("topics")
        .join("fact")
        .join("indexes");
    fs::create_dir_all(&index_dir).unwrap();

    let index = ContentIndex {
        source_file: "crates/auth/src/jwt.rs".to_string(),
        records: vec!["luna.auth_jwt".to_string()],
        built_at: Utc::now(),
        chunk_indexes: vec![0],
        lines: vec![PointerLine::new(
            "built OAuth authentication token system",
            vec!["OAuth".to_string(), "JWT".to_string()],
            Some("2026-11-08:L1-50".to_string()),
            vec!["luna.auth_jwt".to_string()],
        )],
    };
    fs::write(index_dir.join("jwt.index.md"), index.render(1500)).unwrap();

    // Query for "OAuth authentication"
    // rec_auth's body does NOT have "OAuth" directly, but its content index DOES.
    // The closet boost elevates rec_auth to rank 1 with matched_via="drawer+closet" and boost 0.40!
    let results = engine
        .search("OAuth authentication", &SearchFilter::default(), 5)
        .unwrap();

    assert!(!results.is_empty(), "Must return search results");
    let top = &results[0];
    assert_eq!(top.record.id, "luna.auth_jwt");
    assert_eq!(top.matched_via, "drawer+closet");
    assert_eq!(top.closet_boost, 0.40);
    assert!(top.closet_preview.is_some());
    assert!(top.score > 0.40);
}
