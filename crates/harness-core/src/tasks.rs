use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;
use tracing::info;

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

/// A background job that has stopped, pushed to the host instead of waiting to be polled.
///
/// The tail is read once, at completion, so the host can hand the model the end of the log
/// without another round trip. `status` is the same string `process poll` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobCompletion {
    pub id: String,
    pub command: String,
    pub status: String,
    pub elapsed_secs: u64,
    pub log_path: String,
    pub output_tail: String,
}

/// Called exactly once per background job, from the task that reaped it.
pub type JobCompletionNotifier = Arc<dyn Fn(JobCompletion) + Send + Sync>;

struct InternalJob {
    id: String,
    command: String,
    workdir: PathBuf,
    status: Arc<Mutex<String>>,
    started_at: Instant,
    log_path: PathBuf,
    /// Shared with the spawned task: whoever finishes first (a kill or the process itself) takes
    /// the sender, so a job that already stopped can never be reported as killed afterwards.
    kill_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
}

#[derive(Clone)]
pub struct BackgroundTaskQueue {
    jobs_dir: PathBuf,
    jobs: Arc<Mutex<HashMap<String, InternalJob>>>,
    /// Optional sink for completion events. `None` keeps the queue poll-only, which is what
    /// subagents and tests get.
    completion_notifier: Arc<Mutex<Option<JobCompletionNotifier>>>,
    /// `tools.network`: a backgrounded command is still a command the agent started, so it gets
    /// the same boundary rather than escaping it by adding `&`.
    network: crate::tools::netguard::NetworkPolicy,
}

impl BackgroundTaskQueue {
    pub fn new(jobs_dir: PathBuf) -> Self {
        if let Err(error) = std::fs::create_dir_all(&jobs_dir) {
            // Constructor cannot fail; the spawn path re-checks and reports with full context.
            tracing::warn!("cannot create jobs dir '{}': {error}", jobs_dir.display());
        }
        Self {
            jobs_dir,
            jobs: Arc::new(Mutex::new(HashMap::new())),
            completion_notifier: Arc::new(Mutex::new(None)),
            network: crate::tools::netguard::NetworkPolicy::Allow,
        }
    }

    /// Install the callback that wakes the host when a job stops.
    ///
    /// Set after construction because the host owns the channel the callback sends on; the queue
    /// itself must stay usable (and poll-only) when no one is listening.
    pub fn set_completion_notifier(&self, notifier: JobCompletionNotifier) {
        *self
            .completion_notifier
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(notifier);
    }

    /// Apply `tools.network` to every job this queue spawns.
    pub fn with_network_policy(mut self, policy: crate::tools::netguard::NetworkPolicy) -> Self {
        self.network = policy;
        self
    }

