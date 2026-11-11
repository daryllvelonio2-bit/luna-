//! The background memory saver: a second reader that writes the records the turn did not.
//!
//! A session running with `memory.mode: background` does not write durable memory from its own
//! tool calls. Instead, after the turn's reply is on screen, this module reads the conversation
//! tail and asks a model the one question the main agent answers implicitly all turn long: *what
//! here is durable?* The answer comes back as a list of **operations** — add, update, delete,
//! merge, or an explicit `noop` — and every operation is validated against what the store already
//! holds before anything is written.
//!
//! Before the model sees anything, the pass files every complete user/assistant exchange as a raw
//! [`MemoryCategory::Transcript`] record. That half is deterministic: a provider failure, a
//! paraphrased reply, or an explicit `noop` cannot lose the conversation. The records are
//! content-addressed, so the overlapping interval and idle windows are idempotent, and they are
//! kept out of the always-loaded prompt while staying visible to `memory_search`.
//!
//! The model-facing half is deliberately conservative:
//!
//! - the model is a curator, not a summarizer. Its default answer is "save nothing";
//! - it sees the active memory cards *and* the active notes, with ids, so it can choose `update`
//!   or `delete` instead of blindly appending;
//! - every write needs an evidence quote that actually appears in the transcript;
//! - per-pass caps, a confidence floor, a near-duplicate check, and a secret scan run in code, not
//!   in the prompt;
//! - memories are lean index cards and notes are authoritative documents; a verified note gets
//!   a pointer card through the same bridge the agent's `save_note` uses, while an unverified one
//!   stays out of the always-loaded prompt;
//! - only user or tool evidence can create a note. The assistant's own answer is a claim, and is
//!   filed only when the user explicitly asked to keep it.
//!
//! Failure is cheap by design — the caller runs this detached from the turn and only logs errors,
//! so a saver that cannot reach its provider costs a warning, not a reply.
//!
//! The implementation is split by topic: [`prompt`] holds the two system prompts, [`model`] the
//! data types and limits, [`save`] the writer and its model calls, [`plan`] validation,
//! [`apply`] the writes, [`evidence`] normalization and matching, [`parse`] the JSON reader, and
//! [`transcript`] the rendered conversation.

mod apply;
mod evidence;
mod model;
mod parse;
mod plan;
mod prompt;
mod save;
mod transcript;

#[cfg(test)]
mod tests;

pub use model::{
    SaverPolicy, EVIDENCE_MAX_CHARS, INDEX_LIMIT, MAX_INVENTORY_CHARS, MAX_MEMORY_WRITES_PER_PASS,
    MAX_NOTE_WRITES_PER_PASS, MAX_OPERATIONS_PER_PASS, MAX_SCOUT_INVENTORY_CHARS, MESSAGE_CHAR_CAP,
    MIN_CONFIDENCE, MIN_NOTE_CHARS, NOTE_INDEX_LIMIT, TRANSCRIPT_CHAR_BUDGET, TWO_PASS_THRESHOLD,
};
pub use prompt::{EXTRACT_SYSTEM_PROMPT, SAVER_SYSTEM_PROMPT};
pub use save::MemorySaver;
