use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use harness_core::saver::{MemorySaveStatus, SaveReport, SaveStatus};
use harness_core::types::ToolCall;
use harness_core::{
    BackgroundSaver, CompletionOptions, HarnessError, MemoryCategory, MemoryEngine, MemoryRecord,
    Message, ModelProvider, NotesEngine, Role,
};
use harness_notes::FileNotesEngine;

use crate::SqliteMemoryEngine;

use super::evidence::{evidence_supported, strip_evidence_decoration, UNVERIFIED_TAG};
use super::model::{
    Inventory, MAX_MEMORY_WRITES_PER_PASS, MAX_NOTE_WRITES_PER_PASS, MESSAGE_CHAR_CAP,
    TRANSCRIPT_CHAR_BUDGET, TWO_PASS_THRESHOLD,
};
use super::parse::parse_distilled;
use super::save::MemorySaver;
use super::transcript::{render_transcript, truncate, truncate_block, Provenance};
use super::SaverPolicy;

/// A provider that answers with canned strings, and records the prompts it was given.
///
/// One answer is replayed for every call; several answers are consumed in order, which is what
/// a two-pass run needs.
struct CannedProvider {
    answers: std::sync::Mutex<Vec<String>>,
    seen: std::sync::Mutex<Vec<Message>>,
    fail: bool,
}

impl CannedProvider {
    fn new(answer: &str) -> Arc<Self> {
        Self::scripted(&[answer])
    }

    fn scripted(answers: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            answers: std::sync::Mutex::new(
                answers.iter().map(|answer| answer.to_string()).collect(),
            ),
            seen: std::sync::Mutex::new(Vec::new()),
            fail: false,
        })
    }

    fn failing() -> Arc<Self> {
        Arc::new(Self {
            answers: std::sync::Mutex::new(Vec::new()),
            seen: std::sync::Mutex::new(Vec::new()),
            fail: true,
        })
    }

    fn prompt(&self) -> String {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|message| message.role == Role::User)
            .filter_map(|message| message.content.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn systems(&self) -> String {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|message| message.role == Role::System)
            .filter_map(|message| message.content.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[async_trait]
impl ModelProvider for CannedProvider {
    fn provider_id(&self) -> &str {
        "canned"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["canned".to_string()])
    }

    async fn complete(
        &self,
        messages: &[Message],
        _tools: &[harness_core::ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(messages);
        if self.fail {
            return Err(HarnessError::ProviderError {
                provider: "canned".to_string(),
                message: "provider is down".to_string(),
            });
        }
        let answer = {
            let mut answers = self.answers.lock().unwrap_or_else(|e| e.into_inner());
            if answers.len() > 1 {
                answers.remove(0)
            } else {
                answers.first().cloned().unwrap_or_default()
            }
        };
        Ok(Message::assistant(answer))
    }

    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![vec![0.0]; texts.len()])
    }
}

const LONG_BUILD_LINE: &str = "I always build with cargo and run tests with cargo test, then I install the release binary to ~/.local/bin/luna so the next session uses the same build and never guesses the command";

fn conversation() -> Vec<Message> {
    vec![
        Message::system("system prompt"),
        Message::user(LONG_BUILD_LINE),
        Message::assistant("Noted: cargo, and `cargo test`."),
    ]
}

fn saver(
    provider: Arc<dyn ModelProvider>,
) -> (MemorySaver, Arc<SqliteMemoryEngine>, Arc<FileNotesEngine>) {
    let memory = Arc::new(SqliteMemoryEngine::in_memory().expect("memory db"));
    let notes = Arc::new(FileNotesEngine::in_memory().expect("notes dir"));
    let mut policy = SaverPolicy::new("canned");
    policy.two_pass = false;
    let saver = MemorySaver::new(provider, memory.clone(), notes.clone(), policy);
    (saver, memory, notes)
}

fn record(subject: &str, predicate: &str, object: &str) -> MemoryRecord {
    MemoryRecord {
        id: format!("{subject}.{predicate}"),
        subject: subject.to_string(),
        predicate: predicate.to_string(),
        object: object.to_string(),
        name: None,
        path: None,
        raw_content: format!("{subject} {predicate} {object}"),
        category: MemoryCategory::Fact,
        confidence: 1.0,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        is_active: true,
        superseded_by: None,
    }
}

#[tokio::test]
async fn a_memory_the_model_returns_is_written_through_the_save_memory_path() {
    let provider = CannedProvider::new(
        r#"{"memories":[{"subject":"user","predicate":"build_command","object":"cargo test","category":"workflow_rule","confidence":0.9}],"notes":[]}"#,
    );
    let (saver, memory, _notes) = saver(provider.clone());

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.memories, 1);
    assert_eq!(report.notes, 0);
    assert_eq!(report.skipped, 0);

    let stored = memory
        .retrieve_cards("build_command", 5)
        .await
        .expect("retrieves");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].id, "user.build_command");
    assert_eq!(stored[0].object, "cargo test");
    assert_eq!(stored[0].category, MemoryCategory::WorkflowRule);
    assert!((stored[0].confidence - 0.9).abs() < f32::EPSILON);

    assert_eq!(
        report.saved,
        vec![harness_core::SavedMemory {
            name: "user.build_command".to_string(),
            detail: Some("\"cargo test\"".to_string()),
            status: MemorySaveStatus::Saved,
        }]
    );
}

