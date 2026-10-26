//! No-op engines for the eval runner.
//!
//! The eval measures the agent loop, the provider and the tools a case installs. Persistence is not
//! under test, and pointing the suite at the real engines would write to `~/.luna` and leak state
//! between cases — so the runner installs these instead.

use async_trait::async_trait;

use harness_core::{HarnessError, MemoryEngine, MemoryRecord, Note, NotesEngine};

const NOOP: &str = "the eval runner installs a no-op engine; the tool that reaches it is not registered by any case";

pub struct NoopMemory;

#[async_trait]
impl MemoryEngine for NoopMemory {
    async fn retrieve(
        &self,
        _query: &str,
        _limit: usize,
    ) -> Result<Vec<MemoryRecord>, HarnessError> {
        Ok(vec![])
    }

    async fn save(&self, _record: &MemoryRecord) -> Result<(), HarnessError> {
        Ok(())
    }

    async fn update(&self, _id: &str, _updated: &MemoryRecord) -> Result<(), HarnessError> {
        Ok(())
    }

    async fn forget(&self, _id: &str) -> Result<(), HarnessError> {
        Ok(())
    }

    async fn add_note_pointer(&self, _note_id: &str, _pointer: &str) -> Result<(), HarnessError> {
        Ok(())
    }

    fn format_as_system_prompt(&self, _memories: &[MemoryRecord]) -> String {
        String::new()
    }
}

pub struct NoopNotes;

#[async_trait]
impl NotesEngine for NoopNotes {
    async fn save_note(
        &self,
        _title: &str,
        _content: &str,
        _tags: &[String],
    ) -> Result<Note, HarnessError> {
        Err(HarnessError::NotesError(NOOP.to_string()))
    }

    async fn search_notes(&self, _query: &str, _limit: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(vec![])
    }

    async fn list_notes(&self, _limit: usize, _offset: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(vec![])
    }

    async fn read_note(&self, _id: &str) -> Result<Note, HarnessError> {
        Err(HarnessError::NotesError(NOOP.to_string()))
    }

    async fn update_note(
        &self,
        _id: &str,
        _title: Option<&str>,
        _content: Option<&str>,
        _tags: Option<&[String]>,
    ) -> Result<Note, HarnessError> {
        Err(HarnessError::NotesError(NOOP.to_string()))
    }

    async fn delete_note(&self, _id: &str) -> Result<bool, HarnessError> {
        Err(HarnessError::NotesError(NOOP.to_string()))
    }
}
