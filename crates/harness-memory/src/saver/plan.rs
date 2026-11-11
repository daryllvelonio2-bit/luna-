//! Planning a pass: validating one operation, and naming things for the report.

use std::sync::atomic::Ordering;

use harness_core::saver::SaveStorage;
use harness_core::{MemoryCategory, MemoryRecord, Note, SaveReport};

use super::evidence::{
    evidence_supported, mark_unverified, normalize, note_body_len, note_has_authoritative_source,
    note_is_verbatim, operation_is_secret, strip_evidence_decoration, strip_headings,
};
use super::model::{Inventory, Item, Operation, Planned, EVIDENCE_MAX_CHARS, MIN_NOTE_CHARS};
use super::save::MemorySaver;
use super::transcript::{Provenance, RenderedTranscript};

impl MemorySaver {
    /// Turn one raw operation into a validated plan, or record why it was refused.
    pub(super) fn plan_one(
        &self,
        mut operation: Operation,
        inventory: &Inventory,
        transcript: &RenderedTranscript,
        report: &mut SaveReport,
    ) -> Option<Planned> {
        let name = operation.op.trim().to_ascii_lowercase();
        let storage = storage_for(&name);
        let display = operation_name(&operation);

        if name.is_empty() {
            report.skip(storage, display, "missing op");
            return None;
        }
        if name == "noop" {
            report.declined += 1;
            return None;
        }

        let confidence = operation
            .confidence
            .or_else(|| operation.memory.as_ref().and_then(|item| item.confidence))
            .unwrap_or(0.8);
        if confidence < self.policy.min_confidence {
            report.skip(
                storage,
                display,
                format!(
                    "confidence {:.2} below minimum {:.2}",
                    confidence, self.policy.min_confidence
                ),
            );
            return None;
        }

        if !self.evidence_allows(&mut operation, &name, transcript, report, storage, &display) {
            return None;
        }

        if operation_is_secret(&operation) {
            report.skip(storage, display, "secret detected");
            return None;
        }

        match name.as_str() {
            "add_memory" | "add" | "memory_add" => {
                self.plan_memory_add(operation, inventory, report, display)
            }
            "update_memory" | "update" | "memory_update" => {
                self.plan_memory_update(operation, inventory, report, display)
            }
            "delete_memory" | "memory_delete" => {
                self.plan_memory_delete(operation, inventory, report, display)
            }
            "merge_memories" | "memory_merge" => {
                self.plan_memory_merge(operation, inventory, report, display)
            }
            "create_note" | "add_note" | "note_create" => {
                self.plan_note_create(operation, inventory, transcript, report, display)
            }
            "update_note" | "note_update" => {
                self.plan_note_update(operation, inventory, transcript, report, display)
            }
            "merge_notes" | "note_merge" => {
                self.plan_note_merge(operation, inventory, report, display)
            }
            "delete_note" | "note_delete" => {
                self.plan_note_delete(operation, inventory, report, display)
            }
            other => {
                report.skip(storage, display, format!("unknown op '{other}'"));
                None
            }
        }
    }

    /// Check that a non-legacy operation quotes the transcript, or that a legacy note body came
    /// from a user or tool. A refusal is recorded here; `false` means the caller returns `None`.
    fn evidence_allows(
        &self,
        operation: &mut Operation,
        name: &str,
        transcript: &RenderedTranscript,
        report: &mut SaveReport,
        storage: SaveStorage,
        display: &str,
    ) -> bool {
        if !operation.legacy {
            let mut raw_evidence = operation
                .evidence
                .as_deref()
                .unwrap_or("")
                .trim()
                .to_string();
            if raw_evidence.is_empty() && is_memory_operation(name) {
                if let Some(obj) = authoritative_object(operation, transcript) {
                    raw_evidence = obj.to_string();
                }
            }
            if raw_evidence.is_empty() {
                report.skip(storage, display, "missing evidence");
                return false;
            }
            // Models often quote a whole answer, and some prefix the quote with a role label
            // (`USER: "…"`). A long quote is shortened rather than refused: the prefix is still
            // verbatim, and a paraphrase still fails the check below. Cut without an ellipsis so
            // the prefix is an exact substring of the transcript.
            let evidence = strip_evidence_decoration(&raw_evidence);
            let evidence: String = evidence.chars().take(EVIDENCE_MAX_CHARS).collect();
            if !evidence_supported(&evidence, transcript) {
                report.skip(storage, display, "evidence not found in the conversation");
                return false;
            }
            // A card is an assertion of fact, and a note is an authoritative record. Both need
            // a user line or tool output behind them. The assistant's own answer is a claim, not
            // proof: it may only become a note when the user explicitly asked to keep it, and
            // that note is tagged `unverified` so it stays out of the always-loaded prompt.
            let mut authoritative = matches!(
                transcript.provenance_of(&evidence),
                Some(Provenance::User | Provenance::Tool)
            );
            if !authoritative
                && is_memory_operation(name)
                && authoritative_object(operation, transcript).is_some()
            {
                authoritative = true;
            }
            if !authoritative {
                if is_memory_operation(name) {
                    report.skip(
                        storage,
                        display,
                        "assistant-only evidence cannot mint a memory card",
                    );
                    return false;
                }
                if !transcript.user_requested_save() {
                    report.skip(
                        storage,
                        display,
                        "assistant-only evidence cannot create a note",
                    );
                    return false;
                }
                if let Some(note) = operation.note.as_mut() {
                    mark_unverified(note);
                }
            }
        } else if let Some(note) = operation.note.as_mut() {
            // The legacy shape carries no evidence, so check what the note body is made of
            // instead. A body copied from the assistant still needs an explicit keep request.
            if !note.content.trim().is_empty()
                && !note_has_authoritative_source(&note.content, transcript)
            {
                if !transcript.user_requested_save() {
                    report.skip(
                        storage,
                        display,
                        "assistant-only evidence cannot create a note",
                    );
                    return false;
                }
                mark_unverified(note);
            }
        }
        true
    }

