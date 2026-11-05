use async_trait::async_trait;
use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

use super::sandbox::Sandbox;
use crate::approval::{unified_diff, ToolPreview};
use crate::error::HarnessError;
use crate::traits::Tool;
use crate::types::ToolResult;

/// Session-scoped cache of read file contents to enforce read-before-write safety.
#[derive(Debug, Clone, Default)]
pub struct ReadCache {
    cached: Arc<RwLock<HashMap<PathBuf, String>>>,
}

impl ReadCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn record_read(&self, path: &Path, content: &str) {
        let mut map = self.cached.write().await;
        map.insert(path.to_path_buf(), content.to_string());
    }

    pub async fn verify_and_update(
        &self,
        path: &Path,
        current_content: &str,
        new_content: &str,
    ) -> Result<(), String> {
        let mut map = self.cached.write().await;
        if let Some(cached_content) = map.get(path) {
            if cached_content != current_content {
                return Err(format!(
                    "Stale write refused for '{}': file changed on disk since last read.",
                    path.display()
                ));
            }
        } else {
            return Err(format!(
                "Stale write refused for '{}': file was not read in this session prior to write or patch. Call read_file on this path first.",
                path.display()
            ));
        }
        map.insert(path.to_path_buf(), new_content.to_string());
        Ok(())
    }

    /// Verifies patch safety: if cached, ensures no intervening modification; if not yet
    /// cached, accepts the unique on-disk old_string match and registers the new content.
    pub async fn verify_and_update_patch(
        &self,
        path: &Path,
        current_content: &str,
        new_content: &str,
    ) -> Result<(), String> {
        let mut map = self.cached.write().await;
        if let Some(cached_content) = map.get(path) {
            if cached_content != current_content {
                return Err(format!(
                    "Stale write refused for '{}': file changed on disk since last read.",
                    path.display()
                ));
            }
        }
        map.insert(path.to_path_buf(), new_content.to_string());
        Ok(())
    }

    pub async fn record_new_file(&self, path: &Path, content: &str) {
        let mut map = self.cached.write().await;
        map.insert(path.to_path_buf(), content.to_string());
    }
}

/// Resolves a user-supplied path relative to `workspace_root` and refuses anything outside it.
///
/// Kept as a free function because the GUI and the shell tool resolve paths outside any tool
/// struct; the policy itself lives in [`Sandbox`].
pub fn resolve_path(workspace_root: &Path, user_path: &str) -> Result<PathBuf, String> {
    Sandbox::workspace(workspace_root.to_path_buf()).resolve(user_path)
}

/// The first `max_chars` bytes of `s`, never splitting a UTF-8 character.
///
/// `String::truncate` panics when `max_chars` lands in the middle of a multi-byte character,
/// so it must never be used to bound model-facing output: one CJK or emoji file would turn a
/// routine read into a panic. This walks back to the nearest char boundary instead.
pub fn char_safe_cut(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        return s;
    }
    let mut end = max_chars;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The head and tail of `s` within `max_chars` bytes, with `marker` standing in for the middle.
///
/// A plain head cut keeps the beginning of a tool result and throws away the end, which is often
/// the part that matters: an error's last line, a test run's summary, a diff's closing hunk. This
/// keeps both ends and drops the middle, which is where the repetitive bulk usually sits. Returns
/// the input unchanged when it already fits, and never splits a UTF-8 character at either cut.
pub fn clip_middle(s: &str, max_chars: usize, marker: &str) -> String {
    if s.len() <= max_chars {
        return s.to_string();
    }
    let budget = max_chars.saturating_sub(marker.len());
    let head_budget = budget / 2;
    let head = char_safe_cut(s, head_budget);
    let mut tail_start = s.len().saturating_sub(budget - head.len());
    while tail_start < s.len() && !s.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!("{head}{marker}{}", &s[tail_start..])
}

/// Opt-in extra reach for every filesystem tool: `paths.extra_roots`.
///
/// The default is the workspace and nothing else. An operator who wants `/tmp` (or a scratch
/// volume) to be reachable says so in config; a model cannot widen this by asking nicely.
macro_rules! sandboxed_tool {
    ($tool:ty) => {
        impl $tool {
            pub fn with_extra_roots(mut self, extra_roots: Vec<PathBuf>) -> Self {
                self.extra_roots = extra_roots;
                self
            }

            fn sandbox(&self) -> Sandbox {
                Sandbox::with_extra_roots(self.workspace_root.clone(), self.extra_roots.clone())
            }
        }
    };
}

pub struct ReadFileTool {
    workspace_root: PathBuf,
    /// Additional roots the operator allowed (`paths.extra_roots`).
    extra_roots: Vec<PathBuf>,
    max_chars: usize,
    read_cache: ReadCache,
}