#[tokio::test]
async fn the_new_operation_shape_writes_a_memory_with_evidence() {
    let provider = CannedProvider::new(
        r#"{"operations":[{"op":"add_memory","memory":{"subject":"user","predicate":"preference.response_style","object":"concise, no filler","category":"user_profile"},"evidence":"I always build with cargo","confidence":0.95,"reason":"explicit preference"}]}"#,
    );
    let (saver, memory, _notes) = saver(provider);

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.memories, 1);
    let stored = memory
        .retrieve_cards("response_style", 5)
        .await
        .expect("retrieves");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].object, "concise, no filler");
    assert_eq!(stored[0].category, MemoryCategory::UserProfile);
}

#[tokio::test]
async fn a_long_verbatim_quote_is_truncated_and_still_accepted() {
    let long = "I always build with cargo. ".repeat(40);
    let conversation = vec![
        Message::system("system prompt"),
        Message::user(long.clone()),
    ];
    let answer = json!({
        "operations": [{
            "op": "add_memory",
            "memory": {"subject": "user", "predicate": "build", "object": "cargo"},
            "evidence": long,
            "confidence": 0.99
        }]
    })
    .to_string();
    let provider = CannedProvider::new(&answer);
    let (saver, memory, _notes) = saver(provider);

    let report = saver.distil(&conversation).await.expect("distils");

    assert_eq!(
        report.memories, 1,
        "a long verbatim quote must not be refused"
    );
    assert_eq!(
        memory
            .retrieve_cards("cargo", 5)
            .await
            .expect("retrieves")
            .len(),
        1
    );
}

#[tokio::test]
async fn evidence_that_is_not_in_the_transcript_is_refused() {
    let provider = CannedProvider::new(
        r#"{"operations":[{"op":"add_memory","memory":{"subject":"user","predicate":"p","object":"v"},"evidence":"I said something I never said","confidence":0.99}]}"#,
    );
    let (saver, memory, _notes) = saver(provider);

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.memories, 0);
    assert_eq!(report.skipped, 1);
    assert!(memory
        .retrieve_cards("", 10)
        .await
        .expect("retrieves")
        .is_empty());
    assert_eq!(
        report.outcomes[0].reason.as_deref(),
        Some("evidence not found in the conversation")
    );
}

#[tokio::test]
async fn a_noop_is_declined_rather_than_skipped() {
    let provider = CannedProvider::new(
        r#"{"operations":[{"op":"noop","reason":"hypothetical, not approved","confidence":0.0}]}"#,
    );
    let (saver, memory, _notes) = saver(provider);

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.memories, 0);
    assert_eq!(report.skipped, 0);
    assert_eq!(report.declined, 1);
    assert!(memory
        .retrieve_cards("", 10)
        .await
        .expect("retrieves")
        .is_empty());
}

#[tokio::test]
async fn an_update_operation_revises_the_target_in_place() {
    let provider = CannedProvider::new(
        r#"{"operations":[{"op":"update_memory","target_id":"user.build_command","memory":{"object":"cargo build --release"},"evidence":"I always build with cargo","confidence":0.9}]}"#,
    );
    let (saver, memory, _notes) = saver(provider);
    memory
        .save(&record("user", "build_command", "cargo test"))
        .await
        .expect("saves");

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.memories, 1);
    let stored = memory.retrieve_cards("", 10).await.expect("retrieves");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].object, "cargo build --release");
    assert_eq!(stored[0].id, "user.build_command");
    assert_eq!(report.saved[0].status, MemorySaveStatus::Updated);
}

#[tokio::test]
async fn a_delete_operation_deactivates_the_target() {
    let provider = CannedProvider::new(
        r#"{"operations":[{"op":"delete_memory","target_id":"user.build_command","evidence":"I always build with cargo","confidence":0.9,"reason":"replaced by the new build command"}]}"#,
    );
    let (saver, memory, _notes) = saver(provider);
    memory
        .save(&record("user", "build_command", "cargo test"))
        .await
        .expect("saves");

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.memories, 1);
    assert_eq!(report.saved[0].status, MemorySaveStatus::Deleted);
    assert!(memory
        .retrieve_cards("", 10)
        .await
        .expect("retrieves")
        .is_empty());
}

#[tokio::test]
async fn a_target_outside_the_inventory_is_refused() {
    let provider = CannedProvider::new(
        r#"{"operations":[{"op":"delete_memory","target_id":"invented.id","evidence":"I always build with cargo","confidence":0.9,"reason":"nope"}]}"#,
    );
    let (saver, _memory, _notes) = saver(provider);

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.memories, 0);
    assert_eq!(report.skipped, 1);
    assert_eq!(
        report.outcomes[0].reason.as_deref(),
        Some("target not found in inventory")
    );
}

#[tokio::test]
async fn a_secret_is_never_saved() {
    let provider = CannedProvider::new(
        r#"{"operations":[{"op":"add_memory","memory":{"subject":"env","predicate":"api_key","object":"sk-live-abcdefghijklmnopqrstuvwxyz"},"evidence":"I always build with cargo","confidence":0.99}]}"#,
    );
    let (saver, memory, _notes) = saver(provider);

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.memories, 0);
    assert_eq!(report.skipped, 1);
    assert!(memory
        .retrieve_cards("", 10)
        .await
        .expect("retrieves")
        .is_empty());
    assert_eq!(
        report.outcomes[0].reason.as_deref(),
        Some("secret detected")
    );
}

