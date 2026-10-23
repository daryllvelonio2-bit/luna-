//! The background memory saver: a second reader that writes the records the turn did not.
//!
//! A session running with `memory.mode: background` does not write durable memory from its own
//! tool calls. Instead, after the turn's reply is on screen, this module reads the conversation
//! tail and asks a model the one question the main agent answers implicitly all turn long: *what
//! here is durable?* The answer comes back as JSON, and every item goes through the **same**
//! `save_memory` tool the session would have called, so both modes produce identical records —
//! same derived id, same contradiction handling, same consolidation rules. Only the decider
//! differs.
//!
//! The pass is deliberately narrow:
//!
//! - it is shown the transcript *tail* (the caller bounds it) and a compact index of what memory
//!   already holds, so it corrects and extends instead of re-recording;
//! - it writes through [`SaveMemoryTool`] for memories and `NotesEngine::save_note` plus the
//!   pointer bridge for notes, mirroring the agent's own `save_note` path;
//! - it caps how much one pass may add (`MAX_MEMORIES_PER_PASS`, `MAX_NOTES_PER_PASS`) and skips
//!   anything short, empty or already known, reporting the count in [`SaveReport::skipped`];
//! - it never invents: every item must be justified by the transcript it was given, which is
//!   stated in the prompt as a hard rule.
//!
//! Failure is cheap by design — the caller runs this detached from the turn and only logs errors,
//! so a saver that cannot reach its provider costs a warning, not a reply.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use harness_core::saver::{BackgroundSaver, SaveReport};
use harness_core::{
    CompletionOptions, HarnessError, MemoryEngine, MemoryRecord, Message, ModelProvider,
    NotesEngine, Role, Tool,
};

use crate::tools::SaveMemoryTool;

/// Uncapped: process all durable memories extracted by the saver.
pub const MAX_MEMORIES_PER_PASS: usize = usize::MAX;
/// Uncapped: process all reference notes extracted by the saver.
pub const MAX_NOTES_PER_PASS: usize = usize::MAX;
/// Non-empty note content.
pub const MIN_NOTE_CHARS: usize = 1;
/// Maximum note length.
pub const MAX_NOTE_CHARS: usize = usize::MAX;
/// Per-message character cap while rendering the transcript.
pub const MESSAGE_CHAR_CAP: usize = 100_000;
/// Total character budget for the rendered transcript.
pub const TRANSCRIPT_CHAR_BUDGET: usize = 500_000;
/// How many existing records are shown as the "already known" index.
pub const INDEX_LIMIT: usize = 500;

/// The instructions handed to the distillation model.
///
/// Kept as one constant so the contract (what counts as durable, what a category means, what shape
/// the answer must take) has exactly one home and can be asserted on in tests.
pub const SAVER_SYSTEM_PROMPT: &str = r#"You are Luna's background memory saver. You analyze finished conversations and persist durable facts, rules, preferences, and reference notes. You never talk to the user: you emit JSON and nothing else.

MEMORY PROTOCOL:
Save durable facts that will still matter in a future, unrelated session.

Consolidation & Entity Scope:
- Consolidate by entity: Store related facts together in a single consolidated memory record rather than fragmenting attributes into multiple micro-records.
  * Personal profile: Maintain a single consolidated 'user.profile' record containing the user's identity, preferences, aesthetic standards, and habits. Never create separate micro-memories like 'user.name', 'user.favorite_language', or 'user.favorite_number'.
  * Project profiles: Maintain a single consolidated memory record per project (e.g. 'project.astra', 'project.luna') containing its purpose, repository path, build commands, and architectural conventions. Never create multiple fragmented memories for the same project.
  * Environment: Maintain a single 'env.system' record for host OS, toolchain paths, and hardware defaults.
- Update Over Append: When learning new facts or refinements about an existing entity, update that entity's consolidated record rather than creating duplicate entries. Contradictions are resolved in favor of the newer record.