impl ReadFileTool {
    pub fn new(workspace_root: PathBuf, max_chars: usize, read_cache: ReadCache) -> Self {
        Self {
            workspace_root,
            extra_roots: Vec::new(),
            max_chars,
            read_cache,
        }
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read a text file with line numbers and optional pagination. Required before patching or overwriting existing files."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to the file to read" },
                "offset": { "type": "integer", "description": "1-indexed starting line number", "default": 1 },
                "limit": { "type": "integer", "description": "Maximum number of lines to read" }
            },
            "required": ["path"]
        })
    }

    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let path_str = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => p,
            None => {
                return Ok(ToolResult::error(
                    call_id,
                    "Missing required argument 'path'",
                ))
            }
        };

        let mut resolved = match self.sandbox().resolve(path_str) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(call_id, e)),
        };

        if !resolved.exists() {
            let crates_alt = self
                .workspace_root
                .join("crates")
                .join(path_str.trim_start_matches("./"));
            if crates_alt.exists() && crates_alt.is_file() {
                resolved = crates_alt;
            } else {
                return Ok(ToolResult::error(
                    call_id,
                    format!(
                        "File not found: '{}' (relative to workspace root '{}')",
                        path_str,
                        self.workspace_root.display()
                    ),
                ));
            }
        }

        let content = match tokio::fs::read_to_string(&resolved).await {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolResult::error(
                    call_id,
                    format!("Failed to read file: {}", e),
                ))
            }
        };

        // Cache content for read-before-write verification
        self.read_cache.record_read(&resolved, &content).await;

        let offset = args.get("offset").and_then(|o| o.as_u64()).unwrap_or(1) as usize;
        let limit = args
            .get("limit")
            .and_then(|l| l.as_u64())
            .map(|l| l as usize);

        let lines: Vec<&str> = content.lines().collect();
        let start = if offset > 0 { offset - 1 } else { 0 };

        let selected_lines = if start >= lines.len() {
            Vec::new()
        } else {
            let slice = &lines[start..];
            match limit {
                Some(lim) => slice.iter().take(lim).copied().collect(),
                None => slice.to_vec(),
            }
        };

        let total = selected_lines.len();
        let mut output = String::new();
        let mut shown = 0usize;
        // Whether the tail of the last emitted *line* was cut (as opposed to whole lines being
        // omitted). Both are reported, because a cut line cannot be recovered by an offset.
        let mut cut_line_tail = false;

        for (idx, line) in selected_lines.iter().enumerate() {
            let rendered = format!("{}|{}\n", start + idx + 1, line);
            if shown == 0 && rendered.len() > self.max_chars {
                // One line longer than the whole budget: cut it rather than return nothing,
                // which is the worse failure. `char_safe_cut` keeps the cut on a UTF-8
                // boundary — `String::truncate` panics on a mid-character boundary, so a
                // CJK/emoji file used to crash the tool instead of truncating.
                output.push_str(char_safe_cut(&rendered, self.max_chars));
                output.push_str("…\n");
                shown = 1;
                cut_line_tail = true;
                break;
            }
            // Emit whole lines only: a cut mid-line would leave the caller a partial line
            // with no way to tell where it ends, and the budget is respected by stopping
            // before the line that would overflow it.
            if shown > 0 && output.len() + rendered.len() > self.max_chars {
                break;
            }
            output.push_str(&rendered);
            shown += 1;
        }

        if shown < total || cut_line_tail {
            output.push_str(&format!(
                "[truncated at {} chars: showing {shown} of {total} lines; resume with offset={}]",
                self.max_chars,
                start + shown + 1
            ));
        }

        Ok(ToolResult::success(call_id, output))
    }
}

pub struct WriteFileTool {
    workspace_root: PathBuf,
    /// Additional roots the operator allowed (`paths.extra_roots`).
    extra_roots: Vec<PathBuf>,
    read_cache: ReadCache,
}

impl WriteFileTool {
    pub fn new(workspace_root: PathBuf, read_cache: ReadCache) -> Self {
        Self {
            workspace_root,
            extra_roots: Vec::new(),
            read_cache,
        }
    }
}

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file, completely replacing existing content. Overwriting an existing file requires reading it first with read_file in this session."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Target file path" },
                "content": { "type": "string", "description": "Complete content to write" }
            },
            "required": ["path", "content"]
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn preview(&self, args: &serde_json::Value) -> Option<ToolPreview> {
        let path = args.get("path").and_then(|p| p.as_str())?;
        Some(ToolPreview {
            target: Some(path.to_string()),
            diff: None,
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let path_str = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => p,
            None => {
                return Ok(ToolResult::error(
                    call_id,
                    "Missing required argument 'path'",
                ))
            }
        };
        let content = match args.get("content").and_then(|c| c.as_str()) {
            Some(c) => c,
            None => {
                return Ok(ToolResult::error(
                    call_id,
                    "Missing required argument 'content'",
                ))
            }
        };

        let resolved = match self.sandbox().resolve(path_str) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(call_id, e)),
        };

        // Check read-before-write if file exists
        if resolved.exists() {
            let current = match tokio::fs::read_to_string(&resolved).await {
                Ok(c) => c,
                Err(e) => {
                    return Ok(ToolResult::error(
                        call_id,
                        format!("Failed to read existing file: {}", e),
                    ))
                }
            };
            if let Err(refusal) = self
                .read_cache
                .verify_and_update(&resolved, &current, content)
                .await
            {
                return Ok(ToolResult::error(call_id, refusal));
            }
        } else {
            self.read_cache.record_new_file(&resolved, content).await;
        }

        if let Some(parent) = resolved.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return Ok(ToolResult::error(
                    call_id,
                    format!("Failed to create directories for '{}': {}", path_str, e),
                ));
            }
        }

        if let Err(e) = tokio::fs::write(&resolved, content).await {
            return Ok(ToolResult::error(
                call_id,
                format!("Failed to write file '{}': {}", path_str, e),
            ));
        }

        Ok(ToolResult::success(
            call_id,
            format!("Successfully wrote {} bytes to {}", content.len(), path_str),
        ))
    }
}

pub struct PatchTool {
    workspace_root: PathBuf,
    /// Additional roots the operator allowed (`paths.extra_roots`).
    extra_roots: Vec<PathBuf>,
    read_cache: ReadCache,
}

impl PatchTool {
    pub fn new(workspace_root: PathBuf, read_cache: ReadCache) -> Self {
        Self {
            workspace_root,
            extra_roots: Vec::new(),
            read_cache,
        }
    }
}

#[async_trait]
impl Tool for PatchTool {
    fn name(&self) -> &str {
        "patch"
    }