    pub fn spawn(&self, command: &str, workdir: &Path) -> Result<String, HarnessError> {
        let num = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        let id = format!("job_{:06x}", num);
        let log_path = self.jobs_dir.join(format!("{}.log", id));

        if let Err(error) = std::fs::create_dir_all(&self.jobs_dir) {
            return Err(HarnessError::Io(format!(
                "Failed to create jobs directory '{}': {}",
                self.jobs_dir.display(),
                error
            )));
        }
        let log_file = File::create(&log_path).map_err(|e| {
            HarnessError::Io(format!(
                "Failed to create background log file '{}': {}",
                log_path.display(),
                e
            ))
        })?;

        let (kill_tx, kill_rx) = oneshot::channel();
        let status = Arc::new(Mutex::new("running".to_string()));
        let started_at = Instant::now();
        let kill_tx = Arc::new(Mutex::new(Some(kill_tx)));

        // Register before spawning: a command that exits instantly must be findable by `process`
        // the moment its completion event reaches the host, not a scheduler tick later.
        let internal = InternalJob {
            id: id.clone(),
            command: command.to_string(),
            workdir: workdir.to_path_buf(),
            status: status.clone(),
            started_at,
            log_path: log_path.clone(),
            kill_tx: kill_tx.clone(),
        };
        self.jobs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), internal);

        let cmd_str = command.to_string();
        let notify_command = command.to_string();
        let workdir_buf = workdir.to_path_buf();
        let network = self.network;
        let status_clone = status;
        let notifier = self.completion_notifier.clone();
        let notify_id = id.clone();
        let notify_log = log_path.clone();
        let kill_tx_task = kill_tx;

        tokio::spawn(async move {
            // Every exit path — a refused spawn, a kill, a natural exit — lands here exactly once.
            // Setting the status and announcing the completion together is what keeps `process
            // poll` and the host's notification from disagreeing about whether a job is done.
            let finish = move |new_status: String| {
                {
                    let mut st = status_clone.lock().unwrap_or_else(|e| e.into_inner());
                    *st = new_status.clone();
                }
                // The job has stopped: a later `kill` is a no-op rather than a lie.
                let _ = kill_tx_task
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take();
                let callback = notifier.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some(callback) = callback {
                    callback(JobCompletion {
                        id: notify_id.clone(),
                        command: notify_command.clone(),
                        status: new_status,
                        elapsed_secs: started_at.elapsed().as_secs(),
                        log_path: notify_log.to_string_lossy().to_string(),
                        output_tail: read_tail(&notify_log, 40),
                    });
                }
            };

            let std_file = log_file;
            let stdout = std_file
                .try_clone()
                .map(std::process::Stdio::from)
                .unwrap_or_else(|_| std::process::Stdio::null());
            let mut cmd = crate::tools::shell::shell_command(&cmd_str);
            cmd.current_dir(&workdir_buf)
                .stdout(stdout)
                .stderr(std_file);

            if let Err(error) = crate::tools::netguard::apply(&mut cmd, network) {
                finish(format!(
                    "failed to spawn: cannot enforce tools.network = {}: {error}",
                    network.as_str()
                ));
                return;
            }

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    finish(format!("failed to spawn: {}", e));
                    return;
                }
            };

            tokio::select! {
                _ = kill_rx => {
                    #[cfg(windows)]
                    crate::tools::shell::kill_process_group(child.id());
                    let _ = child.kill().await;
                    finish("killed".to_string());
                }
                res = child.wait() => {
                    let status = match res {
                        Ok(status) => {
                            if status.success() {
                                "completed (exit 0)".to_string()
                            } else {
                                format!("failed (exit {})", status.code().unwrap_or(-1))
                            }
                        }
                        Err(e) => format!("error: {}", e),
                    };
                    finish(status);
                }
            }
        });

        info!(id = %id, command = %command, "Spawned background task");
        Ok(id)
    }

    pub fn list(&self) -> Vec<JobSummary> {
        let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        let mut list: Vec<JobSummary> = jobs
            .values()
            .map(|j| JobSummary {
                id: j.id.clone(),
                command: j.command.clone(),
                workdir: j.workdir.to_string_lossy().to_string(),
                status: j.status.lock().unwrap_or_else(|e| e.into_inner()).clone(),
                elapsed_secs: j.started_at.elapsed().as_secs(),
                log_path: j.log_path.to_string_lossy().to_string(),
            })
            .collect();

        list.sort_by(|a, b| b.id.cmp(&a.id));
        list
    }

    pub fn poll(&self, id: &str, tail_lines: usize) -> Result<JobPollResult, HarnessError> {
        let (command, status, started_at, log_path) = {
            let jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
            let job = jobs
                .get(id)
                .ok_or_else(|| HarnessError::ToolExecutionFailed {
                    tool: "process".to_string(),
                    reason: format!("Background job '{}' not found", id),
                })?;

            let status = job.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
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
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(job) = jobs.get_mut(id) {
            let sender = job.kill_tx.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(tx) = sender {
                let _ = tx.send(());
                *job.status.lock().unwrap_or_else(|e| e.into_inner()) = "killed".to_string();
                return Ok(true);
            }
        }
        Ok(false)
    }
}

