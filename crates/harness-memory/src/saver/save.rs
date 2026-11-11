//! [`MemorySaver`]: the independent writer, its model calls, and the store reads it makes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;

use harness_core::saver::{BackgroundSaver, SaveReport};
use harness_core::{
    CompletionOptions, HarnessError, MemoryCategory, MemoryEngine, MemoryRecord, Message,
    ModelProvider, NotesEngine,
};

use super::model::{
    Candidate, Distilled, Inventory, SaverPolicy, INDEX_LIMIT, MAX_INVENTORY_CHARS,
    MAX_SCOUT_INVENTORY_CHARS, NOTE_INDEX_LIMIT, TWO_PASS_THRESHOLD,
};
use super::parse::{parse_candidates, parse_distilled};
use super::prompt::{EXTRACT_SYSTEM_PROMPT, SAVER_SYSTEM_PROMPT};
use super::transcript::{
    exchanges, render_transcript, today, transcript_label, truncate, truncate_block,
};

/// The independent memory writer.
pub struct MemorySaver {
    pub(super) provider: Arc<dyn ModelProvider>,
    pub(super) memory: Arc<dyn MemoryEngine>,
    pub(super) notes: Arc<dyn NotesEngine>,
    pub(super) policy: SaverPolicy,
    /// Runtime gate for `/memory off`; memory records are skipped when false.
    pub(super) memory_enabled: AtomicBool,
    /// Runtime gate for `/notes off`; `policy.notes` remains the startup config.
    pub(super) notes_enabled: AtomicBool,
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
            memory_enabled: AtomicBool::new(true),
            notes_enabled: AtomicBool::new(true),
        }
    }

    /// The prompt for one pass: the inventory, then the conversation.
    ///
    /// Public so the exact text can be asserted on, and so a caller (or a human) can read what the
    /// saver was told without running it.
    pub async fn prompt(&self, tail: &[Message]) -> String {
        let inventory = self.inventory().await;
        let transcript = render_transcript(tail);
        self.curation_prompt(&inventory, &transcript.text)
    }

    /// File every complete user/assistant exchange in `tail` as a raw transcript record.
    ///
    /// This is the mechanical half of the saver. It runs before the model and does not depend on
    /// the model's answer, so a provider outage, a refused note, or a paraphrased reply can no
    /// longer lose the conversation. Records are content-addressed, which makes overlapping
    /// interval/idle tails idempotent, and tool output is deliberately omitted: it is
    /// reproducible, often enormous, and the distilled cards are what make it useful. The user's
    /// words and the model's reply are the part that cannot be reconstructed.
    ///
    /// Returns how many new transcript records were written.
    async fn capture(&self, tail: &[Message]) -> usize {
        if !self.memory_enabled.load(Ordering::Relaxed) {
            return 0;
        }

        let mut written = 0;
        for body in exchanges(tail) {
            let hash = crate::store::ids::delimited_sha256(&[&body], 24);
            let id = format!("transcript.{hash}");
            match self.memory.get_record(&id).await {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(
                        "background saver could not check transcript '{}': {}",
                        id,
                        error
                    );
                    continue;
                }
            }

            let record = MemoryRecord {
                id: id.clone(),
                // A unique subject per exchange is what keeps `save` from superseding the
                // previous transcript: one record per exchange, not one per session.
                subject: id.clone(),
                predicate: "exchange".to_string(),
                object: transcript_label(&body),
                name: None,
                path: None,
                raw_content: body,
                category: MemoryCategory::Transcript,
                confidence: 1.0,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                is_active: true,
                superseded_by: None,
            };
            match self.memory.save(&record).await {
                Ok(()) => written += 1,
                Err(error) => {
                    tracing::warn!("background saver could not file a transcript: {}", error)
                }
            }
        }
        written
    }

    /// Read what the store already holds, bounded and with stable ids.
    ///
    /// Uses [`MemoryEngine::retrieve_cards`] so raw transcripts never enter the inventory or the
    /// curation prompt: they are deliberately long, and the pass would otherwise re-read its own
    /// captured output on every run.
    async fn inventory(&self) -> Inventory {
        let mut records = self
            .memory
            .retrieve_cards("", INDEX_LIMIT + 1)
            .await
            .unwrap_or_default();
        let memories_truncated = records.len().saturating_sub(INDEX_LIMIT);
        records.truncate(INDEX_LIMIT);

        let mut pointers = HashMap::new();
        for record in &records {
            if record.category == MemoryCategory::NotePointer {
                if let Some(note_id) = record.id.strip_prefix("mem_ptr_") {
                    pointers.insert(note_id.to_string(), record.object.clone());
                }
            }
        }

        let mut notes = self
            .notes
            .list_notes(NOTE_INDEX_LIMIT + 1, 0)
            .await
            .unwrap_or_default();
        let notes_truncated = notes.len().saturating_sub(NOTE_INDEX_LIMIT);
        notes.truncate(NOTE_INDEX_LIMIT);

        Inventory {
            memories: records,
            notes,
            pointers,
            memories_truncated,
            notes_truncated,
        }
    }

    /// The user message for the curation pass.
    fn curation_prompt(&self, inventory: &Inventory, transcript: &str) -> String {
        format!(
            "{}\n\n<CONVERSATION>\n{}\n</CONVERSATION>\n\n<OBSERVED_AT>\n{}\n</OBSERVED_AT>",
            self.render_inventory(inventory),
            transcript,
            today()
        )
    }

    /// The inventory block: memory cards with ids, then notes with ids.
    fn render_inventory(&self, inventory: &Inventory) -> String {
        let mut out = String::new();
        out.push_str("CURRENT MEMORY (active; id | category | subject.predicate = object | name | path | confidence | updated)\n");
        let cards: Vec<&MemoryRecord> = inventory
            .memories
            .iter()
            .filter(|record| record.category != MemoryCategory::NotePointer)
            .collect();
        if cards.is_empty() {
            out.push_str("(none)\n");
        }
        for record in &cards {
            out.push_str(&format!(
                "{} | {} | {}.{} = {} | {} | {} | {:.2} | {}\n",
                record.id,
                record.category,
                record.subject,
                record.predicate,
                truncate(record.object.trim(), 160),
                record.name.as_deref().unwrap_or("-"),
                record.path.as_deref().unwrap_or("-"),
                record.confidence,
                record.updated_at.format("%Y-%m-%d"),
            ));
        }
        if inventory.memories_truncated > 0 {
            out.push_str(&format!(
                "INVENTORY TRUNCATED: {} older memories omitted. Only the ids above are valid targets.\n",
                inventory.memories_truncated
            ));
        }

        out.push_str("\nCURRENT NOTES (active; id | title | pointer | tags | updated | size)\n");
        if inventory.notes.is_empty() {
            out.push_str("(none)\n");
        }
        for note in &inventory.notes {
            let pointer = inventory
                .pointers
                .get(&note.id)
                .map(String::as_str)
                .unwrap_or(note.summary_pointer.as_str());
            out.push_str(&format!(
                "{} | {} | {} | {} | {} | {} chars\n",
                note.id,
                truncate(note.title.trim(), 120),
                truncate(pointer.trim(), 80),
                if note.tags.is_empty() {
                    "-".to_string()
                } else {
                    note.tags.join(", ")
                },
                note.updated_at.format("%Y-%m-%d"),
                note.content.chars().count(),
            ));
        }
        if inventory.notes_truncated > 0 {
            out.push_str(&format!(
                "INVENTORY TRUNCATED: {} older notes omitted. Only the ids above are valid targets.\n",
                inventory.notes_truncated
            ));
        }

        truncate_block(&out, MAX_INVENTORY_CHARS)
    }

    /// Compact inventory for Pass 1 (scout): shows existing facts and note titles without full
    /// target IDs, timestamps, or confidence metadata so Pass 1 saves context tokens.
    fn render_compact_inventory(&self, inventory: &Inventory) -> String {
        let mut out = String::new();
        out.push_str("CURRENT MEMORY (already stored)\n");
        let cards: Vec<&MemoryRecord> = inventory
            .memories
            .iter()
            .filter(|record| record.category != MemoryCategory::NotePointer)
            .collect();
        if cards.is_empty() {
            out.push_str("(none)\n");
        }
        for record in &cards {
            let key = if record.predicate.is_empty() || record.predicate == "=" {
                record.subject.clone()
            } else {
                format!("{}.{}", record.subject, record.predicate)
            };
            out.push_str(&format!(
                "- {} = {}\n",
                key,
                truncate(record.object.trim(), 100),
            ));
        }
        out.push_str("\nCURRENT NOTES (already stored)\n");
        if inventory.notes.is_empty() {
            out.push_str("(none)\n");
        }
        for note in &inventory.notes {
            out.push_str(&format!("- {}\n", truncate(note.title.trim(), 90)));
        }
        truncate_block(&out, MAX_SCOUT_INVENTORY_CHARS)
    }

    /// Ask the model what is durable, and apply it. Errors are the caller's to log.
    pub async fn run(&self, tail: &[Message]) -> Result<SaveReport, HarnessError> {
        let inventory = self.inventory().await;
        let transcript = render_transcript(tail);

        if self.policy.two_pass && inventory.total() > TWO_PASS_THRESHOLD {
            let candidates = self.extract(&inventory, &transcript.text).await?;
            if candidates.is_empty() {
                return Ok(SaveReport::default());
            }
            let distilled = self
                .reconcile(&inventory, &transcript.text, &candidates)
                .await?;
            return self
                .apply(distilled.into_operations(), &inventory, &transcript)
                .await;
        }

        let distilled = self.curate(&inventory, &transcript.text).await?;
        self.apply(distilled.into_operations(), &inventory, &transcript)
            .await
    }

    /// One model call with explicit messages, returning the text to parse.
    async fn call_messages(
        &self,
        messages: &[Message],
        max_tokens: u32,
    ) -> Result<String, HarnessError> {
        let options = CompletionOptions {
            model: Some(self.policy.model.clone()),
            temperature: Some(0.0),
            max_tokens: Some(max_tokens),
            reasoning_effort: Some(harness_core::ReasoningEffort::Low),
            ..CompletionOptions::default()
        };
        let response = self.provider.complete(messages, &[], &options).await?;
        let text = response.content.as_deref().unwrap_or_default();
        Ok(
            if (text.is_empty() || text == harness_core::types::NO_TEXT_RESPONSE)
                && response.reasoning.is_some()
            {
                response
                    .reasoning
                    .as_deref()
                    .unwrap_or_default()
                    .to_string()
            } else if !text.contains('{')
                && response
                    .reasoning
                    .as_deref()
                    .map(|r| r.contains('{'))
                    .unwrap_or(false)
            {
                format!(
                    "{}\n{}",
                    text,
                    response.reasoning.as_deref().unwrap_or_default()
                )
            } else {
                text.to_string()
            },
        )
    }

    /// One model call, returning the text to parse.
    async fn call(
        &self,
        system: &str,
        user: String,
        max_tokens: u32,
    ) -> Result<String, HarnessError> {
        self.call_messages(&[Message::system(system), Message::user(user)], max_tokens)
            .await
    }

    async fn curate(
        &self,
        inventory: &Inventory,
        transcript: &str,
    ) -> Result<Distilled, HarnessError> {
        let prompt = self.curation_prompt(inventory, transcript);
        let text = self
            .call(SAVER_SYSTEM_PROMPT, prompt.clone(), 8_192)
            .await?;
        match parse_distilled(&text) {
            Ok(distilled) => Ok(distilled),
            Err(error) => {
                tracing::warn!(
                    "saver JSON parse failed ({}), retrying once with error feedback",
                    error
                );
                let retry_messages = vec![
                    Message::system(SAVER_SYSTEM_PROMPT),
                    Message::user(prompt),
                    Message::assistant(text),
                    Message::user(format!(
                        "Your response was not valid JSON ({error}). Return ONLY a valid JSON object starting with {{\"operations\": [...]}}. Do not include markdown fences, comments, or trailing text."
                    )),
                ];
                let retry_text = self.call_messages(&retry_messages, 8_192).await?;
                parse_distilled(&retry_text)
            }
        }
    }

    async fn extract(
        &self,
        inventory: &Inventory,
        transcript: &str,
    ) -> Result<Vec<Candidate>, HarnessError> {
        let user = format!(
            "{}\n\n<CONVERSATION>\n{}\n</CONVERSATION>",
            self.render_compact_inventory(inventory),
            transcript
        );
        let text = self
            .call(EXTRACT_SYSTEM_PROMPT, user.clone(), 4_096)
            .await?;
        match parse_candidates(&text) {
            Ok(candidates) => Ok(candidates.candidates),
            Err(error) => {
                tracing::warn!(
                    "saver scout JSON parse failed ({}), retrying once with error feedback",
                    error
                );
                let retry_messages = vec![
                    Message::system(EXTRACT_SYSTEM_PROMPT),
                    Message::user(user),
                    Message::assistant(text),
                    Message::user(format!(
                        "Your response was not valid JSON ({error}). Return ONLY a valid JSON object starting with {{\"candidates\": [...]}}. Do not include markdown fences, comments, or trailing text."
                    )),
                ];
                let retry_text = self.call_messages(&retry_messages, 4_096).await?;
                Ok(parse_candidates(&retry_text)?.candidates)
            }
        }
    }

    async fn reconcile(
        &self,
        inventory: &Inventory,
        transcript: &str,
        candidates: &[Candidate],
    ) -> Result<Distilled, HarnessError> {
        let candidates_str = serde_json::to_string_pretty(candidates).unwrap_or_default();
        let user = format!(
            "{}\n\n<CANDIDATES>\n{}\n</CANDIDATES>\n\n<CONVERSATION>\n{}\n</CONVERSATION>\n\n<OBSERVED_AT>\n{}\n</OBSERVED_AT>",
            self.render_inventory(inventory),
            candidates_str,
            transcript,
            today()
        );
        let text = self.call(SAVER_SYSTEM_PROMPT, user.clone(), 8_192).await?;
        match parse_distilled(&text) {
            Ok(distilled) => Ok(distilled),
            Err(error) => {
                tracing::warn!(
                    "saver reconcile JSON parse failed ({}), retrying once with error feedback",
                    error
                );
                let retry_messages = vec![
                    Message::system(SAVER_SYSTEM_PROMPT),
                    Message::user(user),
                    Message::assistant(text),
                    Message::user(format!(
                        "Your response was not valid JSON ({error}). Return ONLY a valid JSON object starting with {{\"operations\": [...]}}. Do not include markdown fences, comments, or trailing text."
                    )),
                ];
                let retry_text = self.call_messages(&retry_messages, 8_192).await?;
                parse_distilled(&retry_text)
            }
        }
    }
}

#[async_trait]
impl BackgroundSaver for MemorySaver {
    async fn distil(&self, tail: &[Message]) -> Result<SaveReport, HarnessError> {
        // Capture first and unconditionally: the raw exchange must survive a provider error or
        // a model that declines to write anything.
        let captured = self.capture(tail).await;
        if captured > 0 {
            tracing::debug!("background saver filed {} raw transcript(s)", captured);
        }
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

    fn set_notes_enabled(&self, enabled: bool) {
        self.notes_enabled.store(enabled, Ordering::Relaxed);
    }

    fn set_memory_enabled(&self, enabled: bool) {
        self.memory_enabled.store(enabled, Ordering::Relaxed);
    }
}