#[tokio::test]
async fn the_prompt_carries_the_rules_the_inventory_and_the_conversation() {
    let provider = CannedProvider::new(r#"{"operations":[]}"#);
    let (saver, memory, _notes) = saver(provider.clone());
    memory
        .save(&record("user", "profile", "Daryll, works on Luna"))
        .await
        .expect("saves");

    saver.distil(&conversation()).await.expect("distils");
    let prompt = provider.prompt();

    assert!(prompt.contains("user.profile = Daryll"), "{prompt}");
    assert!(prompt.contains("CURRENT MEMORY"), "{prompt}");
    assert!(prompt.contains("CURRENT NOTES"), "{prompt}");
    assert!(
        prompt.contains("USER: I always build with cargo"),
        "{prompt}"
    );
    assert!(prompt.contains("LUNA: Noted"), "{prompt}");
    assert!(
        !prompt.contains("system prompt"),
        "the system prompt leaked in"
    );

    let system = provider.systems();
    assert!(system.contains("memory keeper"), "{system}");
    assert!(system.contains("Memory is identity"), "{system}");
    assert!(
        system.contains("100% recall is the design requirement"),
        "{system}"
    );
    assert!(system.contains("Verbatim always"), "{system}");
    assert!(system.contains("VERBATIM PROTOCOL"), "{system}");
    assert!(
        system.contains("A repository or codebase brief"),
        "{system}"
    );
    assert!(system.contains("APPROVAL RULES"), "{system}");
    assert!(system.contains("CONTRADICTION RULES"), "{system}");
}

#[tokio::test]
async fn the_inventory_shows_note_ids_and_hides_their_pointers_from_the_memory_section() {
    let provider = CannedProvider::new(r#"{"operations":[]}"#);
    let (saver, memory, notes) = saver(provider.clone());
    let note = notes
        .save_note(
            "Build Recipe",
            "cargo build --release",
            &["build".to_string()],
        )
        .await
        .expect("saves note");
    memory
        .add_note_pointer(&note.id, &note.summary_pointer)
        .await
        .expect("pointer");

    saver.distil(&conversation()).await.expect("distils");
    let prompt = provider.prompt();

    assert!(prompt.contains("Build Recipe"), "{prompt}");
    assert!(prompt.contains(&note.id), "{prompt}");
    // The pointer record is a note pointer, not a memory card.
    assert!(
        !prompt.contains(&format!("mem_ptr_{} |", note.id)),
        "the pointer leaked into the memory section: {prompt}"
    );
}

#[tokio::test]
async fn a_fact_the_store_already_states_is_skipped_not_duplicated() {
    let provider = CannedProvider::new(
        r#"{"memories":[{"subject":"user","predicate":"build_command","object":"cargo test","category":"workflow_rule"}]}"#,
    );
    let (saver, memory, _notes) = saver(provider);

    let first = saver.distil(&conversation()).await.expect("distils");
    let second = saver.distil(&conversation()).await.expect("distils again");

    assert_eq!(first.memories, 1);
    assert_eq!(second.memories, 0);
    assert_eq!(second.skipped, 1, "a repeat must be reported as skipped");
    assert_eq!(
        memory
            .retrieve_cards("", 10)
            .await
            .expect("retrieves")
            .len(),
        1
    );
}

#[tokio::test]
async fn a_pass_is_capped_and_the_overflow_is_reported() {
    let memories: Vec<String> = (0..10)
        .map(|index| {
            format!(
                r#"{{"subject":"p{index}","predicate":"k","object":"v{index}","category":"fact"}}"#
            )
        })
        .collect();
    // Note bodies must be copied from the transcript, so each one repeats a phrase that
    // actually appears in the canned tail.
    let note_bodies: Vec<String> = (0..5)
        .map(|index| {
            format!(
                "Release note {index}: I always build with cargo and run tests with cargo test before installing the release binary to the local bin directory so the next session uses the same build and never guesses the command"
            )
        })
        .collect();
    let notes: Vec<String> = note_bodies
        .iter()
        .enumerate()
        .map(|(index, body)| format!(r#"{{"title":"Note {index}","content":"{body}","tags":[]}}"#))
        .collect();
    let provider = CannedProvider::new(&format!(
        r#"{{"memories":[{}],"notes":[{}]}}"#,
        memories.join(","),
        notes.join(",")
    ));
    let (saver, _memory, notes_engine) = saver(provider);
    let tail: Vec<Message> = note_bodies.iter().map(Message::user).collect();

    let report = saver.distil(&tail).await.expect("distils");

    assert_eq!(report.memories, MAX_MEMORY_WRITES_PER_PASS);
    assert_eq!(report.notes, MAX_NOTE_WRITES_PER_PASS);
    assert!(report.skipped >= 10);
    assert_eq!(
        notes_engine.list_notes(10, 0).await.expect("lists").len(),
        MAX_NOTE_WRITES_PER_PASS
    );
}

#[tokio::test]
async fn empty_or_malformed_items_are_skipped_and_counted() {
    let provider = CannedProvider::new(
        r#"{"memories":[
             {"subject":"","predicate":"p","object":"v"},
             {"subject":"user","predicate":"","object":"v"},
             {"subject":"user","predicate":"p","object":"   "},
             {"subject":"user","predicate":"p","object":"v"}
           ],
           "notes":[{"title":"Empty","content":""}]}"#,
    );
    let (saver, memory, notes) = saver(provider);

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.memories, 1);
    assert_eq!(report.notes, 0);
    assert_eq!(report.skipped, 4);
    assert_eq!(
        memory
            .retrieve_cards("", 10)
            .await
            .expect("retrieves")
            .len(),
        1
    );
    assert!(notes.list_notes(10, 0).await.expect("lists").is_empty());
}

#[tokio::test]
async fn a_note_is_saved_with_its_pointer_bridge() {
    let provider = CannedProvider::new(&format!(
        r#"{{"memories":[],"notes":[{{"title":"Build Recipe","content":"{}","tags":["build"]}}]}}"#,
        LONG_BUILD_LINE
    ));
    let (saver, memory, notes) = saver(provider);

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.notes, 1);
    let stored = notes.list_notes(10, 0).await.expect("lists");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].title, "Build Recipe");
    let pointers = memory
        .retrieve_cards("mem_ptr", 10)
        .await
        .expect("retrieves");
    assert!(
        !pointers.is_empty(),
        "the note pointer was not registered: {pointers:?}"
    );
    assert_eq!(report.saved_notes[0].name, "Build Recipe");
}

