use std::path::PathBuf;
use std::time::Duration;
use tempfile::tempdir;

use harness_evolver::engine::{CodebaseEvolver, JobRecord, JobStatus};
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

    let evolver = CodebaseEvolver::new(
        repo_temp.path().to_path_buf(),
        jobs_temp.path().to_path_buf(),
    );

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
        current_tool: None,
        modified_files: Vec::new(),
        summary: None,
        base_commit: None,
        owner_pid: None,
    };

    let job_file = jobs_temp.path().join(format!("{}.json", job_id));
    std::fs::write(&job_file, serde_json::to_string_pretty(&job).unwrap()).unwrap();

    let checked = evolver.check_job(job_id).await.expect("check job");
    assert_eq!(checked.id, job_id);
    assert_eq!(checked.task, "Fix mock issue");

    // Stage update with verify_and_stage
    let stage_res = evolver
        .verify_and_stage(job_id, None, Some("FORBIDDEN_ERROR"))
        .await;
    assert!(stage_res.is_ok());

    let pending = evolver.get_pending_update().expect("pending update exists");
    assert_eq!(pending.job_id, job_id);
    assert_eq!(pending.branch, "eng/job_test123");
}

#[tokio::test]
async fn test_check_improvement_tool_native_progress_reading() {
    use harness_core::Tool;
    use harness_evolver::tools::{CheckImprovementTool, ListImprovementJobsTool};
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    let repo_temp = tempfile::tempdir().unwrap();
    let jobs_temp = tempfile::tempdir().unwrap();
    let evolver = Arc::new(CodebaseEvolver::new(
        repo_temp.path().to_path_buf(),
        jobs_temp.path().to_path_buf(),
    ));

    // Create a job with active progress, modified files, and logs
    let job_id = "job_progress_test";
    let job = JobRecord {
        id: job_id.to_string(),
        task: "Refactor parser".to_string(),
        evidence: None,
        branch: "eng/job_progress_test".to_string(),
        worktree_dir: "/tmp/fake_worktree".to_string(),
        status: JobStatus::Running,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
        logs: vec![
            "• Tool: read crates/harness-core/src/parser.rs".to_string(),
            "• Tool: patch crates/harness-core/src/parser.rs".to_string(),
            "  ✔ Output: Successfully patched".to_string(),
        ],
        current_tool: Some("cargo check --workspace".to_string()),
        modified_files: vec!["crates/harness-core/src/parser.rs".to_string()],
        summary: None,
        base_commit: None,
        owner_pid: Some(std::process::id()),
    };

    let job_file = jobs_temp.path().join(format!("{}.json", job_id));
    std::fs::write(&job_file, serde_json::to_string_pretty(&job).unwrap()).unwrap();

    let check_tool = CheckImprovementTool::new(evolver.clone());
    let list_tool = ListImprovementJobsTool::new(evolver.clone());
    let cancel = CancellationToken::new();

    // 1. Calling check_improvement_job with job_id returns rich native readable progress
    let res = check_tool
        .execute("call_1", &serde_json::json!({ "job_id": job_id }), &cancel)
        .await
        .unwrap();
    assert!(!res.is_error);
    assert!(res.content.contains("Job ID: job_progress_test"));
    assert!(res.content.contains("running"));
    assert!(res
        .content
        .contains("Current Activity: cargo check --workspace"));
    assert!(res
        .content
        .contains("Modified Files: crates/harness-core/src/parser.rs"));
    assert!(res.content.contains("Progress Timeline:"));

    // 2. Calling check_improvement_job without args defaults to latest job seamlessly
    let res_latest = check_tool
        .execute("call_2", &serde_json::json!({}), &cancel)
        .await
        .unwrap();
    assert!(!res_latest.is_error);
    assert!(res_latest.content.contains("Job ID: job_progress_test"));

    // 3. Calling list_improvement_jobs shows the job with its current activity
    let res_list = list_tool
        .execute("call_3", &serde_json::json!({}), &cancel)
        .await
        .unwrap();
    assert!(!res_list.is_error);
    assert!(res_list.content.contains("job_progress_test"));
    assert!(res_list
        .content
        .contains("Current activity: cargo check --workspace"));
}
