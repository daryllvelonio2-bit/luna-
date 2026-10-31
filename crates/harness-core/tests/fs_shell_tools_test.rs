use serde_json::json;
use std::sync::Arc;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

use harness_core::tools::{
    register_core_tools, ListFilesTool, PatchTool, ReadCache, ReadFileTool, SearchFilesTool,
    TerminalTool, TodoScope, WriteFileTool,
};
use harness_core::{Config, DefaultToolRegistry, HarnessError, Tool, ToolRegistry};

#[tokio::test]
async fn test_escape_refusal() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache = ReadCache::new();

    let writer = WriteFileTool::new(root.clone(), cache);
    let cancel = CancellationToken::new();

    let res = writer
        .execute(
            "call_1",
            &json!({ "path": "../../etc/hosts", "content": "evil" }),
            &cancel,
        )
        .await
        .unwrap();

    assert!(res.is_error, "Escape path must be refused as error");
    assert!(
        res.content.contains("escapes workspace root"),
        "Error message should mention escaping: {}",
        res.content
    );
}

#[tokio::test]
async fn test_read_file_truncation() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let file_path = root.join("big.txt");

    // Synthetic file larger than budget (e.g. 50_000 chars with max_chars = 1_000)
    let big_data = "a".repeat(50_000);
    tokio::fs::write(&file_path, &big_data).await.unwrap();

    let cache = ReadCache::new();
    let reader = ReadFileTool::new(root.clone(), 1_000, cache);
    let cancel = CancellationToken::new();

    let res = reader
        .execute("call_2", &json!({ "path": "big.txt" }), &cancel)
        .await
        .unwrap();

    assert!(!res.is_error);
    // Whole-line truncation says how much is shown and how to resume, rather than the old
    // bare "character limit reached" with no offset to continue from.
    assert!(
        res.content.contains("[truncated at 1000 chars"),
        "Must announce its budget: {}",
        &res.content[res.content.len().saturating_sub(120)..]
    );
    assert!(
        res.content.contains("resume with offset="),
        "Must tell where to resume"
    );
    // The notice and the emitted lines together stay within the budget plus the notice.
    assert!(
        res.content.len() <= 1_400,
        "output grew past its allowance: {}",
        res.content.len()
    );
}

/// A budget that lands inside a multi-byte character must not panic. `String::truncate` panics
/// on a non-char boundary, so a CJK or emoji file used to turn a routine read into a crash.
#[tokio::test]
async fn test_read_file_truncation_respects_utf8_boundaries() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    // Each CJK char is 3 bytes; a 100-char budget lands mid-character at 100 bytes.
    let cjk = "日本語テキスト".repeat(20);
    assert!(cjk.len() > 100);
    tokio::fs::write(root.join("cjk.txt"), &cjk).await.unwrap();

    let res = ReadFileTool::new(root.clone(), 100, ReadCache::new())
        .execute(
            "cjk",
            &json!({ "path": "cjk.txt" }),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(
        !res.is_error,
        "must not panic or error on a UTF-8 boundary: {}",
        res.content
    );
    assert!(
        res.content.contains("[truncated at 100 chars"),
        "got: {}",
        res.content
    );
}

/// A short read must not claim truncation, and a multi-line read must resume at the line after
/// the last one shown.
#[tokio::test]
async fn test_read_file_reports_resume_offset_accurately() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let body: String = (0..200)
        .map(|i| format!("line {i} of padded content\n"))
        .collect();
    tokio::fs::write(root.join("many.txt"), &body)
        .await
        .unwrap();

    let res = ReadFileTool::new(root.clone(), 200, ReadCache::new())
        .execute(
            "r",
            &json!({ "path": "many.txt" }),
            &CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(
        res.content.contains("[truncated at 200 chars"),
        "got: {}",
        res.content
    );
    // The last full line kept and the resume offset must be consistent: resume points at the
    // line after the last one emitted, so following it never repeats or skips a line.
    let last_shown = res
        .content
        .lines()
        .rev()
        .find(|l| !l.contains("truncated"))
        .and_then(|l| l.split('|').next())
        .and_then(|n| n.parse::<usize>().ok())
        .expect("the last emitted line carries its line number");
    assert!(
        res.content
            .contains(&format!("resume with offset={}", last_shown + 1)),
        "resume offset must follow the last shown line ({last_shown}): {}",
        res.content
    );

    // An exhaustive read of a small file says nothing about truncation.
    tokio::fs::write(root.join("small.txt"), "one\ntwo\n")
        .await
        .unwrap();
    let small = ReadFileTool::new(root.clone(), 100_000, ReadCache::new())
        .execute(
            "s",
            &json!({ "path": "small.txt" }),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        !small.content.contains("truncated"),
        "got: {}",
        small.content
    );
}