#[tokio::test]
async fn a_note_that_paraphrases_the_transcript_is_refused() {
    let provider = CannedProvider::new(
        r#"{"memories":[],"notes":[{"title":"Build Recipe","content":"The user builds with cargo and uses cargo test.","tags":[]}]}"#,
    );
    let (saver, memory, notes) = saver(provider);

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.notes, 0);
    assert_eq!(report.skipped, 1);
    assert!(notes.list_notes(10, 0).await.expect("lists").is_empty());
    assert!(
        memory
            .retrieve_cards("mem_ptr", 10)
            .await
            .expect("retrieves")
            .is_empty(),
        "a refused note must not leave a pointer behind"
    );
}

#[tokio::test]
async fn an_update_note_operation_appends_instead_of_creating_a_second_note() {
    let provider = CannedProvider::new(
        r###"{"operations":[{"op":"update_note","target_id":"build-recipe","note":{"mode":"append","content":"## Verified\n\nI always build with cargo"},"evidence":"I always build with cargo","confidence":0.9,"reason":"new verified step"}]}"###,
    );
    let (saver, _memory, notes) = saver(provider);
    notes
        .save_note("Build Recipe", "cargo test", &[])
        .await
        .expect("saves");

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.notes, 1);
    let stored = notes.list_notes(10, 0).await.expect("lists");
    assert_eq!(stored.len(), 1);
    assert!(stored[0].content.contains("cargo test"));
    assert!(stored[0].content.contains("I always build with cargo"));
    assert_eq!(report.saved_notes[0].status, SaveStatus::Updated);
}

#[tokio::test]
async fn a_delete_note_operation_archives_it_and_removes_the_pointer() {
    let provider = CannedProvider::new(
        r#"{"operations":[{"op":"delete_note","target_id":"build-recipe","evidence":"I always build with cargo","confidence":0.9,"reason":"obsolete"}]}"#,
    );
    let (saver, memory, notes) = saver(provider);
    let note = notes
        .save_note("Build Recipe", "cargo test", &[])
        .await
        .expect("saves");
    memory
        .add_note_pointer(&note.id, &note.summary_pointer)
        .await
        .expect("pointer");

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.notes, 1);
    assert_eq!(report.saved_notes[0].status, SaveStatus::Deleted);
    assert!(notes.list_notes(10, 0).await.expect("lists").is_empty());
    assert!(
        memory
            .retrieve_cards("mem_ptr", 10)
            .await
            .expect("retrieves")
            .is_empty(),
        "the pointer survived the note deletion"
    );
}

#[tokio::test]
async fn notes_can_be_switched_off_without_touching_memories() {
    let provider = CannedProvider::new(&format!(
        r#"{{"memories":[{{"subject":"user","predicate":"p","object":"v","category":"fact"}}],
             "notes":[{{"title":"Doc","content":"{}","tags":[]}}]}}"#,
        LONG_BUILD_LINE
    ));
    let memory = Arc::new(SqliteMemoryEngine::in_memory().expect("memory db"));
    let notes = Arc::new(FileNotesEngine::in_memory().expect("notes dir"));
    let mut policy = SaverPolicy::new("canned");
    policy.notes = false;
    policy.two_pass = false;
    let saver = MemorySaver::new(provider, memory, notes.clone(), policy);

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.memories, 1);
    assert_eq!(report.notes, 0);
    assert_eq!(report.skipped, 1);
    assert!(notes.list_notes(10, 0).await.expect("lists").is_empty());
}

#[tokio::test]
async fn memories_can_be_switched_off_without_touching_notes() {
    let provider = CannedProvider::new(&format!(
        r#"{{"memories":[{{"subject":"user","predicate":"p","object":"v","category":"fact"}}],
             "notes":[{{"title":"Doc","content":"{}","tags":[]}}]}}"#,
        LONG_BUILD_LINE
    ));
    let memory = Arc::new(SqliteMemoryEngine::in_memory().expect("memory db"));
    let notes = Arc::new(FileNotesEngine::in_memory().expect("notes dir"));
    let mut policy = SaverPolicy::new("canned");
    policy.two_pass = false;
    let saver = MemorySaver::new(provider, memory, notes.clone(), policy);
    saver.set_memory_enabled(false);

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.memories, 0);
    assert_eq!(report.notes, 1);
    assert_eq!(report.skipped, 1);
    assert_eq!(notes.list_notes(10, 0).await.expect("lists").len(), 1);
}

#[tokio::test]
async fn two_pass_runs_the_scout_before_the_curator() {
    // A store large enough to trigger the split.
    let memory = Arc::new(SqliteMemoryEngine::in_memory().expect("memory db"));
    let notes = Arc::new(FileNotesEngine::in_memory().expect("notes dir"));
    for index in 0..TWO_PASS_THRESHOLD + 1 {
        memory
            .save(&record("p", &format!("k{index}"), "v"))
            .await
            .expect("saves");
    }

    let provider = CannedProvider::scripted(&[
        r#"{"candidates":[{"kind":"preference","text":"prefers cargo","evidence":"I always build with cargo","confidence":0.9,"scope":"user","storage":"memory"}]}"#,
        r#"{"operations":[{"op":"add_memory","memory":{"subject":"user","predicate":"build_tool","object":"cargo"},"evidence":"I always build with cargo","confidence":0.9}]}"#,
    ]);
    let saver = MemorySaver::new(
        provider.clone(),
        memory.clone(),
        notes,
        SaverPolicy::new("canned"),
    );

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.memories, 1);
    let systems = provider.systems();
    assert!(systems.contains("memory scout"), "{systems}");
    assert!(systems.contains("memory keeper"), "{systems}");
    assert!(systems.contains("verbatim"), "{systems}");
    assert!(provider.prompt().contains("CANDIDATES"));
}

