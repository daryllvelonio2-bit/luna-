use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{info, warn};

use harness_core::{unified_diff, EvolverEngine, HarnessError, ToolPreview};
use crate::sandbox::BuildSandbox;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StagedPatch {
    pub id: String,
    pub description: String,
    pub target_file: String,
    pub original_content: String,
    pub patched_content: String,
    pub created_at: DateTime<Utc>,
    pub is_verified: bool,
}

pub struct CodebaseEvolver {
    repo_root: PathBuf,
    sandbox: BuildSandbox,
    staged_patches: RwLock<HashMap<String, StagedPatch>>,
}

impl CodebaseEvolver {
    pub fn new(repo_root: PathBuf) -> Self {
        Self {
            sandbox: BuildSandbox::new(repo_root.clone()),
            repo_root,
            staged_patches: RwLock::new(HashMap::new()),
        }
    }

    pub async fn stage_file_patch(
        &self,
        description: &str,
        target_file: &str,
        patched_content: &str,
    ) -> Result<String, HarnessError> {
        let full_path = self.repo_root.join(target_file);
        let original_content = if full_path.exists() {
            fs::read_to_string(&full_path).map_err(|e| HarnessError::Io(e.to_string()))?
        } else {
            String::new()
        };

        let patch_id = format!("patch_{}", uuid::Uuid::new_v4());
        let patch = StagedPatch {
            id: patch_id.clone(),
            description: description.to_string(),
            target_file: target_file.to_string(),
            original_content,
            patched_content: patched_content.to_string(),
            created_at: Utc::now(),
            is_verified: false,
        };

        let mut map = self.staged_patches.write().await;
        map.insert(patch_id.clone(), patch);
        info!("Staged patch proposal '{}': {}", patch_id, description);
        Ok(patch_id)
    }
}

#[async_trait]
impl EvolverEngine for CodebaseEvolver {
    async fn inspect_source(&self, path: &str) -> Result<String, HarnessError> {
        let target = self.repo_root.join(path);
        if !target.exists() {
            return Err(HarnessError::EvolverError(format!(
                "Path does not exist: {}",
                target.display()
            )));
        }

        if target.is_dir() {
            let mut entries = Vec::new();
            for entry in walkdir::WalkDir::new(&target)
                .max_depth(2)
                .into_iter()
                .filter_map(Result::ok)
            {
                let p = entry.path();
                if let Ok(rel) = p.strip_prefix(&self.repo_root) {
                    entries.push(rel.display().to_string());
                }
            }
            Ok(entries.join("\n"))
        } else {
            fs::read_to_string(&target).map_err(|e| HarnessError::Io(e.to_string()))
        }
    }

    async fn propose_patch(&self, description: &str, diff_or_content: &str) -> Result<String, HarnessError> {
        // Accepts json payload or content
        let (file, content) = if let Ok(json_val) = serde_json::from_str::<serde_json::Value>(diff_or_content) {
            let f = json_val.get("file").and_then(|f| f.as_str()).unwrap_or("").to_string();
            let c = json_val.get("content").and_then(|c| c.as_str()).unwrap_or("").to_string();
            (f, c)
        } else {
            ("crates/harness-core/src/lib.rs".to_string(), diff_or_content.to_string())
        };

        self.stage_file_patch(description, &file, &content).await
    }

    async fn test_patch_sandbox(&self, patch_id: &str) -> Result<bool, HarnessError> {
        let patch = {
            let map = self.staged_patches.read().await;
            map.get(patch_id).cloned().ok_or_else(|| {
                HarnessError::EvolverError(format!("Staged patch '{}' not found", patch_id))
            })?
        };

        info!("Testing patch '{}' in sandbox", patch_id);
        let res = self
            .sandbox
            .verify_patch(&patch.target_file, &patch.patched_content)
            .await;

        match res {
            Ok(_) => {
                let mut map = self.staged_patches.write().await;
                if let Some(p) = map.get_mut(patch_id) {
                    p.is_verified = true;
                }
                Ok(true)
            }
            Err(e) => {
                warn!("Patch '{}' failed sandbox verification: {}", patch_id, e);
                Err(e)
            }
        }
    }

    async fn apply_patch(&self, patch_id: &str) -> Result<(), HarnessError> {
        let patch = {
            let map = self.staged_patches.read().await;
            map.get(patch_id).cloned().ok_or_else(|| {
                HarnessError::EvolverError(format!("Staged patch '{}' not found", patch_id))
            })?
        };

        let target_path = self.repo_root.join(&patch.target_file);
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).map_err(|e| HarnessError::Io(e.to_string()))?;
        }

        fs::write(&target_path, &patch.patched_content).map_err(|e| HarnessError::Io(e.to_string()))?;
        info!("Applied patch '{}' to {}", patch_id, target_path.display());
        Ok(())
    }

    /// The review material for the approval gate: which file, and the unified diff of staged
    /// content against the file on disk right now.
    async fn preview_patch(&self, patch_id: &str) -> Option<ToolPreview> {
        let patch = {
            let map = self.staged_patches.read().await;
            map.get(patch_id).cloned()?
        };
        let on_disk = fs::read_to_string(self.repo_root.join(&patch.target_file)).ok();
        let old = on_disk.as_deref().unwrap_or(&patch.original_content);

        Some(ToolPreview {
            target: Some(patch.target_file.clone()),
            diff: Some(unified_diff(old, &patch.patched_content, &patch.target_file)),
        })
    }
}
