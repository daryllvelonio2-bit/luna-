//! The saver's data types, the limits it applies, and what a pass plans to write.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use harness_core::{MemoryRecord, Note};

/// Per-pass cap on new or revised memory cards.
pub const MAX_MEMORY_WRITES_PER_PASS: usize = 3;
/// Per-pass cap on new or revised notes.
pub const MAX_NOTE_WRITES_PER_PASS: usize = 2;
/// Per-pass cap on every operation, including deletes.
pub const MAX_OPERATIONS_PER_PASS: usize = 6;
/// Below this confidence an operation is refused. Explicit user statements clear it easily;
/// guesses do not.
pub const MIN_CONFIDENCE: f32 = 0.65;
/// Minimum substantive characters a note body must carry. A shorter body is a one-line fact for
/// a memory card, not a document. Headings and whitespace do not count towards it.
pub const MIN_NOTE_CHARS: usize = 160;
/// Per-message character cap while rendering the transcript.
pub const MESSAGE_CHAR_CAP: usize = 8_000;
/// Total character budget for the rendered transcript.
pub const TRANSCRIPT_CHAR_BUDGET: usize = 24_000;
/// Total character budget for the memory + note inventory.
pub const MAX_INVENTORY_CHARS: usize = 18_000;
/// Compact inventory character budget for Pass 1 (scout).
pub const MAX_SCOUT_INVENTORY_CHARS: usize = 4_500;
/// How many existing records are shown as the "already known" index.
pub const INDEX_LIMIT: usize = 200;
/// How many existing notes are shown.
pub const NOTE_INDEX_LIMIT: usize = 100;
/// Above this many stored items the pass splits into extract + reconcile.
pub const TWO_PASS_THRESHOLD: usize = 20;
/// Longest evidence prefix checked. A longer quote is shortened, not refused: the prefix is still
/// verbatim and its presence still proves the operation came from the conversation.
pub const EVIDENCE_MAX_CHARS: usize = 500;

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
    /// Split into extract + reconcile once the store is large enough to need it.
    pub two_pass: bool,
    /// Refuse operations below this confidence.
    pub min_confidence: f32,
    /// Per-pass memory write cap.
    pub max_memory_writes: usize,
    /// Per-pass note write cap.
    pub max_note_writes: usize,
    /// Per-pass operation cap (including deletes).
    pub max_operations: usize,
}

impl SaverPolicy {
    /// The policy for `model`: every turn, the configured tail, notes on, conservative caps.
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            every_turns: 1,
            tail_messages: 24,
            notes: true,
            two_pass: true,
            min_confidence: MIN_CONFIDENCE,
            max_memory_writes: MAX_MEMORY_WRITES_PER_PASS,
            max_note_writes: MAX_NOTE_WRITES_PER_PASS,
            max_operations: MAX_OPERATIONS_PER_PASS,
        }
    }
}

/// One memory card the model asked to record.
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct Item {
    #[serde(default)]
    pub(super) subject: String,
    #[serde(default)]
    pub(super) predicate: String,
    #[serde(default)]
    pub(super) object: String,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) path: Option<String>,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(default)]
    pub(super) confidence: Option<f32>,
}

/// One note body the model asked to record or change.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(super) struct NoteItem {
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) content: String,
    #[serde(default)]
    pub(super) tags: Vec<String>,
    #[serde(default)]
    pub(super) mode: Option<String>,
}

/// One operation, as the model returned it.
///
/// Models do not always use the exact field names in the prompt. `op` accepts the common aliases,
/// and the memory/note fields may arrive at the top level of the operation instead of nested under
/// `memory`/`note`; [`Operation::normalized`] folds both shapes into the one the planner expects.
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct Operation {
    #[serde(
        default,
        alias = "type",
        alias = "action",
        alias = "operation",
        alias = "kind"
    )]
    pub(super) op: String,
    #[serde(default)]
    pub(super) target_id: Option<String>,
    #[serde(default)]
    pub(super) target_ids: Vec<String>,
    #[serde(default)]
    pub(super) memory: Option<Item>,
    #[serde(default)]
    pub(super) note: Option<NoteItem>,
    #[serde(default)]
    pub(super) evidence: Option<String>,
    #[serde(default)]
    pub(super) confidence: Option<f32>,
    #[serde(default)]
    pub(super) reason: Option<String>,
    // Top-level payload fields, for models that omit the `memory` / `note` wrapper.
    #[serde(default)]
    pub(super) subject: String,
    #[serde(default)]
    pub(super) predicate: String,
    #[serde(default)]
    pub(super) object: String,
    #[serde(default)]
    pub(super) name: Option<String>,
    #[serde(default)]
    pub(super) path: Option<String>,
    #[serde(default)]
    pub(super) category: Option<String>,
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) content: String,
    #[serde(default)]
    pub(super) tags: Vec<String>,
    #[serde(default)]
    pub(super) mode: Option<String>,
    /// Any field the struct did not name. Some models put the operation kind in the key itself:
    /// `{"add_memory": {"subject": …}}` rather than `{"op": "add_memory", "memory": {…}}`.
    #[serde(flatten)]
    pub(super) extra: HashMap<String, serde_json::Value>,
    /// True when this came from the legacy `{memories, notes}` shape, which predates evidence.
    #[serde(skip)]
    pub(super) legacy: bool,
}