    fn description(&self) -> &str {
        "Targeted find-and-replace edit in a file. Requires reading the file in the session first with read_file, and an exact unique string match."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to file to patch" },
                "old_string": { "type": "string", "description": "Exact text to find and replace" },
                "new_string": { "type": "string", "description": "Changed replacement text" },
                "replace_all": { "type": "boolean", "description": "Replace all occurrences (default: false)" }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    fn is_mutating(&self) -> bool {
        true
    }

    async fn preview(&self, args: &serde_json::Value) -> Option<ToolPreview> {
        let path = args.get("path").and_then(|p| p.as_str())?;
        let old_str = args.get("old_string").and_then(|s| s.as_str())?;
        let new_str = args.get("new_string").and_then(|s| s.as_str())?;
        let replace_all = args
            .get("replace_all")
            .and_then(|r| r.as_bool())
            .unwrap_or(false);

        let mut resolved = self.sandbox().resolve(path).ok()?;
        if !resolved.exists() {
            let crates_alt = self
                .workspace_root
                .join("crates")
                .join(path.trim_start_matches("./"));
            if crates_alt.exists() && crates_alt.is_file() {
                resolved = crates_alt;
            }
        }
        let current = std::fs::read_to_string(&resolved).ok()?;

        let patched = if replace_all {
            current.replace(old_str, new_str)
        } else {
            current.replacen(old_str, new_str, 1)
        };

        let diff = unified_diff(&current, &patched, path);
        Some(ToolPreview {
            target: Some(path.to_string()),
            diff: Some(diff),
        })
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let path_str = match args.get("path").and_then(|p| p.as_str()) {
            Some(p) => p,
            None => {
                return Ok(ToolResult::error(
                    call_id,
                    "Missing required argument 'path'",
                ))
            }
        };
        let old_str = match args.get("old_string").and_then(|s| s.as_str()) {
            Some(s) => s,
            None => {
                return Ok(ToolResult::error(
                    call_id,
                    "Missing required argument 'old_string'",
                ))
            }
        };
        let new_str = match args.get("new_string").and_then(|s| s.as_str()) {
            Some(s) => s,
            None => {
                return Ok(ToolResult::error(
                    call_id,
                    "Missing required argument 'new_string'",
                ))
            }
        };
        let replace_all = args
            .get("replace_all")
            .and_then(|r| r.as_bool())
            .unwrap_or(false);

        let mut resolved = match self.sandbox().resolve(path_str) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(call_id, e)),
        };

        if !resolved.exists() {
            let crates_alt = self
                .workspace_root
                .join("crates")
                .join(path_str.trim_start_matches("./"));
            if crates_alt.exists() && crates_alt.is_file() {
                resolved = crates_alt;
            } else {
                return Ok(ToolResult::error(
                    call_id,
                    format!(
                        "File not found: '{}' (relative to workspace root '{}')",
                        path_str,
                        self.workspace_root.display()
                    ),
                ));
            }
        }

        let current = match tokio::fs::read_to_string(&resolved).await {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolResult::error(
                    call_id,
                    format!("Failed to read file: {}", e),
                ))
            }
        };

        let count = current.matches(old_str).count();
        if count == 0 {
            return Ok(ToolResult::error(
                call_id,
                format!("old_string not found in '{}'", path_str),
            ));
        }
        if !replace_all && count > 1 {
            return Ok(ToolResult::error(
                call_id,
                format!(
                    "old_string matched {} times in '{}'; include more context or set replace_all: true",
                    count, path_str
                ),
            ));
        }

        let patched = if replace_all {
            current.replace(old_str, new_str)
        } else {
            current.replacen(old_str, new_str, 1)
        };

        if let Err(refusal) = self
            .read_cache
            .verify_and_update_patch(&resolved, &current, &patched)
            .await
        {
            return Ok(ToolResult::error(call_id, refusal));
        }

        if let Err(e) = tokio::fs::write(&resolved, &patched).await {
            return Ok(ToolResult::error(
                call_id,
                format!("Failed to write patched file '{}': {}", path_str, e),
            ));
        }

        Ok(ToolResult::success(
            call_id,
            format!("Successfully patched '{}'", path_str),
        ))
    }
}

pub struct SearchFilesTool {
    workspace_root: PathBuf,
    /// Additional roots the operator allowed (`paths.extra_roots`).
    extra_roots: Vec<PathBuf>,
}

impl SearchFilesTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            extra_roots: Vec::new(),
        }
    }
}

/// Files above this size are skipped rather than scanned. A search reads whole files into
/// memory to match them, so a multi-gigabyte binary (archive, blend, disk image) inside the
/// tree would stall the scan for minutes and blow up memory.
const MAX_SCANNED_FILE_BYTES: u64 = 2 * 1024 * 1024;

/// Hard ceiling on files visited by one search. The workspace root is often the user's home
/// directory, and an unbounded walk there never terminates in usable time — the agent loop
/// sits waiting and the CLI looks hung.
const MAX_SCANNED_FILES: usize = 20_000;

/// How many files are visited between cancellation checks, so a cancelled search stops
/// promptly without paying for an atomic load on every file.
const CANCEL_CHECK_INTERVAL: usize = 256;

/// Matching lines reported in 'content' mode before searching stops early.
const MAX_MATCHING_LINES: usize = 200;
/// Files reported in 'files_only' mode, and per-file rows in 'count' mode, before searching
/// stops early. Both modes report far fewer items than 'content' emits lines, so they get a
/// larger ceiling rather than sharing the line budget.
const MAX_MATCHING_FILES: usize = 500;