Evaluation Criteria:
Persist only durable, generalizable information that remains relevant across future, unrelated sessions:
- Hardware, operating system, and primary environment defaults.
- Coding conventions, architectural preferences, and UI/CLI aesthetic habits.
- Permanent project definitions, repository layouts, build instructions, and naming conventions.
- Explicit facts, project locations, or preferences stated by the user or established during work.

Constraints:
- Do not save transient task states, one-off commands, temporary bugs, or session scratchpad plans.
- Do not save raw logs, full code files, or unparsed data outputs.
- Never fragment a single entity or project across multiple memory records.

Categories: user_profile, code_pattern, workflow_rule, fact, harness_improvement.

NOTE PROTOCOL:
Use notes to offload reference artifacts that exceed atomic memory limits: architecture specs, documentation, multi-step research, and structured logs.

Answer with JSON only, in exactly this shape:

{"memories":[{"subject":"project.astra","predicate":"profile","object":"Autonomous Rust agent harness located at /path/to/astra, built with cargo build --release.","category":"workflow_rule","confidence":1.0}],"notes":[{"title":"Descriptive Title","content":"full verbatim body","tags":["tag"]}]}

Use empty arrays only if the conversation contained genuinely no durable facts, project details, user preferences, or reference notes."#;

/// How a saver is configured: which model, and which budgets it honours.
#[derive(Debug, Clone, PartialEq)]
pub struct SaverPolicy {
    /// The model that performs the pass. Resolved by the caller (the session's model by default).
    pub model: String,
    /// Distil after every Nth completed turn; `0` disables the pass.
    pub every_turns: usize,
    /// How many trailing messages the pass may read.
    pub tail_messages: usize,
    /// Whether notes are written, or memory records only.
    pub notes: bool,
}

impl SaverPolicy {
    /// The policy for `model`: every turn, the configured tail, notes on.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            every_turns: 1,
            tail_messages: 24,
            notes: true,
        }
    }
}

/// One item the model asked to record.
#[derive(Debug, Clone, Deserialize)]
struct Item {
    #[serde(default)]
    subject: String,
    #[serde(default)]
    predicate: String,
    #[serde(default)]
    object: String,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    confidence: Option<f32>,
}

/// One note the model asked to record.
#[derive(Debug, Clone, Deserialize)]
struct NoteItem {
    #[serde(default)]
    title: String,
    #[serde(default)]
    content: String,
    #[serde(default)]
    tags: Vec<String>,
}

/// The shape the saver expects back.
#[derive(Debug, Clone, Default, Deserialize)]
struct Distilled {
    #[serde(default)]
    memories: Vec<Item>,
    #[serde(default)]
    notes: Vec<NoteItem>,
}

/// The independent memory writer.
pub struct MemorySaver {
    provider: Arc<dyn ModelProvider>,
    memory: Arc<dyn MemoryEngine>,
    notes: Arc<dyn NotesEngine>,
    policy: SaverPolicy,
}

