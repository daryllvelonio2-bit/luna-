//! Applying validated operations through the same tools the session uses.

use std::collections::HashSet;

use serde_json::json;
use tokio_util::sync::CancellationToken;

use harness_core::saver::{SaveReport, SaveStatus, SaveStorage};
use harness_core::{HarnessError, MemoryCategory, Note, Tool};

use crate::tools::{SaveMemoryTool, UpdateMemoryTool};

use super::evidence::note_is_unverified;
use super::model::{Inventory, Item, NoteItem, Operation, Planned};
use super::plan::{memory_display, operation_name, planned_name, planned_storage, storage_for};
use super::save::MemorySaver;
use super::transcript::RenderedTranscript;

impl MemorySaver {
    /// Validate every operation, then write what survives.
    pub(super) async fn apply(
        &self,
        operations: Vec<Operation>,
        inventory: &Inventory,
        transcript: &RenderedTranscript,
    ) -> Result<SaveReport, HarnessError> {
        let mut report = SaveReport::default();
        let mut planned: Vec<Planned> = Vec::new();
        let mut memory_writes = 0usize;
        let mut note_writes = 0usize;
        let mut targets: HashSet<String> = HashSet::new();

        for operation in operations {
            if planned.len() >= self.policy.max_operations {
                report.skip(
                    storage_for(&operation.op),
                    operation_name(&operation),
                    "operation cap reached",
                );
                continue;
            }
            let Some(op) = self.plan_one(operation, inventory, transcript, &mut report) else {
                continue;
            };

            // One operation per target: a second write to the same id would undo the first.
            let target_key = match &op {
                Planned::UpdateMemory { target, .. }
                | Planned::DeleteMemory { target, .. }
                | Planned::UpdateNote { target, .. }
                | Planned::DeleteNote { target, .. } => Some(target.to_ascii_lowercase()),
                Planned::MergeMemories { targets, .. } | Planned::MergeNotes { targets, .. } => {
                    Some(targets.join("|").to_ascii_lowercase())
                }
                _ => None,
            };
            if let Some(key) = target_key {
                if !targets.insert(key) {
                    report.skip(
                        planned_storage(&op),
                        planned_name(&op),
                        "target already changed in this pass",
                    );
                    continue;
                }
            }

            match &op {
                Planned::AddMemory { .. }
                | Planned::UpdateMemory { .. }
                | Planned::MergeMemories { .. } => {
                    if memory_writes >= self.policy.max_memory_writes {
                        report.skip(
                            SaveStorage::Memory,
                            planned_name(&op),
                            "memory write cap reached",
                        );
                        continue;
                    }
                    memory_writes += 1;
                }
                Planned::CreateNote { .. }
                | Planned::UpdateNote { .. }
                | Planned::MergeNotes { .. } => {
                    if note_writes >= self.policy.max_note_writes {
                        report.skip(
                            SaveStorage::Note,
                            planned_name(&op),
                            "note write cap reached",
                        );
                        continue;
                    }
                    note_writes += 1;
                }
                Planned::DeleteMemory { .. } | Planned::DeleteNote { .. } => {}
            }

            planned.push(op);
        }

        let mut written_keys: HashSet<(String, String)> = HashSet::new();
        for rank in 0..=3 {
            for op in planned.iter().filter(|op| op.rank() == rank) {
                self.apply_one(op, inventory, &mut report, &mut written_keys)
                    .await;
            }
        }

        // The summary line says how many operations were refused; this says which rule refused
        // each one. Without it a pass that saves nothing is indistinguishable from a pass the
        // model declined, and the store gives no clue about the gap.
        for outcome in &report.outcomes {
            match &outcome.reason {
                Some(reason) => tracing::debug!(
                    "memory saver {} '{}': {} ({})",
                    outcome.status.as_str(),
                    outcome.name,
                    reason,
                    outcome.storage.as_str()
                ),
                None => tracing::debug!(
                    "memory saver {} '{}' ({})",
                    outcome.status.as_str(),
                    outcome.name,
                    outcome.storage.as_str()
                ),
            }
        }

        Ok(report)
    }

