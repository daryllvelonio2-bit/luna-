use harness_memory::*;
use harness_core::*;

#[tokio::test]
async fn test_memory_superseding_and_active_state() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();

    let mem1 = MemoryRecord {
        id: "mem_1".to_string(),
        subject: "user".to_string(),
        predicate: "preferred_language".to_string(),
        object: "Python".to_string(),
        raw_content: "user preferred_language Python".to_string(),
        category: MemoryCategory::UserProfile,
        confidence: 1.0,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        is_active: true,
        superseded_by: None,
    };

    engine.save(&mem1).await.unwrap();

    let res = engine.retrieve("preferred_language", 5).await.unwrap();
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].object, "Python");

    // Save conflicting record for same subject and predicate -> should supersede mem_1
    let mem2 = MemoryRecord {
        id: "mem_2".to_string(),
        subject: "user".to_string(),
        predicate: "preferred_language".to_string(),
        object: "Rust".to_string(),
        raw_content: "user preferred_language Rust".to_string(),
        category: MemoryCategory::UserProfile,
        confidence: 1.0,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        is_active: true,
        superseded_by: None,
    };

    engine.save(&mem2).await.unwrap();

    let res2 = engine.retrieve("preferred_language", 5).await.unwrap();
    assert_eq!(res2.len(), 1);
    assert_eq!(res2[0].id, "mem_2");
    assert_eq!(res2[0].object, "Rust");
}

#[tokio::test]
async fn test_multiple_note_pointers_remain_active() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();

    engine.add_note_pointer("note_1", "Stripe Payment Gateway Spec 2026").await.unwrap();
    engine.add_note_pointer("note_2", "Rust Harness Architecture").await.unwrap();

    let res = engine.retrieve("Note [ID:", 10).await.unwrap();
    assert_eq!(res.len(), 2, "Both note pointers must remain active");

    let ids: Vec<String> = res.iter().map(|m| m.id.clone()).collect();
    assert!(ids.contains(&"mem_ptr_note_1".to_string()));
    assert!(ids.contains(&"mem_ptr_note_2".to_string()));
}
