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

/// Directories the external-change scan never walks into. Version-control internals and build
/// output change constantly and are never what a person means by "what did that turn do".
pub const DEFAULT_IGNORED_DIRS: [&str; 10] = [
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".mypy_cache",
];

/// Files past this many are not stamped: a workspace that large is better served by git than by a
/// per-turn walk, and the manifest says so rather than silently pretending to have looked.
pub const MAX_SCANNED_FILES: usize = 20_000;

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

/// A file the turn changed that no file tool recorded — a `git checkout`, a `sed -i`, a formatter.
///
/// There is no blob behind these, by construction: the change happened inside a shell command, and
/// snapshotting it would mean snapshotting the whole tree on every turn. They are *named* anyway, so
/// a review can say what it cannot undo instead of quietly leaving it out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnrecordedFile {
    pub path: String,
    pub kind: ChangeKind,
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
    /// Files that changed without a file tool touching them. Empty on turn 1 of a session that was
    /// opened before this scan existed, and empty when `checkpoints.track_external` is off.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unrecorded: Vec<UnrecordedFile>,
    /// Why the scan is incomplete, when it is (a truncated walk, a filesystem that refused).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scan_note: Option<String>,
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

/// What happened to one file between the start of a turn and now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Modified,
    Deleted,
    /// Recorded by the turn but byte-identical afterwards (a no-op write, or a restore).
    Unchanged,
}

impl ChangeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Unchanged => "unchanged",
        }
    }

    /// The one-character marker a stat line uses (`+ ~ - ·`).
    pub fn marker(&self) -> char {
        match self {
            Self::Created => '+',
            Self::Modified => '~',
            Self::Deleted => '-',
            Self::Unchanged => '·',
        }
    }
}

/// One file's change inside a turn.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    pub kind: ChangeKind,
    /// Lines added, by the unified diff's own counting (`+` lines minus the header).
    pub additions: usize,
    pub deletions: usize,
    /// A unified diff, or an explanatory line for a binary/oversized file.
    pub diff: String,
    /// Why the diff is not a text diff, when it is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// A turn, reviewed: what the workspace looked like when it started, and what changed.
///
/// This is the read side of a checkpoint. `/rewind` answers "put it back"; this answers "what did
/// it actually do", which is the question a person asks *before* deciding to rewind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnDiff {
    pub seq: u32,
    pub session_id: String,
    pub created_at: String,
    pub prompt: Option<String>,
    pub files: Vec<FileDiff>,
    /// Changed by the turn, but outside the file tools: visible, not undoable. Kept separate from
    /// `files` because a review that mixed them would imply a rewind can restore them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unrecorded: Vec<UnrecordedFile>,
}

impl TurnDiff {
    pub fn summary(&self) -> DiffSummary {
        let mut summary = DiffSummary {
            files: self.files.len(),
            ..DiffSummary::default()
        };
        for file in &self.files {
            summary.additions += file.additions;
            summary.deletions += file.deletions;
            match file.kind {
                ChangeKind::Created => summary.created += 1,
                ChangeKind::Modified => summary.modified += 1,
                ChangeKind::Deleted => summary.deleted += 1,
                ChangeKind::Unchanged => summary.unchanged += 1,
            }
        }
        summary
    }

    /// Files that actually changed, in manifest order.
    pub fn changed(&self) -> impl Iterator<Item = &FileDiff> {
        self.files
            .iter()
            .filter(|file| file.kind != ChangeKind::Unchanged)
    }

    pub fn is_empty(&self) -> bool {
        self.changed().next().is_none()
    }
}

/// Totals for a turn, and for the session above it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffSummary {
    pub files: usize,
    pub created: usize,
    pub modified: usize,
    pub deleted: usize,
    pub unchanged: usize,
    pub additions: usize,
    pub deletions: usize,
}

