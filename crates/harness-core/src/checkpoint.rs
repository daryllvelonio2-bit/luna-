//! Per-turn file checkpoints, so an edit can be undone.
//!
//! The harness already refuses edits it was not allowed to make; a checkpoint answers the other
//! question — *the edit was allowed and it was wrong*. Before a path-mutating tool runs, the file
//! it is about to touch is copied into a per-turn directory; `/rewind <n>` copies those blobs back
//! and deletes the files the turn created.
//!
//! Shape on disk (inspectable with `ls`, no database, survives a crash mid-turn):
//!
//! ```text
//! ~/.luna/checkpoints/<session_id>/turn_0003/
//!   manifest.json   { seq, created_at, prompt, files: [{ path, existed, blob }] }
//!   blobs/0001      the file's bytes before the turn touched it
//! ```
//!
//! The manifest is rewritten on every capture rather than at the end of the turn, so a turn that
//! panics is still restorable. Only files a *tool* is about to write are captured — nothing else
//! copies the workspace, and `terminal` is not intercepted, because a shell command's targets are
//! not knowable from its argument string. That gap is documented rather than papered over.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::config::CheckpointSettings;
use crate::error::HarnessError;
use crate::traits::ToolMiddleware;
use crate::types::{ToolCall, ToolResult};

/// Default for `checkpoints.keep_turns`.
pub const DEFAULT_KEEP_TURNS: usize = 50;

/// Tools whose `path` argument names a file the call will write. Anything else is left alone.
const PATH_MUTATING_TOOLS: [&str; 2] = ["write_file", "patch"];

/// One file as it was before the turn touched it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckpointFile {
    /// Path as the tool named it, made relative to the workspace when it lies inside it.
    pub path: String,
    /// `false` means the file did not exist: restoring deletes it.
    pub existed: bool,
    /// Blob file name inside `blobs/`, absent when `existed` is `false`.
    pub blob: Option<String>,
}

/// One turn's snapshot, written to `manifest.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub seq: u32,
    pub session_id: String,
    pub created_at: String,
    /// The prompt that opened the turn, when it is known.
    pub prompt: Option<String>,
    pub files: Vec<CheckpointFile>,
}

/// What a restore did, for the console.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RestoreReport {
    pub restored: Vec<String>,
    /// Files the turn created; restoring removes them.
    pub deleted: Vec<String>,
    /// Paths the restore refused to touch, with the reason.
    pub skipped: Vec<String>,
}

impl RestoreReport {
    pub fn is_empty(&self) -> bool {
        self.restored.is_empty() && self.deleted.is_empty()
    }
}

/// A snapshot being filled as a turn runs.
///
/// Shared between the agent loop (which opens it) and the middleware (which adds to it), which is
/// why every method takes `&self` and the state is behind a mutex.
pub struct TurnSnapshot {
    dir: PathBuf,
    workspace: PathBuf,
    session_id: String,
    seq: u32,
    created_at: String,
    prompt: Option<String>,
    files: Mutex<Vec<CheckpointFile>>,
    next_blob: Mutex<u32>,
}

impl TurnSnapshot {
    pub fn seq(&self) -> u32 {
        self.seq
    }

    /// Record `path` before it is modified. A second capture of the same path is a no-op: the
    /// point is the state at the *start* of the turn, not before each individual call.
    pub fn capture(&self, path: &str) -> Result<(), HarnessError> {
        let absolute = self.absolute(path);
        let relative = self.relative_name(&absolute);

        {
            let files = self.files.lock().unwrap();
            if files.iter().any(|file| file.path == relative) {
                return Ok(());
            }
        }

        let entry = if absolute.is_file() {
            let bytes = std::fs::read(&absolute).map_err(|error| {
                HarnessError::Io(format!("checkpoint: cannot read {}: {error}", absolute.display()))
            })?;
            let mut next = self.next_blob.lock().unwrap();
            *next += 1;
            let blob = format!("{:04}", *next);
            let blob_dir = self.dir.join("blobs");
            std::fs::create_dir_all(&blob_dir)?;
            std::fs::write(blob_dir.join(&blob), bytes)?;
            CheckpointFile {
                path: relative,
                existed: true,
                blob: Some(blob),
            }
        } else {
            CheckpointFile {
                path: relative,
                existed: false,
                blob: None,
            }
        };

        self.files.lock().unwrap().push(entry);
        // Written now, not at the end of the turn: a turn that dies must still be restorable.
        self.persist()
    }

    fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            seq: self.seq,
            session_id: self.session_id.clone(),
            created_at: self.created_at.clone(),
            prompt: self.prompt.clone(),
            files: self.files.lock().unwrap().clone(),
        }
    }

    fn persist(&self) -> Result<(), HarnessError> {
        let manifest = serde_json::to_string_pretty(&self.checkpoint())?;
        std::fs::write(self.dir.join("manifest.json"), manifest)?;
        Ok(())
    }

    fn absolute(&self, path: &str) -> PathBuf {
        let candidate = Path::new(path);
        if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.workspace.join(candidate)
        }
    }

    fn relative_name(&self, absolute: &Path) -> String {
        absolute
            .strip_prefix(&self.workspace)
            .map(|rest| rest.to_string_lossy().to_string())
            .unwrap_or_else(|_| absolute.to_string_lossy().to_string())
    }
}