    fn plan_memory_add(
        &self,
        operation: Operation,
        inventory: &Inventory,
        report: &mut SaveReport,
        display: String,
    ) -> Option<Planned> {
        if !self.memory_enabled.load(Ordering::Relaxed) {
            report.skip(SaveStorage::Memory, display, "memory is off");
            return None;
        }
        let item = operation.memory?;
        if item.subject.trim().is_empty()
            || item.predicate.trim().is_empty()
            || item.object.trim().is_empty()
        {
            report.skip(
                SaveStorage::Memory,
                display,
                "empty subject, predicate, or object",
            );
            return None;
        }
        if already_known(
            &inventory.memories,
            item.subject.trim(),
            item.predicate.trim(),
            item.object.trim(),
        ) {
            report.skip(SaveStorage::Memory, display, "already known");
            return None;
        }
        Some(Planned::AddMemory { item })
    }

    fn plan_memory_update(
        &self,
        operation: Operation,
        inventory: &Inventory,
        report: &mut SaveReport,
        display: String,
    ) -> Option<Planned> {
        if !self.memory_enabled.load(Ordering::Relaxed) {
            report.skip(SaveStorage::Memory, display, "memory is off");
            return None;
        }
        let target = operation.target_id.as_deref().unwrap_or("").trim();
        let Some(record) = inventory.memory(target) else {
            report.skip(
                SaveStorage::Memory,
                display,
                "target not found in inventory",
            );
            return None;
        };
        let item = operation.memory?;
        if item.object.trim().is_empty() {
            report.skip(SaveStorage::Memory, display, "empty object");
            return None;
        }
        Some(Planned::UpdateMemory {
            target: record.id.clone(),
            item,
        })
    }

    fn plan_memory_delete(
        &self,
        operation: Operation,
        inventory: &Inventory,
        report: &mut SaveReport,
        display: String,
    ) -> Option<Planned> {
        if !self.memory_enabled.load(Ordering::Relaxed) {
            report.skip(SaveStorage::Memory, display, "memory is off");
            return None;
        }
        let target = operation.target_id.as_deref().unwrap_or("").trim();
        let Some(record) = inventory.memory(target) else {
            report.skip(
                SaveStorage::Memory,
                display,
                "target not found in inventory",
            );
            return None;
        };
        if record.category == MemoryCategory::NotePointer {
            report.skip(
                SaveStorage::Memory,
                display,
                "note pointers are deleted through their note",
            );
            return None;
        }
        let reason = operation.reason.as_deref().unwrap_or("").trim();
        if reason.is_empty() {
            report.skip(SaveStorage::Memory, display, "delete needs a reason");
            return None;
        }
        Some(Planned::DeleteMemory {
            target: record.id.clone(),
            reason: reason.to_string(),
        })
    }

    fn plan_memory_merge(
        &self,
        operation: Operation,
        inventory: &Inventory,
        report: &mut SaveReport,
        display: String,
    ) -> Option<Planned> {
        if !self.memory_enabled.load(Ordering::Relaxed) {
            report.skip(SaveStorage::Memory, display, "memory is off");
            return None;
        }
        let targets = self.resolve_memories(inventory, &operation.target_ids);
        if targets.len() < 2 {
            report.skip(
                SaveStorage::Memory,
                display,
                "merge needs two valid targets",
            );
            return None;
        }
        let item = operation.memory?;
        Some(Planned::MergeMemories { targets, item })
    }