impl Operation {
    /// Fold an aliased or flat shape into `op` plus a `memory` or `note` payload.
    ///
    /// An operation that still names no kind and carries no payload is left alone, so the planner
    /// can refuse it with its usual "missing op" reason.
    fn normalized(mut self) -> Self {
        if self.op.trim().is_empty() {
            self.lift_keyed_operation();
        }
        if self.op.trim().is_empty() {
            self.op = if self.memory.is_some() {
                "add_memory"
            } else if self.note.is_some() || !self.title.trim().is_empty() {
                "create_note"
            } else if !self.subject.trim().is_empty()
                || !self.predicate.trim().is_empty()
                || !self.object.trim().is_empty()
            {
                "add_memory"
            } else {
                ""
            }
            .to_string();
        }
        if self.memory.is_none()
            && (!self.subject.trim().is_empty()
                || !self.predicate.trim().is_empty()
                || !self.object.trim().is_empty())
        {
            self.memory = Some(Item {
                subject: self.subject.clone(),
                predicate: self.predicate.clone(),
                object: self.object.clone(),
                name: self.name.clone(),
                path: self.path.clone(),
                category: self.category.clone(),
                confidence: self.confidence,
            });
        }
        if let Some(ref mut item) = self.memory {
            let sub = item.subject.trim().to_string();
            let pred = item.predicate.trim().to_string();
            let pred_is_placeholder = !pred.is_empty()
                && (!pred.chars().any(|c| c.is_alphanumeric())
                    || matches!(
                        pred.to_ascii_lowercase().as_str(),
                        "=" | "==" | ":" | "-" | "is" | "equals"
                    ));
            if pred_is_placeholder {
                if let Some((prefix, suffix)) = sub.rsplit_once('.') {
                    let prefix_trimmed = prefix.trim();
                    let suffix_trimmed = suffix.trim();
                    if !prefix_trimmed.is_empty()
                        && !suffix_trimmed.is_empty()
                        && suffix_trimmed.chars().any(|c| c.is_alphanumeric())
                    {
                        item.subject = prefix_trimmed.to_string();
                        item.predicate = suffix_trimmed.to_string();
                    } else {
                        item.predicate = "profile".to_string();
                    }
                } else {
                    item.predicate = "profile".to_string();
                }
            } else if pred.is_empty() {
                if let Some((prefix, suffix)) = sub.rsplit_once('.') {
                    let prefix_trimmed = prefix.trim();
                    let suffix_trimmed = suffix.trim();
                    if !prefix_trimmed.is_empty()
                        && !suffix_trimmed.is_empty()
                        && suffix_trimmed.chars().any(|c| c.is_alphanumeric())
                    {
                        item.subject = prefix_trimmed.to_string();
                        item.predicate = suffix_trimmed.to_string();
                    }
                }
            } else {
                let suffix_dot = format!(".{}", pred);
                if sub.len() > suffix_dot.len()
                    && sub
                        .to_ascii_lowercase()
                        .ends_with(&suffix_dot.to_ascii_lowercase())
                {
                    let stripped = sub[..sub.len() - suffix_dot.len()].trim();
                    if !stripped.is_empty() {
                        item.subject = stripped.to_string();
                    }
                }
            }
        }
        if self.note.is_none() && !self.title.trim().is_empty() {
            self.note = Some(NoteItem {
                title: self.title.clone(),
                content: self.content.clone(),
                tags: self.tags.clone(),
                mode: self.mode.clone(),
            });
        }
        self
    }

    /// Read an operation whose kind is the key: `{"add_memory": {…}, "confidence": 0.9}`.
    fn lift_keyed_operation(&mut self) {
        let Some((key, value)) = self
            .extra
            .iter()
            .find(|(key, _)| is_keyed_operation(key))
            .map(|(key, value)| (key.to_ascii_lowercase(), value.clone()))
        else {
            return;
        };
        let object = value.as_object();
        let field = |name: &str| {
            object
                .and_then(|object| object.get(name))
                .and_then(|value| value.as_str())
                .map(str::to_string)
        };
        if self.target_id.is_none() {
            self.target_id = field("target_id");
        }
        if self.target_ids.is_empty() {
            self.target_ids = object
                .and_then(|object| object.get("target_ids"))
                .and_then(|value| value.as_array())
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
        }
        if self.reason.is_none() {
            self.reason = field("reason");
        }
        if self.evidence.is_none() {
            self.evidence = field("evidence");
        }
        if self.confidence.is_none() {
            self.confidence = object
                .and_then(|object| object.get("confidence"))
                .and_then(|value| value.as_f64())
                .map(|value| value as f32);
        }

        // Some models add one wrapper too many: `{"add_memory": {"memory": {…}}}`.
        let payload = object
            .and_then(|object| object.get("memory").or_else(|| object.get("note")))
            .cloned()
            .unwrap_or(value);

        self.op = key.clone();
        match key.as_str() {
            "add_memory" | "memory_add" | "add" | "update_memory" | "memory_update" | "update"
            | "merge_memories" | "memory_merge" => {
                if self.memory.is_none() {
                    self.memory = serde_json::from_value::<Item>(payload).ok();
                }
            }
            "create_note" | "add_note" | "note_create" | "update_note" | "note_update"
            | "merge_notes" | "note_merge"
                if self.note.is_none() =>
            {
                self.note = serde_json::from_value::<NoteItem>(payload).ok();
            }
            _ => {}
        }
    }
}

