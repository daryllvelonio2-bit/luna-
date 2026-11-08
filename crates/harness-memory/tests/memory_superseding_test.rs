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

#[tokio::test]
async fn test_forget_all_memories() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();

    engine
        .save(&record("user.profile", "user.profile", "profile", "Daryll"))
        .await
        .unwrap();
    engine
        .save(&record("env.system", "env.system", "profile", "Arch Linux"))
        .await
        .unwrap();

    assert_eq!(engine.retrieve("user", 5).await.unwrap().len(), 2);

    // Forget all via "all"
    engine.forget("all").await.unwrap();
    assert_eq!(engine.retrieve("user", 5).await.unwrap().len(), 0);
    assert_eq!(engine.retrieve("env", 5).await.unwrap().len(), 0);

    // Forget all via "delete it all"
    engine
        .save(&record("user.profile", "user.profile", "profile", "Daryll"))
        .await
        .unwrap();
    assert_eq!(engine.retrieve("user", 5).await.unwrap().len(), 1);
    engine.forget("delete it all").await.unwrap();
    assert_eq!(engine.retrieve("user", 5).await.unwrap().len(), 0);
}

#[tokio::test]
async fn test_forget_wildcard_memories() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();

    engine
        .save(&record(
            "user.skills.list",
            "user.skills.list",
            "profile",
            "Rust, Python",
        ))
        .await
        .unwrap();
    engine
        .save(&record("user.profile", "user.profile", "profile", "Daryll"))
        .await
        .unwrap();
    engine
        .save(&record(
            "project.luna",
            "project.luna",
            "profile",
            "Harness",
        ))
        .await
        .unwrap();

    assert_eq!(engine.retrieve("skills", 5).await.unwrap().len(), 3);

    // Wildcard forget "user.*"
    engine.forget("user.*").await.unwrap();
    assert!(engine
        .get_record("user.skills.list")
        .await
        .unwrap()
        .is_none());
    assert!(engine.get_record("user.profile").await.unwrap().is_none());
    assert!(engine.get_record("project.luna").await.unwrap().is_some());
    assert_eq!(engine.retrieve("skills", 5).await.unwrap().len(), 1);
}

#[tokio::test]
async fn test_forget_multiple_semicolon_delimited() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();

    engine
        .save(&record(
            "luna.binary.location",
            "luna.binary.location",
            "profile",
            "/usr/bin/luna",
        ))
        .await
        .unwrap();
    engine
        .save(&record(
            "user.skills.list",
            "user.skills.list",
            "profile",
            "Coding",
        ))
        .await
        .unwrap();
    engine
        .save(&record(
            "env.luna.provider",
            "env.luna.provider",
            "profile",
            "antigravity",
        ))
        .await
        .unwrap();

    engine
        .forget("luna.binary.location; user.skills.list; env.luna.provider")
        .await
        .unwrap();

    assert_eq!(engine.retrieve("location", 5).await.unwrap().len(), 0);
    assert_eq!(engine.retrieve("skills", 5).await.unwrap().len(), 0);
    assert_eq!(engine.retrieve("antigravity", 5).await.unwrap().len(), 0);
}

/// Uniqueness is `(subject, predicate)`, not `subject`: two different attributes of the same
/// entity are different facts and must both stay active.
#[tokio::test]
async fn test_different_predicates_of_one_subject_coexist() {
    let engine = SqliteMemoryEngine::in_memory().unwrap();

    engine
        .save(&record(
            "project.luna.build_command",
            "project.luna",
            "build_command",
            "cargo build -p harness-cli --bin luna",
        ))
        .await
        .unwrap();
    engine
        .save(&record(
            "project.luna.test_command",
            "project.luna",
            "test_command",
            "cargo test -p harness-core",
        ))
        .await
        .unwrap();

    let res = engine.retrieve("project.luna", 10).await.unwrap();
    assert_eq!(
        res.len(),
        2,
        "a new predicate must not deactivate the older attribute: {res:?}"
    );
    let objects: Vec<&str> = res.iter().map(|m| m.object.as_str()).collect();
    assert!(objects.contains(&"cargo build -p harness-cli --bin luna"));
    assert!(objects.contains(&"cargo test -p harness-core"));
}

#[tokio::test]
async fn test_update_memory_tool_partial_fields_preserves_existing() {
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    let engine = Arc::new(SqliteMemoryEngine::in_memory().unwrap());
    let mut initial = record(
        "user.profile",
        "user.profile",
        "profile",
        "Initial preference",
    );
    initial.name = Some("Daryll".to_string());
    initial.path = Some("/home/janelle".to_string());
    initial.category = MemoryCategory::UserProfile;
    engine.save(&initial).await.unwrap();

    let update_tool = UpdateMemoryTool::new(engine.clone());
    let cancel = CancellationToken::new();

    // Call update_memory passing ONLY memory_id and updated object/content
    let call_args = serde_json::json!({
        "memory_id": "user.profile",
        "object": "Updated aesthetic preference: plain text"
    });

    let res = update_tool
        .execute("call_1", &call_args, &cancel)
        .await
        .unwrap();
    assert!(!res.is_error);

    // Verify existing record's name, path, and category were preserved
    let updated = engine.get_record("user.profile").await.unwrap().unwrap();
    assert_eq!(updated.object, "Updated aesthetic preference: plain text");
    assert_eq!(updated.name.as_deref(), Some("Daryll"));
    assert_eq!(updated.path.as_deref(), Some("/home/janelle"));
    assert_eq!(updated.category, MemoryCategory::UserProfile);
}

#[tokio::test]
async fn test_forget_memory_tool_all() {
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    let engine = Arc::new(SqliteMemoryEngine::in_memory().unwrap());
    engine
        .save(&record("user.profile", "user.profile", "profile", "Daryll"))
        .await
        .unwrap();
    engine
        .save(&record(
            "project.luna",
            "project.luna",
            "profile",
            "Harness",
        ))
        .await
        .unwrap();

    let forget_tool = ForgetMemoryTool::new(engine.clone());
    let cancel = CancellationToken::new();

    let call_args = serde_json::json!({
        "memory_id": "all"
    });

    let res = forget_tool
        .execute("call_2", &call_args, &cancel)
        .await
        .unwrap();
    assert!(!res.is_error);

    assert_eq!(engine.retrieve("user", 5).await.unwrap().len(), 0);
    assert_eq!(engine.retrieve("luna", 5).await.unwrap().len(), 0);
}
