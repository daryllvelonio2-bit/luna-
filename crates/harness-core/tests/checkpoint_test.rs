//! Checkpoint/rewind contracts: what a turn snapshots, what restoring one does, and what it
//! refuses to do. These are filesystem-level tests — the snapshots are ordinary files, so the
//! assertions read them back the way an operator would after a crash.

use std::sync::Arc;

use harness_core::tools::{ReadCache, WriteFileTool};
use harness_core::*;

struct Fixture {
    _tmp: tempfile::TempDir,
    workspace: std::path::PathBuf,
    store: CheckpointStore,
}

fn fixture(keep_turns: usize) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let store = CheckpointStore::new(tmp.path().join("checkpoints"), workspace.clone())
        .with_keep_turns(keep_turns);
    Fixture {
        _tmp: tmp,
        workspace,
        store,
    }
}

fn write(workspace: &std::path::Path, rel: &str, body: &str) {
    let path = workspace.join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
}

fn read(workspace: &std::path::Path, rel: &str) -> String {
    std::fs::read_to_string(workspace.join(rel)).unwrap()
}

#[tokio::test]
async fn restore_puts_back_the_bytes_the_turn_found() {
    let f = fixture(50);
    write(&f.workspace, "src/main.rs", "fn main() { old(); }");

    let turn = f.store.begin("s1", Some("refactor")).unwrap();
    turn.capture("src/main.rs").unwrap();
    write(&f.workspace, "src/main.rs", "fn main() { new(); }");

    // The manifest is already on disk: a turn that dies must still be restorable.
    assert_eq!(f.store.list("s1").unwrap().len(), 1);

    let report = f.store.restore("s1", 1).unwrap();
    assert_eq!(report.restored, vec!["src/main.rs".to_string()]);
    assert_eq!(read(&f.workspace, "src/main.rs"), "fn main() { old(); }");
}

#[tokio::test]
async fn restoring_a_turn_deletes_the_files_it_created() {
    let f = fixture(50);
    let turn = f.store.begin("s1", None).unwrap();
    turn.capture("generated.rs").unwrap();
    write(&f.workspace, "generated.rs", "// created by the turn");

    let report = f.store.restore("s1", 1).unwrap();
    assert_eq!(report.deleted, vec!["generated.rs".to_string()]);
    assert!(!f.workspace.join("generated.rs").exists());
}

#[tokio::test]
async fn a_file_is_snapshotted_once_per_turn_not_once_per_call() {
    let f = fixture(50);
    write(&f.workspace, "a.txt", "original");

    let turn = f.store.begin("s1", None).unwrap();
    turn.capture("a.txt").unwrap();
    write(&f.workspace, "a.txt", "first edit");
    // Second capture must not overwrite the snapshot with "first edit".
    turn.capture("a.txt").unwrap();
    write(&f.workspace, "a.txt", "second edit");

    let checkpoint = f.store.list("s1").unwrap().remove(0);
    assert_eq!(checkpoint.files.len(), 1, "one entry per path per turn");

    f.store.restore("s1", 1).unwrap();
    assert_eq!(read(&f.workspace, "a.txt"), "original");
}

#[tokio::test]
async fn each_turn_is_restorable_independently() {
    let f = fixture(50);
    write(&f.workspace, "a.txt", "v0");

    let t1 = f.store.begin("s1", None).unwrap();
    t1.capture("a.txt").unwrap();
    write(&f.workspace, "a.txt", "v1");

    let t2 = f.store.begin("s1", None).unwrap();
    t2.capture("a.txt").unwrap();
    write(&f.workspace, "a.txt", "v2");

    // Back to the start of turn 2, then to the start of turn 1.
    f.store.restore("s1", 2).unwrap();
    assert_eq!(read(&f.workspace, "a.txt"), "v1");
    f.store.restore("s1", 1).unwrap();
    assert_eq!(read(&f.workspace, "a.txt"), "v0");
}

#[tokio::test]
async fn keep_turns_prunes_the_oldest_snapshots() {
    let f = fixture(2);
    write(&f.workspace, "a.txt", "v0");

    for turn in 0..4 {
        let snapshot = f.store.begin("s1", None).unwrap();
        snapshot.capture("a.txt").unwrap();
        write(&f.workspace, "a.txt", &format!("v{turn}"));
    }

    let kept: Vec<u32> = f
        .store
        .list("s1")
        .unwrap()
        .iter()
        .map(|checkpoint| checkpoint.seq)
        .collect();
    assert_eq!(kept, vec![3, 4], "only the newest keep_turns snapshots survive");
    assert!(
        f.store.restore("s1", 1).is_err(),
        "a pruned checkpoint cannot be restored"
    );
}

