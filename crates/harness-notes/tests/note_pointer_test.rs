use harness_notes::*;
use harness_core::*;

#[tokio::test]
async fn test_notes_fts_and_pointer_distillation() {
    let engine = SqliteNotesEngine::in_memory().unwrap();

    let title = "Stripe Payment Gateway Integration Specification 2026";
    let content = "This document describes the webhook callbacks, charge dispute handling, and idempotency key conventions for Stripe.";

    let note = engine.save_note(title, content, &["finance".to_string()]).await.unwrap();

    // Summary pointer must strictly not exceed 5 words
    let word_count = note.summary_pointer.split_whitespace().count();
    assert!(word_count >= 1 && word_count <= 5);
    assert_eq!(note.summary_pointer, "Stripe Payment Gateway Integration Specification");

    // Search via FTS
    let search_results = engine.search_notes("idempotency", 5).await.unwrap();
    assert_eq!(search_results.len(), 1);
    assert_eq!(search_results[0].id, note.id);
}

#[tokio::test]
async fn test_delete_note_removes_row_and_fts_entry() {
    let engine = SqliteNotesEngine::in_memory().unwrap();

    let note = engine
        .save_note(
            "Kubernetes Cluster Runbook",
            "Rolling restart procedure for the payments namespace.",
            &[],
        )
        .await
        .unwrap();

    // The note is findable before deletion.
    assert_eq!(engine.search_notes("Rolling restart", 5).await.unwrap().len(), 1);

    // Deletion reports that a row was removed.
    assert!(engine.delete_note(&note.id).await.unwrap());

    // Reading it afterwards fails...
    assert!(engine.read_note(&note.id).await.is_err());

    // ...and the FTS index no longer surfaces it (the `notes_ad` trigger fired).
    assert!(engine.search_notes("Rolling restart", 5).await.unwrap().is_empty());
}

#[tokio::test]
async fn test_delete_missing_note_is_not_an_error() {
    let engine = SqliteNotesEngine::in_memory().unwrap();

    // Deleting an id that was never stored is a no-op, not a failure.
    assert!(!engine.delete_note("note_does_not_exist").await.unwrap());
}