    /// Write one planned operation. A store error is logged and counted, never fatal to the pass.
    async fn apply_one(
        &self,
        planned: &Planned,
        inventory: &Inventory,
        report: &mut SaveReport,
        written_keys: &mut HashSet<(String, String)>,
    ) {
        match planned {
            Planned::AddMemory { item } => {
                self.apply_add_memory(item, inventory, report, written_keys)
                    .await;
            }
            Planned::UpdateMemory { target, item } => {
                self.update_memory(target, item, inventory, report).await;
            }
            Planned::MergeMemories { targets, item } => {
                self.apply_merge_memories(targets, item, inventory, report)
                    .await;
            }
            Planned::CreateNote { note } => {
                self.apply_create_note(note, inventory, report).await;
            }
            Planned::UpdateNote { target, note } => {
                self.update_note(target, note, report).await;
            }
            Planned::MergeNotes { targets, note } => {
                self.apply_merge_notes(targets, note, report).await;
            }
            Planned::DeleteMemory { target, reason } => {
                self.apply_delete_memory(target, reason, inventory, report)
                    .await;
            }
            Planned::DeleteNote { target, reason } => {
                self.apply_delete_note(target, reason, report).await;
            }
        }
    }

    async fn apply_add_memory(
        &self,
        item: &Item,
        inventory: &Inventory,
        report: &mut SaveReport,
        written_keys: &mut HashSet<(String, String)>,
    ) {
        let subject = item.subject.trim();
        let predicate = item.predicate.trim();
        let object = item.object.trim();
        let key = (subject.to_ascii_lowercase(), predicate.to_ascii_lowercase());
        let existed = written_keys.contains(&key)
            || inventory.memories.iter().any(|record| {
                record.subject.eq_ignore_ascii_case(subject)
                    && record.predicate.eq_ignore_ascii_case(predicate)
                    && record.category != MemoryCategory::NotePointer
            });

        let mut args = json!({
            "subject": subject,
            "predicate": predicate,
            "object": object,
            "category": item.category.as_deref().unwrap_or("fact"),
        });
        if let Some(name) = item.name.as_deref().filter(|name| !name.trim().is_empty()) {
            args["name"] = json!(name.trim());
        }
        if let Some(path) = item.path.as_deref().filter(|path| !path.trim().is_empty()) {
            args["path"] = json!(path.trim());
        }
        if let Some(confidence) = item.confidence {
            args["confidence"] = json!(confidence.clamp(0.0, 1.0));
        }

        let display = memory_display(item);
        let display_name = if display.is_empty() || display == "-" {
            if !subject.is_empty() && !predicate.is_empty() {
                format!("{}.{}", subject, predicate)
            } else {
                "memory".to_string()
            }
        } else {
            display
        };
        let detail = if !object.is_empty() {
            Some(format!("\"{}\"", object))
        } else {
            None
        };

        let tool = SaveMemoryTool::new(self.memory.clone());
        let call_id = format!("saver_mem_{}", report.memories + 1);
        match tool
            .execute(&call_id, &args, &CancellationToken::new())
            .await
        {
            Ok(result) if !result.is_error => {
                written_keys.insert(key);
                report.memory_written_with_detail(
                    display_name,
                    detail,
                    if existed {
                        SaveStatus::Updated
                    } else {
                        SaveStatus::Saved
                    },
                );
            }
            Ok(_) => report.skip(SaveStorage::Memory, memory_display(item), "store refused"),
            Err(error) => {
                tracing::warn!("background saver could not write a memory: {}", error);
                report.skip(SaveStorage::Memory, memory_display(item), "store error");
            }
        }
    }

    async fn apply_merge_memories(
        &self,
        targets: &[String],
        item: &Item,
        inventory: &Inventory,
        report: &mut SaveReport,
    ) {
        let Some(first) = targets.first() else {
            return;
        };
        if self.update_memory(first, item, inventory, report).await {
            for target in targets.iter().skip(1) {
                if let Err(error) = self.memory.forget(target).await {
                    tracing::warn!("could not deactivate merged memory '{}': {}", target, error);
                }
            }
            // The update reported `updated`; the merge is the more accurate word.
            if let Some(last) = report.outcomes.last_mut() {
                if last.storage == SaveStorage::Memory && last.status == SaveStatus::Updated {
                    last.status = SaveStatus::Merged;
                }
            }
            if let Some(last) = report.saved.last_mut() {
                last.status = SaveStatus::Merged;
            }
        }
    }

