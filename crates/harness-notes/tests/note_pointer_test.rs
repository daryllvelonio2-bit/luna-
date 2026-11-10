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

/// Archiving is a soft delete: the note leaves the active set but the file stays on disk.
#[tokio::test]
async fn test_archive_note_hides_it_without_destroying_the_file() {
    let engine = FileNotesEngine::in_memory().unwrap();

    let note = engine
        .save_note(
            "Obsolete Deployment Runbook",
            "Old procedure that has been replaced.",
            &[],
        )
        .await
        .unwrap();
    let path = engine.note_path(&note.id).unwrap();
    assert!(path.exists());

    assert!(engine.archive_note(&note.id).await.unwrap());

    assert!(engine.list_notes(10, 0).await.unwrap().is_empty());
    assert!(engine.read_note(&note.id).await.is_err());
    assert!(!path.exists(), "the active copy must be gone");
    let archived = engine
        .dir()
        .join(".archive")
        .join("inbox")
        .join(format!("{}.md", note.id));
    assert!(
        archived.exists(),
        "the archived copy must still exist at {}",
        archived.display()
    );
    assert!(std::fs::read_to_string(&archived)
        .unwrap()
        .contains("Old procedure"));
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
    // The loader tags every note with its project so the graph can group by folder.
    assert_eq!(
        imported.tags,
        vec!["legacy".to_string(), "project:inbox".to_string()]
    );

    // Check that legacy db was renamed to notes.db.imported
    assert!(!legacy_db.exists());
    assert!(temp_dir.join("notes.db.imported").exists());

    // Clean up
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_path_traversal_prevention() {
    let temp_dir = std::env::temp_dir().join(format!("luna-traversal-{}", uuid::Uuid::new_v4()));
    let notes_dir = temp_dir.join("notes");
    let outside_dir = temp_dir.join("outside");
    std::fs::create_dir_all(&notes_dir).unwrap();
    std::fs::create_dir_all(&outside_dir).unwrap();

    let secret_file = outside_dir.join("secret.md");
    std::fs::write(&secret_file, "super secret outside content").unwrap();

    let engine = FileNotesEngine::new(notes_dir.clone(), None).unwrap();

    // 1. read_note traversal must fail
    let traversal_ids = [
        "../outside/secret",
        "../../../../etc/shadow",
        "/etc/passwd",
        "./secret",
        ".secret",
    ];

    for id in traversal_ids {
        assert!(
            engine.read_note(id).await.is_err(),
            "read_note should fail for traversal id '{}'",
            id
        );
        assert!(
            !engine.delete_note(id).await.unwrap_or(false),
            "delete_note should not succeed for traversal id '{}'",
            id
        );
        assert!(
            engine
                .update_note(id, Some("Hacked"), None, None)
                .await
                .is_err(),
            "update_note should fail for traversal id '{}'",
            id
        );
    }

    // Ensure the external secret file was NOT deleted or overwritten
    assert!(secret_file.exists());
    assert_eq!(
        std::fs::read_to_string(&secret_file).unwrap(),
        "super secret outside content"
    );

    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn test_long_note_chunking_and_reconstruction_with_hash_dedupe() {
    let engine = FileNotesEngine::in_memory().unwrap();

    // Create a 16 KB document with distinct paragraphs
    let paragraph = "This is a detailed section of the architecture specification with important technical context.\n".repeat(40);
    let mut content = String::new();
    for i in 1..=6 {
        content.push_str(&format!("## Section {}\n\n", i));
        content.push_str(&paragraph);
        content.push_str("\n\n");
    }
    assert!(content.len() > 14_000, "content len: {}", content.len());

    let note = engine
        .save_note(
            "Large System Specification",
            &content,
            &["spec".to_string()],
        )
        .await
        .unwrap();

    // Verify chunk files were created on disk
    let part1 = engine.note_path(&format!("{}-part-1", note.id)).unwrap();
    let part2 = engine.note_path(&format!("{}-part-2", note.id)).unwrap();
    assert!(part1.exists(), "part 1 must exist at {}", part1.display());
    assert!(part2.exists(), "part 2 must exist at {}", part2.display());

    // Verify reading part 1 directly
    let p1_note = engine
        .read_note(&format!("{}-part-1", note.id))
        .await
        .unwrap();
    assert_eq!(p1_note.parent_id.as_deref(), Some(note.id.as_str()));
    assert_eq!(p1_note.part_number, Some(1));
    assert!(p1_note.content_hash.is_some());

    // Verify reading by parent ID returns full content seamlessly
    let full = engine.read_note(&note.id).await.unwrap();
    assert!(full.content.contains("## Section 1"));
    assert!(full.content.contains("## Section 6"));

    // Hash dedupe: re-saving with same content keeps existing parts intact
    let mtime_before = std::fs::metadata(&part1).unwrap().modified().unwrap();
    tokio::time::sleep(tokio::time::Duration::from_millis(15)).await;
    engine
        .update_note(&note.id, None, Some(&content), None)
        .await
        .unwrap();
    let mtime_after = std::fs::metadata(&part1).unwrap().modified().unwrap();
    assert_eq!(
        mtime_before, mtime_after,
        "hash dedupe must not re-write unchanged parts"
    );

    // Deleting parent note deletes all parts too
    assert!(engine.delete_note(&note.id).await.unwrap());
    assert!(!part1.exists(), "part 1 must be deleted");
    assert!(!part2.exists(), "part 2 must be deleted");
}

#[tokio::test]
async fn test_near_match_reconciliation_converts_to_append_instead_of_duplicate() {
    let engine = FileNotesEngine::in_memory().unwrap();

    let initial = "Step 1: Install dependencies with cargo build.\nStep 2: Run test suite.";
    let note1 = engine
        .save_note("Deployment Recipe", initial, &["devops".to_string()])
        .await
        .unwrap();
    assert_eq!(note1.id, "deployment-recipe");

    // Save with near-match title and additional tag
    let addition =
        "## Verification\n\nStep 3: Check endpoint with curl -f http://localhost:8080/health.";
    let note2 = engine
        .save_note(
            "Deployment Recipe (Production Verified)",
            addition,
            &["devops".to_string(), "prod".to_string()],
        )
        .await
        .unwrap();

    // Reconciled onto existing note: same ID, no duplicate file!
    assert_eq!(note2.id, note1.id);

    // List notes must have exactly 1 note (no duplicate deployment-recipe-1.md)
    let notes = engine.list_notes(10, 0).await.unwrap();
    assert_eq!(notes.len(), 1, "must not create duplicate note files");
    assert!(notes[0].content.contains("Step 1: Install dependencies"));
    assert!(notes[0].content.contains("Step 3: Check endpoint"));
    assert!(notes[0].tags.contains(&"devops".to_string()));
    assert!(notes[0].tags.contains(&"prod".to_string()));

    // Idempotent: saving the exact same content again does not duplicate lines
    let note3 = engine
        .save_note("Deployment Recipe", addition, &[])
        .await
        .unwrap();
    let count = note3.content.matches("Step 3: Check endpoint").count();
    assert_eq!(
        count, 1,
        "content must not be duplicated when already present"
    );
}