/// Maximum characters of a single matching line emitted in 'content' mode before it is truncated.
const MAX_MATCHING_LINE_CHARS: usize = 500;

/// Total characters of matching content across all lines before a search stops early.
const MAX_MATCHING_OUTPUT_CHARS: usize = 100_000;

/// Noise directories that search_files and list_files skip by default.
pub(crate) fn is_noise_dir(name: &str) -> bool {
    matches!(
        name,
        ".git" | "target" | "node_modules" | "checkpoints" | ".checkpoints" | ".cache"
    )
}

/// Truncate a single line to `MAX_MATCHING_LINE_CHARS`, keeping a centered window around `pattern`.
fn truncate_matching_line(line: &str, pattern: &str) -> String {
    if line.len() <= MAX_MATCHING_LINE_CHARS {
        return line.to_string();
    }
    let match_byte = line.find(pattern).unwrap_or(0);
    let half_budget = MAX_MATCHING_LINE_CHARS / 2;
    let raw_start = match_byte.saturating_sub(half_budget);
    let mut start = raw_start;
    while start > 0 && !line.is_char_boundary(start) {
        start = start.saturating_sub(1);
    }
    let raw_end = (start + MAX_MATCHING_LINE_CHARS).min(line.len());
    let mut end = raw_end;
    while end < line.len() && !line.is_char_boundary(end) {
        end += 1;
    }
    let prefix = if start > 0 { "..." } else { "" };
    let suffix = if end < line.len() {
        "... [line truncated]"
    } else {
        ""
    };
    format!("{}{}{}", prefix, &line[start..end], suffix)
}

/// Which ceiling a scan hit. `None` means the walk finished exhaustively.
#[allow(clippy::enum_variant_names)]
enum ScanLimit {
    FileCeiling,
    LineCeiling,
    FileListCeiling,
    OutputCeiling,
}

/// Result of one blocking scan: matching lines, matching files, per-file counts, and which
/// limit stopped the walk early (if any).
struct ScanOutcome {
    matching_lines: Vec<String>,
    matching_files: Vec<String>,
    file_counts: Vec<(String, usize)>,
    total_output_chars: usize,
    /// The ceiling that stopped the walk early, or `None` when it was exhaustive.
    limit: Option<ScanLimit>,
}

impl ScanOutcome {
    /// The one-line notice appended to output when the walk stopped early. Each mode names its
    /// own ceiling and says how to narrow it, so a capped result is never mistaken for a
    /// complete one.
    fn truncation_notice(&self) -> Option<String> {
        let (ceiling, hint) = match self.limit.as_ref()? {
            ScanLimit::FileCeiling => (
                format!("{MAX_SCANNED_FILES} files"),
                "narrow 'path' or add 'file_glob'".to_string(),
            ),
            ScanLimit::LineCeiling => (
                format!("{MAX_MATCHING_LINES} matching lines"),
                "use output_mode 'files_only' or 'count', or narrow 'path'/'file_glob'".to_string(),
            ),
            ScanLimit::FileListCeiling => (
                format!("{MAX_MATCHING_FILES} files"),
                "narrow 'path' or 'file_glob'".to_string(),
            ),
            ScanLimit::OutputCeiling => (
                format!("{MAX_MATCHING_OUTPUT_CHARS} characters of matches"),
                "use output_mode 'files_only' or 'count', or narrow 'path'/'file_glob'".to_string(),
            ),
        };
        Some(format!("[search stopped after {ceiling}; {hint}]"))
    }
}

#[async_trait]
impl Tool for SearchFilesTool {
    fn name(&self) -> &str {
        "search_files"
    }

    fn description(&self) -> &str {
        "Search file contents inside the workspace for a literal text pattern and report the \
         matching lines ('content'), the matching file paths ('files_only'), or per-file match \
         counts ('count'). The pattern is a literal substring — this is not a regex engine, so \
         shell metacharacters such as '|', '(' and '$' match themselves. Files above 2 MB are \
         skipped, the walk stops after 20,000 files, and results are capped (see file_glob); \
         every limit that truncates a result says so in the output."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Literal text to search for (not a regex)" },
                "path": { "type": "string", "description": "Subdirectory to search in (default: workspace root)" },
                "file_glob": {
                    "type": "string",
                    "description": "Filter to matching files. Real glob semantics: '*' and '?'. \
                     A bare pattern ('*.rs', '*_test.rs') matches the file name; a pattern with a \
                     slash ('src/*.rs') matches the workspace-relative path."
                },
                "output_mode": {
                    "type": "string",
                    "enum": ["content", "files_only", "count"],
                    "description": "Output format mode (default: 'content')"
                }
            },
            "required": ["pattern"]
        })
    }

    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        let pattern = match args.get("pattern").and_then(|p| p.as_str()) {
            Some(p) => p,
            None => {
                return Ok(ToolResult::error(
                    call_id,
                    "Missing required argument 'pattern'",
                ))
            }
        };
        // A blank path means the workspace root, same as `list_files`: the parameter documents a
        // default, and a model sending `""` is asking for that default.
        let search_path_str = match args.get("path").and_then(|p| p.as_str()) {
            Some(path) if !path.trim().is_empty() => path,
            _ => ".",
        };
        let file_glob = args.get("file_glob").and_then(|g| g.as_str());
        let output_mode = args
            .get("output_mode")
            .and_then(|m| m.as_str())
            .unwrap_or("content");

        let mut resolved = match self.sandbox().resolve(search_path_str) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(call_id, e)),
        };

        if !resolved.exists() {
            let crates_alt = self
                .workspace_root
                .join("crates")
                .join(search_path_str.trim_start_matches("./"));
            if crates_alt.exists() {
                resolved = crates_alt;
            } else {
                return Ok(ToolResult::error(
                    call_id,
                    format!(
                        "Path not found: '{}' (relative to workspace root '{}')",
                        search_path_str,
                        self.workspace_root.display()
                    ),
                ));
            }
        }

        // The walk and the file reads are blocking syscalls. Running them on the async
        // executor parks a worker thread for the whole scan, which is what froze the
        // interactive loop on large trees — so the scan owns a blocking thread instead.
        let root = self.workspace_root.clone();
        let pattern = pattern.to_string();
        let file_glob = file_glob.map(|g| g.to_string());
        let scan_mode = output_mode.to_string();
        let cancel = cancel_token.clone();

        let scan = tokio::task::spawn_blocking(move || {
            scan_files(
                &root,
                &resolved,
                &pattern,
                file_glob.as_deref(),
                &scan_mode,
                &cancel,
            )
        })
        .await
        .map_err(|e| HarnessError::Io(format!("search task failed: {}", e)))??;

        let mut output = match output_mode {
            "files_only" => scan.matching_files.join("\n"),
            "count" => {
                let mut out = String::new();
                for (file, count) in &scan.file_counts {
                    out.push_str(&format!("{}: {} matches\n", file, count));
                }
                out
            }
            _ => scan.matching_lines.join("\n"),
        };

        if let Some(notice) = scan.truncation_notice() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str(&notice);
        }

        Ok(ToolResult::success(call_id, output))
    }
}

