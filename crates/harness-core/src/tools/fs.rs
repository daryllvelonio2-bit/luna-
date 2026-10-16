use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use async_trait::async_trait;
use serde_json::json;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

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
                "Stale write refused for '{}': file was not read in this session prior to write.",
                path.display()
            ));
        }
        map.insert(path.to_path_buf(), new_content.to_string());
        Ok(())
    }

    pub async fn record_new_file(&self, path: &Path, content: &str) {
        let mut map = self.cached.write().await;
        map.insert(path.to_path_buf(), content.to_string());
    }
}

/// Resolves a user-supplied path relative to workspace_root and asserts it does not escape.
pub fn resolve_path(workspace_root: &Path, user_path: &str) -> Result<PathBuf, String> {
    let p = Path::new(user_path);
    let resolved = if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace_root.join(p)
    };

    let canonical_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());

    if let Ok(canon) = resolved.canonicalize() {
        if !canon.starts_with(&canonical_root) {
            return Err(format!(
                "Path escapes workspace root '{}': {}",
                canonical_root.display(),
                user_path
            ));
        }
        return Ok(canon);
    }

    // For not-yet-existing targets, normalize path components
    let mut normalized = PathBuf::new();
    for comp in resolved.components() {
        match comp {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push("/"),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            std::path::Component::Normal(c) => normalized.push(c),
        }
    }

    if !normalized.starts_with(&canonical_root) && !normalized.starts_with(workspace_root) {
        return Err(format!(
            "Path escapes workspace root '{}': {}",
            canonical_root.display(),
            user_path
        ));
    }

    Ok(normalized)
}

/// The first `max_chars` bytes of `s`, never splitting a UTF-8 character.
///
/// `String::truncate` panics when `max_chars` lands in the middle of a multi-byte character,
/// so it must never be used to bound model-facing output: one CJK or emoji file would turn a
/// routine read into a panic. This walks back to the nearest char boundary instead.
fn char_safe_cut(s: &str, max_chars: usize) -> &str {
    if s.len() <= max_chars {
        return s;
    }
    let mut end = max_chars;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub struct ReadFileTool {
    workspace_root: PathBuf,
    max_chars: usize,
    read_cache: ReadCache,
}

impl ReadFileTool {
    pub fn new(workspace_root: PathBuf, max_chars: usize, read_cache: ReadCache) -> Self {
        Self {
            workspace_root,
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
        "Read a text file with line numbers and optional pagination. Emits whole lines and \
         stops at the character budget, then appends a notice naming the resume offset."
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
            None => return Ok(ToolResult::error(call_id, "Missing required argument 'path'")),
        };

        let resolved = match resolve_path(&self.workspace_root, path_str) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(call_id, e)),
        };

        if !resolved.exists() {
            return Ok(ToolResult::error(
                call_id,
                format!("File not found: {}", path_str),
            ));
        }

        let content = match tokio::fs::read_to_string(&resolved).await {
            Ok(c) => c,
            Err(e) => return Ok(ToolResult::error(call_id, format!("Failed to read file: {}", e))),
        };

        // Cache content for read-before-write verification
        self.read_cache.record_read(&resolved, &content).await;

        let offset = args.get("offset").and_then(|o| o.as_u64()).unwrap_or(1) as usize;
        let limit = args.get("limit").and_then(|l| l.as_u64()).map(|l| l as usize);

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
    read_cache: ReadCache,
}