fn read_tail(path: &Path, count: usize) -> String {
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let Ok(len) = file.metadata().map(|meta| meta.len()) else {
        return String::new();
    };

    // Read backwards in chunks until the requested number of lines is behind us. A completion
    // event is pushed automatically, so this must stay proportional to the tail, not to a build
    // log that may be hundreds of megabytes.
    const CHUNK: u64 = 16 * 1024;
    let mut pos = len;
    let mut buf: Vec<u8> = Vec::new();
    let mut newlines = 0usize;
    while pos > 0 && newlines <= count {
        let read_size = CHUNK.min(pos);
        pos -= read_size;
        if file.seek(SeekFrom::Start(pos)).is_err() {
            break;
        }
        let mut chunk = vec![0u8; read_size as usize];
        if file.read_exact(&mut chunk).is_err() {
            break;
        }
        newlines += chunk.iter().filter(|&&byte| byte == b'\n').count();
        chunk.extend_from_slice(&buf);
        buf = chunk;
    }

    let text = String::from_utf8_lossy(&buf);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(count);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn test_queue() -> (tempfile::TempDir, BackgroundTaskQueue) {
        let dir = tempfile::tempdir().expect("temp dir");
        let queue = BackgroundTaskQueue::new(dir.path().join("jobs"));
        (dir, queue)
    }

    #[tokio::test]
    async fn a_finished_job_announces_its_status_and_tail() {
        let (_dir, queue) = test_queue();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        queue.set_completion_notifier(Arc::new(move |completion| {
            let _ = tx.send(completion);
        }));

        let id = queue
            .spawn("echo luna-job-done", Path::new("."))
            .expect("spawn");
        let completion = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("completion event")
            .expect("notifier fired");

        assert_eq!(completion.id, id);
        assert_eq!(completion.status, "completed (exit 0)");
        assert!(
            completion.output_tail.contains("luna-job-done"),
            "tail: {:?}",
            completion.output_tail
        );
        // The event and `process poll` describe the same finished job.
        let polled = queue.poll(&id, 40).expect("poll");
        assert_eq!(polled.status, "completed (exit 0)");
        assert!(polled.output_tail.contains("luna-job-done"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_killed_job_announces_once_and_cannot_be_killed_again() {
        let (_dir, queue) = test_queue();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        queue.set_completion_notifier(Arc::new(move |completion| {
            let _ = tx.send(completion);
        }));

        let id = queue.spawn("sleep 30", Path::new(".")).expect("spawn");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(queue.kill(&id).expect("kill"));

        let completion = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("completion event")
            .expect("notifier fired");
        assert_eq!(completion.id, id);
        assert_eq!(completion.status, "killed");
        assert!(
            rx.try_recv().is_err(),
            "a completion must fire exactly once"
        );
        assert!(
            !queue.kill(&id).expect("second kill"),
            "a stopped job is no longer killable"
        );
    }

    #[test]
    fn the_tail_is_the_last_lines_of_a_large_log() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("big.log");
        let body: String = (0..5_000).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, body).expect("write log");
        assert_eq!(read_tail(&path, 3), "line 4997\nline 4998\nline 4999");

        // A file without a trailing newline still yields exactly the requested lines.
        std::fs::write(&path, "a\nb\nc").expect("write log");
        assert_eq!(read_tail(&path, 2), "b\nc");
    }

    #[tokio::test]
    async fn a_queue_without_a_notifier_still_completes() {
        let (_dir, queue) = test_queue();
        let id = queue
            .spawn("echo no-listener", Path::new("."))
            .expect("spawn");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let polled = queue.poll(&id, 40).expect("poll");
            if polled.status.starts_with("completed") {
                assert!(polled.output_tail.contains("no-listener"));
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "job never completed: {}",
                polled.status
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}