#[tokio::test]
async fn restore_refuses_to_write_outside_the_workspace() {
    let f = fixture(50);
    let turn = f.store.begin("s1", None).unwrap();
    // A manifest written by something else must not be able to escape the workspace.
    turn.capture("../../etc/passwd").unwrap();

    let report = f.store.restore("s1", 1).unwrap();
    assert!(report.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert!(report.skipped[0].contains("outside the workspace"));
}

#[tokio::test]
async fn a_disabled_controller_opens_no_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();

    let controller = CheckpointController::new(
        tmp.path().join("checkpoints"),
        workspace.clone(),
        &CheckpointSettings {
            enabled: false,
            keep_turns: 5,
        },
    );

    assert!(controller.begin("s1", Some("go")).is_none());
    assert!(controller.current().is_none());
    let call = ToolCall {
        id: "call_1".to_string(),
        name: "write_file".to_string(),
        arguments: serde_json::json!({"path": "a.txt", "content": "x"}),
        signature: None,
    };
    controller.capture_call(&call).unwrap();
    assert!(
        controller.store().list("s1").unwrap().is_empty(),
        "disabled checkpoints must not touch the disk"
    );
}

#[tokio::test]
async fn capture_call_only_snapshots_path_mutating_tools() {
    let f = fixture(50);
    write(&f.workspace, "a.txt", "before");
    let controller = Arc::new(CheckpointController::new(
        f.store.root().to_path_buf(),
        f.workspace.clone(),
        &CheckpointSettings {
            enabled: true,
            keep_turns: 50,
        },
    ));
    controller.begin("s1", Some("edit"));

    let read_call = ToolCall {
        id: "call_1".to_string(),
        name: "read_file".to_string(),
        arguments: serde_json::json!({"path": "a.txt"}),
        signature: None,
    };
    controller.capture_call(&read_call).unwrap();
    assert!(controller.store().list("s1").unwrap()[0].files.is_empty());

    let write_call = ToolCall {
        id: "call_2".to_string(),
        name: "write_file".to_string(),
        arguments: serde_json::json!({"path": "a.txt", "content": "after"}),
        signature: None,
    };
    controller.capture_call(&write_call).unwrap();

    let checkpoint = controller.store().list("s1").unwrap().remove(0);
    assert_eq!(checkpoint.files.len(), 1);
    assert_eq!(checkpoint.files[0].path, "a.txt");
    assert!(checkpoint.files[0].existed);
}

#[tokio::test]
async fn an_absolute_path_outside_the_workspace_is_stored_absolute_and_skipped() {
    let f = fixture(50);
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("secret.txt");
    std::fs::write(&target, "s").unwrap();

    let turn = f.store.begin("s1", None).unwrap();
    turn.capture(target.to_str().unwrap()).unwrap();

    let report = f.store.restore("s1", 1).unwrap();
    assert!(report.is_empty());
    assert_eq!(report.skipped.len(), 1);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "s");
}

#[tokio::test]
async fn the_middleware_snapshots_the_file_before_a_write_lands() {
    use harness_core::ToolMiddleware;

    let f = fixture(50);
    write(&f.workspace, "notes.md", "original");
    let controller = Arc::new(CheckpointController::new(
        f.store.root().to_path_buf(),
        f.workspace.clone(),
        &CheckpointSettings {
            enabled: true,
            keep_turns: 50,
        },
    ));
    controller.begin("s1", None);

    let middleware = CheckpointMiddleware::new(controller.clone());
    let mut call = ToolCall {
        id: "call_1".to_string(),
        name: "patch".to_string(),
        arguments: serde_json::json!({"path": "notes.md", "old_string": "original", "new_string": "patched"}),
        signature: None,
    };
    middleware.on_pre_execute(&mut call).await.unwrap();
    write(&f.workspace, "notes.md", "patched");

    let report = f.store.restore("s1", 1).unwrap();
    assert_eq!(report.restored, vec!["notes.md".to_string()]);
    assert_eq!(read(&f.workspace, "notes.md"), "original");
}

#[tokio::test]
async fn checkpoints_default_to_on_and_fifty_turns() {
    let defaults = CheckpointSettings::default();
    assert!(defaults.enabled, "undo is a safety net, not an experiment");
    assert_eq!(defaults.keep_turns, DEFAULT_KEEP_TURNS);
}

// ---- end-to-end: agent turn → middleware → restore ------------------------------------------