#[tokio::test]
async fn an_empty_candidate_set_skips_the_curator_call() {
    let memory = Arc::new(SqliteMemoryEngine::in_memory().expect("memory db"));
    let notes = Arc::new(FileNotesEngine::in_memory().expect("notes dir"));
    for index in 0..TWO_PASS_THRESHOLD + 1 {
        memory
            .save(&record("p", &format!("k{index}"), "v"))
            .await
            .expect("saves");
    }
    let provider = CannedProvider::scripted(&[r#"{"candidates":[]}"#]);
    let saver = MemorySaver::new(provider, memory, notes, SaverPolicy::new("canned"));

    let report = saver.distil(&conversation()).await.expect("distils");

    assert!(report.is_empty());
    assert_eq!(report.skipped, 0);
}

#[tokio::test]
async fn prose_instead_of_json_is_an_error_naming_what_it_saw() {
    let provider = CannedProvider::new("I have saved three memories for you!");
    let (saver, memory, _notes) = saver(provider);

    let error = saver.distil(&conversation()).await.unwrap_err().to_string();

    assert!(error.contains("did not answer with JSON"), "{error}");
    assert!(error.contains("three memories"), "{error}");
    assert!(memory
        .retrieve_cards("", 10)
        .await
        .expect("retrieves")
        .is_empty());
}

#[tokio::test]
async fn a_provider_failure_surfaces_instead_of_writing_half_a_pass() {
    let (saver, memory, _notes) = saver(CannedProvider::failing());

    assert!(saver.distil(&conversation()).await.is_err());
    assert!(memory
        .retrieve_cards("", 10)
        .await
        .expect("retrieves")
        .is_empty());
}

#[test]
fn a_fenced_answer_is_read_and_a_bare_one_too() {
    assert!(parse_distilled("```json\n{\"operations\":[]}\n```").is_ok());
    assert!(parse_distilled("{\"operations\":[]}").is_ok());
    assert!(parse_distilled("Sure! {\"memories\":[],\"notes\":[]}").is_ok());
    assert!(parse_distilled("no json at all").is_err());
}

#[test]
fn common_model_shapes_are_normalized_before_planning() {
    let distilled = parse_distilled(
        r#"{"operations":[
            {"type":"add_memory","subject":"garden-defense","predicate":"build command","object":"rojo build"},
            {"action":"create_note","title":"Build","content":"rojo build -o build.rbxl"},
            {"operation":"noop","reason":"nothing durable"},
            {"memory":{"subject":"a","predicate":"b","object":"c"}},
            {"add_memory":{"subject":"d","predicate":"e","object":"f"},"confidence":0.9}
        ]}"#,
    )
    .expect("parses");
    let ops = distilled.into_operations();
    assert_eq!(ops[0].op, "add_memory");
    assert_eq!(ops[0].memory.as_ref().expect("memory").object, "rojo build");
    assert_eq!(ops[1].op, "create_note");
    assert_eq!(ops[1].note.as_ref().expect("note").title, "Build");
    assert_eq!(ops[2].op, "noop");
    assert_eq!(ops[3].op, "add_memory");
    assert_eq!(ops[4].op, "add_memory");
    assert_eq!(ops[4].memory.as_ref().expect("memory").object, "f");
    assert_eq!(ops[4].confidence, Some(0.9));
}

#[test]
fn a_keyed_operation_may_wrap_its_payload_one_level_too_deep() {
    let distilled = parse_distilled(
        r#"{"operations":[
            {"add_memory":{"memory":{"subject":"project","predicate":"toolchain","object":"rokit"}}},
            {"create_note":{"note":{"title":"Runbook","content":"verify.sh"}}}
        ]}"#,
    )
    .expect("parses");
    let ops = distilled.into_operations();
    assert_eq!(ops[0].op, "add_memory");
    assert_eq!(ops[0].memory.as_ref().expect("memory").object, "rokit");
    assert_eq!(ops[1].op, "create_note");
    assert_eq!(ops[1].note.as_ref().expect("note").title, "Runbook");
}

#[test]
fn a_keyed_operation_carries_its_evidence_and_reason_from_the_wrapper() {
    let distilled = parse_distilled(
        r#"{"operations":[
            {"add_memory":{"memory":{"subject":"pure-gpt","predicate":"build command","object":"cargo build --release"},"evidence":"I always build with cargo","confidence":0.95,"reason":"explicit preference"}}
        ]}"#,
    )
    .expect("parses");
    let ops = distilled.into_operations();
    assert_eq!(ops[0].op, "add_memory");
    assert_eq!(
        ops[0].evidence.as_deref(),
        Some("I always build with cargo")
    );
    assert_eq!(ops[0].confidence, Some(0.95));
    assert_eq!(ops[0].reason.as_deref(), Some("explicit preference"));
}

