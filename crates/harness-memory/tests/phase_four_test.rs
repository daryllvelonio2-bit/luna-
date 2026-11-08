use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use chrono::Utc;
use harness_memory::{MarkdownMemoryEngine, MemoryStore, SearchFilter, StoreConfig};

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luna-phase4-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn create_mock_sessions_db(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE sessions (
            id            TEXT PRIMARY KEY,
            created_at    TEXT NOT NULL,
            updated_at    TEXT NOT NULL,
            title         TEXT NOT NULL DEFAULT '',
            name          TEXT NOT NULL DEFAULT '',
            model         TEXT NOT NULL DEFAULT '',
            compact_model TEXT NOT NULL DEFAULT ''
        );

        CREATE TABLE messages (
            id               INTEGER PRIMARY KEY AUTOINCREMENT,
            session_id       TEXT NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
            seq              INTEGER NOT NULL,
            role             TEXT NOT NULL,
            content          TEXT,
            reasoning        TEXT,
            tool_calls_json  TEXT,
            tool_result_json TEXT,
            usage_json       TEXT,
            is_summary       INTEGER NOT NULL DEFAULT 0,
            compacted_at     TEXT,
            created_at       TEXT NOT NULL,
            UNIQUE (session_id, seq)
        );
        "#,
    )
    .unwrap();

    let now_str = Utc::now().to_rfc3339();

    // Session 1: Kubernetes Deployment
    conn.execute(
        "INSERT INTO sessions (id, created_at, updated_at, title, name, model) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            "sess_k8s_deploy",
            &now_str,
            &now_str,
            "Deploying Kubernetes Cluster",
            "k8s-cluster",
            "openai:gpt-4o"
        ],
    )
    .unwrap();

    // Turn 1
    conn.execute(
        "INSERT INTO messages (session_id, seq, role, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            "sess_k8s_deploy",
            1,
            "user",
            "How do we deploy the Kubernetes cluster with Helm?",
            &now_str
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, seq, role, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            "sess_k8s_deploy",
            2,
            "assistant",
            "To deploy the Kubernetes cluster with Helm, run helm upgrade --install luna ./chart -f values-prod.yaml.",
            &now_str
        ],
    )
    .unwrap();

    // Turn 2
    conn.execute(
        "INSERT INTO messages (session_id, seq, role, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            "sess_k8s_deploy",
            3,
            "user",
            "What namespace should we use?",
            &now_str
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, seq, role, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            "sess_k8s_deploy",
            4,
            "assistant",
            "Use the luna-production namespace with strict RBAC security policies.",
            &now_str
        ],
    )
    .unwrap();

    // Session 2: OAuth PKCE
    conn.execute(
        "INSERT INTO sessions (id, created_at, updated_at, title, name, model) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            "sess_auth_pkce",
            &now_str,
            &now_str,
            "Authentication Protocol",
            "auth-pkce",
            "anthropic:claude-3-5-sonnet"
        ],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO messages (session_id, seq, role, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            "sess_auth_pkce",
            1,
            "user",
            "Explain the OAuth2 PKCE flow for desktop clients.",
            &now_str
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO messages (session_id, seq, role, content, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![
            "sess_auth_pkce",
            2,
            "assistant",
            "The PKCE flow generates a cryptographically random code_verifier and code_challenge to prevent authorization code interception.",
            &now_str
        ],
    )
    .unwrap();
}

