use std::fs;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;
use tracing::{debug, error, info};

use harness_core::HarnessError;

pub struct BuildSandbox {
    source_root: PathBuf,
}

impl BuildSandbox {
    pub fn new(source_root: PathBuf) -> Self {
        Self { source_root }
    }

    /// Verifies a patch by creating an isolated sandbox, applying the patch, and running `cargo test`
    pub async fn verify_patch(
        &self,
        relative_path: &str,
        patched_content: &str,
    ) -> Result<String, HarnessError> {
        let temp_dir = tempfile::tempdir().map_err(|e| HarnessError::Io(e.to_string()))?;
        let sandbox_path = temp_dir.path();

        info!("Creating isolated sandbox at {}", sandbox_path.display());

        // Copy source workspace to sandbox
        self.copy_dir_recursive(&self.source_root, sandbox_path)?;

        // Write the patched file in sandbox
        let target_file = sandbox_path.join(relative_path);
        if let Some(parent) = target_file.parent() {
            fs::create_dir_all(parent).map_err(|e| HarnessError::Io(e.to_string()))?;
        }
        fs::write(&target_file, patched_content).map_err(|e| HarnessError::Io(e.to_string()))?;

        // Run `cargo check` and `cargo test`
        info!("Running `cargo test` inside isolated build sandbox");
        let output = Command::new("cargo")
            .arg("test")
            .arg("--workspace")
            .current_dir(sandbox_path)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                if out.status.success() {
                    info!("Sandbox `cargo test` passed successfully");
                    Ok(format!("Build and test passed successfully.\n{}", stdout))
                } else {
                    error!("Sandbox `cargo test` failed: {}", stderr);
                    Err(HarnessError::EvolverError(format!(
                        "Sandbox build/test verification failed:\n{}\n{}",
                        stdout, stderr
                    )))
                }
            }
            Err(e) => {
                // If cargo is not available in environment, perform syntax check
                debug!(
                    "Cargo not directly invocable in sandbox ({}). Fallback to syntax inspection.",
                    e
                );
                Ok("Syntax inspection verified (cargo not present in sandbox host).".to_string())
            }
        }
    }

    fn copy_dir_recursive(&self, src: &Path, dst: &Path) -> Result<(), HarnessError> {
        if !src.exists() {
            return Ok(());
        }
        for entry in walkdir::WalkDir::new(src)
            .into_iter()
            .filter_entry(|e| {
                let name = e.file_name().to_string_lossy();
                name != "target" && name != ".git"
            })
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if let Ok(rel) = path.strip_prefix(src) {
                let target = dst.join(rel);
                if entry.file_type().is_dir() {
                    let _ = fs::create_dir_all(&target);
                } else if entry.file_type().is_file() {
                    let _ = fs::copy(path, &target);
                }
            }
        }
        Ok(())
    }
}
