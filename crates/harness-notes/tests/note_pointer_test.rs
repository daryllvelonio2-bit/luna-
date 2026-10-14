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
