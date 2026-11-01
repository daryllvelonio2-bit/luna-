use harness_core::*;
use harness_notes::*;

#[tokio::test]
async fn test_notes_fts_and_pointer_distillation() {
    let engine = FileNotesEngine::in_memory().unwrap();

    let title = "Stripe Payment Gateway Integration Specification 2026";
    let content = "This document describes the webhook callbacks, charge dispute handling, and idempotency key conventions for Stripe.";

    let note = engine
        .save_note(title, content, &["finance".to_string()])
        .await
        .unwrap();

    // Summary pointer must strictly not exceed 5 words
    let word_count = note.summary_pointer.split_whitespace().count();
    assert!((1..=5).contains(&word_count));
    assert_eq!(
        note.summary_pointer,
        "Stripe Payment Gateway Integration Specification"
    );

    // Search via FTS
    let search_results = engine.search_notes("idempotency", 5).await.unwrap();
    assert_eq!(search_results.len(), 1);
    assert_eq!(search_results[0].id, note.id);
}

#[tokio::test]
async fn test_delete_note_removes_row_and_fts_entry() {
    let engine = FileNotesEngine::in_memory().unwrap();

    let note = engine
        .save_note(
            "Kubernetes Cluster Runbook",
            "Rolling restart procedure for the payments namespace.",
            &[],
        )
        .await
        .unwrap();

    // The note is findable before deletion.
    assert_eq!(
        engine
            .search_notes("Rolling restart", 5)
            .await
            .unwrap()
            .len(),
        1
    );

    // Deletion reports that a row was removed.
    assert!(engine.delete_note(&note.id).await.unwrap());

    // Reading it afterwards fails...
    assert!(engine.read_note(&note.id).await.is_err());

    // ...and the FTS index no longer surfaces it (the `notes_ad` trigger fired).
    assert!(engine
        .search_notes("Rolling restart", 5)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn test_delete_missing_note_is_not_an_error() {
    let engine = FileNotesEngine::in_memory().unwrap();

    // Deleting an id that was never stored is a no-op, not a failure.
    assert!(!engine.delete_note("note_does_not_exist").await.unwrap());
}

#[tokio::test]
async fn test_list_notes_orders_by_updated_and_paginates() {
    let engine = std::sync::Arc::new(FileNotesEngine::in_memory().unwrap());

    engine.save_note("Note 1", "Content 1", &[]).await.unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    engine.save_note("Note 2", "Content 2", &[]).await.unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    engine.save_note("Note 3", "Content 3", &[]).await.unwrap();

    // List all
    let all = engine.list_notes(10, 0).await.unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(all[0].title, "Note 3");
    assert_eq!(all[1].title, "Note 2");
    assert_eq!(all[2].title, "Note 1");

    // Paginate limit 2, offset 1
    let paged = engine.list_notes(2, 1).await.unwrap();
    assert_eq!(paged.len(), 2);
    assert_eq!(paged[0].title, "Note 2");
    assert_eq!(paged[1].title, "Note 1");

    // Test ListNotesTool
    let tool = ListNotesTool::new(engine.clone());
    let cancel = tokio_util::sync::CancellationToken::new();
    let res = tool
        .execute(
            "call_1",
            &serde_json::json!({ "limit": 2, "offset": 0 }),
            &cancel,
        )
        .await
        .unwrap();
    let list: Vec<serde_json::Value> = serde_json::from_str(&res.content).unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0]["title"], "Note 3");
}

#[tokio::test]
async fn test_legacy_notes_db_import_and_rename() {
    let temp_dir = std::env::temp_dir().join(format!("luna-notes-legacy-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).unwrap();

    let legacy_db = temp_dir.join("notes.db");
    {
        let conn = rusqlite::Connection::open(&legacy_db).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE notes (
                id TEXT PRIMARY KEY,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                summary_pointer TEXT NOT NULL,
                tags TEXT NOT NULL DEFAULT '[]',
                created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
                updated_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP
            );
            INSERT INTO notes (id, title, content, summary_pointer, tags)
            VALUES ('legacy-doc', 'Legacy Document Title', 'Content of the legacy doc.', 'Legacy Document', '["legacy"]');
            "#,
        ).unwrap();
    }

    let notes_dir = temp_dir.join("notes");
    let engine = FileNotesEngine::new(notes_dir.clone(), Some(legacy_db.clone())).unwrap();

    let imported = engine.read_note("legacy-doc").await.unwrap();
    assert_eq!(imported.title, "Legacy Document Title");
    assert_eq!(imported.content, "Content of the legacy doc.");
    assert_eq!(imported.summary_pointer, "Legacy Document");
    assert_eq!(imported.tags, vec!["legacy".to_string()]);

    // Check that legacy db was renamed to notes.db.imported
    assert!(!legacy_db.exists());
    assert!(temp_dir.join("notes.db.imported").exists());

    // Clean up
    let _ = std::fs::remove_dir_all(&temp_dir);
}