impl WriteFileTool {
    pub fn new(workspace_root: PathBuf, read_cache: ReadCache) -> Self {
        Self {
            workspace_root,
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
        "Write content to a file, completely replacing existing content. Enforces read-before-write."
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
            None => return Ok(ToolResult::error(call_id, "Missing required argument 'path'")),
        };
        let content = match args.get("content").and_then(|c| c.as_str()) {
            Some(c) => c,
            None => return Ok(ToolResult::error(call_id, "Missing required argument 'content'")),
        };

        let resolved = match resolve_path(&self.workspace_root, path_str) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(call_id, e)),
        };

        // Check read-before-write if file exists
        if resolved.exists() {
            let current = match tokio::fs::read_to_string(&resolved).await {
                Ok(c) => c,
                Err(e) => return Ok(ToolResult::error(call_id, format!("Failed to read existing file: {}", e))),
            };
            if let Err(refusal) = self.read_cache.verify_and_update(&resolved, &current, content).await {
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
    read_cache: ReadCache,
}

impl PatchTool {
    pub fn new(workspace_root: PathBuf, read_cache: ReadCache) -> Self {
        Self {
            workspace_root,
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
        "Targeted find-and-replace edit in a file. Requires exact string match."
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
        let replace_all = args.get("replace_all").and_then(|r| r.as_bool()).unwrap_or(false);

        let resolved = resolve_path(&self.workspace_root, path).ok()?;
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
            None => return Ok(ToolResult::error(call_id, "Missing required argument 'path'")),
        };
        let old_str = match args.get("old_string").and_then(|s| s.as_str()) {
            Some(s) => s,
            None => return Ok(ToolResult::error(call_id, "Missing required argument 'old_string'")),
        };
        let new_str = match args.get("new_string").and_then(|s| s.as_str()) {
            Some(s) => s,
            None => return Ok(ToolResult::error(call_id, "Missing required argument 'new_string'")),
        };
        let replace_all = args.get("replace_all").and_then(|r| r.as_bool()).unwrap_or(false);

        let resolved = match resolve_path(&self.workspace_root, path_str) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(call_id, e)),
        };

        if !resolved.exists() {
            return Ok(ToolResult::error(
                call_id,
                format!("File not found: {}", path_str),
            ));
        }

        let current = match tokio::fs::read_to_string(&resolved).await {
            Ok(c) => c,
            Err(e) => return Ok(ToolResult::error(call_id, format!("Failed to read file: {}", e))),
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

        if let Err(refusal) = self.read_cache.verify_and_update(&resolved, &current, &patched).await {
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
}

impl SearchFilesTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
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

/// Which ceiling a scan hit. `None` means the walk finished exhaustively.
enum ScanLimit {
    FileCeiling,
    LineCeiling,
    FileListCeiling,
}

/// Result of one blocking scan: matching lines, matching files, per-file counts, and which
/// limit stopped the walk early (if any).
struct ScanOutcome {
    matching_lines: Vec<String>,
    matching_files: Vec<String>,
    file_counts: Vec<(String, usize)>,
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
            None => return Ok(ToolResult::error(call_id, "Missing required argument 'pattern'")),
        };
        let search_path_str = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
        let file_glob = args.get("file_glob").and_then(|g| g.as_str());
        let output_mode = args.get("output_mode").and_then(|m| m.as_str()).unwrap_or("content");

        let resolved = match resolve_path(&self.workspace_root, search_path_str) {
            Ok(p) => p,
            Err(e) => return Ok(ToolResult::error(call_id, e)),
        };

        if !resolved.exists() {
            return Ok(ToolResult::error(
                call_id,
                format!("Directory not found: {}", search_path_str),
            ));
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
            scan_files(&root, &resolved, &pattern, file_glob.as_deref(), &scan_mode, &cancel)
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

/// Iterative wildcard matcher over chars: `*` (any run, including separators) and `?` (exactly
/// one). No recursion, so a pattern of many stars cannot blow the stack, and no allocation.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    // Backtrack anchors: where a failed `*` resumes in the pattern, and in the text.
    let (mut star_p, mut star_t): (Option<usize>, usize) = (None, 0);

    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
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
    p[pi..].iter().all(|c| *c == '*')
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
        limit: None,
    };
    // Per-file counts are only rendered in 'count' mode; 'content' and 'files_only' never
    // read them, so the map stays empty there.
    let needs_counts = output_mode == "count";
    // The ceiling that applies to the results this mode actually renders. 'content' is bounded
    // by lines; the two list modes by files. Each mode checks its own ceiling and records which
    // one stopped the walk, so the notice names the limit the caller hit.
    let file_list_mode = output_mode != "content";
    let result_cap = if file_list_mode {
        MAX_MATCHING_FILES
    } else {
        MAX_MATCHING_LINES
    };

    for (visited, entry) in walkdir::WalkDir::new(resolved)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != ".git" && name != "target" && name != "node_modules"
        })
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
        if (outcome.matching_lines.len() + outcome.matching_files.len()) >= result_cap {
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

        if entry.metadata().map(|m| m.len()).unwrap_or(0) > MAX_SCANNED_FILE_BYTES {
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
                    if outcome.matching_lines.len() < MAX_MATCHING_LINES {
                        outcome
                            .matching_lines
                            .push(format!("{}:{}: {}", rel_path, idx + 1, line));
                    }
                }
            }

            if matches_in_file > 0 {
                outcome.matching_files.push(rel_path.clone());
                if needs_counts {
                    outcome.file_counts.push((rel_path, matches_in_file));
                }
            }
        }
    }

    Ok(outcome)
}