    fn plan_note_create(
        &self,
        operation: Operation,
        inventory: &Inventory,
        transcript: &RenderedTranscript,
        report: &mut SaveReport,
        display: String,
    ) -> Option<Planned> {
        if !self.notes_writable() {
            report.skip(SaveStorage::Note, display, "notes are off");
            return None;
        }
        let note = operation.note?;
        if note.title.trim().is_empty() || note.content.trim().is_empty() {
            report.skip(SaveStorage::Note, display, "empty title or content");
            return None;
        }
        if note_body_len(&note.content) < MIN_NOTE_CHARS {
            report.skip(SaveStorage::Note, display, "note too short");
            return None;
        }
        if note_already_known(&inventory.notes, note.title.trim(), &note.content) {
            report.skip(SaveStorage::Note, display, "already known");
            return None;
        }
        if !note_is_verbatim(&note.content, transcript) {
            report.skip(SaveStorage::Note, display, "note is not verbatim");
            return None;
        }
        Some(Planned::CreateNote { note })
    }

    fn plan_note_update(
        &self,
        operation: Operation,
        inventory: &Inventory,
        transcript: &RenderedTranscript,
        report: &mut SaveReport,
        display: String,
    ) -> Option<Planned> {
        if !self.notes_writable() {
            report.skip(SaveStorage::Note, display, "notes are off");
            return None;
        }
        let target = operation.target_id.as_deref().unwrap_or("").trim();
        let Some(note) = inventory.note(target) else {
            report.skip(SaveStorage::Note, display, "target not found in inventory");
            return None;
        };
        let update = operation.note?;
        let has_change = !update.title.trim().is_empty()
            || !update.content.trim().is_empty()
            || !update.tags.is_empty();
        if !has_change {
            report.skip(SaveStorage::Note, display, "note update changes nothing");
            return None;
        }
        let append = update
            .mode
            .as_deref()
            .map(|mode| mode.eq_ignore_ascii_case("append"))
            .unwrap_or(true);
        if !update.content.trim().is_empty() {
            if !append && note_body_len(&update.content) < MIN_NOTE_CHARS {
                report.skip(SaveStorage::Note, display, "note too short");
                return None;
            }
            if append && normalize(&note.content).contains(&normalize(&update.content)) {
                report.skip(SaveStorage::Note, display, "already known");
                return None;
            }
            if !note_is_verbatim(&update.content, transcript) {
                report.skip(SaveStorage::Note, display, "note is not verbatim");
                return None;
            }
        }
        Some(Planned::UpdateNote {
            target: note.id.clone(),
            note: update,
        })
    }

    fn plan_note_merge(
        &self,
        operation: Operation,
        inventory: &Inventory,
        report: &mut SaveReport,
        display: String,
    ) -> Option<Planned> {
        if !self.notes_writable() {
            report.skip(SaveStorage::Note, display, "notes are off");
            return None;
        }
        let targets = self.resolve_notes(inventory, &operation.target_ids);
        if targets.len() < 2 {
            report.skip(SaveStorage::Note, display, "merge needs two valid targets");
            return None;
        }
        let note = operation.note?;
        Some(Planned::MergeNotes { targets, note })
    }

    fn plan_note_delete(
        &self,
        operation: Operation,
        inventory: &Inventory,
        report: &mut SaveReport,
        display: String,
    ) -> Option<Planned> {
        if !self.notes_writable() {
            report.skip(SaveStorage::Note, display, "notes are off");
            return None;
        }
        let target = operation.target_id.as_deref().unwrap_or("").trim();
        let Some(note) = inventory.note(target) else {
            report.skip(SaveStorage::Note, display, "target not found in inventory");
            return None;
        };
        let reason = operation.reason.as_deref().unwrap_or("").trim();
        if reason.is_empty() {
            report.skip(SaveStorage::Note, display, "delete needs a reason");
            return None;
        }
        Some(Planned::DeleteNote {
            target: note.id.clone(),
            reason: reason.to_string(),
        })
    }

    fn notes_writable(&self) -> bool {
        self.policy.notes && self.notes_enabled.load(Ordering::Relaxed)
    }

    fn resolve_memories(&self, inventory: &Inventory, ids: &[String]) -> Vec<String> {
        ids.iter()
            .filter_map(|id| inventory.memory(id).map(|record| record.id.clone()))
            .collect()
    }