#[tokio::test]
async fn test_phase4_sessions_db_mining_and_hybrid_query() {
    let root = temp_root("sessions_db");
    let store = MemoryStore::open(StoreConfig::new(root.clone())).unwrap();

    let db_path = root.join("sessions.db");
    create_mock_sessions_db(&db_path);

    // 1. Mine sessions.db into the store
    let report = store.mine_sessions(&db_path, None, None).unwrap();
    assert_eq!(report.files, 2, "Expected 2 sessions mined");
    assert_eq!(report.records, 3, "Expected 3 conversation exchanges mined");

    // 2. Query for Kubernetes Helm deployment
    let filter = SearchFilter {
        domain: Some("conversations".to_string()),
        topic: None,
        since: None,
        before: None,
    };
    let hits = store
        .search("helm values-prod kubernetes", &filter, 5)
        .unwrap();
    assert!(!hits.is_empty(), "Should retrieve mined session exchanges");
    let top = &hits[0];
    assert_eq!(top.record.domain, "conversations");
    assert_eq!(top.record.topic, "k8s-cluster");
    assert!(
        top.record
            .body
            .contains("helm upgrade --install luna ./chart -f values-prod.yaml"),
        "Top hit must contain the Helm command"
    );

    // 3. Query for PKCE code_challenge
    let pkce_hits = store
        .search("code_verifier code_challenge interception", &filter, 5)
        .unwrap();
    assert!(
        !pkce_hits.is_empty(),
        "Should retrieve PKCE session exchange"
    );
    assert_eq!(pkce_hits[0].record.topic, "auth-pkce");
    assert!(pkce_hits[0]
        .record
        .body
        .contains("cryptographically random code_verifier"));

    // 4. Verify content indexes were created
    let indexes = store.content_indexes().unwrap();
    assert_eq!(
        indexes.len(),
        2,
        "Each session should have its content index"
    );
}

#[tokio::test]
async fn test_phase4_convo_file_mining() {
    let root = temp_root("convo_files");
    let store = MemoryStore::open(StoreConfig::new(root.clone())).unwrap();

    let chat_dir = root.join("exports");
    fs::create_dir_all(&chat_dir).unwrap();

    // 1. Write JSONL chat export
    let jsonl_content = r#"{"role": "user", "content": "How do we configure Tantivy for full-text search?"}
{"role": "assistant", "content": "Configure Tantivy with a text field using the English tokenizer and BM25 scoring."}
{"role": "user", "content": "What about the commit interval?"}
{"role": "assistant", "content": "Call index_writer.commit() after batch indexing to flush segments to disk."}"#;
    fs::write(chat_dir.join("tantivy_chat.jsonl"), jsonl_content).unwrap();

    // 2. Write Markdown transcript
    let md_content = r#"> User: How should we structure the daily journal?
Assistant: Store journals under journal/<agent>/<date>.md as append-only markdown files with timestamp headers.

> User: Can other agents read it?
Assistant: Yes, journal_read allows querying by agent and date range."#;
    fs::write(chat_dir.join("journal_design.md"), md_content).unwrap();

    // Mine exports
    let report = store.mine_convos(&chat_dir, None, None).unwrap();
    assert_eq!(report.files, 2);
    assert_eq!(report.records, 4);

    let filter = SearchFilter {
        domain: Some("conversations".to_string()),
        topic: None,
        since: None,
        before: None,
    };
    let hits = store
        .search("Tantivy BM25 scoring index_writer", &filter, 5)
        .unwrap();
    assert!(!hits.is_empty());
    assert!(hits[0]
        .record
        .body
        .contains("English tokenizer and BM25 scoring"));

    let hits_journal = store
        .search("journal append-only timestamp headers", &filter, 5)
        .unwrap();
    assert!(!hits_journal.is_empty());
    assert!(hits_journal[0]
        .record
        .body
        .contains("journal/<agent>/<date>.md"));
}

#[tokio::test]
async fn test_phase4_agent_journal_lifecycle() {
    let root = temp_root("journal");
    let store = MemoryStore::open(StoreConfig::new(root)).unwrap();

    // 1. Write first entry for Luna
    let e1 = store
        .journal_write("Luna", "Investigated memory store architecture.", None)
        .unwrap();
    assert_eq!(e1.agent, "luna");
    assert_eq!(e1.content, "Investigated memory store architecture.");

    // 2. Write second entry on the same day for Luna
    let e2 = store
        .journal_write(
            "Luna",
            "Implemented Phase 4 session miner and event log.",
            None,
        )
        .unwrap();
    assert_eq!(e2.agent, "luna");
    assert_eq!(
        e2.content,
        "Implemented Phase 4 session miner and event log."
    );

    // 3. Read back entries for Luna
    let entries = store.journal_read("Luna", None, None).unwrap();
    assert_eq!(entries.len(), 2, "Expected 2 journal entries for Luna");
    // Sorted descending by timestamp
    assert_eq!(
        entries[0].content,
        "Implemented Phase 4 session miner and event log."
    );
    assert_eq!(
        entries[1].content,
        "Investigated memory store architecture."
    );

    // 4. Codex journal
    store
        .journal_write("Codex", "Reviewing pull request #42.", None)
        .unwrap();
    let codex_entries = store.journal_read("codex", None, None).unwrap();
    assert_eq!(codex_entries.len(), 1);
    assert_eq!(codex_entries[0].content, "Reviewing pull request #42.");

    // 5. Query non-existent agent
    let empty = store.journal_read("ghost", None, None).unwrap();
    assert!(empty.is_empty());
}