#[test]
fn a_role_label_or_surrounding_quotes_do_not_hide_a_verbatim_quote() {
    assert_eq!(
        strip_evidence_decoration(r#"USER: "I always build with cargo""#),
        "I always build with cargo"
    );
    assert_eq!(
        strip_evidence_decoration("Assistant — cargo test --all"),
        "cargo test --all"
    );
    assert_eq!(
        strip_evidence_decoration("LUNA: pure-gpt is a Rust runner..."),
        "pure-gpt is a Rust runner"
    );
    assert_eq!(strip_evidence_decoration("plain quote"), "plain quote");
}

#[test]
fn a_truncated_answer_is_closed_when_what_it_contains_is_complete() {
    assert!(parse_distilled(r#"{"operations":[{"op":"noop","reason":"x"}"#).is_ok());
    assert!(parse_distilled(r#"{"operations":[{"op":"noop","reason":"x"#).is_ok());
}

#[test]
fn a_trailing_object_or_note_does_not_invalidate_the_first_answer() {
    assert!(parse_distilled("{\"operations\":[]}{\"operations\":[]}").is_ok());
    assert!(parse_distilled("{\"operations\":[]} That is all.").is_ok());
    // Braces inside strings must not end the object early.
    assert!(parse_distilled("{\"operations\":[],\"note\":\"use { and }\"}").is_ok());
}

#[test]
fn the_transcript_renders_speakers_calls_and_stays_inside_its_budget() {
    let mut call = Message::assistant("");
    call.tool_calls = vec![ToolCall {
        id: "1".to_string(),
        name: "terminal".to_string(),
        arguments: json!({}),
        signature: None,
    }];
    let rendered = render_transcript(&[
        Message::system("dropped"),
        Message::user("hi"),
        call,
        Message::tool(harness_core::ToolResult::success("1", "ok")),
    ]);
    assert!(rendered.contains("USER: hi"), "{rendered}");
    assert!(rendered.contains("LUNA: (called terminal)"), "{rendered}");
    assert!(!rendered.contains("dropped"), "{rendered}");

    let long: Vec<Message> = (0..20)
        .map(|_| Message::user("z".repeat(MESSAGE_CHAR_CAP)))
        .collect();
    let rendered = render_transcript(&long);
    assert!(
        rendered.chars().count() <= TRANSCRIPT_CHAR_BUDGET + 32,
        "budget blown: {}",
        rendered.chars().count()
    );
    assert!(
        rendered.starts_with("(earlier turns omitted)"),
        "{rendered}"
    );
}

async fn transcripts(memory: &SqliteMemoryEngine) -> Vec<MemoryRecord> {
    memory
        .retrieve("", 100)
        .await
        .expect("retrieves")
        .into_iter()
        .filter(|record| record.category == MemoryCategory::Transcript)
        .collect()
}

#[tokio::test]
async fn raw_exchanges_are_filed_verbatim_even_when_the_model_fails() {
    let (saver, memory, _notes) = saver(CannedProvider::failing());
    let tail = vec![
        Message::user("I build this repo with cargo build --release"),
        Message::assistant("Understood: `cargo build --release`."),
        Message::tool(harness_core::ToolResult::success(
            "call-1",
            "Finished release [optimized] target(s)",
        )),
        Message::user("Tests run with cargo test --all"),
        Message::assistant("And `cargo test --all`."),
    ];

    // The provider is down, but the raw exchange is the part that must not depend on it.
    assert!(saver.distil(&tail).await.is_err());

    let stored = transcripts(&memory).await;
    assert_eq!(stored.len(), 2, "{stored:?}");
    let bodies: Vec<&str> = stored
        .iter()
        .map(|record| record.raw_content.as_str())
        .collect();
    assert!(
        bodies.contains(
            &"> I build this repo with cargo build --release\n\nUnderstood: `cargo build --release`."
        ),
        "{bodies:?}"
    );
    assert!(
        bodies.contains(&"> Tests run with cargo test --all\n\nAnd `cargo test --all`."),
        "{bodies:?}"
    );
    assert!(
        stored
            .iter()
            .all(|record| !record.raw_content.contains("Finished release")),
        "tool output must not be filed as conversation: {stored:?}"
    );
    // The always-loaded block stays cards-only.
    assert!(memory
        .retrieve_cards("", 10)
        .await
        .expect("retrieves")
        .is_empty());
}

#[tokio::test]
async fn raw_capture_is_idempotent_across_overlapping_tails() {
    let (saver, memory, _notes) = saver(CannedProvider::new(
        r#"{"operations":[{"op":"noop","reason":"nothing"}]}"#,
    ));
    let tail = vec![
        Message::user("deploy with make release"),
        Message::assistant("Deploys use `make release`."),
    ];

    saver.distil(&tail).await.expect("distils");
    // The interval pass and the idle pass see overlapping windows. The second read must not
    // create a second copy of the same exchange.
    saver.distil(&tail).await.expect("distils again");
    assert_eq!(transcripts(&memory).await.len(), 1);

    let mut extended = tail.clone();
    extended.push(Message::user("Rollback with make rollback"));
    extended.push(Message::assistant("Rollback uses `make rollback`."));
    saver.distil(&extended).await.expect("distils the new tail");
    assert_eq!(transcripts(&memory).await.len(), 2);
}

#[tokio::test]
async fn memory_off_skips_raw_capture() {
    let (saver, memory, _notes) = saver(CannedProvider::new(
        r#"{"operations":[{"op":"noop","reason":"nothing"}]}"#,
    ));
    saver.set_memory_enabled(false);
    saver
        .distil(&[Message::user("remember this"), Message::assistant("stored")])
        .await
        .expect("distils");
    assert!(transcripts(&memory).await.is_empty());
}

#[test]
fn truncation_marks_what_it_cut() {
    assert_eq!(truncate("abcdef", 10), "abcdef");
    assert_eq!(truncate("abcdef", 4), "abc…");
    assert_eq!(truncate_block("abcdef", 10), "abcdef");
    assert!(truncate_block(&"z".repeat(100), 20).contains("INVENTORY TRUNCATED"));
}

#[test]
fn evidence_matching_ignores_case_and_whitespace_but_not_paraphrase() {
    let transcript = "USER: I always   build with cargo\n";
    assert!(evidence_supported("i always build with cargo", transcript));
    assert!(!evidence_supported(
        "I usually build with cargo",
        transcript
    ));
    assert!(!evidence_supported("", transcript));
}

#[test]
fn provenance_names_the_speaker_and_prefers_a_user_or_tool_line() {
    let transcript = render_transcript(&[
        Message::user("run cargo test"),
        Message::assistant("I will run cargo test now"),
        Message::tool(harness_core::ToolResult::success(
            "1",
            "cargo test: 12 passed",
        )),
    ]);
    assert_eq!(
        transcript.provenance_of("run cargo test"),
        Some(Provenance::User)
    );
    assert_eq!(
        transcript.provenance_of("cargo test: 12 passed"),
        Some(Provenance::Tool)
    );
    assert_eq!(
        transcript.provenance_of("I will run cargo test now"),
        Some(Provenance::Assistant)
    );
    assert_eq!(transcript.provenance_of("not in the transcript"), None);
}

#[tokio::test]
async fn an_assistant_only_claim_cannot_mint_a_memory_card() {
    let provider = CannedProvider::new(
        r#"{"operations":[{"op":"add_memory","memory":{"subject":"pure-gpt","predicate":"tests","object":"none"},"evidence":"Noted: cargo, and `cargo test`.","confidence":0.95,"reason":"assistant said so"}]}"#,
    );
    let (saver, memory, _notes) = saver(provider);

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.memories, 0, "{report:?}");
    assert!(
        memory
            .retrieve_cards("", 10)
            .await
            .expect("retrieves")
            .is_empty(),
        "an assistant-only claim became a card"
    );
}

#[tokio::test]
async fn an_assistant_only_note_is_refused() {
    let answer = "Noted: cargo, and `cargo test`. The build uses cargo and the tests use cargo test, and the release binary is installed into the local bin directory so the next session reuses it.";
    let tail = vec![
        Message::user("How do I build this project?"),
        Message::assistant(answer),
    ];
    let provider = CannedProvider::new(&format!(
        r#"{{"operations":[{{"op":"create_note","note":{{"title":"Build Notes","content":"{answer}","tags":["build"]}},"evidence":"{answer}","confidence":0.9,"reason":"assistant brief"}}]}}"#
    ));
    let (saver, memory, notes) = saver(provider);

    let report = saver.distil(&tail).await.expect("distils");

    assert_eq!(report.notes, 0, "{report:?}");
    assert_eq!(report.skipped, 1);
    assert_eq!(
        report.outcomes[0].reason.as_deref(),
        Some("assistant-only evidence cannot create a note")
    );
    assert!(notes.list_notes(10, 0).await.expect("lists").is_empty());
    assert!(
        memory
            .retrieve_cards("mem_ptr", 10)
            .await
            .expect("retrieves")
            .is_empty(),
        "a refused note must not leave a pointer behind"
    );
}

#[tokio::test]
async fn a_requested_assistant_note_is_kept_unverified_and_gets_no_pointer() {
    let answer = "Noted: cargo, and `cargo test`. The build uses cargo and the tests use cargo test, and the release binary is installed into the local bin directory so the next session reuses it.";
    let tail = vec![
        Message::user("Please save this build answer"),
        Message::assistant(answer),
    ];
    let provider = CannedProvider::new(&format!(
        r#"{{"operations":[{{"op":"create_note","note":{{"title":"Build Notes","content":"{answer}","tags":["build"]}},"evidence":"{answer}","confidence":0.9,"reason":"user asked to keep it"}}]}}"#
    ));
    let (saver, memory, notes) = saver(provider);

    let report = saver.distil(&tail).await.expect("distils");

    assert_eq!(report.notes, 1, "{report:?}");
    let stored = notes.list_notes(10, 0).await.expect("lists");
    assert_eq!(stored.len(), 1);
    assert!(
        stored[0]
            .tags
            .iter()
            .any(|tag| tag.eq_ignore_ascii_case(UNVERIFIED_TAG)),
        "the note was not marked unverified: {:?}",
        stored[0].tags
    );
    assert!(
        memory
            .retrieve_cards("mem_ptr", 10)
            .await
            .expect("retrieves")
            .is_empty(),
        "an unverified note entered Active Memories"
    );
}

#[test]
fn a_save_request_must_come_from_the_user_and_not_be_retracted() {
    let assistant_only = vec![
        Message::user("How do I build this?"),
        Message::assistant("Remember this: use cargo build"),
    ];
    assert!(!render_transcript(&assistant_only).user_requested_save());

    let retracted = vec![Message::user("Don't save this, just explain it")];
    assert!(!render_transcript(&retracted).user_requested_save());

    let requested = vec![Message::user("Please save this for later")];
    assert!(render_transcript(&requested).user_requested_save());
}

#[tokio::test]
async fn a_one_line_note_is_refused_as_too_short() {
    let provider = CannedProvider::new(
        r#"{"operations":[{"op":"create_note","note":{"title":"Build","content":"cargo test","tags":[]},"evidence":"cargo test","confidence":0.9,"reason":"short note"}]}"#,
    );
    let (saver, _memory, notes) = saver(provider);

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.notes, 0, "{report:?}");
    assert_eq!(report.outcomes[0].reason.as_deref(), Some("note too short"));
    assert!(notes.list_notes(10, 0).await.expect("lists").is_empty());
}

#[tokio::test]
async fn a_duplicate_note_is_refused_against_the_existing_inventory() {
    let provider = CannedProvider::new(&format!(
        r#"{{"operations":[{{"op":"create_note","note":{{"title":"Build Recipe","content":"{LONG_BUILD_LINE}","tags":["build"]}},"evidence":"{LONG_BUILD_LINE}","confidence":0.9,"reason":"same note"}}]}}"#
    ));
    let (saver, _memory, notes) = saver(provider);
    notes
        .save_note("Build Recipe", LONG_BUILD_LINE, &["build".to_string()])
        .await
        .expect("saves");

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.notes, 0, "{report:?}");
    assert_eq!(report.outcomes[0].reason.as_deref(), Some("already known"));
    assert_eq!(notes.list_notes(10, 0).await.expect("lists").len(), 1);
}

#[tokio::test]
async fn a_malformed_first_response_is_recovered_by_the_repair_retry() {
    let provider = CannedProvider::scripted(&[
        "this is not valid json at all",
        r#"{"operations":[{"op":"add_memory","memory":{"subject":"user","predicate":"preference.build","object":"cargo"},"evidence":"I always build with cargo","confidence":0.95}]}"#,
    ]);
    let (saver, memory, _notes) = saver(provider);

    let report = saver.distil(&conversation()).await.expect("distils");

    assert_eq!(report.memories, 1);
    let stored = memory.retrieve_cards("", 10).await.expect("retrieves");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].object, "cargo");
}