    async fn apply_create_note(
        &self,
        note: &NoteItem,
        inventory: &Inventory,
        report: &mut SaveReport,
    ) {
        let title = note.title.trim();
        let content = note.content.trim();
        let existed = inventory
            .notes
            .iter()
            .any(|existing| existing.title.eq_ignore_ascii_case(title));
        match self.notes.save_note(title, content, &note.tags).await {
            Ok(saved) => {
                self.refresh_note_pointer(&saved).await;
                let line_count = content.lines().count();
                let detail = Some(format!("{} lines", line_count));
                report.note_written_with_detail(
                    saved.title.clone(),
                    detail,
                    if existed {
                        SaveStatus::Updated
                    } else {
                        SaveStatus::Saved
                    },
                );
            }
            Err(error) => {
                tracing::warn!("background saver could not write a note: {}", error);
                report.skip(SaveStorage::Note, title, "store error");
            }
        }
    }

    async fn apply_merge_notes(
        &self,
        targets: &[String],
        note: &NoteItem,
        report: &mut SaveReport,
    ) {
        let Some(first) = targets.first() else {
            return;
        };
        let merged = self.update_note(first, note, report).await;
        if merged.is_some() {
            for target in targets.iter().skip(1) {
                self.remove_note(target).await;
            }
            if let Some(last) = report.outcomes.last_mut() {
                if last.storage == SaveStorage::Note && last.status == SaveStatus::Updated {
                    last.status = SaveStatus::Merged;
                }
            }
            if let Some(last) = report.saved_notes.last_mut() {
                last.status = SaveStatus::Merged;
            }
        }
    }

    async fn apply_delete_memory(
        &self,
        target: &str,
        reason: &str,
        inventory: &Inventory,
        report: &mut SaveReport,
    ) {
        match self.memory.forget(target).await {
            Ok(()) => {
                let display_name = inventory
                    .memory(target)
                    .map(|r| format!("{}.{}", r.subject, r.predicate))
                    .unwrap_or_else(|| target.to_string());
                let detail =
                    (!reason.trim().is_empty()).then(|| format!("reason: {}", reason.trim()));
                report.memory_written_with_detail(display_name, detail, SaveStatus::Deleted);
            }
            Err(error) => {
                tracing::warn!(
                    "could not delete memory '{}' ({}): {}",
                    target,
                    reason,
                    error
                );
                report.skip(SaveStorage::Memory, target, "store error");
            }
        }
    }

    async fn apply_delete_note(&self, target: &str, reason: &str, report: &mut SaveReport) {
        if self.remove_note(target).await {
            let detail = (!reason.trim().is_empty()).then(|| format!("reason: {}", reason.trim()));
            report.note_written_with_detail(target.to_string(), detail, SaveStatus::Deleted);
        } else {
            report.skip(
                SaveStorage::Note,
                target,
                format!("could not archive: {reason}"),
            );
        }
    }

    /// Revise a memory card through the same partial-update tool the session uses.
    async fn update_memory(
        &self,
        target: &str,
        item: &Item,
        inventory: &Inventory,
        report: &mut SaveReport,
    ) -> bool {
        let mut args = json!({ "memory_id": target });
        if !item.object.trim().is_empty() {
            args["object"] = json!(item.object.trim());
        }
        if !item.subject.trim().is_empty() {
            args["subject"] = json!(item.subject.trim());
        }
        if !item.predicate.trim().is_empty() {
            args["predicate"] = json!(item.predicate.trim());
        }
        if let Some(name) = item
            .name
            .as_deref()
            .filter(|name| !name.trim().is_empty() && *name != "-")
        {
            args["name"] = json!(name.trim());
        }
        if let Some(path) = item
            .path
            .as_deref()
            .filter(|path| !path.trim().is_empty() && *path != "-")
        {
            args["path"] = json!(path.trim());
        }
        if let Some(category) = item.category.as_deref().filter(|c| !c.trim().is_empty()) {
            args["category"] = json!(category.trim());
        }

        let existing = inventory.memory(target);
        let display_name = {
            let item_disp = memory_display(item);
            if !item_disp.is_empty() && item_disp != "-" && item_disp != "." {
                item_disp
            } else if let Some(rec) = existing {
                rec.name
                    .as_deref()
                    .filter(|n| !n.trim().is_empty() && *n != "-")
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("{}.{}", rec.subject.trim(), rec.predicate.trim()))
            } else {
                target.to_string()
            }
        };