#[tokio::test]
async fn test_read_before_write_and_stale_refusal() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache = ReadCache::new();
    let cancel = CancellationToken::new();

    let writer = WriteFileTool::new(root.clone(), cache.clone());
    let reader = ReadFileTool::new(root.clone(), 100_000, cache.clone());
    let patcher = PatchTool::new(root.clone(), cache.clone());

    // 1. Initial write of new file succeeds
    let res = writer
        .execute(
            "call_write_1",
            &json!({ "path": "test.txt", "content": "Hello World\nLine 2\n" }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(!res.is_error);

    // 2. Mutate out-of-band directly on disk
    tokio::fs::write(root.join("test.txt"), "Changed out of band!\n")
        .await
        .unwrap();

    // 3. Attempt write without reading -> refused
    let stale_res = writer
        .execute(
            "call_write_2",
            &json!({ "path": "test.txt", "content": "Overwriting..." }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(stale_res.is_error);
    assert!(
        stale_res.content.contains("Stale write refused"),
        "Expected stale refusal: {}",
        stale_res.content
    );

    // 4. Now read the file to update read cache
    let read_res = reader
        .execute("call_read_1", &json!({ "path": "test.txt" }), &cancel)
        .await
        .unwrap();
    assert!(!read_res.is_error);

    // 5. Subsequent patch succeeds
    let patch_res = patcher
        .execute(
            "call_patch_1",
            &json!({
                "path": "test.txt",
                "old_string": "Changed out of band!",
                "new_string": "Properly patched!"
            }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        !patch_res.is_error,
        "Patch after read should succeed: {}",
        patch_res.content
    );

    let final_content = tokio::fs::read_to_string(root.join("test.txt"))
        .await
        .unwrap();
    assert!(final_content.contains("Properly patched!"));
}

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

#[tokio::test]
async fn test_search_files() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    tokio::fs::write(root.join("src_a.rs"), "fn calculate_total() { 42 }")
        .await
        .unwrap();
    tokio::fs::write(root.join("src_b.txt"), "calculate_total in docs")
        .await
        .unwrap();

    let searcher = SearchFilesTool::new(root.clone());

    let res = searcher
        .execute(
            "call_search",
            &json!({ "pattern": "calculate_total", "file_glob": "*.rs" }),
            &cancel,
        )
        .await
        .unwrap();

    assert!(!res.is_error);
    assert!(res.content.contains("src_a.rs"));
    assert!(
        !res.content.contains("src_b.txt"),
        "Should filter by glob *.rs"
    );
}

/// A search must not read arbitrarily large files: the workspace root is often the user's
/// home directory, where an archive or media file dwarfs the pattern being looked for.
#[tokio::test]
async fn test_search_files_skips_oversized_files() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    tokio::fs::write(root.join("small.rs"), "needle")
        .await
        .unwrap();
    // ~3.5 MB of matching text: larger than the scan ceiling, so it is skipped outright.
    tokio::fs::write(root.join("big.rs"), "needle\n".repeat(512 * 1024))
        .await
        .unwrap();

    let res = SearchFilesTool::new(root.clone())
        .execute("call_big", &json!({ "pattern": "needle" }), &cancel)
        .await
        .unwrap();

    assert!(res.content.contains("small.rs"));
    assert!(
        !res.content.contains("big.rs"),
        "files above the scan ceiling must be skipped, not read into memory"
    );
}

/// A cancelled search must abort rather than finish the walk, otherwise cancelling a turn
/// leaves the scan running in the background.
#[tokio::test]
async fn test_search_files_honours_cancellation() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    tokio::fs::write(root.join("a.txt"), "needle")
        .await
        .unwrap();

    let cancel = CancellationToken::new();
    cancel.cancel();

    let err = SearchFilesTool::new(root)
        .execute("call_cancelled", &json!({ "pattern": "needle" }), &cancel)
        .await
        .expect_err("a cancelled search must abort instead of scanning");

    assert!(matches!(err, HarnessError::Cancelled));
}

/// The count mode reports a per-file total, and the content and file-list modes stay
/// independent of it.
#[tokio::test]
async fn test_search_files_output_modes() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    tokio::fs::write(root.join("two.txt"), "needle\nneedle\n")
        .await
        .unwrap();
    tokio::fs::write(root.join("one.txt"), "needle\n")
        .await
        .unwrap();
    let searcher = SearchFilesTool::new(root.clone());

    let counts = searcher
        .execute(
            "c1",
            &json!({ "pattern": "needle", "output_mode": "count" }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        counts.content.contains("two.txt: 2 matches"),
        "got: {}",
        counts.content
    );
    assert!(
        counts.content.contains("one.txt: 1 matches"),
        "got: {}",
        counts.content
    );

    let files = searcher
        .execute(
            "c2",
            &json!({ "pattern": "needle", "output_mode": "files_only" }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(files.content.contains("two.txt") && files.content.contains("one.txt"));
    assert!(
        !files.content.contains("matches"),
        "files_only must not emit counts"
    );
}

/// The glob filter must be real glob matching: suffix-only matching reported `cars` and
/// `vendors` for `*.rs` (names ending in the letters "rs"), and ignored path-shaped
/// patterns like `src/*.rs` entirely.
#[tokio::test]
async fn test_search_files_glob_is_real_glob_not_suffix() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    tokio::fs::write(root.join("needle.rs"), "needle")
        .await
        .unwrap();
    // Names that end in the letters "rs": must NOT match `*.rs`.
    tokio::fs::write(root.join("cars"), "needle").await.unwrap();
    tokio::fs::write(root.join("vendors"), "needle")
        .await
        .unwrap();
    // A `?` pattern matches exactly one char; `helper_test.rs` must not match `*_tes?.rs`'s
    // sibling mistake, and `a.rs` must not match `?.txt`.
    tokio::fs::write(root.join("a.rs"), "needle").await.unwrap();
    tokio::fs::create_dir_all(root.join("src")).await.unwrap();
    tokio::fs::write(root.join("src/helper_test.rs"), "needle")
        .await
        .unwrap();

    let searcher = SearchFilesTool::new(root.clone());

    let star_rs = searcher
        .execute(
            "g1",
            &json!({ "pattern": "needle", "file_glob": "*.rs" }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        star_rs.content.contains("needle.rs"),
        "got: {}",
        star_rs.content
    );
    assert!(star_rs.content.contains("src/helper_test.rs"));
    assert!(
        !star_rs.content.contains("cars:") && !star_rs.content.contains("vendors:"),
        "files merely ending in 'rs' must not match '*.rs': {}",
        star_rs.content
    );

    // `?` is exactly one character, so `*_tes?.rs` matches `helper_test.rs`.
    let question = searcher
        .execute(
            "g2",
            &json!({ "pattern": "needle", "file_glob": "*_tes?.rs" }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        question.content.contains("helper_test.rs"),
        "got: {}",
        question.content
    );
    assert!(
        !question.content.contains("needle.rs"),
        "got: {}",
        question.content
    );

    // A path-shaped glob matches the workspace-relative path.
    let path_glob = searcher
        .execute(
            "g3",
            &json!({ "pattern": "needle", "file_glob": "src/*.rs" }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        path_glob.content.contains("src/helper_test.rs"),
        "path-shaped globs must work: {}",
        path_glob.content
    );
    assert!(
        !path_glob.content.contains("needle.rs:"),
        "path-shaped glob must not match root-level files: {}",
        path_glob.content
    );

    // A literal glob with no wildcard must equal the file name.
    let literal = searcher
        .execute(
            "g4",
            &json!({ "pattern": "needle", "file_glob": "cars" }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(literal.content.contains("cars:"));
    assert!(
        !literal.content.contains("vendors"),
        "got: {}",
        literal.content
    );
}

/// Every output mode must announce its own ceiling when it stops early. The cap used to be
/// shared (driven by matching lines), so 'files_only' and 'count' were truncated at 200 with
/// no notice at all — the caller believed the result was exhaustive.
#[tokio::test]
async fn test_search_files_announces_each_modes_ceiling() {
    let cancel = CancellationToken::new();

    // 600 files, each matching once: past the file-list ceiling (500) and the line ceiling (200).
    let many = tempdir().unwrap();
    for i in 0..600 {
        tokio::fs::write(many.path().join(format!("f{i}.txt")), "needle\n")
            .await
            .unwrap();
    }
    let searcher = SearchFilesTool::new(many.path().to_path_buf());

    let content = searcher
        .execute("m1", &json!({ "pattern": "needle" }), &cancel)
        .await
        .unwrap();
    assert!(
        content.content.contains("matching lines"),
        "got: {}",
        content.content
    );
    // The 'content' cap counts lines, and each file here has one match, so it is bounded
    // by the line ceiling's advice to switch modes.
    assert!(
        content.content.contains("files_only"),
        "content mode should point at the list modes: {}",
        content.content
    );

    let files_only = searcher
        .execute(
            "m2",
            &json!({ "pattern": "needle", "output_mode": "files_only" }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        files_only.content.contains("files"),
        "files_only must announce its ceiling: {}",
        files_only.content
    );

    let count = searcher
        .execute(
            "m3",
            &json!({ "pattern": "needle", "output_mode": "count" }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        count.content.contains("files"),
        "count must announce its ceiling: {}",
        count.content
    );
}

/// A search that finishes without hitting a ceiling must not claim truncation.
#[tokio::test]
async fn test_search_files_exhaustive_result_has_no_notice() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();
    tokio::fs::write(root.join("a.txt"), "needle\n")
        .await
        .unwrap();

    let res = SearchFilesTool::new(root)
        .execute("e1", &json!({ "pattern": "needle" }), &cancel)
        .await
        .unwrap();
    assert!(res.content.contains("a.txt"));
    assert!(
        !res.content.contains("search stopped"),
        "an exhaustive search must not claim truncation: {}",
        res.content
    );
}

#[tokio::test]
async fn test_mutating_declarations() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache = ReadCache::new();

    let reader = ReadFileTool::new(root.clone(), 100_000, cache.clone());
    let writer = WriteFileTool::new(root.clone(), cache.clone());
    let patcher = PatchTool::new(root.clone(), cache.clone());
    let searcher = SearchFilesTool::new(root.clone());
    let lister = ListFilesTool::new(root.clone());
    let term = TerminalTool::new(root.clone(), 10, 10_000);

    assert!(!reader.is_mutating(), "read_file must be non-mutating");
    assert!(!searcher.is_mutating(), "search_files must be non-mutating");
    assert!(!lister.is_mutating(), "list_files must be non-mutating");
    assert!(writer.is_mutating(), "write_file must be mutating");
    assert!(patcher.is_mutating(), "patch must be mutating");
    assert!(term.is_mutating(), "terminal must be mutating");
}

#[tokio::test]
async fn the_registry_registers_every_default_tool() {
    let mut config = Config::default();
    let dir = tempdir().unwrap();
    config.paths.workspace_root = dir.path().to_path_buf();

    let registry: Arc<dyn ToolRegistry> = Arc::new(DefaultToolRegistry::new());
    let cache = ReadCache::new();

    register_core_tools(
        &registry,
        &config,
        cache,
        TodoScope::new(dir.path().join("todos"), "test"),
    )
    .await
    .unwrap();

    let tools = registry.list_tools().await.unwrap();
    let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();

    // Everything `tools.enabled` names by default, which is all ten minus the opt-in `process`.
    // The web pair is in that set: a registry that quietly dropped them is a registry whose model
    // reaches for `curl` instead.
    assert_eq!(tool_names.len(), 9, "{tool_names:?}");
    for expected in [
        "read_file",
        "write_file",
        "patch",
        "search_files",
        "list_files",
        "todo_list",
        "terminal",
        "web_search",
        "web_fetch",
    ] {
        assert!(
            tool_names.contains(&expected.to_string()),
            "{expected} is missing from the registry: {tool_names:?}"
        );
    }
}

/// `tools.network: deny` says this machine does not talk to the network. The seccomp filter behind
/// it confines a `terminal` child, not the harness process, so the web tools are withdrawn rather
/// than left to bypass the very boundary that setting draws.
#[tokio::test]
async fn denying_the_network_and_disabling_web_access_both_withhold_the_web_tools() {
    let mut config = Config::default();
    let dir = tempdir().unwrap();
    config.paths.workspace_root = dir.path().to_path_buf();
    config.tools.network = harness_core::tools::netguard::NetworkPolicy::Deny;

    let registry: Arc<dyn ToolRegistry> = Arc::new(DefaultToolRegistry::new());
    register_core_tools(
        &registry,
        &config,
        ReadCache::new(),
        TodoScope::new(dir.path().join("todos"), "test"),
    )
    .await
    .unwrap();
    let names: Vec<String> = registry
        .list_tools()
        .await
        .unwrap()
        .iter()
        .map(|tool| tool.name.clone())
        .collect();

    assert!(!names.contains(&"web_search".to_string()), "{names:?}");
    assert!(!names.contains(&"web_fetch".to_string()), "{names:?}");
    // Only the sockets are bounded: the filesystem and shell surface is untouched.
    assert!(names.contains(&"terminal".to_string()), "{names:?}");
    assert!(names.contains(&"read_file".to_string()), "{names:?}");

    // `tools.web.enabled: false` is the same absence for a user who wants no web access at all,
    // and it is reachable while the network policy allows everything.
    config.tools.network = harness_core::tools::netguard::NetworkPolicy::Allow;
    config.tools.web.enabled = false;
    let registry: Arc<dyn ToolRegistry> = Arc::new(DefaultToolRegistry::new());
    register_core_tools(
        &registry,
        &config,
        ReadCache::new(),
        TodoScope::new(dir.path().join("todos"), "test"),
    )
    .await
    .unwrap();
    let names: Vec<String> = registry
        .list_tools()
        .await
        .unwrap()
        .iter()
        .map(|tool| tool.name.clone())
        .collect();
    assert!(!names.contains(&"web_search".to_string()), "{names:?}");
    assert!(!names.contains(&"web_fetch".to_string()), "{names:?}");
}

/// `list_files` is the filename-discovery surface that previously forced a `terminal` call.
/// It must be non-mutating, so it never prompts under the HITL approval gate.
#[tokio::test]
async fn test_list_files_is_non_mutating() {
    let dir = tempdir().unwrap();
    let reader = ListFilesTool::new(dir.path().to_path_buf());
    assert!(!reader.is_mutating(), "list_files is read-only");
}

/// A model that asks for the workspace root sometimes sends `path: ""` rather than "." or
/// omitting the field. That is asking for the documented default, not making a mistake, and it
/// must not cost a failed tool call.
#[tokio::test]
async fn a_blank_path_lists_the_workspace_root() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    tokio::fs::write(root.join("main.rs"), "fn main() {}")
        .await
        .unwrap();

    let res = ListFilesTool::new(root)
        .execute("l1", &json!({ "path": "" }), &cancel)
        .await
        .unwrap();

    assert!(!res.is_error, "a blank path must not fail: {}", res.content);
    assert!(res.content.contains("main.rs"), "got: {}", res.content);
}

/// The same blank-path rule holds for the search surface.
#[tokio::test]
async fn a_blank_path_searches_the_workspace_root() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    tokio::fs::write(root.join("notes.md"), "needle here")
        .await
        .unwrap();

    let res = SearchFilesTool::new(root)
        .execute("s1", &json!({ "pattern": "needle", "path": "" }), &cancel)
        .await
        .unwrap();

    assert!(!res.is_error, "a blank path must not fail: {}", res.content);
    assert!(res.content.contains("notes.md"), "got: {}", res.content);
}

#[tokio::test]
async fn test_list_files_lists_tree_with_dir_markers() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    tokio::fs::write(root.join("main.rs"), "fn main() {}")
        .await
        .unwrap();
    tokio::fs::write(root.join("notes.md"), "# notes")
        .await
        .unwrap();
    tokio::fs::create_dir_all(root.join("src")).await.unwrap();
    tokio::fs::write(root.join("src/lib.rs"), "pub fn x() {}")
        .await
        .unwrap();

    let res = ListFilesTool::new(root)
        .execute("l1", &json!({}), &cancel)
        .await
        .unwrap();

    assert!(!res.is_error, "got: {}", res.content);
    // Recursive, workspace-relative, directories carry a trailing slash.
    assert!(res.content.contains("main.rs"));
    assert!(res.content.contains("notes.md"));
    assert!(res.content.contains("src/"));
    assert!(res.content.contains("src/lib.rs"));
    // Sorted, so two listings of the same tree are identical.
    let lines: Vec<&str> = res.content.lines().collect();
    let mut sorted = lines.clone();
    sorted.sort();
    assert_eq!(lines, sorted, "output must be sorted");
    // An exhaustive listing says nothing about truncation.
    assert!(
        !res.content.contains("listing stopped"),
        "got: {}",
        res.content
    );
}

/// Finding files by name is the missing capability; a bare glob must match entry names,
/// and path-shaped globs the workspace-relative path.
#[tokio::test]
async fn test_list_files_glob_finds_by_name() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    tokio::fs::write(root.join("agent.rs"), "x").await.unwrap();
    tokio::fs::write(root.join("cars"), "x").await.unwrap();
    tokio::fs::create_dir_all(root.join("src")).await.unwrap();
    tokio::fs::write(root.join("src/helper_test.rs"), "x")
        .await
        .unwrap();

    let lister = ListFilesTool::new(root.clone());

    // Bare glob: matches entry names, and `cars` must not match `*.rs`.
    let by_ext = lister
        .execute("g1", &json!({ "glob": "*.rs" }), &cancel)
        .await
        .unwrap();
    assert!(
        by_ext.content.contains("agent.rs"),
        "got: {}",
        by_ext.content
    );
    assert!(by_ext.content.contains("src/helper_test.rs"));
    assert!(
        !by_ext.content.contains("cars"),
        "suffix matching must not creep in: {}",
        by_ext.content
    );

    // Path-shaped glob: matches the workspace-relative path.
    let by_path = lister
        .execute("g2", &json!({ "glob": "src/*.rs" }), &cancel)
        .await
        .unwrap();
    assert!(
        by_path.content.contains("src/helper_test.rs"),
        "got: {}",
        by_path.content
    );
    assert!(
        !by_path.content.contains("agent.rs"),
        "path glob must not match root files: {}",
        by_path.content
    );

    // A glob with no wildcard must equal the entry name.
    let literal = lister
        .execute("g3", &json!({ "glob": "cars" }), &cancel)
        .await
        .unwrap();
    assert!(literal.content.contains("cars"));
    assert!(
        !literal.content.contains("agent.rs"),
        "got: {}",
        literal.content
    );
}

/// Directories cannot be passed to the listing: a path that is a file is a caller error, and
/// an escaping path must be refused rather than silently clamped.
#[tokio::test]
async fn test_list_files_refuses_file_and_escape() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    tokio::fs::write(root.join("main.rs"), "x").await.unwrap();

    let res = ListFilesTool::new(root.clone())
        .execute("e1", &json!({ "path": "main.rs" }), &cancel)
        .await
        .unwrap();
    assert!(res.is_error, "a file is not listable");
    assert!(
        res.content.contains("Not a directory"),
        "got: {}",
        res.content
    );

    let res = ListFilesTool::new(root)
        .execute("e2", &json!({ "path": "../../etc" }), &cancel)
        .await
        .unwrap();
    assert!(res.is_error, "escaping the workspace must be refused");
    assert!(
        res.content.contains("escapes workspace root"),
        "got: {}",
        res.content
    );
}

/// A listing is read by a model rather than a pager, so it must be capped and must say so.
#[tokio::test]
async fn test_list_files_announces_its_ceiling() {
    let cancel = CancellationToken::new();

    let many = tempdir().unwrap();
    for i in 0..1_050 {
        tokio::fs::write(many.path().join(format!("f{i}.txt")), "x")
            .await
            .unwrap();
    }

    let res = ListFilesTool::new(many.path().to_path_buf())
        .execute("c1", &json!({}), &cancel)
        .await
        .unwrap();

    assert!(
        res.content.contains("[listing stopped after"),
        "a capped listing must announce the cap: {}",
        &res.content[res.content.len().saturating_sub(140)..]
    );
    assert!(
        res.content.contains("glob"),
        "the advice names the narrowing lever: {}",
        res.content
    );
}

/// `.git`, `target` and `node_modules` are noise for an agent, exactly as in `search_files`.
#[tokio::test]
async fn test_list_files_skips_noise_directories() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    tokio::fs::write(root.join("keep.rs"), "x").await.unwrap();
    for noise in [".git", "target", "node_modules"] {
        tokio::fs::create_dir_all(root.join(noise)).await.unwrap();
        tokio::fs::write(root.join(noise).join("hidden"), "x")
            .await
            .unwrap();
    }

    let res = ListFilesTool::new(root)
        .execute("n1", &json!({}), &cancel)
        .await
        .unwrap();

    assert!(res.content.contains("keep.rs"), "got: {}", res.content);
    for noise in ["hidden", ".git/", "target/", "node_modules/"] {
        assert!(
            !res.content.contains(noise),
            "'{noise}' must be skipped: {}",
            res.content
        );
    }
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