/// Whether a file passes the caller's `file_glob`.
///
/// The glob is matched against the file name for bare patterns (`*.rs`, `*_test.rs`) and against
/// the workspace-relative path for patterns that contain a separator (`src/*.rs`), which is what
/// a user writing a path-shaped pattern means. Matching is real glob semantics: `*` spans any
/// run of characters (path separators included), `?` matches exactly one. A pattern that is a
/// literal with no wildcard must equal the file name or path exactly — `rs` no longer matches
/// `cars`, and `cars` no longer matches `*.rs`.
fn glob_matches(glob: &str, fname: &str, path: &Path, workspace_root: &Path) -> bool {
    if glob.contains('/') {
        let rel = path.strip_prefix(workspace_root).unwrap_or(path);
        return glob_match(glob, &rel.to_string_lossy().replace('\\', "/"));
    }
    glob_match(glob, fname)
}

/// Iterative wildcard matcher: `*` (any run, including separators) and `?` (exactly one). No
/// recursion, so a pattern of many stars cannot blow the stack. ASCII input — tool names, rule
/// arguments, paths — matches over bytes with no allocation; only non-ASCII falls back to a
/// char vector.
fn glob_match(pattern: &str, text: &str) -> bool {
    if pattern.is_ascii() && text.is_ascii() {
        glob_match_slice(pattern.as_bytes(), text.as_bytes(), b'*', b'?')
    } else {
        glob_match_slice(
            &pattern.chars().collect::<Vec<_>>(),
            &text.chars().collect::<Vec<_>>(),
            '*',
            '?',
        )
    }
}

/// Two-pointer matcher with a single backtrack anchor for the last `*`, generic over the item
/// type so the ASCII path works on bytes without allocating.
fn glob_match_slice<T: PartialEq + Copy>(pattern: &[T], text: &[T], star: T, any: T) -> bool {
    let (mut pi, mut ti) = (0usize, 0usize);
    // Backtrack anchors: where a failed `*` resumes in the pattern, and in the text.
    let (mut star_p, mut star_t): (Option<usize>, usize) = (None, 0);

    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == any || pattern[pi] == text[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == star {
            star_p = Some(pi);
            star_t = ti;
            pi += 1;
        } else if let Some(sp) = star_p {
            // Mismatch after a star: the star absorbs one more text char and we retry.
            pi = sp + 1;
            star_t += 1;
            ti = star_t;
        } else {
            return false;
        }
    }
    // Remaining pattern must be all stars.
    pattern[pi..].iter().all(|c| *c == star)
}