/// Owns the checkpoint directory and the turn currently open.
pub struct CheckpointController {
    store: CheckpointStore,
    enabled: bool,
    current: Mutex<Option<std::sync::Arc<TurnSnapshot>>>,
}

impl CheckpointController {
    pub fn new(root: PathBuf, workspace: PathBuf, settings: &CheckpointSettings) -> Self {
        Self {
            store: CheckpointStore::new(root, workspace).with_keep_turns(settings.keep_turns),
            enabled: settings.enabled,
            current: Mutex::new(None),
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn store(&self) -> &CheckpointStore {
        &self.store
    }

    /// Open a snapshot for this turn. Returns `None` when checkpoints are disabled.
    pub fn begin(&self, session_id: &str, prompt: Option<&str>) -> Option<std::sync::Arc<TurnSnapshot>> {
        if !self.enabled {
            return None;
        }
        match self.store.begin(session_id, prompt) {
            Ok(snapshot) => {
                let snapshot = std::sync::Arc::new(snapshot);
                *self.current.lock().unwrap() = Some(snapshot.clone());
                Some(snapshot)
            }
            Err(error) => {
                // A checkpoint that cannot be written must not stop the turn; it *is* a warning,
                // because the user has silently lost their undo for this turn.
                warn!("cannot open a checkpoint for this turn: {error}");
                None
            }
        }
    }

    /// Close the open snapshot; later captures during the same process are dropped.
    pub fn finish(&self) {
        *self.current.lock().unwrap() = None;
    }

    pub fn current(&self) -> Option<std::sync::Arc<TurnSnapshot>> {
        self.current.lock().unwrap().clone()
    }

    /// Capture the file a call is about to write, if it is a path-mutating tool.
    pub fn capture_call(&self, call: &ToolCall) -> Result<(), HarnessError> {
        if !PATH_MUTATING_TOOLS.contains(&call.name.as_str()) {
            return Ok(());
        }
        let Some(path) = call.arguments.get("path").and_then(|value| value.as_str()) else {
            return Ok(());
        };
        match self.current() {
            Some(snapshot) => snapshot.capture(path),
            None => Ok(()),
        }
    }
}

/// Registry middleware that fills the open snapshot before a write lands.
pub struct CheckpointMiddleware {
    controller: std::sync::Arc<CheckpointController>,
}

impl CheckpointMiddleware {
    pub fn new(controller: std::sync::Arc<CheckpointController>) -> Self {
        Self { controller }
    }
}

#[async_trait]
impl ToolMiddleware for CheckpointMiddleware {
    async fn on_pre_execute(&self, call: &mut ToolCall) -> Result<(), HarnessError> {
        // A checkpoint failure must not block the write: the user asked for it, and the tool has
        // its own read-before-write protection. This is undo, not permission.
        if let Err(error) = self.controller.capture_call(call) {
            warn!(tool = %call.name, "checkpoint capture failed: {error}");
        }
        Ok(())
    }

    async fn on_post_execute(
        &self,
        _call: &ToolCall,
        _result: &mut ToolResult,
    ) -> Result<(), HarnessError> {
        Ok(())
    }
}

/// The on-disk checkpoint area.
pub struct CheckpointStore {
    root: PathBuf,
    workspace: PathBuf,
    keep_turns: usize,
}

impl CheckpointStore {
    pub fn new(root: PathBuf, workspace: PathBuf) -> Self {
        Self {
            root,
            workspace,
            keep_turns: DEFAULT_KEEP_TURNS,
        }
    }

    pub fn with_keep_turns(mut self, keep_turns: usize) -> Self {
        self.keep_turns = if keep_turns == 0 {
            DEFAULT_KEEP_TURNS
        } else {
            keep_turns
        };
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn session_dir(&self, session_id: &str) -> PathBuf {
        self.root.join(sanitise(session_id))
    }

    /// Open the next turn's snapshot for `session_id`.
    pub fn begin(&self, session_id: &str, prompt: Option<&str>) -> Result<TurnSnapshot, HarnessError> {
        let dir = self.session_dir(session_id);
        std::fs::create_dir_all(&dir)?;

        let seq = self.list(session_id)?.last().map(|checkpoint| checkpoint.seq + 1).unwrap_or(1);
        let turn_dir = dir.join(turn_dir_name(seq));
        std::fs::create_dir_all(turn_dir.join("blobs"))?;

        let snapshot = TurnSnapshot {
            dir: turn_dir,
            workspace: self.workspace.clone(),
            session_id: session_id.to_string(),
            seq,
            created_at: now_rfc3339(),
            prompt: prompt.map(str::to_string),
            files: Mutex::new(Vec::new()),
            next_blob: Mutex::new(0),
        };
        snapshot.persist()?;
        self.prune(session_id)?;
        Ok(snapshot)
    }

    /// Every checkpoint for the session, oldest first.
    pub fn list(&self, session_id: &str) -> Result<Vec<Checkpoint>, HarnessError> {
        let dir = self.session_dir(session_id);
        if !dir.is_dir() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let manifest = entry.path().join("manifest.json");
            if !manifest.is_file() {
                continue;
            }
            let raw = match std::fs::read_to_string(&manifest) {
                Ok(raw) => raw,
                Err(error) => {
                    warn!(path = %manifest.display(), "cannot read checkpoint manifest: {error}");
                    continue;
                }
            };
            match serde_json::from_str::<Checkpoint>(&raw) {
                Ok(checkpoint) => out.push(checkpoint),
                Err(error) => {
                    warn!(path = %manifest.display(), "skipping malformed checkpoint: {error}")
                }
            }
        }
        out.sort_by_key(|checkpoint| checkpoint.seq);
        Ok(out)
    }

    /// Put the workspace back the way it was when turn `seq` started.
    ///
    /// Restoring is per-file and refuses anything outside the workspace, so a manifest cannot be
    /// used to write to an arbitrary path.
    pub fn restore(&self, session_id: &str, seq: u32) -> Result<RestoreReport, HarnessError> {
        let checkpoint = self
            .list(session_id)?
            .into_iter()
            .find(|checkpoint| checkpoint.seq == seq)
            .ok_or_else(|| {
                HarnessError::ConfigurationError(format!(
                    "no checkpoint {seq} in session {session_id}"
                ))
            })?;

        let turn_dir = self.session_dir(session_id).join(turn_dir_name(seq));
        let mut report = RestoreReport::default();

        for file in &checkpoint.files {
            let target = self.workspace.join(&file.path);
            if !self.inside_workspace(&target) {
                report
                    .skipped
                    .push(format!("{} (outside the workspace)", file.path));
                continue;
            }

            if file.existed {
                let Some(blob) = file.blob.as_ref() else {
                    report.skipped.push(format!("{} (no blob recorded)", file.path));
                    continue;
                };
                let blob_path = turn_dir.join("blobs").join(blob);
                let bytes = std::fs::read(&blob_path).map_err(|error| {
                    HarnessError::Io(format!("cannot read {}: {error}", blob_path.display()))
                })?;
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&target, bytes)?;
                report.restored.push(file.path.clone());
            } else if target.exists() {
                std::fs::remove_file(&target)?;
                report.deleted.push(file.path.clone());
            } else {
                report
                    .skipped
                    .push(format!("{} (already absent)", file.path));
            }
        }

        debug!(
            session = session_id,
            seq,
            restored = report.restored.len(),
            deleted = report.deleted.len(),
            "checkpoint restored"
        );
        Ok(report)
    }

    /// Drop the oldest turns past `keep_turns`.
    fn prune(&self, session_id: &str) -> Result<(), HarnessError> {
        let checkpoints = self.list(session_id)?;
        if checkpoints.len() <= self.keep_turns {
            return Ok(());
        }
        let dir = self.session_dir(session_id);
        for checkpoint in checkpoints.iter().take(checkpoints.len() - self.keep_turns) {
            let path = dir.join(turn_dir_name(checkpoint.seq));
            if let Err(error) = std::fs::remove_dir_all(&path) {
                warn!(path = %path.display(), "cannot prune checkpoint: {error}");
            }
        }
        Ok(())
    }

    fn inside_workspace(&self, target: &Path) -> bool {
        // Lexical containment: a restore must not follow `..` out of the workspace, and must not
        // depend on the path existing (deleting a created file is the normal case).
        let mut normalised = PathBuf::new();
        for component in target.components() {
            match component {
                std::path::Component::ParentDir => {
                    normalised.pop();
                }
                other => normalised.push(other.as_os_str()),
            }
        }
        match (normalised.canonicalize(), self.workspace.canonicalize()) {
            (Ok(target), Ok(workspace)) => target.starts_with(workspace),
            // Neither side exists yet: compare the lexical forms.
            _ => normalised.starts_with(&self.workspace),
        }
    }
}

fn turn_dir_name(seq: u32) -> String {
    format!("turn_{seq:04}")
}

/// Session ids are user-supplied strings; they become a single directory component.
fn sanitise(session_id: &str) -> String {
    let cleaned: String = session_id
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' { ch } else { '_' })
        .collect();
    if cleaned.is_empty() || cleaned == "." || cleaned == ".." {
        "session".to_string()
    } else {
        cleaned
    }
}

fn now_rfc3339() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Seconds since the epoch is enough for ordering and does not drag in a date library.
    format!("{now}")
}

/// Group checkpoints by seq for callers that want to show "turn n: files" lines.
pub fn summarise(checkpoints: &[Checkpoint]) -> BTreeMap<u32, usize> {
    checkpoints
        .iter()
        .map(|checkpoint| (checkpoint.seq, checkpoint.files.len()))
        .collect()
}