impl DiffSummary {
    /// `+2 ~1 -0 ·3` — the stat line, with the unchanged count omitted when there is none.
    pub fn stat_line(&self) -> String {
        let mut line = format!("+{} ~{} -{}", self.created, self.modified, self.deleted);
        if self.unchanged > 0 {
            line.push_str(&format!(" ·{}", self.unchanged));
        }
        line
    }
}

fn sanitize_checkpoint_prompt(raw: &str) -> String {
    if !raw.contains("data:image/") && raw.len() <= 4096 {
        return raw.to_string();
    }
    let mut out = String::new();
    let mut rest = raw;
    while let Some(start) = rest.find("data:image/") {
        let (before, after_start) = rest.split_at(start);
        out.push_str(before);
        let end = after_start
            .find(char::is_whitespace)
            .unwrap_or(after_start.len());
        out.push_str("[image]");
        rest = &after_start[end..];
    }
    out.push_str(rest);
    if out.len() > 4096 {
        let mut end = 4096;
        while !out.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        out.truncate(end);
        out.push_str("... [truncated]");
    }
    out
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
    /// Path → stamp for the workspace when the turn opened; `None` when tracking is off.
    baseline: Option<TreeScan>,
    ignore: Vec<String>,
    unrecorded: Mutex<Vec<UnrecordedFile>>,
    scan_note: Mutex<Option<String>>,
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
            let files = self.files.lock().unwrap_or_else(|e| e.into_inner());
            if files.iter().any(|file| file.path == relative) {
                return Ok(());
            }
        }

        let entry = if absolute.is_file() {
            let bytes = std::fs::read(&absolute).map_err(|error| {
                HarnessError::Io(format!(
                    "checkpoint: cannot read {}: {error}",
                    absolute.display()
                ))
            })?;
            let mut next = self.next_blob.lock().unwrap_or_else(|e| e.into_inner());
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

        self.files
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(entry);
        // Written now, not at the end of the turn: a turn that dies must still be restorable.
        self.persist()
    }

    fn checkpoint(&self) -> Checkpoint {
        Checkpoint {
            seq: self.seq,
            session_id: self.session_id.clone(),
            created_at: self.created_at.clone(),
            prompt: self.prompt.clone(),
            files: self.files.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            unrecorded: self
                .unrecorded
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            scan_note: self
                .scan_note
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }

    /// Close the turn: rescan the workspace and record what changed without a file tool.
    ///
    /// Runs once, from the agent loop's guard, so a turn that errors still gets its review filled
    /// in. A scan that cannot finish leaves a note rather than an empty list — "we did not look" and
    /// "nothing happened" must not read the same.
    pub fn finish(&self) {
        let Some(baseline) = self.baseline.as_ref() else {
            return;
        };
        let after = TreeScan::of(&self.workspace, &self.ignore);
        let recorded: Vec<String> = self
            .files
            .lock()
            .unwrap()
            .iter()
            .map(|file| file.path.clone())
            .collect();
        let changes = baseline.changes_against(&after, &recorded);
        *self.unrecorded.lock().unwrap_or_else(|e| e.into_inner()) = changes;
        *self.scan_note.lock().unwrap_or_else(|e| e.into_inner()) = after.truncated.then(|| {
            format!(
                "the workspace walk stopped early (more than {MAX_SCANNED_FILES} files, or an unreadable directory): \
                 changes made by shell commands may be missing from this list"
            )
        });
        if let Err(error) = self.persist() {
            warn!(
                seq = self.seq,
                "cannot write the finished checkpoint: {error}"
            );
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
            store: CheckpointStore::new(root, workspace)
                .with_keep_turns(settings.keep_turns)
                .with_external_tracking(settings.track_external, settings.ignore.clone()),
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

    pub fn workspace(&self) -> PathBuf {
        self.store.workspace()
    }

    pub fn set_workspace(&self, workspace: PathBuf) {
        self.store.set_workspace(workspace);
    }

    /// Open a snapshot for this turn. Returns `None` when checkpoints are disabled.
    pub fn begin(
        &self,
        session_id: &str,
        prompt: Option<&str>,
    ) -> Option<std::sync::Arc<TurnSnapshot>> {
        if !self.enabled {
            return None;
        }
        match self.store.begin(session_id, prompt) {
            Ok(snapshot) => {
                let snapshot = std::sync::Arc::new(snapshot);
                *self.current.lock().unwrap_or_else(|e| e.into_inner()) = Some(snapshot.clone());
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
    ///
    /// The snapshot is finished (workspace rescan included) *before* it is dropped, so the manifest
    /// on disk is complete even though the turn's guard is what triggers this.
    pub fn finish(&self) {
        let current = self
            .current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        if let Some(snapshot) = current {
            snapshot.finish();
        }
    }

    pub fn current(&self) -> Option<std::sync::Arc<TurnSnapshot>> {
        self.current
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
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
    workspace: std::sync::RwLock<PathBuf>,
    keep_turns: usize,
    track_external: bool,
    ignore: Vec<String>,
}

impl CheckpointStore {
    pub fn new(root: PathBuf, workspace: PathBuf) -> Self {
        Self {
            root,
            workspace: std::sync::RwLock::new(workspace),
            keep_turns: DEFAULT_KEEP_TURNS,
            track_external: false,
            ignore: DEFAULT_IGNORED_DIRS
                .iter()
                .map(|dir| dir.to_string())
                .collect(),
        }
    }

    pub fn workspace(&self) -> PathBuf {
        self.workspace
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn set_workspace(&self, workspace: PathBuf) {
        *self.workspace.write().unwrap_or_else(|e| e.into_inner()) = workspace;
    }

    /// Watch the workspace for changes the file tools did not record (`checkpoints.track_external`).
    pub fn with_external_tracking(mut self, enabled: bool, ignore: Vec<String>) -> Self {
        self.track_external = enabled;
        if !ignore.is_empty() {
            self.ignore = ignore;
        }
        self
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
    pub fn begin(
        &self,
        session_id: &str,
        prompt: Option<&str>,
    ) -> Result<TurnSnapshot, HarnessError> {
        let dir = self.session_dir(session_id);
        std::fs::create_dir_all(&dir)?;

        let seq = self
            .list(session_id)?
            .last()
            .map(|checkpoint| checkpoint.seq + 1)
            .unwrap_or(1);
        let turn_dir = dir.join(turn_dir_name(seq));
        std::fs::create_dir_all(turn_dir.join("blobs"))?;

        // Stamped before the turn runs, so a change made by a shell command has a "before" to be
        // compared against. Metadata only: this is a directory walk, not a copy of the tree.
        let ws = self.workspace();
        let baseline = self.track_external.then(|| TreeScan::of(&ws, &self.ignore));

        let snapshot = TurnSnapshot {
            dir: turn_dir,
            workspace: ws,
            session_id: session_id.to_string(),
            seq,
            created_at: now_rfc3339(),
            prompt: prompt.map(sanitize_checkpoint_prompt),
            files: Mutex::new(Vec::new()),
            next_blob: Mutex::new(0),
            baseline,
            ignore: self.ignore.clone(),
            unrecorded: Mutex::new(Vec::new()),
            scan_note: Mutex::new(None),
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
        let ws = self.workspace();

        for file in &checkpoint.files {
            let target = ws.join(&file.path);
            if !self.inside_workspace(&target) {
                report
                    .skipped
                    .push(format!("{} (outside the workspace)", file.path));
                continue;
            }

            if file.existed {
                let Some(blob) = file.blob.as_ref() else {
                    report
                        .skipped
                        .push(format!("{} (no blob recorded)", file.path));
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

        // Everything the shell changed is a file this manifest cannot put back. Saying so is the
        // difference between a rewind that worked and a rewind that looked like it worked.
        for unrecorded in &checkpoint.unrecorded {
            report.skipped.push(format!(
                "{} (changed by a shell command — no snapshot to restore)",
                unrecorded.path
            ));
        }

        debug!(
            session = session_id,
            seq,
            restored = report.restored.len(),
            deleted = report.deleted.len(),
            unrecorded = checkpoint.unrecorded.len(),
            "checkpoint restored"
        );
        Ok(report)
    }

    /// Review a turn: the workspace's files as the turn left them, against the snapshot taken when
    /// it started.
    ///
    /// Reads only. Nothing here writes to the workspace, so asking "what did that turn do" is
    /// always safe — including on a turn you are about to rewind.
    pub fn diff(&self, session_id: &str, seq: u32) -> Result<TurnDiff, HarnessError> {
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
        let mut files = Vec::with_capacity(checkpoint.files.len());
        let ws = self.workspace();

        for file in &checkpoint.files {
            let before = match (file.existed, file.blob.as_ref()) {
                (true, Some(blob)) => match std::fs::read(turn_dir.join("blobs").join(blob)) {
                    Ok(bytes) => Some(bytes),
                    Err(error) => {
                        warn!(path = %file.path, "checkpoint blob unreadable: {error}");
                        None
                    }
                },
                _ => None,
            };

            let target = ws.join(&file.path);
            let after = std::fs::read(&target).ok();

            files.push(file_diff(&file.path, before, after));
        }

        // A file that a tool recorded *and* the scan saw changed is covered by the blob: drop it
        // from the unrecorded list rather than reporting the same edit twice.
        let recorded: Vec<&str> = checkpoint
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect();
        let unrecorded = checkpoint
            .unrecorded
            .into_iter()
            .filter(|file| !recorded.contains(&file.path.as_str()))
            .collect();

        Ok(TurnDiff {
            seq: checkpoint.seq,
            session_id: checkpoint.session_id,
            created_at: checkpoint.created_at,
            prompt: checkpoint.prompt,
            files,
            unrecorded,
        })
    }

    /// The same review for every turn of a session, oldest first.
    pub fn diff_all(&self, session_id: &str) -> Result<Vec<TurnDiff>, HarnessError> {
        self.list(session_id)?
            .into_iter()
            .map(|checkpoint| self.diff(session_id, checkpoint.seq))
            .collect()
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
        let ws = self.workspace();
        match (normalised.canonicalize(), ws.canonicalize()) {
            (Ok(target), Ok(workspace)) => target.starts_with(workspace),
            // Neither side exists yet: compare the lexical forms.
            _ => normalised.starts_with(&ws),
        }
    }
}

/// Classify one file and produce its diff.
///
/// The kinds are derived from the *bytes*, not from the manifest: a turn that wrote a file back to
/// exactly what it was did not change it, and saying otherwise would make every rewind look
/// destructive. Binary content is reported as a kind and a note rather than as mojibake.
fn file_diff(path: &str, before: Option<Vec<u8>>, after: Option<Vec<u8>>) -> FileDiff {
    let kind = match (&before, &after) {
        (None, None) => ChangeKind::Unchanged,
        (None, Some(_)) => ChangeKind::Created,
        (Some(_), None) => ChangeKind::Deleted,
        (Some(old), Some(new)) if old == new => ChangeKind::Unchanged,
        (Some(_), Some(_)) => ChangeKind::Modified,
    };

    if kind == ChangeKind::Unchanged {
        return FileDiff {
            path: path.to_string(),
            kind,
            additions: 0,
            deletions: 0,
            diff: String::new(),
            note: None,
        };
    }

    let old_text = before.as_deref().map(std::str::from_utf8);
    let new_text = after.as_deref().map(std::str::from_utf8);
    let (old_text, new_text) = match (old_text.transpose(), new_text.transpose()) {
        (Ok(old), Ok(new)) => (old.unwrap_or(""), new.unwrap_or("")),
        _ => {
            return FileDiff {
                path: path.to_string(),
                kind,
                additions: 0,
                deletions: 0,
                diff: String::new(),
                note: Some("binary file — content differs, no text diff".to_string()),
            }
        }
    };

    let diff = crate::approval::unified_diff(old_text, new_text, path);
    let (additions, deletions) = count_changes(&diff);
    FileDiff {
        path: path.to_string(),
        kind,
        additions,
        deletions,
        diff,
        note: None,
    }
}

/// Count `+`/`-` body lines, skipping the `+++`/`---` headers.
fn count_changes(diff: &str) -> (usize, usize) {
    let mut additions = 0;
    let mut deletions = 0;
    for line in diff.lines() {
        if line.starts_with("+++") || line.starts_with("---") {
            continue;
        }
        if line.starts_with('+') {
            additions += 1;
        } else if line.starts_with('-') {
            deletions += 1;
        }
    }
    (additions, deletions)
}

/// A file's identity without its contents: enough to notice a change, cheap enough to collect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    size: u64,
    modified_ns: u128,
}

/// One walk of the workspace, keyed by workspace-relative path.
#[derive(Debug, Default)]
struct TreeScan {
    files: std::collections::BTreeMap<String, FileStamp>,
    truncated: bool,
}

impl TreeScan {
    fn of(root: &Path, ignore: &[String]) -> Self {
        let mut scan = Self::default();
        let mut budget = MAX_SCANNED_FILES;
        let mut stack = vec![root.to_path_buf()];

        while let Some(dir) = stack.pop() {
            let entries = match std::fs::read_dir(&dir) {
                Ok(entries) => entries,
                // A directory the process may not read is not a reason to fail a turn; the note
                // says the walk was partial, and "partial" is honest.
                Err(_) => {
                    scan.truncated = true;
                    continue;
                }
            };
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if ignore.iter().any(|ignored| ignored == &name) {
                    continue;
                }
                let file_type = match entry.file_type() {
                    Ok(file_type) => file_type,
                    Err(_) => continue,
                };
                let path = entry.path();
                if file_type.is_dir() {
                    stack.push(path);
                    continue;
                }
                if !file_type.is_file() {
                    // Symlinks and devices are not files this can meaningfully stamp.
                    continue;
                }
                if budget == 0 {
                    scan.truncated = true;
                    return scan;
                }
                budget -= 1;
                let Ok(metadata) = entry.metadata() else {
                    continue;
                };
                let modified_ns = metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|elapsed| elapsed.as_nanos())
                    .unwrap_or(0);
                let relative = path
                    .strip_prefix(root)
                    .map(|rest| rest.to_string_lossy().to_string())
                    .unwrap_or_else(|_| path.to_string_lossy().to_string());
                scan.files.insert(
                    relative,
                    FileStamp {
                        size: metadata.len(),
                        modified_ns,
                    },
                );
            }
        }
        scan
    }

    /// What changed between `self` (the baseline) and `after`, excluding paths the file tools
    /// already recorded — those have a blob and a kind of their own, and listing them twice would
    /// make a rewind look like it covered more than it does.
    fn changes_against(&self, after: &TreeScan, recorded: &[String]) -> Vec<UnrecordedFile> {
        let mut changes = Vec::new();
        for (path, stamp) in &after.files {
            if recorded.iter().any(|known| known == path) {
                continue;
            }
            match self.files.get(path) {
                None => changes.push(UnrecordedFile {
                    path: path.clone(),
                    kind: ChangeKind::Created,
                }),
                Some(before) if before != stamp => changes.push(UnrecordedFile {
                    path: path.clone(),
                    kind: ChangeKind::Modified,
                }),
                Some(_) => {}
            }
        }
        for path in self.files.keys() {
            if recorded.iter().any(|known| known == path) {
                continue;
            }
            if !after.files.contains_key(path) {
                changes.push(UnrecordedFile {
                    path: path.clone(),
                    kind: ChangeKind::Deleted,
                });
            }
        }
        changes.sort_by(|left, right| left.path.cmp(&right.path));
        changes
    }
}

fn turn_dir_name(seq: u32) -> String {
    format!("turn_{seq:04}")
}

/// Session ids are user-supplied strings; they become a single directory component.
fn sanitise(session_id: &str) -> String {
    let cleaned: String = session_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.' {
                ch
            } else {
                '_'
            }
        })
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