    fn resolve_notes(&self, inventory: &Inventory, ids: &[String]) -> Vec<String> {
        ids.iter()
            .filter_map(|id| inventory.note(id).map(|note| note.id.clone()))
            .collect()
    }
}

/// The word a refused operation's storage should be reported under.
pub(super) fn storage_for(op: &str) -> SaveStorage {
    if op.contains("note") {
        SaveStorage::Note
    } else {
        SaveStorage::Memory
    }
}

/// Whether an operation writes a memory card rather than a note.
fn is_memory_operation(name: &str) -> bool {
    matches!(
        name,
        "add_memory"
            | "add"
            | "memory_add"
            | "update_memory"
            | "update"
            | "memory_update"
            | "delete_memory"
            | "memory_delete"
            | "merge_memories"
            | "memory_merge"
    )
}

/// The memory object when it is long enough to stand in for a quote and the transcript says a
/// user or tool produced it.
fn authoritative_object<'a>(
    operation: &'a Operation,
    transcript: &RenderedTranscript,
) -> Option<&'a str> {
    let obj = operation
        .memory
        .as_ref()
        .map(|m| m.object.trim())
        .filter(|o| o.chars().filter(|c| c.is_alphanumeric()).count() >= 4)?;
    matches!(
        transcript.provenance_of(obj),
        Some(Provenance::User | Provenance::Tool)
    )
    .then_some(obj)
}

pub(super) fn planned_storage(op: &Planned) -> SaveStorage {
    match op {
        Planned::AddMemory { .. }
        | Planned::UpdateMemory { .. }
        | Planned::DeleteMemory { .. }
        | Planned::MergeMemories { .. } => SaveStorage::Memory,
        _ => SaveStorage::Note,
    }
}

/// The friendly name of a raw operation, before validation.
pub(super) fn operation_name(operation: &Operation) -> String {
    operation
        .note
        .as_ref()
        .map(|note| note.title.trim())
        .filter(|title| !title.is_empty())
        .map(str::to_string)
        .or_else(|| {
            operation
                .memory
                .as_ref()
                .map(memory_display)
                .filter(|name| !name.is_empty())
        })
        .or_else(|| operation.target_id.clone())
        .unwrap_or_else(|| "(unnamed)".to_string())
}

/// The friendly name of a planned operation.
pub(super) fn planned_name(op: &Planned) -> String {
    match op {
        Planned::AddMemory { item } | Planned::MergeMemories { item, .. } => {
            let display = memory_display(item);
            if display.is_empty() || display == "-" {
                "(unnamed memory)".to_string()
            } else {
                display
            }
        }
        Planned::UpdateMemory { target, item } => {
            let display = memory_display(item);
            if display.is_empty() || display == "-" || display == "." {
                target.clone()
            } else {
                display
            }
        }
        Planned::CreateNote { note }
        | Planned::UpdateNote { note, .. }
        | Planned::MergeNotes { note, .. } => {
            if note.title.trim().is_empty() {
                "(untitled note)".to_string()
            } else {
                note.title.trim().to_string()
            }
        }
        Planned::DeleteMemory { target, .. } | Planned::DeleteNote { target, .. } => target.clone(),
    }
}

pub(super) fn memory_display(item: &Item) -> String {
    if let Some(name) = item
        .name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty() && *name != "-" && *name != "none")
    {
        return name.to_string();
    }
    let sub = item.subject.trim();
    let pred = item.predicate.trim();
    if !sub.is_empty() && !pred.is_empty() {
        format!("{}.{}", sub, pred)
    } else if !sub.is_empty() {
        sub.to_string()
    } else if !pred.is_empty() {
        pred.to_string()
    } else {
        String::new()
    }
}

/// Whether the store already states this triple.
fn already_known(records: &[MemoryRecord], subject: &str, predicate: &str, object: &str) -> bool {
    records.iter().any(|record| {
        record.subject.eq_ignore_ascii_case(subject)
            && record.predicate.eq_ignore_ascii_case(predicate)
            && normalize(record.object.trim()) == normalize(object)
    })
}

/// Whether the store already holds this note, by title or body. A body seen in a previous pass is
/// the same note; filing it again would only duplicate the file.
fn note_already_known(notes: &[Note], title: &str, content: &str) -> bool {
    let body = normalize(&strip_headings(content));
    notes.iter().any(|note| {
        (!title.is_empty()
            && (note.title.eq_ignore_ascii_case(title) || note.id.eq_ignore_ascii_case(title)))
            || (!body.is_empty() && normalize(&strip_headings(&note.content)) == body)
    })
}