struct WriteProvider {
    turns: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl ModelProvider for WriteProvider {
    fn provider_id(&self) -> &str {
        "mock"
    }

    async fn fetch_models(&self) -> Result<Vec<String>, HarnessError> {
        Ok(vec!["mock".to_string()])
    }

    async fn complete(
        &self,
        _messages: &[Message],
        _tools: &[ToolDefinition],
        _options: &CompletionOptions,
    ) -> Result<Message, HarnessError> {
        // The tool refuses to overwrite an unread file, so the model reads it first — the review
        // before the write is part of the flow this test exercises.
        let turn = self.turns.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        match turn {
            0 => Ok(Message::assistant_with_tools(vec![ToolCall {
                id: "call_1".to_string(),
                name: "read_file".to_string(),
                arguments: serde_json::json!({"path": "goal.md"}),
                signature: None,
            }])),
            1 => Ok(Message::assistant_with_tools(vec![ToolCall {
                id: "call_2".to_string(),
                name: "write_file".to_string(),
                arguments: serde_json::json!({"path": "goal.md", "content": "rewritten by the model"}),
                signature: None,
            }])),
            _ => Ok(Message::assistant("done")),
        }
    }

    async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, HarnessError> {
        Ok(vec![])
    }
}

struct NoMemory;
#[async_trait::async_trait]
impl MemoryEngine for NoMemory {
    async fn retrieve(&self, _q: &str, _l: usize) -> Result<Vec<MemoryRecord>, HarnessError> {
        Ok(vec![])
    }
    async fn save(&self, _r: &MemoryRecord) -> Result<(), HarnessError> {
        Ok(())
    }
    async fn update(&self, _i: &str, _r: &MemoryRecord) -> Result<(), HarnessError> {
        Ok(())
    }
    async fn forget(&self, _i: &str) -> Result<(), HarnessError> {
        Ok(())
    }
    async fn add_note_pointer(&self, _i: &str, _p: &str) -> Result<(), HarnessError> {
        Ok(())
    }
    fn format_as_system_prompt(&self, _m: &[MemoryRecord]) -> String {
        String::new()
    }
}

struct NoNotes;
#[async_trait::async_trait]
impl NotesEngine for NoNotes {
    async fn save_note(&self, _t: &str, _c: &str, _tags: &[String]) -> Result<Note, HarnessError> {
        unimplemented!()
    }
    async fn search_notes(&self, _q: &str, _l: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(vec![])
    }
    async fn list_notes(&self, _l: usize, _o: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(vec![])
    }
    async fn read_note(&self, _i: &str) -> Result<Note, HarnessError> {
        unimplemented!()
    }
    async fn update_note(
        &self,
        _i: &str,
        _t: Option<&str>,
        _c: Option<&str>,
        _tags: Option<&[String]>,
    ) -> Result<Note, HarnessError> {
        unimplemented!()
    }
    async fn delete_note(&self, _i: &str) -> Result<bool, HarnessError> {
        unimplemented!()
    }
}

struct NoEvolver;
#[async_trait::async_trait]
impl EvolverEngine for NoEvolver {
    async fn inspect_source(&self, _p: &str) -> Result<String, HarnessError> {
        Ok(String::new())
    }
    async fn propose_patch(&self, _d: &str, _diff: &str) -> Result<String, HarnessError> {
        Ok(String::new())
    }
    async fn test_patch_sandbox(&self, _p: &str) -> Result<bool, HarnessError> {
        Ok(true)
    }
    async fn apply_patch(&self, _p: &str) -> Result<(), HarnessError> {
        Ok(())
    }
}

#[tokio::test]
async fn a_whole_turn_is_undoable_through_the_production_wiring() {
    use tokio_util::sync::CancellationToken;

    let tmp = tempfile::tempdir().unwrap();
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join("goal.md"), "the original goal").unwrap();

    let controller = Arc::new(CheckpointController::new(
        tmp.path().join("checkpoints"),
        workspace.clone(),
        &CheckpointSettings::default(),
    ));

    let registry = Arc::new(DefaultToolRegistry::new());
    let read_cache = ReadCache::new();
    registry
        .register(Arc::new(harness_core::tools::ReadFileTool::new(
            workspace.clone(),
            100_000,
            read_cache.clone(),
        )))
        .await
        .unwrap();
    registry
        .register(Arc::new(WriteFileTool::new(
            workspace.clone(),
            read_cache,
        )))
        .await
        .unwrap();
    registry
        .register_middleware(Arc::new(CheckpointMiddleware::new(controller.clone())))
        .await;

    let agent = Agent::new(
        Arc::new(WriteProvider {
            turns: std::sync::atomic::AtomicUsize::new(0),
        }),
        Arc::new(NoMemory),
        Arc::new(NoNotes),
        Arc::new(NoEvolver),
        registry,
        AgentConfig::default(),
    )
    .with_approval(Arc::new(AllowAll), ApprovalSettings::default())
    .with_checkpoints(controller.clone());

    let outcome = agent
        .run_turn("rewrite the goal", CancellationToken::new())
        .await
        .expect("turn completes");
    assert!(!outcome.tool_calls[1].is_error, "the write must succeed: {}", outcome.tool_calls[1].output);
    assert_eq!(
        std::fs::read_to_string(workspace.join("goal.md")).unwrap(),
        "rewritten by the model"
    );

    // The agent closed the snapshot at the end of the turn; `/rewind 1` is what the user runs.
    assert!(controller.current().is_none(), "the turn must close its snapshot");
    let checkpoints = controller.store().list("unbound").unwrap();
    assert_eq!(checkpoints.len(), 1);
    assert_eq!(checkpoints[0].prompt.as_deref(), Some("rewrite the goal"));

    let report = controller.store().restore("unbound", 1).unwrap();
    assert_eq!(report.restored, vec!["goal.md".to_string()]);
    assert_eq!(
        std::fs::read_to_string(workspace.join("goal.md")).unwrap(),
        "the original goal"
    );
}