#[tokio::test]
async fn test_phase4_event_log_and_coordination() {
    let root = temp_root("events");
    let store = MemoryStore::open(StoreConfig::new(root)).unwrap();

    // 1. Append delegation event
    let ev1 = store
        .event_append(
            "main",
            "delegation",
            "claude",
            Some("codex"),
            "Implement the record writer.",
            Some("corr_42"),
            Some("open"),
        )
        .unwrap();
    assert!(ev1.id.starts_with("evt_"));
    assert_eq!(ev1.event_type, "delegation");
    assert_eq!(ev1.from, "claude");
    assert_eq!(ev1.to.as_deref(), Some("codex"));
    assert_eq!(ev1.status, "open");

    // 2. Append broadcast event
    let ev2 = store
        .event_append(
            "main",
            "announcement",
            "luna",
            None,
            "Reindex completed across all domains.",
            None,
            Some("info"),
        )
        .unwrap();
    assert_eq!(ev2.from, "luna");

    // 3. List events
    let all_events = store.event_list("main", None, None, None).unwrap();
    assert_eq!(all_events.len(), 2);

    // 4. List unacked by codex
    let unacked_codex = store.event_list("main", None, None, Some("codex")).unwrap();
    assert_eq!(unacked_codex.len(), 2);

    // 5. Codex acknowledges event 1
    store.event_ack("main", "codex", &ev1.id).unwrap();

    let unacked_codex_after = store.event_list("main", None, None, Some("codex")).unwrap();
    assert_eq!(unacked_codex_after.len(), 1);
    assert_eq!(unacked_codex_after[0].id, ev2.id);

    // Claude acknowledges the broadcast event 2 before waiting for a directed task event
    store.event_ack("main", "claude", &ev2.id).unwrap();

    // 6. Polling event_wait
    let store_clone = MemoryStore::open(StoreConfig::new(store.root().to_path_buf())).unwrap();
    tokio::spawn(async move {
        tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        store_clone
            .event_append(
                "main",
                "task_done",
                "codex",
                Some("claude"),
                "Record writer tests passing.",
                Some("corr_42"),
                Some("completed"),
            )
            .unwrap();
    });

    let waited = store.event_wait("main", "claude", 3000).await.unwrap();
    assert!(
        waited.is_some(),
        "event_wait should have received the event"
    );
    let received = waited.unwrap();
    assert_eq!(received.from, "codex");
    assert_eq!(received.to.as_deref(), Some("claude"));
    assert_eq!(received.body, "Record writer tests passing.");
}

#[tokio::test]
async fn test_phase4_byte_artifacts() {
    let root = temp_root("artifacts");
    let engine = MarkdownMemoryEngine::from_root(root).unwrap();

    let binary_data =
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00\x00\x10\x00\x00\x00\x10\x08\x06\x00\x00\x00";
    let mut meta = HashMap::new();
    meta.insert("content_type".to_string(), "image/png".to_string());
    meta.insert("author".to_string(), "luna".to_string());

    // 1. Put artifact
    let art_id = engine
        .artifact_put(Some("flowchart_png"), binary_data, meta)
        .unwrap();
    assert_eq!(art_id, "flowchart-png");

    // 2. Get artifact
    let retrieved = engine.artifact_get(&art_id).unwrap();
    assert!(retrieved.is_some());
    let (payload, metadata) = retrieved.unwrap();
    assert_eq!(payload, binary_data, "Artifact bytes must match verbatim");
    assert_eq!(metadata.size_bytes, binary_data.len());
    assert_eq!(metadata.extra.get("content_type").unwrap(), "image/png");
    assert_eq!(metadata.extra.get("author").unwrap(), "luna");

    // 3. Non-existent artifact returns None
    assert!(engine.artifact_get("missing_artifact").unwrap().is_none());
}