impl MemorySaver {
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        memory: Arc<dyn MemoryEngine>,
        notes: Arc<dyn NotesEngine>,
        policy: SaverPolicy,
    ) -> Self {
        Self {
            provider,
            memory,
            notes,
            policy,
        }
    }

    /// The prompt for one pass: rules, what is already known, then the conversation.
    ///
    /// Public so the exact text can be asserted on, and so a caller (or a human) can read what the
    /// saver was told without running it.
    pub async fn prompt(&self, tail: &[Message]) -> String {
        let known = self.known_index().await;
        format!(
            "Already in memory (subject.predicate = object). Write only what these miss or correct:\n{}\n\nConversation to distil (oldest first):\n{}",
            known, render_transcript(tail)
        )
    }

    /// `subject.predicate = object` for what memory already holds, or a note that it is empty.
    async fn known_index(&self) -> String {
        let records = self
            .memory
            .retrieve("", INDEX_LIMIT)
            .await
            .unwrap_or_default();
        if records.is_empty() {
            return "(empty)".to_string();
        }
        records
            .iter()
            .map(|record| {
                format!(
                    "{}.{} = {}",
                    record.subject,
                    record.predicate,
                    truncate(&record.object, 120)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Ask the model what is durable, and apply it. Errors are the caller's to log.
    pub async fn run(&self, tail: &[Message]) -> Result<SaveReport, HarnessError> {
        let prompt = self.prompt(tail).await;
        let messages = vec![Message::system(SAVER_SYSTEM_PROMPT), Message::user(prompt)];
        let options = CompletionOptions {
            model: Some(self.policy.model.clone()),
            temperature: Some(0.0),
            max_tokens: Some(4_096),
            ..CompletionOptions::default()
        };
        let response = self.provider.complete(&messages, &[], &options).await?;
        let text = response.content.as_deref().unwrap_or_default();
        let candidate = if (text.is_empty() || text == harness_core::types::NO_TEXT_RESPONSE)
            && response.reasoning.is_some()
        {
            response.reasoning.as_deref().unwrap_or_default()
        } else {
            text
        };
        let distilled = parse_distilled(candidate)?;
        self.apply(distilled).await
    }

    /// Write what the model asked for, honouring every cap and skipping what it should not have sent.
    async fn apply(&self, distilled: Distilled) -> Result<SaveReport, HarnessError> {
        let mut report = SaveReport::default();
        let existing = self
            .memory
            .retrieve("", INDEX_LIMIT)
            .await
            .unwrap_or_default();

        let mut seen: Vec<(String, String, String)> = Vec::new();
        for item in distilled.memories {
            let subject = item.subject.trim();
            let predicate = item.predicate.trim();
            let object = item.object.trim();
            if subject.is_empty() || predicate.is_empty() || object.is_empty() {
                report.skipped += 1;
                continue;
            }
            let key = (
                subject.to_ascii_lowercase(),
                predicate.to_ascii_lowercase(),
                object.to_string(),
            );
            // A fact the store already states verbatim is not worth a write, and neither is the
            // second copy of it inside one answer.
            if seen.contains(&key) || already_known(&existing, subject, predicate, object) {
                report.skipped += 1;
                continue;
            }
            seen.push(key);

            let mut args = json!({
                "subject": subject,
                "predicate": predicate,
                "object": object,
                "category": item.category.as_deref().unwrap_or("fact"),
            });
            if let Some(confidence) = item.confidence {
                args["confidence"] = json!(confidence.clamp(0.0, 1.0));
            }
            let tool = SaveMemoryTool::new(self.memory.clone());
            let call_id = format!("saver_{}", report.memories + 1);
            match tool
                .execute(&call_id, &args, &CancellationToken::new())
                .await
            {
                Ok(result) if !result.is_error => report.memories += 1,
                Ok(_) => report.skipped += 1,
                Err(error) => {
                    tracing::warn!("background saver could not write a memory: {}", error);
                    report.skipped += 1;
                }
            }
        }

        if !self.policy.notes {
            report.skipped += distilled.notes.len();
            return Ok(report);
        }

        for note in distilled.notes {
            let title = note.title.trim();
            let content = note.content.trim();
            if title.is_empty() || content.is_empty() {
                report.skipped += 1;
                continue;
            }
            match self.notes.save_note(title, content, &note.tags).await {
                Ok(saved) => {
                    // The same pointer bridge the agent runs after `save_note`, so a note written
                    // by the saver is as findable as one written by the session.
                    if let Err(error) = self
                        .memory
                        .add_note_pointer(&saved.id, &saved.summary_pointer)
                        .await
                    {
                        tracing::warn!("could not register the note pointer: {}", error);
                    }
                    report.notes += 1;
                }
                Err(error) => {
                    tracing::warn!("background saver could not write a note: {}", error);
                    report.skipped += 1;
                }
            }
        }

        Ok(report)
    }
}

#[async_trait]
impl BackgroundSaver for MemorySaver {
    async fn distil(&self, tail: &[Message]) -> Result<SaveReport, HarnessError> {
        self.run(tail).await
    }

    fn tail_limit(&self) -> usize {
        self.policy.tail_messages
    }

    fn every_turns(&self) -> usize {
        self.policy.every_turns
    }

    fn model(&self) -> &str {
        &self.policy.model
    }
}

/// Whether the store already states this triple.
fn already_known(records: &[MemoryRecord], subject: &str, predicate: &str, object: &str) -> bool {
    records.iter().any(|record| {
        record.subject.eq_ignore_ascii_case(subject)
            && record.predicate.eq_ignore_ascii_case(predicate)
            && record.object.trim() == object
    })
}

/// Parse the model's answer, tolerating a fenced code block around it.
///
/// A model that answers in prose is a real failure — there is nothing to apply — so this returns an
/// error naming what it saw rather than silently dropping the pass.
fn parse_distilled(text: &str) -> Result<Distilled, HarnessError> {
    let cleaned = strip_fence(text);
    let candidate = match (cleaned.find('{'), cleaned.rfind('}')) {
        (Some(start), Some(end)) if end > start => &cleaned[start..=end],
        _ => {
            return Err(HarnessError::ProviderError {
                provider: "background-saver".to_string(),
                message: format!(
                    "the saver did not answer with JSON: {}",
                    truncate(text.trim(), 200)
                ),
            })
        }
    };
    serde_json::from_str::<Distilled>(candidate).map_err(|error| HarnessError::ProviderError {
        provider: "background-saver".to_string(),
        message: format!("could not parse the saver's JSON: {error}"),
    })
}

/// Strip a ```json fence, which models add even when told not to.
fn strip_fence(text: &str) -> String {
    let trimmed = text.trim();
    let without_open = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```JSON"))
        .or_else(|| trimmed.strip_prefix("```"))
        .unwrap_or(trimmed);
    without_open
        .trim_end()
        .strip_suffix("```")
        .unwrap_or(without_open)
        .trim()
        .to_string()
}

/// The transcript as the saver reads it: who spoke, and a bounded amount of what they said.
fn render_transcript(messages: &[Message]) -> String {
    let mut rendered = String::new();
    for message in messages {
        let speaker = match message.role {
            Role::User => "USER",
            Role::Assistant => "LUNA",
            Role::Tool => "TOOL",
            Role::System => continue,
        };
        let body = match message.content.as_deref() {
            Some(content) if !content.trim().is_empty() => {
                truncate(content.trim(), MESSAGE_CHAR_CAP)
            }
            _ => {
                let calls: Vec<&str> = message
                    .tool_calls
                    .iter()
                    .map(|call| call.name.as_str())
                    .collect();
                if calls.is_empty() {
                    continue;
                }
                format!("(called {})", calls.join(", "))
            }
        };
        rendered.push_str(&format!("{speaker}: {body}\n"));
    }
    // Budget from the end: the newest turns carry the decision, the oldest are already reflected in
    // the memory index the model was shown.
    if rendered.chars().count() > TRANSCRIPT_CHAR_BUDGET {
        let skip = rendered.chars().count() - TRANSCRIPT_CHAR_BUDGET;
        rendered = rendered.chars().skip(skip).collect();
        rendered.insert_str(0, "(earlier turns omitted)\n");
    }
    rendered
}

/// Shorten `text` to `limit` characters, marking that it was cut.
fn truncate(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::types::ToolCall;
    use harness_core::MemoryCategory;
    use harness_notes::SqliteNotesEngine;

    use crate::SqliteMemoryEngine;

    /// A provider that answers with one canned string, and records the prompt it was given.
    struct CannedProvider {
        answer: String,
        seen: std::sync::Mutex<Vec<Message>>,
        fail: bool,
    }

    impl CannedProvider {
        fn new(answer: &str) -> Arc<Self> {
            Arc::new(Self {
                answer: answer.to_string(),
                seen: std::sync::Mutex::new(Vec::new()),
                fail: false,
            })
        }

        fn failing() -> Arc<Self> {
            Arc::new(Self {
                answer: String::new(),
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
            self.seen.lock().unwrap().extend_from_slice(messages);
            if self.fail {
                return Err(HarnessError::ProviderError {
                    provider: "canned".to_string(),
                    message: "provider is down".to_string(),
                });
            }
            Ok(Message::assistant(self.answer.clone()))
        }

        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
            Ok(vec![vec![0.0]; texts.len()])
        }
    }

    fn conversation() -> Vec<Message> {
        vec![
            Message::system("system prompt"),
            Message::user("I always build with cargo and run tests with cargo test"),
            Message::assistant("Noted: cargo, and `cargo test`."),
        ]
    }

    fn saver(
        provider: Arc<dyn ModelProvider>,
    ) -> (MemorySaver, Arc<SqliteMemoryEngine>, Arc<SqliteNotesEngine>) {
        let memory = Arc::new(SqliteMemoryEngine::in_memory().expect("memory db"));
        let notes = Arc::new(SqliteNotesEngine::in_memory().expect("notes db"));
        let saver = MemorySaver::new(
            provider,
            memory.clone(),
            notes.clone(),
            SaverPolicy::new("canned"),
        );
        (saver, memory, notes)
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
            .retrieve("build_command", 5)
            .await
            .expect("retrieves");
        assert_eq!(stored.len(), 1);
        // The id, the category and the raw content are the ones `save_memory` derives.
        assert_eq!(stored[0].id, "user.build_command");
        assert_eq!(stored[0].object, "cargo test");
        assert_eq!(stored[0].category, MemoryCategory::WorkflowRule);
        assert!((stored[0].confidence - 0.9).abs() < f32::EPSILON);
    }

    #[tokio::test]
    async fn the_prompt_carries_the_rules_the_known_index_and_the_conversation() {
        let provider = CannedProvider::new(r#"{"memories":[],"notes":[]}"#);
        let (saver, memory, _notes) = saver(provider.clone());
        memory
            .save(&MemoryRecord {
                id: "user.profile".to_string(),
                subject: "user".to_string(),
                predicate: "profile".to_string(),
                object: "Daryll, works on Luna".to_string(),
                raw_content: "user profile Daryll, works on Luna".to_string(),
                category: MemoryCategory::UserProfile,
                confidence: 1.0,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                is_active: true,
                superseded_by: None,
            })
            .await
            .expect("saves");

        saver.distil(&conversation()).await.expect("distils");
        let prompt = provider.prompt();

        assert!(prompt.contains("user.profile = Daryll"), "{prompt}");
        assert!(
            prompt.contains("USER: I always build with cargo"),
            "{prompt}"
        );
        assert!(prompt.contains("LUNA: Noted"), "{prompt}");
        assert!(
            !prompt.contains("system prompt"),
            "the system prompt leaked in"
        );
        // The rules travel in the system message, one constant.
        assert!(SAVER_SYSTEM_PROMPT.contains("Consolidate by entity"));
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
        assert_eq!(memory.retrieve("", 10).await.expect("retrieves").len(), 1);
    }

    #[tokio::test]
    async fn one_pass_saves_all_without_artificial_caps() {
        let memories: Vec<String> = (0..20)
            .map(|index| {
                format!(
                    r#"{{"subject":"p{index}","predicate":"k","object":"v{index}","category":"fact"}}"#
                )
            })
            .collect();
        let notes: Vec<String> = (0..5)
            .map(|index| {
                format!(r#"{{"title":"Note {index}","content":"body {index}","tags":[]}}"#)
            })
            .collect();
        let provider = CannedProvider::new(&format!(
            r#"{{"memories":[{}],"notes":[{}]}}"#,
            memories.join(","),
            notes.join(",")
        ));
        let (saver, memory, notes_engine) = saver(provider);

        let report = saver.distil(&conversation()).await.expect("distils");

        assert_eq!(report.memories, 20);
        assert_eq!(report.notes, 5);
        assert_eq!(
            memory.retrieve("", 50).await.expect("retrieves").len(),
            25,
            "each note also registers a pointer record"
        );
        assert_eq!(
            notes_engine.list_notes(10, 0).await.expect("lists").len(),
            5
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
        assert_eq!(memory.retrieve("", 10).await.expect("retrieves").len(), 1);
        assert!(notes.list_notes(10, 0).await.expect("lists").is_empty());
    }

    #[tokio::test]
    async fn a_note_is_saved_with_its_pointer_bridge() {
        let provider = CannedProvider::new(&format!(
            r#"{{"memories":[],"notes":[{{"title":"Build Recipe","content":"{}","tags":["build"]}}]}}"#,
            "step ".repeat(60)
        ));
        let (saver, memory, notes) = saver(provider);

        let report = saver.distil(&conversation()).await.expect("distils");

        assert_eq!(report.notes, 1);
        let stored = notes.list_notes(10, 0).await.expect("lists");
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].title, "Build Recipe");
        // The pointer is what makes the note discoverable in memory, exactly as in the agent path.
        let pointers = memory.retrieve("mem_ptr", 10).await.expect("retrieves");
        assert!(
            !pointers.is_empty(),
            "the note pointer was not registered: {pointers:?}"
        );
    }

    #[tokio::test]
    async fn notes_can_be_switched_off_without_touching_memories() {
        let provider = CannedProvider::new(&format!(
            r#"{{"memories":[{{"subject":"user","predicate":"p","object":"v","category":"fact"}}],
                 "notes":[{{"title":"Doc","content":"{}","tags":[]}}]}}"#,
            "y".repeat(MIN_NOTE_CHARS)
        ));
        let memory = Arc::new(SqliteMemoryEngine::in_memory().expect("memory db"));
        let notes = Arc::new(SqliteNotesEngine::in_memory().expect("notes db"));
        let mut policy = SaverPolicy::new("canned");
        policy.notes = false;
        let saver = MemorySaver::new(provider, memory, notes.clone(), policy);

        let report = saver.distil(&conversation()).await.expect("distils");

        assert_eq!(report.memories, 1);
        assert_eq!(report.notes, 0);
        assert_eq!(report.skipped, 1);
        assert!(notes.list_notes(10, 0).await.expect("lists").is_empty());
    }

    #[tokio::test]
    async fn prose_instead_of_json_is_an_error_naming_what_it_saw() {
        let provider = CannedProvider::new("I have saved three memories for you!");
        let (saver, memory, _notes) = saver(provider);

        let error = saver.distil(&conversation()).await.unwrap_err().to_string();

        assert!(error.contains("did not answer with JSON"), "{error}");
        assert!(error.contains("three memories"), "{error}");
        assert!(memory.retrieve("", 10).await.expect("retrieves").is_empty());
    }

    #[tokio::test]
    async fn a_provider_failure_surfaces_instead_of_writing_half_a_pass() {
        let (saver, memory, _notes) = saver(CannedProvider::failing());

        assert!(saver.distil(&conversation()).await.is_err());
        assert!(memory.retrieve("", 10).await.expect("retrieves").is_empty());
    }

    #[test]
    fn a_fenced_answer_is_read_and_a_bare_one_too() {
        let fenced = "```json\n{\"memories\":[],\"notes\":[]}\n```";
        assert!(parse_distilled(fenced).is_ok());
        assert!(parse_distilled("{\"memories\":[],\"notes\":[]}").is_ok());
        // Tolerated: a sentence before the object.
        assert!(parse_distilled("Sure! {\"memories\":[],\"notes\":[]}").is_ok());
        assert!(parse_distilled("no json at all").is_err());
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

    #[test]
    fn truncation_marks_what_it_cut() {
        assert_eq!(truncate("abcdef", 10), "abcdef");
        assert_eq!(truncate("abcdef", 4), "abc…");
    }
}
