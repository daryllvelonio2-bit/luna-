use harness_core::*;
use harness_memory::*;

#[tokio::test]
async fn test_memory_superseding_and_active_state() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();

    let mem1 = MemoryRecord {
        id: "mem_1".to_string(),
        subject: "user".to_string(),
        predicate: "preferred_language".to_string(),
        object: "Python".to_string(),
        name: None,
        path: None,
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
        name: None,
        path: None,
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

    engine
        .add_note_pointer("note_1", "Stripe Payment Gateway Spec 2026")
        .await
        .unwrap();
    engine
        .add_note_pointer("note_2", "Rust Harness Architecture")
        .await
        .unwrap();

    let res = engine.retrieve("Note", 10).await.unwrap();
    assert_eq!(res.len(), 2, "Both note pointers must remain active");

    let ids: Vec<String> = res.iter().map(|m| m.id.clone()).collect();
    assert!(ids.contains(&"mem_ptr_note_1".to_string()));
    assert!(ids.contains(&"mem_ptr_note_2".to_string()));
}

fn record(id: &str, subject: &str, predicate: &str, object: &str) -> MemoryRecord {
    MemoryRecord {
        id: id.to_string(),
        subject: subject.to_string(),
        predicate: predicate.to_string(),
        object: object.to_string(),
        name: None,
        path: None,
        raw_content: format!("{} {} {}", subject, predicate, object),
        category: MemoryCategory::Fact,
        confidence: 1.0,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        is_active: true,
        superseded_by: None,
    }
}

/// Regression: `update_memory` derived the new id from subject+predicate, so revising the same
/// entity produced the *same* primary key as the record being superseded and the INSERT died with
/// `UNIQUE constraint failed: memories.id`. Same-id updates must revise in place.
#[tokio::test]
async fn test_update_with_identical_derived_id_revises_in_place() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();

    engine
        .save(&record("project.x.profile", "project.x", "profile", "v1"))
        .await
        .unwrap();

    let mut updated = record("project.x.profile", "project.x", "profile", "v2");
    updated.created_at = chrono::Utc::now() - chrono::Duration::hours(5);

    engine
        .update("project.x.profile", &updated)
        .await
        .expect("updating a record onto its own derived id must not hit a UNIQUE constraint");

    let res = engine.retrieve("project.x", 10).await.unwrap();
    assert_eq!(res.len(), 1, "exactly one active record expected");
    assert_eq!(res[0].id, "project.x.profile");
    assert_eq!(res[0].object, "v2");
    assert!(res[0].is_active, "an in-place revision must stay active");
    assert_eq!(
        res[0].superseded_by, None,
        "a record must never supersede itself"
    );
}

#[tokio::test]
async fn test_update_with_changed_id_supersedes_old_record() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();

    engine
        .save(&record("user.old", "user", "old", "stale"))
        .await
        .unwrap();
    engine
        .update("user.old", &record("user.new", "user", "new", "fresh"))
        .await
        .unwrap();

    let fresh = engine.retrieve("fresh", 10).await.unwrap();
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].id, "user.new");
    assert_eq!(fresh[0].superseded_by, None);

    let stale = engine.retrieve("stale", 10).await.unwrap();
    assert!(
        stale.iter().all(|m| m.id != "user.old"),
        "superseded record must not surface in active retrieval"
    );
}

#[tokio::test]
async fn test_update_accepts_rendered_display_name() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();

    // The system prompt renders `subject.predicate`, while the stored id is the sanitized form.
    engine
        .save(&record(
            "project_x_profile.profile",
            "project.x.profile",
            "profile",
            "v1",
        ))
        .await
        .unwrap();

    engine
        .update(
            "project.x.profile.profile",
            &record(
                "project_x_profile.profile",
                "project.x.profile",
                "profile",
                "v2",
            ),
        )
        .await
        .expect("display-name ids must resolve to the stored record");

    let res = engine.retrieve("project.x.profile", 10).await.unwrap();
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].id, "project_x_profile.profile");
    assert_eq!(res[0].object, "v2");
    assert!(res[0].is_active);
}

#[tokio::test]
async fn test_only_one_active_memory_per_topic() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();

    let mut rec1 = record("project.astra.1", "project.astra", "profile", "v1 details");
    rec1.name = Some("Astra".to_string());
    rec1.path = Some("/home/janelle/astra".to_string());
    engine.save(&rec1).await.unwrap();

    let mut rec2 = record("project.astra.2", "project.astra", "profile", "v2 details");
    rec2.name = Some("Astra".to_string());
    rec2.path = Some("/home/janelle/astra".to_string());
    engine.save(&rec2).await.unwrap();

    let res = engine.retrieve("astra", 10).await.unwrap();
    assert_eq!(res.len(), 1, "only 1 active record per topic must exist");
    assert_eq!(res[0].id, "project.astra.2");
    assert_eq!(res[0].object, "v2 details");
    assert_eq!(res[0].name.as_deref(), Some("Astra"));
    assert_eq!(res[0].path.as_deref(), Some("/home/janelle/astra"));

    let prompt = engine.format_as_system_prompt(&res);
    assert!(prompt.contains("project.astra: Astra · v2 details · path: /home/janelle/astra"));
}

#[tokio::test]
async fn test_update_missing_record_is_an_error() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();
    let err = engine
        .update(
            "does.not.exist",
            &record("does.not.exist", "does", "not", "x"),
        )
        .await
        .expect_err("updating an unknown id must fail loudly");
    assert!(format!("{}", err).contains("no such record"));
}

/// Re-saving the same entity reuses its derived id; the conflict set must not include the row
/// being written, or the save would deactivate itself.
#[tokio::test]
async fn test_resaving_same_id_keeps_record_active() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();

    engine
        .save(&record("user.profile", "user", "profile", "v1"))
        .await
        .unwrap();
    engine
        .save(&record("user.profile", "user", "profile", "v2"))
        .await
        .unwrap();

    let res = engine.retrieve("user", 10).await.unwrap();
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].object, "v2");
    assert!(res[0].is_active);
}

#[tokio::test]
async fn test_note_pointer_reshadowing_same_note_stays_active() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();

    engine
        .add_note_pointer("note_9", "Old Pointer Title")
        .await
        .unwrap();
    engine
        .add_note_pointer("note_9", "New Pointer Title")
        .await
        .unwrap();

    let res = engine.retrieve("Pointer", 10).await.unwrap();
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].object, "New Pointer Title");
    assert!(res[0].is_active);
}