/// Whether `key` is an operation name a model used as a wrapper key.
fn is_keyed_operation(key: &str) -> bool {
    matches!(
        key.trim().to_ascii_lowercase().as_str(),
        "add_memory"
            | "memory_add"
            | "add"
            | "update_memory"
            | "memory_update"
            | "update"
            | "delete_memory"
            | "memory_delete"
            | "merge_memories"
            | "memory_merge"
            | "create_note"
            | "add_note"
            | "note_create"
            | "update_note"
            | "note_update"
            | "merge_notes"
            | "note_merge"
            | "delete_note"
            | "note_delete"
            | "noop"
    )
}

/// The shape the saver expects back.
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct Distilled {
    #[serde(default)]
    pub(super) operations: Vec<Operation>,
    #[serde(default)]
    pub(super) memories: Vec<Item>,
    #[serde(default)]
    pub(super) notes: Vec<NoteItem>,
}

impl Distilled {
    /// Fold the legacy `{memories, notes}` shape into add operations.
    pub(super) fn into_operations(self) -> Vec<Operation> {
        let mut operations: Vec<Operation> = self
            .operations
            .into_iter()
            .map(Operation::normalized)
            .collect();
        for item in self.memories {
            let confidence = item.confidence;
            operations.push(Operation {
                op: "add_memory".to_string(),
                memory: Some(item),
                confidence,
                legacy: true,
                ..Operation::default()
            });
        }
        for note in self.notes {
            operations.push(Operation {
                op: "create_note".to_string(),
                note: Some(note),
                legacy: true,
                ..Operation::default()
            });
        }
        operations
    }
}

/// One candidate from the extract pass.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub(super) struct Candidate {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    pub(super) evidence: String,
    #[serde(default)]
    pub(super) confidence: Option<f32>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    storage: Option<String>,
}

/// The extract pass's answer.
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct CandidateSet {
    #[serde(default)]
    pub(super) candidates: Vec<Candidate>,
}

/// What the store already holds, in the form the model reads and the applier resolves against.
pub(super) struct Inventory {
    pub(super) memories: Vec<MemoryRecord>,
    pub(super) notes: Vec<Note>,
    /// note id -> 1-5 word pointer, joined from the `mem_ptr_<note_id>` records.
    pub(super) pointers: HashMap<String, String>,
    pub(super) memories_truncated: usize,
    pub(super) notes_truncated: usize,
}

impl Inventory {
    pub(super) fn total(&self) -> usize {
        self.memories.len() + self.notes.len()
    }

    pub(super) fn memory(&self, id: &str) -> Option<&MemoryRecord> {
        self.memories
            .iter()
            .find(|record| record.id.eq_ignore_ascii_case(id.trim()))
    }

    pub(super) fn note(&self, id: &str) -> Option<&Note> {
        self.notes
            .iter()
            .find(|note| note.id.eq_ignore_ascii_case(id.trim()))
    }
}

/// One validated operation, ready to apply.
pub(super) enum Planned {
    AddMemory {
        item: Item,
    },
    UpdateMemory {
        target: String,
        item: Item,
    },
    DeleteMemory {
        target: String,
        reason: String,
    },
    MergeMemories {
        targets: Vec<String>,
        item: Item,
    },
    CreateNote {
        note: NoteItem,
    },
    UpdateNote {
        target: String,
        note: NoteItem,
    },
    MergeNotes {
        targets: Vec<String>,
        note: NoteItem,
    },
    DeleteNote {
        target: String,
        reason: String,
    },
}

impl Planned {
    /// Apply memory changes first, then note changes, then deletes; a pointer refresh must see the
    /// note it points at.
    pub(super) fn rank(&self) -> u8 {
        match self {
            Self::AddMemory { .. } | Self::UpdateMemory { .. } | Self::MergeMemories { .. } => 0,
            Self::CreateNote { .. } | Self::UpdateNote { .. } | Self::MergeNotes { .. } => 1,
            Self::DeleteMemory { .. } => 2,
            Self::DeleteNote { .. } => 3,
        }
    }
}
