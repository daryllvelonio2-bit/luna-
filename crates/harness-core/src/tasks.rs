use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tracing::{info, warn};

use crate::error::HarnessError;

static JOB_COUNTER: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobSummary {
    pub id: String,
    pub command: String,
    pub workdir: String,
    pub status: String,
    pub elapsed_secs: u64,
    pub log_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobPollResult {
    pub id: String,
    pub command: String,
    pub status: String,
    pub elapsed_secs: u64,
    pub log_path: String,
    pub output_tail: String,
}

struct InternalJob {
    id: String,
    command: String,
    workdir: PathBuf,
    status: Arc<Mutex<String>>,
    started_at: Instant,
    log_path: PathBuf,
    kill_tx: Option<oneshot::Sender<()>>,
}

#[derive(Clone)]
pub struct BackgroundTaskQueue {
    jobs_dir: PathBuf,
    jobs: Arc<Mutex<HashMap<String, InternalJob>>>,
}

impl BackgroundTaskQueue {
    pub fn new(jobs_dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&jobs_dir);
        Self {
            jobs_dir,
            jobs: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn spawn(&self, command: &str, workdir: &Path) -> Result<String, HarnessError> {
        let num = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = format!("job_{:06x}", num);
        let log_path = self.jobs_dir.join(format!("{}.log", id));

        let _ = std::fs::create_dir_all(&self.jobs_dir);
        let log_file = File::create(&log_path).map_err(|e| {
            HarnessError::Io(format!("Failed to create background log file '{}': {}", log_path.display(), e))
        })?;

        let (kill_tx, kill_rx) = oneshot::channel();
        let status = Arc::new(Mutex::new("running".to_string()));

        let cmd_str = command.to_string();
        let workdir_buf = workdir.to_path_buf();
        let status_clone = status.clone();

        tokio::spawn(async move {
            let std_file = log_file;
            let mut cmd = tokio::process::Command::new("sh");
            cmd.arg("-c")
                .arg(&cmd_str)
                .current_dir(&workdir_buf)
                .stdout(std_file.try_clone().unwrap_or_else(|_| File::create("/dev/null").unwrap()))
                .stderr(std_file);

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    let mut st = status_clone.lock().unwrap();
                    *st = format!("failed to spawn: {}", e);
                    return;
                }
            };

            tokio::select! {
                _ = kill_rx => {
                    let _ = child.kill().await;
                    let mut st = status_clone.lock().unwrap();
                    *st = "killed".to_string();
                }
                res = child.wait() => {
                    let mut st = status_clone.lock().unwrap();
                    match res {
                        Ok(status) => {
                            if status.success() {
                                *st = "completed (exit 0)".to_string();
                            } else {
                                *st = format!("failed (exit {})", status.code().unwrap_or(-1));
                            }
                        }
                        Err(e) => {
                            *st = format!("error: {}", e);
                        }
                    }
                }
            }
        });

        info!(id = %id, command = %command, "Spawned background task");

        let internal = InternalJob {
            id: id.clone(),
            command: command.to_string(),
            workdir: workdir.to_path_buf(),
            status,
            started_at: Instant::now(),
            log_path,
            kill_tx: Some(kill_tx),
        };

        self.jobs.lock().unwrap().insert(id.clone(), internal);
        Ok(id)
    }

    pub fn list(&self) -> Vec<JobSummary> {
        let jobs = self.jobs.lock().unwrap();
        let mut list: Vec<JobSummary> = jobs
            .values()
            .map(|j| JobSummary {
                id: j.id.clone(),
                command: j.command.clone(),
                workdir: j.workdir.to_string_lossy().to_string(),
                status: j.status.lock().unwrap().clone(),
                elapsed_secs: j.started_at.elapsed().as_secs(),
                log_path: j.log_path.to_string_lossy().to_string(),
            })
            .collect();

        list.sort_by(|a, b| b.id.cmp(&a.id));
        list
    }

    pub fn poll(&self, id: &str, tail_lines: usize) -> Result<JobPollResult, HarnessError> {
        let (command, status, started_at, log_path) = {
            let jobs = self.jobs.lock().unwrap();
            let job = jobs
                .get(id)
                .ok_or_else(|| HarnessError::ToolExecutionFailed {
                    tool: "process".to_string(),
                    reason: format!("Background job '{}' not found", id),
                })?;

            let status = job.status.lock().unwrap().clone();
            (
                job.command.clone(),
                status,
                job.started_at,
                job.log_path.clone(),
            )
        };

        let output_tail = read_tail(&log_path, tail_lines.max(1));

        Ok(JobPollResult {
            id: id.to_string(),
            command,
            status,
            elapsed_secs: started_at.elapsed().as_secs(),
            log_path: log_path.to_string_lossy().to_string(),
            output_tail,
        })
    }

    pub fn kill(&self, id: &str) -> Result<bool, HarnessError> {
        let mut jobs = self.jobs.lock().unwrap();
        if let Some(job) = jobs.get_mut(id) {
            if let Some(tx) = job.kill_tx.take() {
                let _ = tx.send(());
                *job.status.lock().unwrap() = "killed".to_string();
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn read_tail(path: &Path, count: usize) -> String {
    let Ok(file) = File::open(path) else {
        return String::new();
    };
    let reader = BufReader::new(file);
    let mut lines = Vec::new();
    for line in reader.lines().filter_map(Result::ok) {
        lines.push(line);
        if lines.len() > count * 2 {
            lines.drain(0..count);
        }
    }
    let start = lines.len().saturating_sub(count);
    lines[start..].join("\n")
}