#[tokio::test]
async fn a_stitched_note_across_different_turns_is_refused() {
    let multi_turn = vec![
        Message::user("First topic is apples"),
        Message::assistant("Understood apples"),
        Message::user("Second topic is oranges"),
        Message::assistant("Understood oranges"),
    ];
    let provider = CannedProvider::new(
        r###"{"operations":[{"op":"create_note","note":{"title":"Fruits","content":"## Apples\n\nFirst topic is apples\n\n## Oranges\n\nSecond topic is oranges","tags":["fruit"]},"evidence":"First topic is apples","confidence":0.9}]}"###,
    );
    let (saver, _memory, notes) = saver(provider);

    let report = saver.distil(&multi_turn).await.expect("distils");

    assert_eq!(report.notes, 0);
    assert_eq!(report.skipped, 1);
    assert!(notes.list_notes(10, 0).await.expect("lists").is_empty());
}

#[test]
fn json_parser_skips_schema_echoes_and_repairs_unquoted_keys_and_truncated_tails() {
    let with_schema_echo = r#"Thinking: I need to emit {"operations": [...]} with {subject, predicate, object, name?, path?, category?}.
{"operations":[{"op":"add_memory","memory":{subject:"user.preference",predicate:"voice",object:"Kokoro am_michael",category:"preference",},"evidence":"I like am_michael","confidence":0.95}]}"#;
    let parsed = parse_distilled(with_schema_echo).expect("skips schema echo and repairs keys");
    let ops = parsed.into_operations();
    assert_eq!(ops.len(), 1);
    let item = ops[0].memory.as_ref().expect("memory item");
    assert_eq!(item.subject, "user.preference");
    assert_eq!(item.predicate, "voice");
    assert_eq!(item.object, "Kokoro am_michael");

    let truncated_second_op = r#"{"operations":[{"add_memory":{"memory":{"subject":"user.preference.voice","predicate":"=","object":"Kokoro am_michael","category":"preference"}},"evidence":"USER: I like am_michael","confidence":0.9},{"add_memory":{"memory":{"subject":"user.pref"#;
    let recovered =
        parse_distilled(truncated_second_op).expect("recovers completed first operation");
    let recovered_ops = recovered.into_operations();
    // The parser successfully closes the truncated second operation without throwing away the
    // payload; the planner's validation (`plan_one`) then filters out the empty second operation
    // (empty object/evidence) while keeping the complete first operation.
    assert_eq!(recovered_ops[0].op, "add_memory");
    let first = recovered_ops[0].memory.as_ref().expect("first memory item");
    assert_eq!(first.subject, "user.preference");
    assert_eq!(first.predicate, "voice");
    assert_eq!(first.object, "Kokoro am_michael");

    // When passed through plan_one, only the valid complete operation survives
    let (saver, _memory, _notes) = saver(CannedProvider::new(truncated_second_op));
    let transcript = render_transcript(&[Message::user("I like am_michael")]);
    let inventory = Inventory {
        memories: Vec::new(),
        notes: Vec::new(),
        pointers: HashMap::new(),
        memories_truncated: 0,
        notes_truncated: 0,
    };
    let mut report = SaveReport::default();
    let planned: Vec<_> = recovered_ops
        .into_iter()
        .filter_map(|op| saver.plan_one(op, &inventory, &transcript, &mut report))
        .collect();
    assert_eq!(planned.len(), 1);
}

#[test]
fn evidence_matching_tolerates_markdown_formatting_and_mid_quote_ellipses() {
    let transcript = "USER: I always build with `cargo build --release` and then install the binary to ~/.local/bin/luna\n";
    assert!(evidence_supported(
        "I always build with cargo build --release",
        transcript
    ));
    assert!(evidence_supported(
        "I always build with ... install the binary to ~/.local/bin/luna",
        transcript
    ));
    assert!(!evidence_supported(
        "install the binary ... I always build with",
        transcript
    ));
}