/// Walk `resolved` and match `pattern` against every text file, bailing out on cancellation,
/// on oversized files, and on the visited-file ceiling.
fn scan_files(
    workspace_root: &Path,
    resolved: &Path,
    pattern: &str,
    file_glob: Option<&str>,
    output_mode: &str,
    cancel: &CancellationToken,
) -> Result<ScanOutcome, HarnessError> {
    let mut outcome = ScanOutcome {
        matching_lines: Vec::new(),
        matching_files: Vec::new(),
        file_counts: Vec::new(),
        total_output_chars: 0,
        limit: None,
    };
    // Per-file counts are only rendered in 'count' mode; 'content' and 'files_only' never
    // read them, so the map stays empty there.
    let needs_counts = output_mode == "count";
    // The ceiling that applies to the results this mode actually renders. 'content' is bounded
    // by lines; the two list modes by files. Each mode checks its own ceiling and records which
    // one stopped the walk, so the notice names the limit the caller hit.
    let file_list_mode = output_mode != "content";

    for (visited, entry) in walkdir::WalkDir::new(resolved)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !is_noise_dir(&e.file_name().to_string_lossy()))
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .enumerate()
    {
        if visited % CANCEL_CHECK_INTERVAL == 0 && cancel.is_cancelled() {
            return Err(HarnessError::Cancelled);
        }
        if visited >= MAX_SCANNED_FILES {
            outcome.limit = Some(ScanLimit::FileCeiling);
            break;
        }
        let reached_cap = if file_list_mode {
            outcome.matching_files.len() >= MAX_MATCHING_FILES
        } else {
            outcome.matching_lines.len() >= MAX_MATCHING_LINES
        };
        if reached_cap {
            outcome.limit = Some(if file_list_mode {
                ScanLimit::FileListCeiling
            } else {
                ScanLimit::LineCeiling
            });
            break;
        }

        let path = entry.path();

        if let Some(glob) = file_glob {
            let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !glob_matches(glob, fname, path, workspace_root) {
                continue;
            }
        }

        // A file whose metadata cannot be read is treated as off-limits, not as zero bytes:
        // failing closed keeps an unreadable huge file from being slurped into memory.
        let size = match entry.metadata() {
            Ok(meta) => meta.len(),
            Err(_) => continue,
        };
        if size > MAX_SCANNED_FILE_BYTES {
            continue;
        }

        let rel_path = path
            .strip_prefix(workspace_root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        // Non-UTF-8 files are binary for our purposes; skipping them is the intent.
        if let Ok(content) = std::fs::read_to_string(path) {
            let mut matches_in_file = 0;
            for (idx, line) in content.lines().enumerate() {
                if line.contains(pattern) {
                    matches_in_file += 1;
                    if !file_list_mode {
                        if outcome.matching_lines.len() >= MAX_MATCHING_LINES {
                            outcome.limit = Some(ScanLimit::LineCeiling);
                            break;
                        }
                        let formatted = format!(
                            "{}:{}: {}",
                            rel_path,
                            idx + 1,
                            truncate_matching_line(line, pattern)
                        );
                        outcome.total_output_chars += formatted.len() + 1;
                        outcome.matching_lines.push(formatted);
                        if outcome.total_output_chars >= MAX_MATCHING_OUTPUT_CHARS {
                            outcome.limit = Some(ScanLimit::OutputCeiling);
                            break;
                        }
                    }
                }
            }

            if matches_in_file > 0 {
                outcome.matching_files.push(rel_path.clone());
                if needs_counts {
                    outcome.file_counts.push((rel_path, matches_in_file));
                }
            }

            if outcome.limit.is_some() {
                break;
            }
        }
    }

    Ok(outcome)
}

/// How many entries one `list_files` call reports before it stops and says so. A listing is
/// read by a model rather than a pager, so an unbounded one floods the transcript.
const MAX_LISTED_ENTRIES: usize = 1_000;

/// Character budget for a listing, independent of the entry count.
///
/// A thousand entries is not a bound on bytes: nested paths, wide trees and generated files turned
/// one `list_files` call into 9 MB of transcript in a real session. The entry cap still decides
/// which entries are shown; this cap decides how much text one answer may carry.
const MAX_LISTED_OUTPUT_CHARS: usize = 40_000;

/// Default depth of a `list_files` walk. The root is depth 0, so this lists eight levels below it.
///
/// The normal source tree fits comfortably; generated output and vendored trees are what grow past
/// it, and the byte cap only ever ran after the walk had already produced the listing. Bounding
/// the walk keeps the tool from doing work whose answer would be clipped away.
const DEFAULT_LIST_DEPTH: usize = 8;

/// The deepest walk a caller may request explicitly.
const MAX_LIST_DEPTH: usize = 64;

/// How many entries the walk collects before it stops, independent of `MAX_LISTED_ENTRIES`.
/// The display cap is applied *after* sorting, so the walk has to see enough of the tree to
/// guarantee the alphabetically-first entries are found, while a pathological tree still
/// cannot grow the buffer without bound.
const MAX_COLLECTED_ENTRIES: usize = 50_000;

pub struct ListFilesTool {
    workspace_root: PathBuf,
    /// Additional roots the operator allowed (`paths.extra_roots`).
    extra_roots: Vec<PathBuf>,
}

impl ListFilesTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            extra_roots: Vec::new(),
        }
    }
}

#[async_trait]
impl Tool for ListFilesTool {
    fn name(&self) -> &str {
        "list_files"
    }

    fn description(&self) -> &str {
        "List directory contents inside the workspace as workspace-relative paths, one per line, \
         with directories carrying a trailing slash. Recursive to 'depth' levels (default 8); use \
         'path' to bound it, or 'glob' to find files by name. 'glob' matches exactly as in \
         search_files: a bare pattern ('*.rs', '*_test.rs') matches the entry name, a pattern with \
         a slash ('src/*.rs') matches the workspace-relative path. .git, target and node_modules \
         are skipped, and results are capped (every cap announces itself in the output)."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory to list (default: workspace root)" },
                "glob": { "type": "string", "description": "Filter entries by glob: bare patterns match the entry name, slash patterns match the workspace-relative path" },
                "depth": { "type": "integer", "description": "Maximum directory depth to walk below 'path' (default: 8, maximum: 64); 1 lists only direct children" }
            },
            "required": []
        })
    }

    fn is_mutating(&self) -> bool {
        false
    }

    async fn execute(
        &self,
        call_id: &str,
        args: &serde_json::Value,
        _cancel_token: &CancellationToken,
    ) -> Result<ToolResult, HarnessError> {
        // A blank path means the workspace root: the parameter documents a default, and a model
        // that sends `""` is asking for that default, not making a mistake worth a failed call.
        let path_str = match args.get("path").and_then(|p| p.as_str()) {
            Some(path) if !path.trim().is_empty() => path,
            _ => ".",
        };
        let glob = args.get("glob").and_then(|g| g.as_str());
        let depth = args
            .get("depth")
            .and_then(|d| d.as_u64())
            .map(|depth| (depth as usize).clamp(1, MAX_LIST_DEPTH))
            .unwrap_or(DEFAULT_LIST_DEPTH);

        let resolved = match self.sandbox().resolve(path_str) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(call_id, e)),
        };
        if !resolved.exists() {
            return Ok(ToolResult::error(
                call_id,
                format!("Directory not found: {}", path_str),
            ));
        }
        if !resolved.is_dir() {
            return Ok(ToolResult::error(
                call_id,
                format!("Not a directory: {}", path_str),
            ));
        }

        let root = self.workspace_root.clone();
        let glob = glob.map(|g| g.to_string());
        // The walk is a blocking syscall; owning a blocking thread keeps a large tree from
        // parking an async worker, the same reason `search_files` runs its scan this way.
        let request_glob = glob.clone();
        let listed = tokio::task::spawn_blocking(move || {
            list_entries(&root, &resolved, glob.as_deref(), depth)
        })
        .await
        .map_err(|e| HarnessError::Io(format!("list task failed: {}", e)))?;
        let entries = listed.entries;

        // An empty result must say *why* it is empty: a silent success reads as "nothing to
        // see here" when the truth is a glob that matched nothing or an empty directory.
        let mut output = if entries.is_empty() {
            match request_glob {
                Some(g) => format!("[no entries match glob '{}']", g),
                None => format!("[nothing under '{}']", path_str),
            }
        } else {
            let mut listing = entries.join("\n");
            listing.push('\n');
            listing
        };
        if listed.depth_capped {
            output.push_str(&format!(
                "[listing stopped at depth {depth}; use 'path' to go deeper or raise 'depth']\n"
            ));
        }
        if listed.entries_capped {
            output.push_str(&format!(
                "[listing stopped after {MAX_LISTED_ENTRIES} entries; narrow 'path' or add 'glob']"
            ));
        }
        if output.len() > MAX_LISTED_OUTPUT_CHARS {
            output = clip_middle(
                &output,
                MAX_LISTED_OUTPUT_CHARS,
                "\n[listing clipped: middle omitted; narrow 'path' or add 'glob']\n",
            );
        }

        Ok(ToolResult::success(call_id, output))
    }
}

