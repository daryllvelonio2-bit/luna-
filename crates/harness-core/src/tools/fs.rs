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
        "Read a text file with line numbers and optional pagination. Truncates at budget."
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

        let mut output = String::new();
        for (idx, line) in selected_lines.iter().enumerate() {
            output.push_str(&format!("{}|{}\n", start + idx + 1, line));
        }

        if output.len() > self.max_chars {
            output.truncate(self.max_chars);
            output.push_str("\n\n[Truncated: character limit reached]");
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

#[async_trait]
impl Tool for SearchFilesTool {
    fn name(&self) -> &str {
        "search_files"
    }

    fn description(&self) -> &str {
        "Search file contents or find files by name inside the workspace."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regex or text pattern to search for" },
                "path": { "type": "string", "description": "Subdirectory to search in (default: workspace root)" },
                "file_glob": { "type": "string", "description": "Filter files by extension or pattern (e.g. '*.rs')" },
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
        _cancel_token: &CancellationToken,
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

        let mut matching_lines = Vec::new();
        let mut matching_files = Vec::new();
        let mut file_counts: Vec<(String, usize)> = Vec::new();

        for entry in walkdir::WalkDir::new(&resolved)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                name != ".git" && name != "target" && name != "node_modules"
            })
            .filter_map(Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }

            let path = entry.path();
            if let Some(glob) = file_glob {
                let fname = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let ext_filter = glob.trim_start_matches('*').trim_start_matches('.');
                if !fname.ends_with(ext_filter) {
                    continue;
                }
            }

            let rel_path = path
                .strip_prefix(&self.workspace_root)
                .unwrap_or(path)
                .to_string_lossy()
                .to_string();

            if let Ok(content) = std::fs::read_to_string(path) {
                let mut matches_in_file = 0;
                for (idx, line) in content.lines().enumerate() {
                    if line.contains(pattern) {
                        matches_in_file += 1;
                        if matching_lines.len() < 200 {
                            matching_lines.push(format!("{}:{}: {}", rel_path, idx + 1, line));
                        }
                    }
                }

                if matches_in_file > 0 {
                    matching_files.push(rel_path.clone());
                    file_counts.push((rel_path, matches_in_file));
                }
            }
        }

        let output = match output_mode {
            "files_only" => matching_files.join("\n"),
            "count" => {
                let mut out = String::new();
                for (f, count) in file_counts {
                    out.push_str(&format!("{}: {} matches\n", f, count));
                }
                out
            }
            _ => matching_lines.join("\n"),
        };

        Ok(ToolResult::success(call_id, output))
    }
}