/// How many entries one `list_files` call reports before it stops and says so. A listing is
/// read by a model rather than a pager, so an unbounded one floods the transcript.
const MAX_LISTED_ENTRIES: usize = 1_000;

/// How many entries the walk collects before it stops, independent of `MAX_LISTED_ENTRIES`.
/// The display cap is applied *after* sorting, so the walk has to see enough of the tree to
/// guarantee the alphabetically-first entries are found, while a pathological tree still
/// cannot grow the buffer without bound.
const MAX_COLLECTED_ENTRIES: usize = 50_000;

pub struct ListFilesTool {
    workspace_root: PathBuf,
}

impl ListFilesTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait]
impl Tool for ListFilesTool {
    fn name(&self) -> &str {
        "list_files"
    }

    fn description(&self) -> &str {
        "List directory contents inside the workspace as workspace-relative paths, one per line, \
         with directories carrying a trailing slash. Recursive; use 'path' to bound it or 'glob' \
         to find files by name. 'glob' matches exactly as in search_files: a bare pattern \
         ('*.rs', '*_test.rs') matches the entry name, a pattern with a slash ('src/*.rs') \
         matches the workspace-relative path. .git, target and node_modules are skipped, and \
         results are capped (every cap announces itself in the output)."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Directory to list (default: workspace root)" },
                "glob": { "type": "string", "description": "Filter entries by glob: bare patterns match the entry name, slash patterns match the workspace-relative path" }
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
        let path_str = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
        let glob = args.get("glob").and_then(|g| g.as_str());

        let resolved = match resolve_path(&self.workspace_root, path_str) {
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
        let (entries, truncated) = tokio::task::spawn_blocking(move || {
            list_entries(&root, &resolved, glob.as_deref())
        })
        .await
        .map_err(|e| HarnessError::Io(format!("list task failed: {}", e)))?;

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
        if truncated {
            output.push_str(&format!(
                "[listing stopped after {MAX_LISTED_ENTRIES} entries; narrow 'path' or add 'glob']"
            ));
        }

        Ok(ToolResult::success(call_id, output))
    }
}

/// Recursively list entries under `resolved`, skipping noise directories and applying the
/// optional glob. Returns the sorted entries and whether the cap stopped the walk early.
fn list_entries(
    workspace_root: &Path,
    resolved: &Path,
    glob: Option<&str>,
) -> (Vec<String>, bool) {
    let mut entries: Vec<String> = Vec::new();
    let mut truncated = false;

    for entry in walkdir::WalkDir::new(resolved)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            name != ".git" && name != "target" && name != "node_modules"
        })
        .filter_map(Result::ok)
    {
        // The walk yields the root itself first; echoing it back is noise, not a listing.
        if entry.depth() == 0 {
            continue;
        }

        let path = entry.path();
        let is_dir = entry.file_type().is_dir();

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
            truncated = true;
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
        truncated = true;
    }

    (entries, truncated)
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

        let (entries, truncated) = list_entries(root, root, None);

        assert!(truncated, "dropping entries must be announced");
        assert_eq!(entries.len(), MAX_LISTED_ENTRIES);
        assert_eq!(entries.first().map(String::as_str), Some("00000.txt"));
        let expected_last = format!("{:05}.txt", MAX_LISTED_ENTRIES - 1);
        assert_eq!(entries.last().map(String::as_str), Some(expected_last.as_str()));
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

        let (matched, _) = list_entries(root, root, Some("*rust-harness*"));
        assert_eq!(matched, vec!["rust-harness.md".to_string()]);

        let (dirs, _) = list_entries(root, root, Some("*agent-harness*"));
        assert_eq!(dirs, vec!["rust-agent-harness/".to_string()]);

        let (nothing, _) = list_entries(root, root, Some("*nope*"));
        assert!(nothing.is_empty(), "an unmatched glob returns nothing, not a guess");
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
        assert!(glob_matches("*helper*", "helper_test.rs", &file, root));
        assert!(!glob_matches("tests/*.rs", "helper_test.rs", &file, root));
    }
}
