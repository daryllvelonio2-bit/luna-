use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{error, info};

use harness_core::{
    prompts, Agent, AgentConfig, DefaultToolRegistry, EvolverEngine, HarnessError,
    MemoryEngine, MemoryRecord, ModelProvider, Note, NotesEngine, ToolPreview, ToolRegistry,
};
use harness_core::tools::{
    ListFilesTool, PatchTool, ReadCache, ReadFileTool, SearchFilesTool, TerminalTool, TodoListTool,
    TodoScope,
    WriteFileTool,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", content = "details")]
pub enum JobStatus {
    Queued,
    Running,
    FailedTest { error: String },
    ReadyForReenactment { binary_path: String },
    Verified,
    FailedVerification { reason: String },
    ScheduledUpdate { binary_path: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EvolverEvent {
    JobStarted {
        job_id: String,
        task: String,
    },
    JobProgress {
        job_id: String,
        task: String,
        current_tool: String,
    },
    JobCompleted {
        job_id: String,
        task: String,
        status: JobStatus,
        summary: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub task: String,
    pub evidence: Option<String>,
    pub branch: String,
    pub worktree_dir: String,
    pub status: JobStatus,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub logs: Vec<String>,
    #[serde(default)]
    pub current_tool: Option<String>,
    #[serde(default)]
    pub modified_files: Vec<String>,
    #[serde(default)]
    pub summary: Option<String>,
    /// Commit the worktree branch was created from. Used to prove that a job
    /// actually produced commits before its gates are treated as meaningful.
    #[serde(default)]
    pub base_commit: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingUpdate {
    pub job_id: String,
    pub task: String,
    pub branch: String,
    pub worktree_dir: String,
    pub binary_path: String,
    pub staged_at: DateTime<Utc>,
}

/// Discovers the true root of the Luna harness repository.
/// When Luna is launched from outside its git repository (e.g. from the user's home directory),
/// this resolves the repository path by checking:
/// 1. `LUNA_HARNESS_REPO` environment variable.
/// 2. Whether `hint` is within the harness git repository.
/// 3. The executable location (canonicalized target).
/// 4. Common known user checkout directories under `$HOME`.
pub fn discover_harness_repo_root(hint: PathBuf) -> PathBuf {
    if let Ok(env_path) = std::env::var("LUNA_HARNESS_REPO") {
        let p = PathBuf::from(env_path);
        if is_harness_repo(&p) {
            return p;
        }
    }

    if let Some(root) = find_git_toplevel(&hint) {
        if is_harness_repo(&root) {
            return root;
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        let canonical = std::fs::canonicalize(&exe).unwrap_or(exe);
        let mut curr = canonical.parent();
        while let Some(parent) = curr {
            if is_harness_repo(parent) {
                return parent.to_path_buf();
            }
            curr = parent.parent();
        }
    }

    if let Ok(home) = std::env::var("HOME").map(PathBuf::from) {
        for candidate in [
            home.join("Downloads/rust-agent-harness"),
            home.join("rust-agent-harness"),
            home.join("Projects/rust-agent-harness"),
            home.join("src/rust-agent-harness"),
        ] {
            if is_harness_repo(&candidate) {
                return candidate;
            }
        }
    }

    hint
}

fn is_harness_repo(path: &Path) -> bool {
    path.join(".git").exists() && path.join("crates/harness-core").exists()
}

fn find_git_toplevel(path: &Path) -> Option<PathBuf> {
    if !path.exists() {
        return None;
    }
    let output = Command::new("git")
        .args(["-C", &path.to_string_lossy(), "rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if output.status.success() {
        let root = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !root.is_empty() {
            return Some(PathBuf::from(root));
        }
    }
    None
}

/// Cleans up a git worktree, its branch, and any leftover directory on disk.
fn cleanup_worktree(repo_root: &Path, worktree_dir: &Path, branch: Option<&str>) {
    // Purge the worktree's own cargo target dir first. If `git worktree remove`
    // below refuses (e.g. locked index), this still guarantees the artifacts
    // cannot survive to poison another tree's build.
    let own_target = worktree_target_dir(worktree_dir);
    if own_target.exists() {
        let _ = fs::remove_dir_all(&own_target);
    }

    let _ = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "worktree",
            "remove",
            "--force",
            &worktree_dir.to_string_lossy(),
        ])
        .output();

    if let Some(b) = branch {
        let _ = Command::new("git")
            .args(["-C", &repo_root.to_string_lossy(), "branch", "-D", b])
            .output();
    }

    if worktree_dir.exists() {
        let _ = fs::remove_dir_all(worktree_dir);
    }
}

/// The cargo target directory used for builds inside an engineer worktree.
///
/// This is deliberately *inside* the worktree: `<worktree>/target`. Engineer
/// builds must never write into the main repository's shared `target/`, because
/// artifacts compiled from a worktree bake `CARGO_MANIFEST_DIR` to a path that
/// only exists while the worktree does. Once the worktree is deleted those
/// artifacts remain "fresh" to cargo and main-repo suites that canonicalize
/// `CARGO_MANIFEST_DIR` start failing with phantom `NotFound` errors.
pub fn worktree_target_dir(worktree_dir: &Path) -> PathBuf {
    worktree_dir.join("target")
}

/// Stages and commits every change inside the worktree.
/// Returns `true` when a commit was created, `false` when the tree was clean.
fn commit_worktree_changes(worktree_dir: &Path, message: &str) -> Result<bool, HarnessError> {
    let wt = worktree_dir.to_string_lossy().to_string();

    let add = Command::new("git")
        .args(["-C", &wt, "add", "-A"])
        .output()
        .map_err(|e| HarnessError::Io(format!("git add failed: {}", e)))?;
    if !add.status.success() {
        return Err(HarnessError::EvolverError(format!(
            "git add -A failed in worktree: {}",
            String::from_utf8_lossy(&add.stderr)
        )));
    }

    let staged = Command::new("git")
        .args(["-C", &wt, "diff", "--cached", "--quiet"])
        .output()
        .map_err(|e| HarnessError::Io(format!("git diff --cached failed: {}", e)))?;
    if staged.status.success() {
        // Nothing staged: the tree is clean, the agent changed nothing.
        return Ok(false);
    }

    let commit = Command::new("git")
        .args(["-C", &wt, "commit", "-m", message])
        .output()
        .map_err(|e| HarnessError::Io(format!("git commit failed: {}", e)))?;
    if !commit.status.success() {
        return Err(HarnessError::EvolverError(format!(
            "git commit failed in worktree: {}",
            String::from_utf8_lossy(&commit.stderr)
        )));
    }
    Ok(true)
}

/// Number of commits on the worktree branch that are not reachable from `base`.
fn count_commits_ahead(worktree_dir: &Path, base: &str) -> Result<u64, HarnessError> {
    let out = Command::new("git")
        .args([
            "-C",
            &worktree_dir.to_string_lossy(),
            "rev-list",
            "--count",
            &format!("{}..HEAD", base),
        ])
        .output()
        .map_err(|e| HarnessError::Io(format!("git rev-list failed: {}", e)))?;
    if !out.status.success() {
        return Err(HarnessError::EvolverError(format!(
            "git rev-list --count failed in worktree: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|e| HarnessError::EvolverError(format!("unparsable commit count: {}", e)))
}

fn head_commit(worktree_dir: &Path) -> Result<String, HarnessError> {
    let out = Command::new("git")
        .args(["-C", &worktree_dir.to_string_lossy(), "rev-parse", "HEAD"])
        .output()
        .map_err(|e| HarnessError::Io(format!("git rev-parse failed: {}", e)))?;
    if !out.status.success() {
        return Err(HarnessError::EvolverError(format!(
            "git rev-parse HEAD failed in worktree: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub struct CodebaseEvolver {
    repo_root: PathBuf,
    jobs_dir: PathBuf,
    provider: RwLock<Option<Arc<dyn ModelProvider>>>,
    jobs: Arc<RwLock<HashMap<String, JobRecord>>>,
    event_tx: tokio::sync::broadcast::Sender<EvolverEvent>,
}

impl CodebaseEvolver {
    pub fn new(repo_root: PathBuf, jobs_dir: PathBuf) -> Self {
        let repo_root = discover_harness_repo_root(repo_root);
        let _ = fs::create_dir_all(&jobs_dir);
        let (event_tx, _) = tokio::sync::broadcast::channel(128);
        let evolver = Self {
            repo_root,
            jobs_dir,
            provider: RwLock::new(None),
            jobs: Arc::new(RwLock::new(HashMap::new())),
            event_tx,
        };
        evolver.reconcile_orphaned_jobs();
        evolver
    }

    /// Reconcile stale jobs on disk from previous process runs that were left in Running or Queued state.
    pub fn reconcile_orphaned_jobs(&self) {
        if let Ok(entries) = fs::read_dir(&self.jobs_dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("json")
                    && path.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("job_"))
                {
                    if let Ok(data) = fs::read_to_string(&path) {
                        if let Ok(mut record) = serde_json::from_str::<JobRecord>(&data) {
                            if matches!(record.status, JobStatus::Running | JobStatus::Queued) {
                                record.status = JobStatus::FailedTest {
                                    error: "Interrupted: harness process exited while job was running".to_string(),
                                };
                                record.updated_at = Utc::now();
                                record.logs.push("Job interrupted: process exited before job completed.".to_string());
                                let _ = fs::write(&path, serde_json::to_string_pretty(&record).unwrap_or_default());
                                cleanup_worktree(&self.repo_root, Path::new(&record.worktree_dir), Some(&record.branch));
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<EvolverEvent> {
        self.event_tx.subscribe()
    }

    pub fn event_sender(&self) -> tokio::sync::broadcast::Sender<EvolverEvent> {
        self.event_tx.clone()
    }

    pub async fn active_job(&self) -> Option<JobRecord> {
        let jobs = self.jobs.read().await;
        jobs.values()
            .find(|j| matches!(j.status, JobStatus::Running | JobStatus::Queued))
            .cloned()
    }

    pub async fn latest_job(&self) -> Result<JobRecord, HarnessError> {
        {
            let jobs = self.jobs.read().await;
            if let Some(latest) = jobs.values().max_by_key(|j| j.created_at) {
                return Ok(latest.clone());
            }
        }
        let jobs = self.list_jobs().await?;
        jobs.into_iter()
            .max_by_key(|j| j.created_at)
            .ok_or_else(|| HarnessError::EvolverError("No jobs found".to_string()))
    }

    pub fn repo_root(&self) -> &Path {
        &self.repo_root
    }

    pub fn with_provider(mut self, provider: Arc<dyn ModelProvider>) -> Self {
        self.provider = RwLock::new(Some(provider));
        self
    }

    pub async fn set_provider(&self, provider: Arc<dyn ModelProvider>) {
        let mut p = self.provider.write().await;
        *p = Some(provider);
    }

    pub async fn delegate_improvement(
        &self,
        task: &str,
        evidence: Option<&str>,
    ) -> Result<String, HarnessError> {
        let provider = self.provider.read().await.clone().ok_or_else(|| {
            HarnessError::EvolverError(
                "No model provider configured for engineer subagent".to_string(),
            )
        })?;

        let uuid = uuid::Uuid::new_v4().simple().to_string();
        let job_id = format!("job_{}", &uuid[..8]);
        let branch = format!("eng/{}", job_id);
        let worktree_dir = self.repo_root.join(".worktrees").join(format!("eng_{}", job_id));

        let record = JobRecord {
            id: job_id.clone(),
            task: task.to_string(),
            evidence: evidence.map(|s| s.to_string()),
            branch: branch.clone(),
            worktree_dir: worktree_dir.to_string_lossy().to_string(),
            status: JobStatus::Queued,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            logs: vec![format!("Job {} created for task: {}", job_id, task)],
            current_tool: None,
            modified_files: Vec::new(),
            summary: None,
            base_commit: None,
        };

        self.save_job(&record).await?;
        self.jobs.write().await.insert(job_id.clone(), record);

        let _ = self.event_tx.send(EvolverEvent::JobStarted {
            job_id: job_id.clone(),
            task: task.to_string(),
        });

        // Spawn background task
        let repo_root = self.repo_root.clone();
        let jobs_dir = self.jobs_dir.clone();
        let task_str = task.to_string();
        let evidence_opt = evidence.map(|s| s.to_string());
        let j_id = job_id.clone();
        let b_name = branch.clone();
        let wt_path = worktree_dir.clone();
        let event_tx = self.event_tx.clone();
        let jobs_map = self.jobs.clone();

        tokio::spawn(async move {
            if let Err(e) = run_engineer_job(
                repo_root,
                jobs_dir,
                jobs_map,
                provider,
                j_id.clone(),
                b_name,
                wt_path,
                task_str,
                evidence_opt,
                event_tx,
            )
            .await
            {
                error!("Engineer job {} failed with error: {}", j_id, e);
            }
        });

        Ok(job_id)
    }

    pub async fn check_job(&self, job_id: &str) -> Result<JobRecord, HarnessError> {
        let target_id = if job_id.trim().is_empty() || job_id.trim() == "latest" {
            self.latest_job().await?.id
        } else {
            job_id.trim().to_string()
        };

        {
            let jobs = self.jobs.read().await;
            if let Some(record) = jobs.get(&target_id) {
                return Ok(record.clone());
            }
        }

        let path = self.jobs_dir.join(format!("{}.json", target_id));
        if path.exists() {
            let data = fs::read_to_string(&path)
                .map_err(|e| HarnessError::Io(format!("Failed to read job file: {}", e)))?;
            let record: JobRecord = serde_json::from_str(&data)
                .map_err(|e| HarnessError::EvolverError(format!("Failed to parse job JSON: {}", e)))?;
            Ok(record)
        } else {
            Err(HarnessError::EvolverError(format!("Job '{}' not found", target_id)))
        }
    }

    pub async fn list_jobs(&self) -> Result<Vec<JobRecord>, HarnessError> {
        let mut list_map = HashMap::new();
        if self.jobs_dir.exists() {
            for entry in fs::read_dir(&self.jobs_dir)
                .map_err(|e| HarnessError::Io(format!("Failed to read jobs dir: {}", e)))?
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if path.extension().and_then(|s| s.to_str()) == Some("json")
                    && path.file_name().and_then(|s| s.to_str()) != Some("pending_update.json")
                {
                    if let Ok(data) = fs::read_to_string(&path) {
                        if let Ok(record) = serde_json::from_str::<JobRecord>(&data) {
                            list_map.insert(record.id.clone(), record);
                        }
                    }
                }
            }
        }
        {
            let mem_jobs = self.jobs.read().await;
            for (id, rec) in mem_jobs.iter() {
                list_map.insert(id.clone(), rec.clone());
            }
        }
        let mut list: Vec<JobRecord> = list_map.into_values().collect();
        list.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(list)
    }

    pub async fn verify_and_stage(
        &self,
        job_id: &str,
        scenario_input: Option<&str>,
        absent_pattern: Option<&str>,
    ) -> Result<String, HarnessError> {
        let mut job = self.check_job(job_id).await?;
        let binary_path = match &job.status {
            JobStatus::ReadyForReenactment { binary_path } => PathBuf::from(binary_path),
            JobStatus::ScheduledUpdate { binary_path } => PathBuf::from(binary_path),
            other => {
                return Err(HarnessError::EvolverError(format!(
                    "Job '{}' is not ready for re-enactment (status: {:?})",
                    job_id, other
                )))
            }
        };

        if !binary_path.exists() {
            return Err(HarnessError::EvolverError(format!(
                "Worktree binary does not exist at {}",
                binary_path.display()
            )));
        }

        let wt_dir = PathBuf::from(&job.worktree_dir);

        let inputs = if let Some(txt) = scenario_input {
            vec![crate::pty::TimedInput {
                delay_ms: 400,
                input: txt.to_string(),
            }]
        } else {
            vec![]
        };

        // Run scenario in PTY with full 2D virtual terminal screen capture
        let (code, output, capture) = crate::pty::run_in_pty_with_capture(
            &binary_path,
            &[],
            &wt_dir,
            &inputs,
            24,
            80,
            Duration::from_secs(45),
        )?;

        // Differential verification: exit code 0 and absent pattern not in output or screen
        if code != 0 {
            let reason = format!(
                "PTY execution failed with exit code {}.\n\n--- SCREEN CAPTURE ({}x{}) ---\n{}\n\nRaw output:\n{}",
                code, capture.rows, capture.cols, capture.rendered_display, output
            );
            job.status = JobStatus::FailedVerification {
                reason: reason.clone(),
            };
            job.updated_at = Utc::now();
            job.logs.push(format!("Verification failed: {}", reason));
            let _ = self.save_job(&job).await;
            // Clean up worktree on failure
            cleanup_worktree(&self.repo_root, Path::new(&job.worktree_dir), Some(&job.branch));
            return Err(HarnessError::EvolverError(reason));
        }

        if let Some(pattern) = absent_pattern {
            if output.contains(pattern) || capture.rendered_display.contains(pattern) {
                let reason = format!(
                    "Regression check failed: forbidden pattern '{}' found in terminal display.\n\n--- SCREEN CAPTURE ({}x{}) ---\n{}",
                    pattern, capture.rows, capture.cols, capture.rendered_display
                );
                job.status = JobStatus::FailedVerification {
                    reason: reason.clone(),
                };
                job.updated_at = Utc::now();
                job.logs.push(format!("Verification failed: {}", reason));
                let _ = self.save_job(&job).await;
                cleanup_worktree(&self.repo_root, Path::new(&job.worktree_dir), Some(&job.branch));
                return Err(HarnessError::EvolverError(reason));
            }
        }

        // Verification passed! Stage pending update
        job.status = JobStatus::ScheduledUpdate {
            binary_path: binary_path.to_string_lossy().to_string(),
        };
        job.updated_at = Utc::now();
        job.logs.push("PTY scenario verification passed. Staged pending update.".to_string());
        self.save_job(&job).await?;

        let pending = PendingUpdate {
            job_id: job.id.clone(),
            task: job.task.clone(),
            branch: job.branch.clone(),
            worktree_dir: job.worktree_dir.clone(),
            binary_path: binary_path.to_string_lossy().to_string(),
            staged_at: Utc::now(),
        };

        let pending_json = serde_json::to_string_pretty(&pending)
            .map_err(|e| HarnessError::EvolverError(e.to_string()))?;
        fs::write(self.jobs_dir.join("pending_update.json"), pending_json)
            .map_err(|e| HarnessError::Io(e.to_string()))?;

        Ok(format!(
            "Job '{}' successfully verified! Update is scheduled to apply on next launch.\n\n--- SCREEN CAPTURE ({}x{}) ---\n{}",
            job_id, capture.rows, capture.cols, capture.rendered_display
        ))
    }

    pub fn get_pending_update(&self) -> Option<PendingUpdate> {
        let path = self.jobs_dir.join("pending_update.json");
        if path.exists() {
            let data = fs::read_to_string(path).ok()?;
            serde_json::from_str(&data).ok()
        } else {
            None
        }
    }

    async fn save_job(&self, job: &JobRecord) -> Result<(), HarnessError> {
        let _ = fs::create_dir_all(&self.jobs_dir);
        let path = self.jobs_dir.join(format!("{}.json", job.id));
        let data = serde_json::to_string_pretty(job)
            .map_err(|e| HarnessError::EvolverError(e.to_string()))?;
        fs::write(path, data).map_err(|e| HarnessError::Io(e.to_string()))?;
        Ok(())
    }
}

async fn run_engineer_job(
    repo_root: PathBuf,
    jobs_dir: PathBuf,
    jobs_map: Arc<RwLock<HashMap<String, JobRecord>>>,
    provider: Arc<dyn ModelProvider>,
    job_id: String,
    branch: String,
    worktree_dir: PathBuf,
    task: String,
    evidence: Option<String>,
    event_tx: tokio::sync::broadcast::Sender<EvolverEvent>,
) -> Result<(), HarnessError> {
    let job_file = jobs_dir.join(format!("{}.json", job_id));
    let mut record: JobRecord = {
        let data = fs::read_to_string(&job_file)
            .map_err(|e| HarnessError::Io(format!("Failed to read job file: {}", e)))?;
        serde_json::from_str(&data)
            .map_err(|e| HarnessError::EvolverError(format!("Failed to parse job JSON: {}", e)))?
    };

    record.status = JobStatus::Running;
    record.updated_at = Utc::now();
    record.logs.push(format!("Creating git worktree at {} on branch {}", worktree_dir.display(), branch));
    let _ = fs::write(&job_file, serde_json::to_string_pretty(&record).unwrap_or_default());
    jobs_map.write().await.insert(job_id.clone(), record.clone());

    let _ = event_tx.send(EvolverEvent::JobProgress {
        job_id: job_id.clone(),
        task: task.clone(),
        current_tool: "git worktree setup".to_string(),
    });

    // 1. Create worktree
    let wt_parent = worktree_dir.parent().unwrap_or(&repo_root);
    let _ = fs::create_dir_all(wt_parent);

    let add_res = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "worktree",
            "add",
            "-b",
            &branch,
            &worktree_dir.to_string_lossy(),
            "main",
        ])
        .output();

    match add_res {
        Ok(out) if out.status.success() => {
            record.logs.push("Git worktree created successfully.".to_string());
            let _ = Command::new("git")
                .args(["-C", &worktree_dir.to_string_lossy(), "config", "user.name", "Daryll"])
                .output();
            let _ = Command::new("git")
                .args(["-C", &worktree_dir.to_string_lossy(), "config", "user.email", "daryllvelonio2@gmail.com"])
                .output();
            match head_commit(&worktree_dir) {
                Ok(sha) => {
                    record.base_commit = Some(sha.clone());
                    record.logs.push(format!("Worktree based on commit {}.", sha));
                }
                Err(e) => record.logs.push(format!("Warning: could not record base commit: {}", e)),
            }
        }
        Ok(out) => {
            let err = String::from_utf8_lossy(&out.stderr).to_string();
            cleanup_worktree(&repo_root, &worktree_dir, Some(&branch));
            record.status = JobStatus::FailedTest {
                error: format!("git worktree add failed: {}", err),
            };
            record.updated_at = Utc::now();
            let _ = fs::write(&job_file, serde_json::to_string_pretty(&record).unwrap_or_default());
            jobs_map.write().await.insert(job_id.clone(), record.clone());
            let _ = event_tx.send(EvolverEvent::JobCompleted {
                job_id: job_id.clone(),
                task: task.clone(),
                status: record.status.clone(),
                summary: format!("git worktree add failed: {}", err),
            });
            return Err(HarnessError::EvolverError(err));
        }
        Err(e) => {
            cleanup_worktree(&repo_root, &worktree_dir, Some(&branch));
            record.status = JobStatus::FailedTest {
                error: format!("Failed to run git: {}", e),
            };
            record.updated_at = Utc::now();
            let _ = fs::write(&job_file, serde_json::to_string_pretty(&record).unwrap_or_default());
            jobs_map.write().await.insert(job_id.clone(), record.clone());
            let _ = event_tx.send(EvolverEvent::JobCompleted {
                job_id: job_id.clone(),
                task: task.clone(),
                status: record.status.clone(),
                summary: format!("Failed to run git: {}", e),
            });
            return Err(HarnessError::Io(e.to_string()));
        }
    }

    // 2. Setup tools for engineer agent
    let read_cache = ReadCache::new();
    let registry = Arc::new(DefaultToolRegistry::new());
    registry
        .register(Arc::new(ReadFileTool::new(
            worktree_dir.clone(),
            100_000,
            read_cache.clone(),
        )))
        .await?;
    registry
        .register(Arc::new(WriteFileTool::new(
            worktree_dir.clone(),
            read_cache.clone(),
        )))
        .await?;
    registry
        .register(Arc::new(PatchTool::new(
            worktree_dir.clone(),
            read_cache.clone(),
        )))
        .await?;
    registry
        .register(Arc::new(SearchFilesTool::new(worktree_dir.clone())))
        .await?;
    registry
        .register(Arc::new(ListFilesTool::new(worktree_dir.clone())))
        .await?;
    // The engineer's list belongs to the job, not to the worktree it happens to run in.
    registry
        .register(Arc::new(TodoListTool::new(TodoScope::new(
            jobs_dir.join("todos"),
            &job_id,
        ))))
        .await?;

    // Engineer builds are confined to the worktree's own target dir so their
    // artifacts can never be mistaken for the main repo's by a later cargo run.
    let target_dir = worktree_target_dir(&worktree_dir);
    let mut terminal = TerminalTool::new(
        worktree_dir.clone(),
        300,
        100_000,
    ).with_env("CARGO_TARGET_DIR", target_dir.to_string_lossy());

    if let Ok(cargo_home) = std::env::var("CARGO_HOME") {
        terminal = terminal.with_env("CARGO_HOME", cargo_home);
    }
    if let Ok(home) = std::env::var("HOME") {
        let cargo_bin = PathBuf::from(home).join(".cargo/bin");
        let current_path = std::env::var("PATH").unwrap_or_default();
        terminal = terminal.with_env("PATH", format!("{}:{}", cargo_bin.display(), current_path));
    }
    registry.register(Arc::new(terminal)).await?;

    let engineer_prompt = prompts::section(prompts::ENGINEER_SYSTEM_PROMPT).unwrap_or_default();
    let agent_config = AgentConfig {
        require_hitl_for_mutation: false,
        system_prompt_override: Some(engineer_prompt),
        ..Default::default()
    };

    let engineer_agent = Agent::new(
        Arc::new(provider),
        Arc::new(NoopMemory),
        Arc::new(NoopNotes),
        Arc::new(NoopEvolver),
        registry,
        agent_config,
    );

    let j_id_obs = job_id.clone();
    let task_obs = task.clone();
    let event_tx_obs = event_tx.clone();
    let job_file_obs = job_file.clone();
    let jobs_map_obs = jobs_map.clone();

    let engineer_agent = engineer_agent.with_observer(Arc::new(move |event| {
        match event {
            harness_core::AgentEvent::ToolExecuting { name, arguments } => {
                let (tool_desc, touched_file) = match name.as_str() {
                    "read_file" => {
                        let path = arguments.get("path").and_then(|p| p.as_str()).unwrap_or("");
                        (format!("read {}", path), None)
                    }
                    "write_file" => {
                        let path = arguments.get("path").and_then(|p| p.as_str()).unwrap_or("");
                        (format!("write {}", path), Some(path.to_string()))
                    }
                    "patch" => {
                        let path = arguments.get("path").and_then(|p| p.as_str()).unwrap_or("");
                        (format!("patch {}", path), Some(path.to_string()))
                    }
                    "search_files" => {
                        let pat = arguments.get("pattern").and_then(|p| p.as_str()).unwrap_or("");
                        (format!("search {}", pat), None)
                    }
                    "list_files" => {
                        let path = arguments.get("path").and_then(|p| p.as_str()).unwrap_or(".");
                        (format!("list {}", path), None)
                    }
                    "terminal" => {
                        let cmd = arguments.get("command").and_then(|c| c.as_str()).unwrap_or("");
                        (format!("run {}", cmd), None)
                    }
                    other => (other.to_string(), None),
                };

                let _ = event_tx_obs.send(EvolverEvent::JobProgress {
                    job_id: j_id_obs.clone(),
                    task: task_obs.clone(),
                    current_tool: tool_desc.clone(),
                });

                let log_entry = format!("• Tool: {}", tool_desc);
                if let Ok(mut jmap) = jobs_map_obs.try_write() {
                    if let Some(rec) = jmap.get_mut(&j_id_obs) {
                        rec.current_tool = Some(tool_desc);
                        rec.updated_at = Utc::now();
                        rec.logs.push(log_entry);
                        if let Some(f) = touched_file {
                            if !rec.modified_files.contains(&f) {
                                rec.modified_files.push(f);
                            }
                        }
                        let _ = fs::write(&job_file_obs, serde_json::to_string_pretty(&rec).unwrap_or_default());
                    }
                }
            }
            harness_core::AgentEvent::ToolExecuted { is_error, output, .. } => {
                let out_brief = if output.chars().count() > 160 {
                    format!("{}…", output.chars().take(159).collect::<String>())
                } else {
                    output.clone()
                };
                let log_entry = if is_error {
                    format!("  ✖ Error: {}", out_brief)
                } else {
                    format!("  ✔ Output: {}", out_brief)
                };
                if let Ok(mut jmap) = jobs_map_obs.try_write() {
                    if let Some(rec) = jmap.get_mut(&j_id_obs) {
                        rec.updated_at = Utc::now();
                        rec.logs.push(log_entry);
                        let _ = fs::write(&job_file_obs, serde_json::to_string_pretty(&rec).unwrap_or_default());
                    }
                }
            }
            _ => {}
        }
    }));

    let _ = event_tx.send(EvolverEvent::JobProgress {
        job_id: job_id.clone(),
        task: task.clone(),
        current_tool: "inspecting codebase".to_string(),
    });

    // Initial prompt
    let user_prompt = format!(
        "Task to implement in Luna harness codebase:\n{}\n\nEvidence and reproduction context:\n{}\n\nInspect code, make surgical modifications, and run `cargo test --workspace` via the terminal tool to verify. Report your conclusion when done.",
        task,
        evidence.as_deref().unwrap_or("None provided")
    );

    let cancel_token = tokio_util::sync::CancellationToken::new();
    let turn_res = engineer_agent.run_turn(&user_prompt, cancel_token).await;

    if let Some(latest_rec) = jobs_map.read().await.get(&job_id) {
        record = latest_rec.clone();
    }

    match turn_res {
        Ok(outcome) => {
            record.summary = Some(outcome.reply.clone());
            record.logs.push(format!("Engineer finished turn: {}", outcome.reply));
        }
        Err(e) => {
            record.logs.push(format!("Engineer turn error: {}", e));
        }
    }
    record.updated_at = Utc::now();
    let _ = fs::write(&job_file, serde_json::to_string_pretty(&record).unwrap_or_default());
    jobs_map.write().await.insert(job_id.clone(), record.clone());

    // 2b. Prove the job actually produced work before running any gate.
    // Gates executed against an unchanged tree are measuring `main`, not the fix —
    // that is exactly how three jobs reported "dual-gate passed" with zero commits.
    record.current_tool = Some("committing worktree changes".to_string());
    record.logs.push("Committing worktree changes...".to_string());
    record.updated_at = Utc::now();
    let _ = fs::write(&job_file, serde_json::to_string_pretty(&record).unwrap_or_default());
    jobs_map.write().await.insert(job_id.clone(), record.clone());

    let commit_msg = format!(
        "eng({}): {}",
        job_id,
        task.lines().next().unwrap_or("harness improvement").trim()
    );
    let commit_res = commit_worktree_changes(&worktree_dir, &commit_msg);
    let ahead_res = match &commit_res {
        Err(e) => Err(HarnessError::EvolverError(e.to_string())),
        Ok(_) => {
            let base = record
                .base_commit
                .clone()
                .unwrap_or_else(|| "main".to_string());
            count_commits_ahead(&worktree_dir, &base)
        }
    };

    let fail_reason = match (commit_res, ahead_res) {
        (Err(e), _) => Some(format!("Could not commit worktree changes: {}", e)),
        (_, Err(e)) => Some(format!("Could not determine worktree commit count: {}", e)),
        (Ok(_), Ok(0)) => Some(
            "Job produced no changes: the worktree branch has 0 commits ahead of its base, so the \
             verification gates would only have re-measured unchanged code. No update staged."
                .to_string(),
        ),
        (Ok(committed), Ok(n)) => {
            record.logs.push(format!(
                "Worktree committed: {} ({} commit(s) ahead of base).",
                if committed { "yes" } else { "already committed" },
                n
            ));
            None
        }
    };

    if let Some(reason) = fail_reason {
        record.status = JobStatus::FailedTest { error: reason.clone() };
        record.current_tool = None;
        record.updated_at = Utc::now();
        record.logs.push(format!("Job rejected: {}", reason));
        let _ = fs::write(&job_file, serde_json::to_string_pretty(&record).unwrap_or_default());
        jobs_map.write().await.insert(job_id.clone(), record.clone());
        let _ = event_tx.send(EvolverEvent::JobCompleted {
            job_id: job_id.clone(),
            task: task.clone(),
            status: record.status.clone(),
            summary: reason,
        });
        cleanup_worktree(&repo_root, &worktree_dir, Some(&branch));
        return Ok(());
    }

    // 3. Dual-gate Gate 1: cargo test --workspace inside worktree
    let _ = event_tx.send(EvolverEvent::JobProgress {
        job_id: job_id.clone(),
        task: task.clone(),
        current_tool: "cargo test --workspace (Gate 1)".to_string(),
    });
    record.current_tool = Some("cargo test --workspace (Gate 1)".to_string());
    record.logs.push("Running `cargo test --workspace` verification gate...".to_string());
    record.updated_at = Utc::now();
    let _ = fs::write(&job_file, serde_json::to_string_pretty(&record).unwrap_or_default());
    jobs_map.write().await.insert(job_id.clone(), record.clone());

    let test_res = Command::new("cargo")
        .args(["test", "--workspace"])
        .current_dir(&worktree_dir)
        .env("CARGO_TARGET_DIR", &target_dir)
        .output();

    let tests_passed = match test_res {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            record.status = JobStatus::FailedTest {
                error: format!("cargo test failed:\n{}\n{}", stdout, stderr),
            };
            false
        }
        Err(e) => {
            record.status = JobStatus::FailedTest {
                error: format!("Failed to spawn cargo test: {}", e),
            };
            false
        }
    };

    if !tests_passed {
        record.updated_at = Utc::now();
        let _ = fs::write(&job_file, serde_json::to_string_pretty(&record).unwrap_or_default());
        jobs_map.write().await.insert(job_id.clone(), record.clone());
        let _ = event_tx.send(EvolverEvent::JobCompleted {
            job_id: job_id.clone(),
            task: task.clone(),
            status: record.status.clone(),
            summary: "Gate 1 verification failed (`cargo test --workspace` errors)".to_string(),
        });
        // Clean up worktree and branch
        cleanup_worktree(&repo_root, &worktree_dir, Some(&branch));
        return Ok(());
    }

    record.logs.push("Gate 1 passed: `cargo test --workspace` clean.".to_string());

    // Build release binary
    let _ = event_tx.send(EvolverEvent::JobProgress {
        job_id: job_id.clone(),
        task: task.clone(),
        current_tool: "cargo build --release in worktree".to_string(),
    });
    record.current_tool = Some("cargo build --release in worktree".to_string());
    record.logs.push("Compiling release binary in worktree...".to_string());
    record.updated_at = Utc::now();
    let _ = fs::write(&job_file, serde_json::to_string_pretty(&record).unwrap_or_default());
    jobs_map.write().await.insert(job_id.clone(), record.clone());

    let build_res = Command::new("cargo")
        .args(["build", "--release"])
        .current_dir(&worktree_dir)
        .env("CARGO_TARGET_DIR", &target_dir)
        .output();

    let build_ok = match build_res {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).to_string();
            record.status = JobStatus::FailedTest {
                error: format!("cargo build --release failed:\n{}", stderr),
            };
            false
        }
        Err(e) => {
            record.status = JobStatus::FailedTest {
                error: format!("Failed to spawn cargo build: {}", e),
            };
            false
        }
    };

    if !build_ok {
        record.updated_at = Utc::now();
        let _ = fs::write(&job_file, serde_json::to_string_pretty(&record).unwrap_or_default());
        jobs_map.write().await.insert(job_id.clone(), record.clone());
        let _ = event_tx.send(EvolverEvent::JobCompleted {
            job_id: job_id.clone(),
            task: task.clone(),
            status: record.status.clone(),
            summary: "Worktree release compilation failed".to_string(),
        });
        cleanup_worktree(&repo_root, &worktree_dir, Some(&branch));
        return Ok(());
    }

    let binary_path = worktree_dir.join("target").join("release").join("luna");
    if !binary_path.exists() {
        let reason = format!(
            "Release binary missing at {} after a successful `cargo build --release` in the worktree. \
             Refusing to stage an update whose artifact does not exist.",
            binary_path.display()
        );
        record.status = JobStatus::FailedTest { error: reason.clone() };
        record.current_tool = None;
        record.updated_at = Utc::now();
        record.logs.push(reason.clone());
        let _ = fs::write(&job_file, serde_json::to_string_pretty(&record).unwrap_or_default());
        jobs_map.write().await.insert(job_id.clone(), record.clone());
        let _ = event_tx.send(EvolverEvent::JobCompleted {
            job_id: job_id.clone(),
            task: task.clone(),
            status: record.status.clone(),
            summary: reason,
        });
        cleanup_worktree(&repo_root, &worktree_dir, Some(&branch));
        return Ok(());
    }
    let pending = PendingUpdate {
        job_id: record.id.clone(),
        task: record.task.clone(),
        branch: record.branch.clone(),
        worktree_dir: record.worktree_dir.clone(),
        binary_path: binary_path.to_string_lossy().to_string(),
        staged_at: Utc::now(),
    };
    if let Ok(pending_json) = serde_json::to_string_pretty(&pending) {
        let _ = fs::write(jobs_dir.join("pending_update.json"), pending_json);
    }

    record.current_tool = None;
    record.status = JobStatus::ScheduledUpdate {
        binary_path: binary_path.to_string_lossy().to_string(),
    };
    record.updated_at = Utc::now();
    record.logs.push(format!(
        "Worktree binary built at {}. Pending update staged for next startup.",
        binary_path.display()
    ));

    let _ = fs::write(&job_file, serde_json::to_string_pretty(&record).unwrap_or_default());
    jobs_map.write().await.insert(job_id.clone(), record.clone());

    let _ = event_tx.send(EvolverEvent::JobCompleted {
        job_id: job_id.clone(),
        task: task.clone(),
        status: record.status.clone(),
        summary: "Dual-gate passed (`cargo test` clean, release binary built). Pending update staged.".to_string(),
    });

    Ok(())
}

/// Applies a pending update: merges git branch, swaps binary, cleans worktree and branch.
pub fn apply_update_at_startup(repo_root: &Path, jobs_dir: &Path) -> Result<Option<String>, HarnessError> {
    let repo_root_buf = discover_harness_repo_root(repo_root.to_path_buf());
    let repo_root = repo_root_buf.as_path();
    let pending_file = jobs_dir.join("pending_update.json");
    if !pending_file.exists() {
        return Ok(None);
    }

    let data = fs::read_to_string(&pending_file)
        .map_err(|e| HarnessError::Io(format!("Failed to read pending update file: {}", e)))?;
    let pending: PendingUpdate = serde_json::from_str(&data)
        .map_err(|e| HarnessError::EvolverError(format!("Failed to parse pending update: {}", e)))?;

    info!("Applying pending update for job {}", pending.job_id);

    // 1. Validate the artifact before touching main, so a stale pending record
    // can never merge a branch whose binary is already gone.
    let target_binary = repo_root.join("target/release/luna");
    let src_binary = PathBuf::from(&pending.binary_path);
    if !src_binary.exists() {
        let _ = fs::remove_file(&pending_file);
        return Err(HarnessError::EvolverError(format!(
            "Pending update references a missing worktree binary at {}; discarding it without merging.",
            src_binary.display()
        )));
    }

    // 2. Merge branch into main
    let merge_out = Command::new("git")
        .args([
            "-C",
            &repo_root.to_string_lossy(),
            "merge",
            "--no-edit",
            &pending.branch,
        ])
        .output()
        .map_err(|e| HarnessError::Io(format!("Failed to execute git merge: {}", e)))?;

    if !merge_out.status.success() {
        let err = String::from_utf8_lossy(&merge_out.stderr).to_string();
        let already = err.contains("Already up to date");
        return Err(HarnessError::EvolverError(format!(
            "Failed to merge branch {} into main{}: {}",
            pending.branch,
            if already { " (branch carries no commits)" } else { "" },
            err
        )));
    }

    // 3. Replace release binary
    fs::copy(&src_binary, &target_binary).map_err(|e| {
        HarnessError::Io(format!(
            "Failed to install {} -> {}: {}",
            src_binary.display(),
            target_binary.display(),
            e
        ))
    })?;

    // 4. Clean up worktree and branch
    cleanup_worktree(repo_root, Path::new(&pending.worktree_dir), Some(&pending.branch));

    // 5. Delete pending update marker
    let _ = fs::remove_file(&pending_file);

    Ok(Some(format!(
        "Successfully applied improvement from job '{}': {}",
        pending.job_id, pending.task
    )))
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

    async fn propose_patch(&self, _desc: &str, _diff: &str) -> Result<String, HarnessError> {
        Err(HarnessError::EvolverError("propose_patch is deprecated; use delegate_improvement".to_string()))
    }

    async fn test_patch_sandbox(&self, _id: &str) -> Result<bool, HarnessError> {
        Err(HarnessError::EvolverError("test_patch_sandbox is deprecated; use delegate_improvement".to_string()))
    }

    async fn apply_patch(&self, _id: &str) -> Result<(), HarnessError> {
        Err(HarnessError::EvolverError("apply_patch is deprecated; use delegate_improvement".to_string()))
    }

    async fn preview_patch(&self, _patch_id: &str) -> Option<ToolPreview> {
        None
    }
}

// Minimal no-op engines for the ephemeral engineer subagent
#[derive(Clone)]
struct NoopMemory;

#[async_trait]
impl MemoryEngine for NoopMemory {
    async fn retrieve(&self, _query: &str, _limit: usize) -> Result<Vec<MemoryRecord>, HarnessError> {
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

#[derive(Clone)]
struct NoopNotes;

#[async_trait]
impl NotesEngine for NoopNotes {
    async fn save_note(&self, _title: &str, _content: &str, _tags: &[String]) -> Result<Note, HarnessError> {
        Err(HarnessError::NotesError("Notes not enabled in engineer subagent".to_string()))
    }
    async fn search_notes(&self, _query: &str, _limit: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(vec![])
    }
    async fn list_notes(&self, _limit: usize, _offset: usize) -> Result<Vec<Note>, HarnessError> {
        Ok(vec![])
    }
    async fn read_note(&self, _id: &str) -> Result<Note, HarnessError> {
        Err(HarnessError::NotesError("Notes not enabled in engineer subagent".to_string()))
    }
    async fn update_note(&self, _id: &str, _t: Option<&str>, _c: Option<&str>, _tags: Option<&[String]>) -> Result<Note, HarnessError> {
        Err(HarnessError::NotesError("Notes not enabled in engineer subagent".to_string()))
    }
    async fn delete_note(&self, _id: &str) -> Result<bool, HarnessError> {
        Ok(false)
    }
}

#[derive(Clone)]
struct NoopEvolver;

#[async_trait]
impl EvolverEngine for NoopEvolver {
    async fn inspect_source(&self, _path: &str) -> Result<String, HarnessError> {
        Err(HarnessError::EvolverError("Recursion prohibited in engineer subagent".to_string()))
    }
    async fn propose_patch(&self, _desc: &str, _diff: &str) -> Result<String, HarnessError> {
        Err(HarnessError::EvolverError("Recursion prohibited in engineer subagent".to_string()))
    }
    async fn test_patch_sandbox(&self, _id: &str) -> Result<bool, HarnessError> {
        Err(HarnessError::EvolverError("Recursion prohibited in engineer subagent".to_string()))
    }
    async fn apply_patch(&self, _id: &str) -> Result<(), HarnessError> {
        Err(HarnessError::EvolverError("Recursion prohibited in engineer subagent".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discover_harness_repo_root_finds_actual_repo() {
        let discovered = discover_harness_repo_root(PathBuf::from("/tmp"));
        assert!(
            discovered.join("crates/harness-core").exists(),
            "must locate repo root with crates/harness-core, got: {:?}",
            discovered
        );
        assert!(
            discovered.join(".git").exists(),
            "must locate repo root with .git, got: {:?}",
            discovered
        );
    }

    #[test]
    fn discover_harness_repo_root_respects_env_var() {
        let temp = tempfile::tempdir().unwrap();
        let repo_path = temp.path().join("fake_repo");
        fs::create_dir_all(repo_path.join(".git")).unwrap();
        fs::create_dir_all(repo_path.join("crates/harness-core")).unwrap();

        std::env::set_var("LUNA_HARNESS_REPO", repo_path.to_string_lossy().to_string());
        let discovered = discover_harness_repo_root(PathBuf::from("/tmp"));
        std::env::remove_var("LUNA_HARNESS_REPO");

        assert_eq!(discovered, repo_path);
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git binary must be runnable for these tests");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q"]);
        git(dir, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git(dir, &["config", "user.email", "tests@example.invalid"]);
        git(dir, &["config", "user.name", "Engine Tests"]);
    }

    /// Regression: engineer builds must never write into the shared repo target/.
    #[test]
    fn worktree_target_dir_is_isolated_from_shared_repo_target() {
        let repo = PathBuf::from("/home/janelle/Downloads/rust-agent-harness");
        let worktree = repo.join(".worktrees/eng_job_deadbeef");
        let isolated = worktree_target_dir(&worktree);

        assert_eq!(isolated, worktree.join("target"));
        assert!(isolated.starts_with(&worktree));
        assert_ne!(isolated, repo.join("target"));
        assert!(!isolated.starts_with(repo.join("target")));
    }

    /// Regression: deleting a worktree purges its artifacts and leaves the
    /// main repo's target/ intact.
    #[test]
    fn cleanup_worktree_purges_own_target_and_leaves_shared_target_alone() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        let shared = repo.join("target/debug");
        fs::create_dir_all(&shared).unwrap();
        fs::write(shared.join("luna"), b"shared").unwrap();

        let worktree = repo.join(".worktrees/eng_job_x");
        let own_target = worktree_target_dir(&worktree);
        fs::create_dir_all(&own_target).unwrap();
        fs::write(own_target.join("luna"), b"worktree").unwrap();

        cleanup_worktree(&repo, &worktree, Some("eng/job_x"));

        assert!(!worktree.exists(), "worktree dir must be removed");
        assert!(!own_target.exists(), "worktree target/ must be purged");
        assert!(
            shared.join("luna").exists(),
            "the shared repo target/ must never be touched by worktree cleanup"
        );
    }

    /// Regression: a job that changed nothing must show 0 commits ahead, which is
    /// exactly what the false-success jobs reported as "dual-gate passed".
    #[test]
    fn commit_gate_distinguishes_noop_jobs_from_real_work() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        init_repo(&repo);
        fs::write(repo.join("file.txt"), "v1").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-m", "init"]);
        let base = head_commit(&repo).unwrap();

        // Clean tree: no commit created, nothing ahead of base.
        assert!(
            !commit_worktree_changes(&repo, "eng(job_x): noop").unwrap(),
            "a clean worktree must not produce a commit"
        );
        assert_eq!(
            count_commits_ahead(&repo, &base).unwrap(),
            0,
            "a no-op job must be detected as 0 commits ahead"
        );

        // Real work: one commit, one ahead of base.
        fs::write(repo.join("file.txt"), "v2").unwrap();
        assert!(
            commit_worktree_changes(&repo, "eng(job_x): fix").unwrap(),
            "a dirty worktree must be committed"
        );
        assert_eq!(count_commits_ahead(&repo, &base).unwrap(), 1);
    }
}