/// What one walked listing found, and which bound stopped it.
struct ListedEntries {
    entries: Vec<String>,
    /// The display cap dropped entries.
    entries_capped: bool,
    /// The walk stopped at `depth` while a directory still had children below it.
    depth_capped: bool,
}

/// Recursively list entries under `resolved`, skipping noise directories and applying the
/// optional glob. `depth` is the maximum walk depth below the listed directory (root is 0).
fn list_entries(
    workspace_root: &Path,
    resolved: &Path,
    glob: Option<&str>,
    depth: usize,
) -> ListedEntries {
    let mut entries: Vec<String> = Vec::new();
    let mut entries_capped = false;
    let mut depth_capped = false;

    for entry in walkdir::WalkDir::new(resolved)
        .max_depth(depth)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !is_noise_dir(&e.file_name().to_string_lossy()))
        .filter_map(Result::ok)
    {
        // The walk yields the root itself first; echoing it back is noise, not a listing.
        if entry.depth() == 0 {
            continue;
        }

        let path = entry.path();
        let is_dir = entry.file_type().is_dir();

        // A directory at the boundary may have children the walk never visited. Say so even when
        // the glob filters the directory itself out: its contents can still match.
        if !depth_capped && entry.depth() >= depth && is_dir {
            depth_capped = true;
        }

        if let Some(g) = glob {
            let Some(fname) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !glob_matches(g, fname, path, workspace_root) {
                continue;
            }
        }

        // The collection cap counts *matches*, not visited entries, so a glob that matches
        // only a few files deep in a large tree still returns all of them.
        if entries.len() >= MAX_COLLECTED_ENTRIES {
            entries_capped = true;
            break;
        }

        let rel = path
            .strip_prefix(workspace_root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");

        entries.push(if is_dir { format!("{rel}/") } else { rel });
    }

    // Sorted so two listings of the same tree are identical, and — because the display cap is
    // applied *after* sorting — a capped subset is the alphabetically-first entries rather
    // than whatever order the filesystem happened to hand back.
    entries.sort();
    if entries.len() > MAX_LISTED_ENTRIES {
        entries.truncate(MAX_LISTED_ENTRIES);
        entries_capped = true;
    }

    ListedEntries {
        entries,
        entries_capped,
        depth_capped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_star_spans_any_run_including_separators() {
        assert!(glob_match("*.rs", "agent.rs"));
        assert!(glob_match("*.rs", "session/compaction.rs"));
        // A bare pattern matches the file name, not the path — `src/*.rs` is the path form.
        assert!(!glob_match("*.rs", "src/cars"));
    }

    #[test]
    fn glob_question_matches_exactly_one_char() {
        assert!(glob_match("a?c.txt", "abc.txt"));
        assert!(!glob_match("a?c.txt", "ac.txt"));
        assert!(!glob_match("a?c.txt", "abbc.txt"));
    }

    #[test]
    fn glob_requires_exact_match_without_wildcards() {
        // The old suffix matcher let `rs` match `cars`; a literal must now equal the text.
        assert!(glob_match("cars", "cars"));
        assert!(!glob_match("rs", "cars"));
        assert!(!glob_match("cars", "vendors"));
    }

    #[test]
    fn glob_many_stars_do_not_blow_the_stack() {
        let pattern = "*".repeat(500) + "needle";
        assert!(glob_match(&pattern, &("x".repeat(2000) + "needle")));
        assert!(!glob_match(&pattern, &("x".repeat(2000) + "nope")));
    }

    #[test]
    fn glob_empty_pattern_matches_only_empty_text() {
        assert!(glob_match("", ""));
        assert!(!glob_match("", "a"));
        // A trailing star on an empty text still matches.
        assert!(glob_match("*", ""));
    }

    /// A capped listing must be the alphabetically-first entries, not whichever ones the
    /// filesystem happened to hand back: `list_entries` sorts before it truncates.
    #[test]
    fn list_entries_caps_after_sorting_so_a_truncated_listing_is_predictable() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for i in 0..(MAX_LISTED_ENTRIES + 500) {
            std::fs::write(root.join(format!("{:05}.txt", i)), "x").unwrap();
        }

        let listed = list_entries(root, root, None, DEFAULT_LIST_DEPTH);

        assert!(listed.entries_capped, "dropping entries must be announced");
        assert_eq!(listed.entries.len(), MAX_LISTED_ENTRIES);
        assert_eq!(
            listed.entries.first().map(String::as_str),
            Some("00000.txt")
        );
        let expected_last = format!("{:05}.txt", MAX_LISTED_ENTRIES - 1);
        assert_eq!(
            listed.entries.last().map(String::as_str),
            Some(expected_last.as_str())
        );
    }

    /// A bare glob matches the entry *name*, so `*rust-harness*` finds `rust-harness.md` but
    /// not a sibling directory called `rust-agent-harness` — the pattern has to spell out the
    /// name it wants.
    #[test]
    fn list_entries_bare_glob_matches_the_entry_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("rust-agent-harness")).unwrap();
        std::fs::write(root.join("rust-harness.md"), "x").unwrap();

        let matched = list_entries(root, root, Some("*rust-harness*"), DEFAULT_LIST_DEPTH);
        assert_eq!(matched.entries, vec!["rust-harness.md".to_string()]);

        let dirs = list_entries(root, root, Some("*agent-harness*"), DEFAULT_LIST_DEPTH);
        assert_eq!(dirs.entries, vec!["rust-agent-harness/".to_string()]);

        let nothing = list_entries(root, root, Some("*nope*"), DEFAULT_LIST_DEPTH);
        assert!(
            nothing.entries.is_empty(),
            "an unmatched glob returns nothing, not a guess"
        );
    }

    /// The walk stops at the requested depth and says so, rather than producing a listing the
    /// byte cap would only cut after the work was done.
    #[test]
    fn list_entries_stops_at_the_depth_limit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src").join("deep").join("deeper")).unwrap();
        std::fs::write(root.join("src").join("main.rs"), "x").unwrap();
        std::fs::write(root.join("src").join("deep").join("nested.rs"), "x").unwrap();
        std::fs::write(
            root.join("src").join("deep").join("deeper").join("lost.rs"),
            "x",
        )
        .unwrap();

        let listed = list_entries(root, root, None, 2);

        assert!(
            listed.entries.contains(&"src/".to_string()),
            "{:?}",
            listed.entries
        );
        assert!(
            listed.entries.contains(&"src/deep/".to_string()),
            "{:?}",
            listed.entries
        );
        assert!(
            listed.entries.contains(&"src/main.rs".to_string()),
            "{:?}",
            listed.entries
        );
        assert!(
            listed
                .entries
                .iter()
                .all(|entry| !entry.ends_with("nested.rs") && !entry.ends_with("lost.rs")),
            "nothing below the boundary is walked: {:?}",
            listed.entries
        );
        assert!(listed.depth_capped, "the cut is announced");
    }

    #[test]
    fn clip_middle_keeps_both_ends_and_stays_within_budget() {
        let text = format!("HEAD{}TAIL", "m".repeat(10_000));
        let out = clip_middle(&text, 200, "[...]");
        assert!(out.starts_with("HEAD"), "the head survives");
        assert!(out.ends_with("TAIL"), "the tail survives");
        assert!(out.contains("[...]"), "the omission is announced");
        assert!(out.len() <= 200, "the result fits the budget");
        assert_eq!(clip_middle("fits", 200, "[...]"), "fits");
    }

    /// The cut lands on a character boundary at both ends, so a CJK or emoji listing cannot turn
    /// a bounded answer into a panic or a broken string.
    #[test]
    fn clip_middle_never_splits_a_multibyte_character() {
        let text = "é".repeat(500);
        let out = clip_middle(&text, 101, "[...]");
        assert!(out.len() <= 101);
        assert!(out.chars().all(|c| c == 'é' || "[...]".contains(c)));
    }

    #[test]
    fn glob_matches_uses_path_for_slash_patterns() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let file = root.join("src").join("helper_test.rs");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(&file, "x").unwrap();

        // Bare pattern: matched against the file name.
        assert!(glob_matches("*_test.rs", "helper_test.rs", &file, root));
        // Path-shaped pattern: matched against the workspace-relative path.
        assert!(glob_matches("src/*.rs", "helper_test.rs", &file, root));
        assert_matches_contains(&file, root);
        assert!(!glob_matches("tests/*.rs", "helper_test.rs", &file, root));
    }

    fn assert_matches_contains(file: &std::path::Path, root: &std::path::Path) {
        assert!(glob_matches("*helper*", "helper_test.rs", file, root));
    }

    #[test]
    fn resolve_path_expands_tilde() {
        let home_path = crate::config::home_dir();
        if home_path != std::path::Path::new(".") {
            let resolved = resolve_path(&home_path, "~/").unwrap();
            let expected = home_path.canonicalize().unwrap_or(home_path);
            // `canonicalize` returns a `\\?\` verbatim path on Windows while the resolver strips
            // it; compare the two paths as the user would see them.
            let plain = |path: &std::path::Path| {
                path.to_string_lossy()
                    .trim_start_matches(r"\\?\")
                    .to_string()
            };
            assert_eq!(plain(&resolved), plain(&expected));
        }
    }
}

sandboxed_tool!(ReadFileTool);
sandboxed_tool!(WriteFileTool);
sandboxed_tool!(PatchTool);
sandboxed_tool!(SearchFilesTool);
sandboxed_tool!(ListFilesTool);
