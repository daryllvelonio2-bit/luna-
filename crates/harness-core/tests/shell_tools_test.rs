use serde_json::json;
use std::sync::Arc;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

use harness_core::tools::{PatchTool, ReadCache, TerminalTool, TodoScope};
use harness_core::{Config, DefaultToolRegistry, Tool, ToolRegistry};

#[tokio::test]
async fn test_terminal_exit_code_and_timeout() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    let term = TerminalTool::new(root.clone(), 5, 50_000);

    // "false" command exits with 1: must be ToolResult with is_error = true, not Err
    let res = term
        .execute("call_term_1", &json!({ "command": "false" }), &cancel)
        .await
        .unwrap();
    assert!(res.is_error);
    assert!(res.content.contains("exit code: 1"));

    // "echo hi" command exits 0
    let success_res = term
        .execute(
            "call_term_2",
            &json!({ "command": "echo 'luna rocks'" }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(!success_res.is_error);
    assert!(success_res.content.contains("luna rocks"));

    // A grep with 0 matches exits 1, but is a benign query result, not an execution error
    let grep_res = term
        .execute(
            "call_term_grep",
            &json!({ "command": "grep 'nonexistent_pattern_12345' /dev/null" }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        !grep_res.is_error,
        "grep exit 1 should not be marked as is_error"
    );
    assert!(grep_res.content.contains("no matches found"));

    // Timeout test
    let term_short = TerminalTool::new(root.clone(), 1, 50_000);
    let timeout_res = term_short
        .execute("call_term_3", &json!({ "command": "sleep 10" }), &cancel)
        .await
        .unwrap();
    assert!(timeout_res.is_error);
    assert!(timeout_res.content.contains("timed out"));
    // The model's next move after a timeout is to try again, so the message has to say how: the
    // luna session that hit this twice repeated a 120s cargo test and timed out again, because
    // nothing told it the budget was configurable.
    assert!(
        timeout_res.content.contains("timeout_s"),
        "the timeout message must name the knob that raises it: {}",
        timeout_res.content
    );
}

#[cfg(unix)]
#[tokio::test]
async fn terminal_forwards_output_while_the_command_runs() {
    let dir = tempdir().unwrap();
    let term = TerminalTool::new(dir.path().to_path_buf(), 5, 50_000);
    let cancel = CancellationToken::new();

    let streamed = Arc::new(std::sync::Mutex::new(String::new()));
    let sink = streamed.clone();
    let progress: harness_core::ToolProgress =
        Arc::new(move |chunk: &str| sink.lock().unwrap().push_str(chunk));

    // Both pipes are forwarded, because a compiler writes half its diagnosis to stderr.
    let res = term
        .execute_with_progress(
            "call_stream",
            &json!({ "command": "printf 'alpha\\nbeta\\n'; printf 'gamma\\n' >&2" }),
            &cancel,
            Some(&progress),
        )
        .await
        .unwrap();

    assert!(!res.is_error, "{}", res.content);
    for line in ["alpha", "beta", "gamma"] {
        assert!(
            res.content.contains(line),
            "the result still carries the whole output ({line} missing): {}",
            res.content
        );
    }
    let streamed = streamed.lock().unwrap().clone();
    for line in ["alpha", "beta", "gamma"] {
        assert!(
            streamed.contains(line),
            "the live stream saw {line}: {streamed:?}"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn terminal_flushes_a_line_before_a_slow_command_finishes() {
    let dir = tempdir().unwrap();
    let term = TerminalTool::new(dir.path().to_path_buf(), 10, 50_000);
    let cancel = CancellationToken::new();

    let started = std::time::Instant::now();
    let first_at = Arc::new(std::sync::Mutex::new(None));
    let sink = first_at.clone();
    let progress: harness_core::ToolProgress = Arc::new(move |chunk: &str| {
        if chunk.contains("early") {
            let mut slot = sink.lock().unwrap();
            if slot.is_none() {
                *slot = Some(std::time::Instant::now());
            }
        }
    });

    // The first line lands immediately, then the command stays quiet for longer than the flush
    // interval. A coalescing reader that only flushed on the next read would hide "early" until
    // the sleep ended, which is exactly the regression this guards.
    let res = term
        .execute_with_progress(
            "call_flush",
            &json!({ "command": "printf 'early\\n'; sleep 1.2; printf 'late\\n'" }),
            &cancel,
            Some(&progress),
        )
        .await
        .unwrap();

    assert!(!res.is_error, "{}", res.content);
    let first = first_at
        .lock()
        .unwrap()
        .expect("the early line reached the observer");
    assert!(
        first.duration_since(started) < std::time::Duration::from_millis(900),
        "the early line waited for the command to finish: {:?}",
        first.duration_since(started)
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_timed_out_command_leaves_no_children_behind() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();
    let term = TerminalTool::new(root.clone(), 1, 50_000);

    // The shell dies on the timeout; its own child does not. That child is what makes a timeout
    // *cascade*: a killed `cargo test` leaves cargo and rustc compiling in the background, holding
    // the target-directory lock, so the retry blocks on that lock and times out too. In the luna
    // session this test is drawn from, two identical `cargo test --package harness-cli` calls were
    // killed at the 120s mark and the third — same command, larger budget — finished in 33s,
    // because the first two had kept building after their shells were gone.
    //
    // The trailing `; echo` is what makes the shell fork instead of exec-ing the inner one in
    // place: without a second command there is no grandchild, and the bug hides.
    let marker = root.join("grandchild_survived");
    let res = term
        .execute(
            "call_orphan",
            &json!({ "command": "sh -c 'sleep 3; touch grandchild_survived' ; echo done" }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(res.is_error, "the command must time out: {}", res.content);

    // Give the grandchild more than its 3s of life to prove itself either way.
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    assert!(
        !tokio::fs::try_exists(&marker).await.unwrap(),
        "the timed-out command's child kept running: {} appeared",
        marker.display()
    );
}

#[test]
fn test_rewrite_sudo_command() {
    use harness_core::tools::shell::{contains_sudo, rewrite_sudo_command};

    let (cmd, count) = rewrite_sudo_command("sudo apt update");
    assert_eq!(cmd, "sudo -S -p '' apt update");
    assert_eq!(count, 1);
    assert!(contains_sudo("sudo apt update"));

    let (cmd, count) = rewrite_sudo_command("/usr/bin/sudo make install");
    assert_eq!(cmd, "/usr/bin/sudo -S -p '' make install");
    assert_eq!(count, 1);

    let (cmd, count) = rewrite_sudo_command("echo 'sudo apt update'");
    assert_eq!(cmd, "echo 'sudo apt update'");
    assert_eq!(count, 0);
    assert!(!contains_sudo("echo 'sudo apt update'"));

    let (cmd, count) = rewrite_sudo_command("echo \"sudo\"");
    assert_eq!(cmd, "echo \"sudo\"");
    assert_eq!(count, 0);

    let (cmd, count) = rewrite_sudo_command("VAR=1 sudo apt update && sudo apt install -y curl");
    assert_eq!(
        cmd,
        "VAR=1 sudo -S -p '' apt update && sudo -S -p '' apt install -y curl"
    );
    assert_eq!(count, 2);
}

struct MockSudoPrompter {
    calls: std::sync::atomic::AtomicUsize,
    password: Option<String>,
}

#[async_trait::async_trait]
impl harness_core::tools::SudoPrompter for MockSudoPrompter {
    async fn prompt_sudo_password(
        &self,
        _command: &str,
    ) -> Result<Option<String>, harness_core::HarnessError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(self.password.clone())
    }
}

#[cfg(unix)]
#[tokio::test]
async fn test_terminal_sudo_password_prompts_and_caches_in_session() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    // Create a mock `sudo` binary inside test workspace that reads password from stdin
    let mock_sudo = root.join("sudo");
    tokio::fs::write(
        &mock_sudo,
        "#!/bin/sh\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = \"-S\" ]; then shift; continue; fi\n  if [ \"$1\" = \"-p\" ]; then shift 2; continue; fi\n  break\ndone\nread -r input_pwd\nif [ \"$input_pwd\" = \"correct_pass_123\" ]; then\n  echo \"authenticated\"\n  exit 0\nelse\n  echo \"sudo: 1 incorrect password attempt\"\n  exit 1\nfi\n",
    )
    .await
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&mock_sudo).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&mock_sudo, perms).unwrap();
    }

    let prompter = Arc::new(MockSudoPrompter {
        calls: std::sync::atomic::AtomicUsize::new(0),
        password: Some("correct_pass_123".to_string()),
    });
    let session_cache = Arc::new(std::sync::Mutex::new(None));

    let path_var = format!(
        "{}:{}",
        root.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let term = TerminalTool::new(root.clone(), 5, 50_000)
        .with_env("PATH", path_var)
        .with_sudo_cache(session_cache.clone())
        .with_sudo_prompter(prompter.clone());

    // Initially cache is empty
    assert!(session_cache.lock().unwrap().is_none());

    // First command requiring sudo: triggers prompter, caches password
    let res1 = term
        .execute(
            "call_sudo_1",
            &json!({ "command": "sudo test_cmd" }),
            &cancel,
        )
        .await
        .unwrap();

    assert!(
        !res1.is_error,
        "mock sudo should succeed with correct password: {}",
        res1.content
    );
    assert_eq!(prompter.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(
        session_cache.lock().unwrap().as_deref(),
        Some("correct_pass_123")
    );

    // Second sudo command reuses the cached password from the session without prompting again!
    let res2 = term
        .execute(
            "call_sudo_2",
            &json!({ "command": "sudo test_cmd" }),
            &cancel,
        )
        .await
        .unwrap();

    assert!(!res2.is_error);
    assert_eq!(
        prompter.calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "second sudo invocation must use cached password from session"
    );

    // Now test cache invalidation on authentication failure
    *session_cache.lock().unwrap() = Some("wrong_pass".to_string());
    let res3 = term
        .execute(
            "call_sudo_3",
            &json!({ "command": "sudo test_cmd" }),
            &cancel,
        )
        .await
        .unwrap();

    assert!(res3.is_error);
    assert!(res3.content.contains("incorrect password attempt"));
    // Cache must have been cleared!
    assert!(
        session_cache.lock().unwrap().is_none(),
        "authentication failure must invalidate the session cached password"
    );
}

#[test]
fn test_sudo_auth_failure_detection() {
    use harness_core::tools::shell::is_sudo_auth_failure;

    assert!(is_sudo_auth_failure("sudo: 1 incorrect password attempt"));
    assert!(is_sudo_auth_failure("sudo: authentication failed"));
    assert!(is_sudo_auth_failure("sudo: a password is required"));
    assert!(!is_sudo_auth_failure("total 42\n-rw-r--r-- 1 root root"));
}

#[cfg(unix)]
#[tokio::test]
async fn test_background_task_queue_and_process_tool() {
    let tmp = tempfile::tempdir().unwrap();
    let jobs_dir = tmp.path().join("jobs");
    let queue = Arc::new(harness_core::tasks::BackgroundTaskQueue::new(jobs_dir));
    let term =
        TerminalTool::new(tmp.path().to_path_buf(), 5, 50_000).with_background_queue(queue.clone());
    let proc_tool = harness_core::tools::ProcessTool::new(queue.clone());
    let cancel = tokio_util::sync::CancellationToken::new();

    // Spawn a background command
    let res = term
        .execute(
            "c_bg",
            &json!({ "command": "echo 'hello from bg'; sleep 0.1; echo 'done bg'", "background": true }),
            &cancel,
        )
        .await
        .unwrap();

    assert!(!res.is_error);
    let val: serde_json::Value = serde_json::from_str(&res.content).unwrap();
    let job_id = val["job_id"].as_str().unwrap().to_string();
    assert_eq!(val["status"], "running");

    // List jobs
    let list_res = proc_tool
        .execute("c_list", &json!({ "action": "list" }), &cancel)
        .await
        .unwrap();
    assert!(list_res.content.contains(&job_id));

    // Wait a moment for completion
    let mut poll_res = proc_tool
        .execute(
            "c_poll",
            &json!({ "action": "poll", "job_id": job_id }),
            &cancel,
        )
        .await
        .unwrap();
    for _ in 0..30 {
        if poll_res.content.contains("completed (exit 0)") {
            break;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        poll_res = proc_tool
            .execute(
                "c_poll",
                &json!({ "action": "poll", "job_id": job_id }),
                &cancel,
            )
            .await
            .unwrap();
    }
    assert!(poll_res.content.contains("completed (exit 0)"));
    assert!(poll_res.content.contains("done bg"));
}

/// Regression: `terminal` and `process` registered through `register_core_tools_with_sudo`
/// must share ONE `BackgroundTaskQueue`.
///
/// The bug: each `match` arm built its own queue, so `terminal(background=true)` returned a
/// `job_id` that `process poll` could not find ("Background job 'x' not found"). A single spawn/
/// list/poll underneath one shared queue is what the host wiring now depends on.
#[cfg(unix)]
#[tokio::test]
async fn test_registry_wired_terminal_background_job_is_pollable() {
    let tmp = tempdir().unwrap();
    let mut config = Config::default();
    config.paths.workspace_root = tmp.path().to_path_buf();
    config.paths.jobs_dir = tmp.path().join("jobs");
    config.tools.enabled = vec!["terminal".to_string(), "process".to_string()];

    // What the host (chat.rs) now does: create the queue once, hand it to registration, and
    // keep a handle for `/jobs` and the task viewer.
    let host_queue = Arc::new(harness_core::tasks::BackgroundTaskQueue::new(
        config.paths.jobs_dir.clone(),
    ));

    let registry = Arc::new(DefaultToolRegistry::new());
    let reg_dyn: Arc<dyn ToolRegistry> = registry.clone();
    let shared = harness_core::tools::register_core_tools_shared_queue(
        &reg_dyn,
        &config,
        ReadCache::new(),
        None,
        None,
        host_queue.clone(),
        TodoScope::new(tmp.path().join("todos"), "test"),
    )
    .await
    .expect("core tools register");

    // Both tools must be present, i.e. registration did not drop one of them.
    let names: Vec<String> = reg_dyn
        .list_tools()
        .await
        .unwrap()
        .into_iter()
        .map(|d| d.name)
        .collect();
    assert!(
        names.contains(&"terminal".to_string()),
        "terminal missing: {:?}",
        names
    );
    assert!(
        names.contains(&"process".to_string()),
        "process missing: {:?}",
        names
    );

    // Spawn through the queue the host owns — if `terminal` had built its own queue, this job
    // would be invisible to anything else holding `host_queue`.
    let cancel = tokio_util::sync::CancellationToken::new();
    let term = TerminalTool::new(tmp.path().to_path_buf(), 5, 50_000)
        .with_background_queue(shared.clone());
    let spawn = term
        .execute(
            "c_spawn",
            &json!({ "command": "echo 'bg via registry'; sleep 0.1; echo 'finished'", "background": true }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(!spawn.is_error, "spawn failed: {}", spawn.content);
    let val: serde_json::Value = serde_json::from_str(&spawn.content).unwrap();
    let job_id = val["job_id"].as_str().unwrap().to_string();

    // The host-visible queue must see the job immediately — the old bug made this invisible.
    assert!(
        host_queue.list().iter().any(|j| j.id == job_id),
        "host queue cannot see the job terminal spawned into the shared queue"
    );

    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    let poll_re = host_queue
        .list()
        .into_iter()
        .find(|j| j.id == job_id)
        .expect("job still present");
    assert!(
        poll_re.status.starts_with("completed"),
        "job did not complete: {}",
        poll_re.status
    );
}

/// Regression: passing an existing queue in must not create a second one — the returned value
/// is the very same `Arc`, so hosts and tools can never drift apart.
#[tokio::test]
async fn test_register_core_tools_shared_queue_is_not_copied() {
    let tmp = tempdir().unwrap();
    let mut config = Config::default();
    config.paths.workspace_root = tmp.path().to_path_buf();
    config.paths.jobs_dir = tmp.path().join("jobs");
    config.tools.enabled = vec!["terminal".to_string(), "process".to_string()];

    let host_queue = Arc::new(harness_core::tasks::BackgroundTaskQueue::new(
        config.paths.jobs_dir.clone(),
    ));

    let registry = Arc::new(DefaultToolRegistry::new());
    let reg_dyn: Arc<dyn ToolRegistry> = registry.clone();
    let returned = harness_core::tools::register_core_tools_shared_queue(
        &reg_dyn,
        &config,
        ReadCache::new(),
        None,
        None,
        host_queue.clone(),
        TodoScope::new(tmp.path().join("todos"), "test"),
    )
    .await
    .expect("core tools register with shared queue");

    assert!(
        Arc::ptr_eq(&host_queue, &returned),
        "registration must reuse the caller's queue, not allocate a second one"
    );
}

#[tokio::test]
async fn test_delegate_task_tool() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.paths.workspace_root = tmp.path().to_path_buf();

    struct EchoProvider;
    #[async_trait::async_trait]
    impl harness_core::traits::ModelProvider for EchoProvider {
        fn provider_id(&self) -> &str {
            "mock"
        }
        async fn fetch_models(&self) -> Result<Vec<String>, harness_core::HarnessError> {
            Ok(vec![])
        }
        async fn complete(
            &self,
            msgs: &[harness_core::types::Message],
            _tools: &[harness_core::types::ToolDefinition],
            _opts: &harness_core::types::CompletionOptions,
        ) -> Result<harness_core::types::Message, harness_core::HarnessError> {
            let last_user = msgs
                .iter()
                .rev()
                .find(|m| m.role == harness_core::types::Role::User)
                .and_then(|m| m.content.as_deref())
                .unwrap_or("");
            Ok(harness_core::types::Message::assistant(format!(
                "Subagent completed: {}",
                last_user
            )))
        }
        async fn embed(
            &self,
            _texts: &[String],
        ) -> Result<Vec<Vec<f32>>, harness_core::HarnessError> {
            Ok(vec![])
        }
    }

    struct MockMem;
    #[async_trait::async_trait]
    impl harness_core::traits::MemoryEngine for MockMem {
        async fn retrieve(
            &self,
            _q: &str,
            _l: usize,
        ) -> Result<Vec<harness_core::MemoryRecord>, harness_core::HarnessError> {
            Ok(vec![])
        }
        async fn save(
            &self,
            _r: &harness_core::MemoryRecord,
        ) -> Result<(), harness_core::HarnessError> {
            Ok(())
        }
        async fn update(
            &self,
            _id: &str,
            _r: &harness_core::MemoryRecord,
        ) -> Result<(), harness_core::HarnessError> {
            Ok(())
        }
        async fn forget(&self, _id: &str) -> Result<(), harness_core::HarnessError> {
            Ok(())
        }
        async fn add_note_pointer(
            &self,
            _id: &str,
            _p: &str,
        ) -> Result<(), harness_core::HarnessError> {
            Ok(())
        }
        fn format_as_system_prompt(&self, _m: &[harness_core::MemoryRecord]) -> String {
            String::new()
        }
    }

    struct MockNotes;
    #[async_trait::async_trait]
    impl harness_core::traits::NotesEngine for MockNotes {
        async fn save_note(
            &self,
            _t: &str,
            _c: &str,
            _tags: &[String],
        ) -> Result<harness_core::Note, harness_core::HarnessError> {
            unreachable!("not used by this test")
        }
        async fn search_notes(
            &self,
            _q: &str,
            _l: usize,
        ) -> Result<Vec<harness_core::Note>, harness_core::HarnessError> {
            Ok(vec![])
        }
        async fn list_notes(
            &self,
            _l: usize,
            _o: usize,
        ) -> Result<Vec<harness_core::Note>, harness_core::HarnessError> {
            Ok(vec![])
        }
        async fn read_note(
            &self,
            _id: &str,
        ) -> Result<harness_core::Note, harness_core::HarnessError> {
            unreachable!("not used by this test")
        }
        async fn update_note(
            &self,
            _id: &str,
            _t: Option<&str>,
            _c: Option<&str>,
            _tags: Option<&[String]>,
        ) -> Result<harness_core::Note, harness_core::HarnessError> {
            unreachable!("not used by this test")
        }
        async fn delete_note(&self, _id: &str) -> Result<bool, harness_core::HarnessError> {
            unreachable!("not used by this test")
        }
    }

    let delegate = harness_core::tools::DelegateTaskTool::new(
        Arc::new(EchoProvider),
        Arc::new(MockMem),
        Arc::new(MockNotes),
        None,
        config,
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    let res = delegate
        .execute(
            "call_del",
            &json!({
                "tasks": [
                    { "goal": "Analyze database schema", "context": "table users" },
                    { "goal": "Audit security headers" }
                ],
                "background": false
            }),
            &cancel,
        )
        .await
        .unwrap();

    assert!(!res.is_error);
    assert!(res.content.contains("Subagent Task Results"));
    assert!(res.content.contains("Goal: Analyze database schema"));
    assert!(res.content.contains("Goal: Audit security headers"));
    assert!(res.content.contains("Subagent completed"));
}

#[tokio::test]
async fn test_delegate_task_tool_background_dispatch_and_polling() {
    let tmp = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.paths.workspace_root = tmp.path().to_path_buf();

    struct EchoProvider;
    #[async_trait::async_trait]
    impl harness_core::traits::ModelProvider for EchoProvider {
        fn provider_id(&self) -> &str {
            "mock"
        }
        async fn fetch_models(&self) -> Result<Vec<String>, harness_core::HarnessError> {
            Ok(vec![])
        }
        async fn complete(
            &self,
            msgs: &[harness_core::types::Message],
            _tools: &[harness_core::types::ToolDefinition],
            _opts: &harness_core::types::CompletionOptions,
        ) -> Result<harness_core::types::Message, harness_core::HarnessError> {
            let last_user = msgs
                .iter()
                .rev()
                .find(|m| m.role == harness_core::types::Role::User)
                .and_then(|m| m.content.as_deref())
                .unwrap_or("");
            Ok(harness_core::types::Message::assistant(format!(
                "Bg subagent done: {}",
                last_user
            )))
        }
        async fn embed(
            &self,
            _texts: &[String],
        ) -> Result<Vec<Vec<f32>>, harness_core::HarnessError> {
            Ok(vec![])
        }
    }

    struct MockMem;
    #[async_trait::async_trait]
    impl harness_core::traits::MemoryEngine for MockMem {
        async fn retrieve(
            &self,
            _q: &str,
            _l: usize,
        ) -> Result<Vec<harness_core::MemoryRecord>, harness_core::HarnessError> {
            Ok(vec![])
        }
        async fn save(
            &self,
            _r: &harness_core::MemoryRecord,
        ) -> Result<(), harness_core::HarnessError> {
            Ok(())
        }
        async fn update(
            &self,
            _id: &str,
            _r: &harness_core::MemoryRecord,
        ) -> Result<(), harness_core::HarnessError> {
            Ok(())
        }
        async fn forget(&self, _id: &str) -> Result<(), harness_core::HarnessError> {
            Ok(())
        }
        async fn add_note_pointer(
            &self,
            _id: &str,
            _p: &str,
        ) -> Result<(), harness_core::HarnessError> {
            Ok(())
        }
        fn format_as_system_prompt(&self, _m: &[harness_core::MemoryRecord]) -> String {
            String::new()
        }
    }

    struct MockNotes;
    #[async_trait::async_trait]
    impl harness_core::traits::NotesEngine for MockNotes {
        async fn save_note(
            &self,
            _t: &str,
            _c: &str,
            _tags: &[String],
        ) -> Result<harness_core::Note, harness_core::HarnessError> {
            unreachable!("not used by this test")
        }
        async fn search_notes(
            &self,
            _q: &str,
            _l: usize,
        ) -> Result<Vec<harness_core::Note>, harness_core::HarnessError> {
            Ok(vec![])
        }
        async fn list_notes(
            &self,
            _l: usize,
            _o: usize,
        ) -> Result<Vec<harness_core::Note>, harness_core::HarnessError> {
            Ok(vec![])
        }
        async fn read_note(
            &self,
            _id: &str,
        ) -> Result<harness_core::Note, harness_core::HarnessError> {
            unreachable!("not used by this test")
        }
        async fn update_note(
            &self,
            _id: &str,
            _t: Option<&str>,
            _c: Option<&str>,
            _tags: Option<&[String]>,
        ) -> Result<harness_core::Note, harness_core::HarnessError> {
            unreachable!("not used by this test")
        }
        async fn delete_note(&self, _id: &str) -> Result<bool, harness_core::HarnessError> {
            unreachable!("not used by this test")
        }
    }

    let delegate = harness_core::tools::DelegateTaskTool::new(
        Arc::new(EchoProvider),
        Arc::new(MockMem),
        Arc::new(MockNotes),
        None,
        config,
    );

    let cancel = tokio_util::sync::CancellationToken::new();
    // 1. Dispatch in background (default)
    let res = delegate
        .execute(
            "call_del_bg",
            &json!({
                "tasks": [
                    { "goal": "Find all usages of foo" }
                ]
            }),
            &cancel,
        )
        .await
        .unwrap();

    assert!(!res.is_error);
    assert!(res
        .content
        .contains("Subagent Task(s) Dispatched in Background"));
    assert!(res.content.contains("Status: running"));

    // 2. List tasks
    let list_res = delegate
        .execute("call_list", &json!({ "action": "list" }), &cancel)
        .await
        .unwrap();
    assert!(!list_res.is_error);
    assert!(list_res.content.contains("Delegated Subagent Tasks"));
    assert!(list_res.content.contains("Find all usages of foo"));

    // 3. Wait for completion
    let wait_res = delegate
        .execute(
            "call_wait",
            &json!({ "action": "wait", "timeout_secs": 10 }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(!wait_res.is_error);
    assert!(wait_res.content.contains("Completed"));
    assert!(wait_res.content.contains("Bg subagent done"));
}

#[tokio::test]
async fn test_terminal_rtk_rewrite_and_prefix() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();

    // 1. Prefix-only tool (rtk_rewrite = false)
    let term_prefix = TerminalTool::new(root.clone(), 5, 50_000)
        .with_rtk_rewrite(false)
        .with_prefix(Some("rtk".to_string()));

    let cmd1 = term_prefix.resolve_command("cargo test").await;
    assert_eq!(cmd1, "rtk cargo test");

    // Command already prefixed shouldn't be double-prefixed
    let cmd2 = term_prefix.resolve_command("rtk cargo test").await;
    assert_eq!(cmd2, "rtk cargo test");

    // 2. RTK rewrite tool (rtk_rewrite = true)
    let term_rtk = TerminalTool::new(root.clone(), 5, 50_000).with_rtk_rewrite(true);

    // If RTK is installed on this machine, "cargo test" should rewrite to "rtk cargo test"
    let resolved = term_rtk.resolve_command("cargo test").await;
    if harness_core::tools::shell::find_rtk_executable(None).is_some() {
        assert_eq!(resolved, "rtk cargo test");
    } else {
        assert_eq!(resolved, "cargo test");
    }

    // 2b. Under `tools.network = deny` the rewrite is skipped: a rewriting daemon executes the
    // command in its own process, which the child's seccomp filter does not cover.
    let term_denied = TerminalTool::new(root.clone(), 5, 50_000)
        .with_rtk_rewrite(true)
        .with_network_policy(harness_core::tools::netguard::NetworkPolicy::Deny);
    assert_eq!(
        term_denied.resolve_command("cargo test").await,
        "cargo test",
        "deny must not hand the command to a daemon outside the filter"
    );
    // ...while an explicit prefix still applies, because a prefix is a child of this process and
    // therefore inherits the filter.
    let denied_prefixed = TerminalTool::new(root.clone(), 5, 50_000)
        .with_rtk_rewrite(true)
        .with_prefix(Some("rtk".to_string()))
        .with_network_policy(harness_core::tools::netguard::NetworkPolicy::Deny);
    assert_eq!(
        denied_prefixed.resolve_command("cargo test").await,
        "rtk cargo test"
    );

    // 3. Config YAML parsing with terminal_prefix and rtk_rewrite
    let yaml = r#"
tools:
  terminal_prefix: ~/.local/bin/rtk
  rtk_rewrite: true
"#;
    let cfg: Config = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(
        cfg.tools.terminal_prefix.as_deref(),
        Some("~/.local/bin/rtk")
    );
    assert!(cfg.tools.rtk_rewrite);
}

#[tokio::test]
async fn test_scratch_dir_allowed_and_patch_without_prior_read() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache = ReadCache::new();
    let cancel = CancellationToken::new();

    // 1. A temp-dir path is *outside* the workspace, so it is refused unless the operator opted in
    //    via `paths.extra_roots`. Sharing `/tmp` with every other process is not a default.
    let tmp_file = std::env::temp_dir().join("luna_scratch_test.txt");
    let refused = harness_core::tools::fs::resolve_path(&root, tmp_file.to_str().unwrap());
    assert!(
        refused.is_err(),
        "a temp path must not be reachable by default: {refused:?}"
    );
    let sandbox = harness_core::tools::sandbox::Sandbox::with_extra_roots(
        root.clone(),
        vec![std::env::temp_dir()],
    );
    let allowed = sandbox.resolve(tmp_file.to_str().unwrap());
    assert!(
        allowed.is_ok(),
        "an opted-in root must be reachable: {allowed:?}"
    );

    // 2. Patch file on disk without prior read succeeds when old_string uniquely matches
    let test_file = root.join("untracked_file.txt");
    tokio::fs::write(&test_file, "initial content to be patched\nsecond line\n")
        .await
        .unwrap();

    let patcher = PatchTool::new(root.clone(), cache.clone());
    let patch_res = patcher
        .execute(
            "call_patch_direct",
            &json!({
                "path": "untracked_file.txt",
                "old_string": "initial content to be patched",
                "new_string": "updated content successfully applied"
            }),
            &cancel,
        )
        .await
        .unwrap();

    assert!(
        !patch_res.is_error,
        "Patch without prior read should succeed when old_string matches: {}",
        patch_res.content
    );
    let updated = tokio::fs::read_to_string(&test_file).await.unwrap();
    assert!(updated.contains("updated content successfully applied"));
}

#[tokio::test]
async fn test_todo_list_read_only_gating_and_status_aliases() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();
    let todo_tool =
        harness_core::tools::TodoListTool::new(TodoScope::new(root.join("todos"), "test"));

    // todo_list is an internal scratchpad: never requires approval for any action
    assert!(!todo_tool.is_mutating());
    assert!(!todo_tool.is_mutating_call(&json!({ "action": "list" })));
    assert!(!todo_tool.is_mutating_call(&json!({})));
    assert!(!todo_tool.is_mutating_call(&json!({ "action": "add", "text": "new task" })));

    // Add a task
    let add_res = todo_tool
        .execute(
            "call_add",
            &json!({ "action": "add", "text": "Task A" }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(!add_res.is_error);

    // Update status using alias 'completed'
    let update_res = todo_tool
        .execute(
            "call_update",
            &json!({ "action": "update", "id": 0, "status": "completed" }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        !update_res.is_error,
        "Status alias 'completed' must be accepted: {}",
        update_res.content
    );
    assert!(update_res.content.contains("done"));
}

// ---------------------------------------------------------------------------
// `tools.network`: a socket boundary the child inherits.
//
// The probe runs as a *child of the tool*, not in this process: the whole point is what the child
// inherits. CI has no network worth reaching, so the assertions are about errno values —
// `EPERM` when the policy denies, `ECONNREFUSED` when it allows — which are stable either way.
// ---------------------------------------------------------------------------

/// Child half of the network test: report what a socket attempt got, without installing anything.
/// Run by the tool under test, so the filter it installs (or does not) is the only variable.
#[cfg(unix)]
#[test]
fn network_probe_child() {
    if std::env::var("LUNA_NETPROBE").is_err() {
        return;
    }
    // AF_UNIX is not the network: local IPC must keep working under `deny`.
    match std::os::unix::net::UnixDatagram::unbound() {
        Ok(_) => println!("UNIX:OK"),
        Err(error) => println!("UNIX:ERR:{}", error.raw_os_error().unwrap_or(-1)),
    }
    match std::net::TcpStream::connect(("127.0.0.1", 9)) {
        Ok(_) => println!("PROBE:CONNECTED"),
        Err(error) => println!("PROBE:ERR:{}", error.raw_os_error().unwrap_or(-1)),
    }
}

#[cfg(target_os = "linux")]
fn run_probe(network: harness_core::tools::netguard::NetworkPolicy) -> String {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let exe = std::env::current_exe().expect("test binary path");
    let tool = TerminalTool::new(root, 30, 100_000)
        .with_env("LUNA_NETPROBE", "1")
        .with_network_policy(network);
    let cancel = CancellationToken::new();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let result = tool
            .execute(
                "call_net",
                &json!({
                    "command": format!("{} --exact network_probe_child --nocapture", exe.display()),
                }),
                &cancel,
            )
            .await
            .unwrap();
        result.content
    })
}

#[cfg(target_os = "linux")]
#[test]
fn a_denied_network_is_enforced_in_the_child_and_keeps_local_sockets() {
    let output = run_probe(harness_core::tools::netguard::NetworkPolicy::Deny);
    assert!(
        output.contains("UNIX:OK"),
        "AF_UNIX must still work under deny: {output}"
    );
    assert!(
        output.lines().any(|line| line.trim() == "PROBE:ERR:1"),
        "a TCP socket must be refused with EPERM(1): {output}"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn an_allowed_network_is_left_alone() {
    let output = run_probe(harness_core::tools::netguard::NetworkPolicy::Allow);
    assert!(output.contains("UNIX:OK"), "got: {output}");
    assert!(
        !output.lines().any(|line| line.trim() == "PROBE:ERR:1"),
        "allow must not install a filter: {output}"
    );
    assert!(
        output.contains("PROBE:ERR:111") || output.contains("PROBE:CONNECTED"),
        "the probe reached the kernel's own answer (ECONNREFUSED): {output}"
    );
}

#[tokio::test]
async fn test_terminal_allowlist_enforcement_and_operator_chaining() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    let tool = TerminalTool::new(root, 10, 10_000)
        .with_allowlist(Some(vec!["echo".to_string(), "ls".to_string()]));

    // Allowed simple command
    let res = tool
        .execute("call_1", &json!({ "command": "echo hello" }), &cancel)
        .await
        .unwrap();
    assert!(!res.is_error, "echo must be allowed");
    assert!(res.content.contains("hello"));

    // Disallowed command
    let res = tool
        .execute("call_2", &json!({ "command": "whoami" }), &cancel)
        .await
        .unwrap();
    assert!(res.is_error, "whoami must be refused");
    assert!(res
        .content
        .contains("not permitted by tools.terminal_allowlist policy"));

    // Operator chaining attempt (&&, ;, |)
    let res = tool
        .execute(
            "call_3",
            &json!({ "command": "echo hello && whoami" }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(res.is_error, "command chaining must be refused");
    assert!(res
        .content
        .contains("Command chaining and substitution operators"));

    let res = tool
        .execute(
            "call_4",
            &json!({ "command": "echo hello; whoami" }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(res.is_error, "semicolon chaining must be refused");
    assert!(res
        .content
        .contains("Command chaining and substitution operators"));
}