        let tool = UpdateMemoryTool::new(self.memory.clone());
        let call_id = format!("saver_mem_{}", report.memories + 1);
        match tool
            .execute(&call_id, &args, &CancellationToken::new())
            .await
        {
            Ok(result) if !result.is_error => {
                let detail = if let Some(rec) = existing {
                    if !rec.object.trim().is_empty()
                        && !item.object.trim().is_empty()
                        && rec.object.trim() != item.object.trim()
                    {
                        Some(format!(
                            "\"{}\" → \"{}\"",
                            rec.object.trim(),
                            item.object.trim()
                        ))
                    } else if !item.object.trim().is_empty() {
                        Some(format!("\"{}\"", item.object.trim()))
                    } else {
                        None
                    }
                } else if !item.object.trim().is_empty() {
                    Some(format!("\"{}\"", item.object.trim()))
                } else {
                    None
                };

                report.memory_written_with_detail(display_name, detail, SaveStatus::Updated);
                true
            }
            Ok(_) => {
                report.skip(SaveStorage::Memory, display_name, "store refused update");
                false
            }
            Err(error) => {
                tracing::warn!("background saver could not update a memory: {}", error);
                report.skip(SaveStorage::Memory, display_name, "store error");
                false
            }
        }
    }

    /// Revise a note, appending unless the operation says `replace`. Returns the note title.
    async fn update_note(
        &self,
        target: &str,
        update: &NoteItem,
        report: &mut SaveReport,
    ) -> Option<String> {
        let existing = match self.notes.read_note(target).await {
            Ok(note) => note,
            Err(error) => {
                tracing::warn!("could not read note '{}' to update it: {}", target, error);
                report.skip(SaveStorage::Note, target, "note not readable");
                return None;
            }
        };

        let append = update
            .mode
            .as_deref()
            .map(|mode| mode.eq_ignore_ascii_case("append"))
            .unwrap_or(true);
        let content = if update.content.trim().is_empty() {
            None
        } else if append {
            Some(format!(
                "{}\n\n{}",
                existing.content.trim_end(),
                update.content.trim()
            ))
        } else {
            Some(update.content.trim().to_string())
        };
        let title = (!update.title.trim().is_empty()).then_some(update.title.trim());
        let tags = (!update.tags.is_empty()).then_some(update.tags.as_slice());

        match self
            .notes
            .update_note(&existing.id, title, content.as_deref(), tags)
            .await
        {
            Ok(saved) => {
                self.refresh_note_pointer(&saved).await;
                let mode_str = if append { "append" } else { "replace" };
                let lines = update.content.trim().lines().count();
                let detail = Some(format!("{mode_str}, +{lines} lines"));
                report.note_written_with_detail(saved.title.clone(), detail, SaveStatus::Updated);
                Some(saved.title)
            }
            Err(error) => {
                tracing::warn!("background saver could not update a note: {}", error);
                report.skip(SaveStorage::Note, existing.title, "store error");
                None
            }
        }
    }

    /// Refresh a note's pointer card, unless the note is unverified. An unverified note is the
    /// assistant's own claim: it stays searchable as a file but must not enter every prompt.
    async fn refresh_note_pointer(&self, note: &Note) {
        if note_is_unverified(&note.tags) {
            if let Err(error) = self.memory.remove_note_pointer(&note.id).await {
                tracing::warn!(
                    "could not remove the pointer for unverified note '{}': {}",
                    note.id,
                    error
                );
            }
            return;
        }
        if let Err(error) = self
            .memory
            .add_note_pointer(&note.id, &note.summary_pointer)
            .await
        {
            tracing::warn!("could not register the note pointer: {}", error);
        }
    }

    /// Archive a note (soft delete) and drop its pointer. Returns false when neither worked.
    async fn remove_note(&self, id: &str) -> bool {
        let archived = self.notes.archive_note(id).await.unwrap_or(false);
        if !archived {
            match self.notes.delete_note(id).await {
                Ok(true) => {}
                Ok(false) => return false,
                Err(error) => {
                    tracing::warn!("could not delete note '{}': {}", id, error);
                    return false;
                }
            }
        }
        if let Err(error) = self.memory.remove_note_pointer(id).await {
            tracing::warn!("could not remove the pointer for note '{}': {}", id, error);
        }
        true
    }
}
