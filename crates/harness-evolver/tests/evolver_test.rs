use std::path::PathBuf;
use std::time::Duration;
use tempfile::tempdir;

use harness_evolver::engine::{CodebaseEvolver, JobRecord, JobStatus, PendingUpdate};
use harness_evolver::pty::run_in_pty;

#[test]
fn test_pty_execution_basic() {
    let temp = tempdir().expect("tempdir");
    let (code, output) = run_in_pty(
        &PathBuf::from("/bin/sh"),
        &["-c", "echo 'hello from pty'"],
        temp.path(),
        None,
        Duration::from_secs(5),
    )
    .expect("pty run");

    assert_eq!(code, 0);
    assert!(output.contains("hello from pty"));
}

#[test]
fn test_pty_execution_with_input() {
    let temp = tempdir().expect("tempdir");
    let (code, output) = run_in_pty(
        &PathBuf::from("/bin/cat"),
        &[],
        temp.path(),
        Some("feed input to cat\n\x04"),
        Duration::from_secs(5),
    )
    .expect("pty run");

    assert_eq!(code, 0);
    assert!(output.contains("feed input to cat"));
}

#[tokio::test]
async fn test_job_lifecycle_and_pending_update() {
    let repo_temp = tempdir().expect("repo tempdir");
    let jobs_temp = tempdir().expect("jobs tempdir");

    let evolver = CodebaseEvolver::new(repo_temp.path().to_path_buf(), jobs_temp.path().to_path_buf());

    // Initially no pending update
    assert!(evolver.get_pending_update().is_none());

    // Create a mock job record
    let job_id = "job_test123";
    let worktree_dir = repo_temp.path().join(".worktrees/eng_job_test123");
    std::fs::create_dir_all(&worktree_dir).unwrap();

    let job = JobRecord {
        id: job_id.to_string(),
        task: "Fix mock issue".to_string(),
        evidence: Some("Mock error output".to_string()),
        branch: "eng/job_test123".to_string(),
        worktree_dir: worktree_dir.to_string_lossy().to_string(),
        status: JobStatus::ReadyForReenactment {
            binary_path: "/bin/true".to_string(),
        },
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        logs: vec!["Job initialized".to_string()],
    };

    let job_file = jobs_temp.path().join(format!("{}.json", job_id));
    std::fs::write(&job_file, serde_json::to_string_pretty(&job).unwrap()).unwrap();

    let checked = evolver.check_job(job_id).await.expect("check job");
    assert_eq!(checked.id, job_id);
    assert_eq!(checked.task, "Fix mock issue");

    // Stage update with verify_and_stage
    let stage_res = evolver.verify_and_stage(job_id, None, Some("FORBIDDEN_ERROR")).await;
    assert!(stage_res.is_ok());

    let pending = evolver.get_pending_update().expect("pending update exists");
    assert_eq!(pending.job_id, job_id);
    assert_eq!(pending.branch, "eng/job_test123");
}
