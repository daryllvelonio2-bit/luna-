use std::sync::Arc;
use serde_json::json;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

use harness_core::tools::{
    register_core_tools, ListFilesTool, PatchTool, ReadCache, ReadFileTool, SearchFilesTool,
    TerminalTool, WriteFileTool,
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
    assert!(res.content.contains("resume with offset="), "Must tell where to resume");
    // The notice and the emitted lines together stay within the budget plus the notice.
    assert!(res.content.len() <= 1_400, "output grew past its allowance: {}", res.content.len());
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
        .execute("cjk", &json!({ "path": "cjk.txt" }), &CancellationToken::new())
        .await
        .unwrap();

    assert!(!res.is_error, "must not panic or error on a UTF-8 boundary: {}", res.content);
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
    let body: String = (0..200).map(|i| format!("line {i} of padded content\n")).collect();
    tokio::fs::write(root.join("many.txt"), &body).await.unwrap();

    let res = ReadFileTool::new(root.clone(), 200, ReadCache::new())
        .execute("r", &json!({ "path": "many.txt" }), &CancellationToken::new())
        .await
        .unwrap();

    assert!(res.content.contains("[truncated at 200 chars"), "got: {}", res.content);
    // The last full line kept and the resume offset must be consistent: resume points at the
    // line after the last one emitted, so following it never repeats or skips a line.
    let last_shown = res.content
        .lines()
        .rev()
        .find(|l| !l.contains("truncated"))
        .and_then(|l| l.split('|').next())
        .and_then(|n| n.parse::<usize>().ok())
        .expect("the last emitted line carries its line number");
    assert!(
        res.content.contains(&format!("resume with offset={}", last_shown + 1)),
        "resume offset must follow the last shown line ({last_shown}): {}",
        res.content
    );

    // An exhaustive read of a small file says nothing about truncation.
    tokio::fs::write(root.join("small.txt"), "one\ntwo\n").await.unwrap();
    let small = ReadFileTool::new(root.clone(), 100_000, ReadCache::new())
        .execute("s", &json!({ "path": "small.txt" }), &CancellationToken::new())
        .await
        .unwrap();
    assert!(!small.content.contains("truncated"), "got: {}", small.content);
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
    assert!(!patch_res.is_error, "Patch after read should succeed: {}", patch_res.content);

    let final_content = tokio::fs::read_to_string(root.join("test.txt")).await.unwrap();
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
        .execute("call_term_2", &json!({ "command": "echo 'luna rocks'" }), &cancel)
        .await
        .unwrap();
    assert!(!success_res.is_error);
    assert!(success_res.content.contains("luna rocks"));

    // Timeout test
    let term_short = TerminalTool::new(root.clone(), 1, 50_000);
    let timeout_res = term_short
        .execute("call_term_3", &json!({ "command": "sleep 10" }), &cancel)
        .await
        .unwrap();
    assert!(timeout_res.is_error);
    assert!(timeout_res.content.contains("timed out"));
}

#[tokio::test]
async fn test_search_files() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    tokio::fs::write(root.join("src_a.rs"), "fn calculate_total() { 42 }").await.unwrap();
    tokio::fs::write(root.join("src_b.txt"), "calculate_total in docs").await.unwrap();

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
    assert!(!res.content.contains("src_b.txt"), "Should filter by glob *.rs");
}

/// A search must not read arbitrarily large files: the workspace root is often the user's
/// home directory, where an archive or media file dwarfs the pattern being looked for.
#[tokio::test]
async fn test_search_files_skips_oversized_files() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    tokio::fs::write(root.join("small.rs"), "needle").await.unwrap();
    // ~3.5 MB of matching text: larger than the scan ceiling, so it is skipped outright.
    tokio::fs::write(root.join("big.rs"), "needle\n".repeat(512 * 1024)).await.unwrap();

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
    tokio::fs::write(root.join("a.txt"), "needle").await.unwrap();

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

    tokio::fs::write(root.join("two.txt"), "needle\nneedle\n").await.unwrap();
    tokio::fs::write(root.join("one.txt"), "needle\n").await.unwrap();
    let searcher = SearchFilesTool::new(root.clone());

    let counts = searcher
        .execute("c1", &json!({ "pattern": "needle", "output_mode": "count" }), &cancel)
        .await
        .unwrap();
    assert!(counts.content.contains("two.txt: 2 matches"), "got: {}", counts.content);
    assert!(counts.content.contains("one.txt: 1 matches"), "got: {}", counts.content);

    let files = searcher
        .execute("c2", &json!({ "pattern": "needle", "output_mode": "files_only" }), &cancel)
        .await
        .unwrap();
    assert!(files.content.contains("two.txt") && files.content.contains("one.txt"));
    assert!(!files.content.contains("matches"), "files_only must not emit counts");
}

/// The glob filter must be real glob matching: suffix-only matching reported `cars` and
/// `vendors` for `*.rs` (names ending in the letters "rs"), and ignored path-shaped
/// patterns like `src/*.rs` entirely.
#[tokio::test]
async fn test_search_files_glob_is_real_glob_not_suffix() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    tokio::fs::write(root.join("needle.rs"), "needle").await.unwrap();
    // Names that end in the letters "rs": must NOT match `*.rs`.
    tokio::fs::write(root.join("cars"), "needle").await.unwrap();
    tokio::fs::write(root.join("vendors"), "needle").await.unwrap();
    // A `?` pattern matches exactly one char; `helper_test.rs` must not match `*_tes?.rs`'s
    // sibling mistake, and `a.rs` must not match `?.txt`.
    tokio::fs::write(root.join("a.rs"), "needle").await.unwrap();
    tokio::fs::create_dir_all(root.join("src")).await.unwrap();
    tokio::fs::write(root.join("src/helper_test.rs"), "needle").await.unwrap();

    let searcher = SearchFilesTool::new(root.clone());

    let star_rs = searcher
        .execute("g1", &json!({ "pattern": "needle", "file_glob": "*.rs" }), &cancel)
        .await
        .unwrap();
    assert!(star_rs.content.contains("needle.rs"), "got: {}", star_rs.content);
    assert!(star_rs.content.contains("src/helper_test.rs"));
    assert!(
        !star_rs.content.contains("cars:") && !star_rs.content.contains("vendors:"),
        "files merely ending in 'rs' must not match '*.rs': {}",
        star_rs.content
    );

    // `?` is exactly one character, so `*_tes?.rs` matches `helper_test.rs`.
    let question = searcher
        .execute("g2", &json!({ "pattern": "needle", "file_glob": "*_tes?.rs" }), &cancel)
        .await
        .unwrap();
    assert!(question.content.contains("helper_test.rs"), "got: {}", question.content);
    assert!(!question.content.contains("needle.rs"), "got: {}", question.content);

    // A path-shaped glob matches the workspace-relative path.
    let path_glob = searcher
        .execute("g3", &json!({ "pattern": "needle", "file_glob": "src/*.rs" }), &cancel)
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
        .execute("g4", &json!({ "pattern": "needle", "file_glob": "cars" }), &cancel)
        .await
        .unwrap();
    assert!(literal.content.contains("cars:"));
    assert!(!literal.content.contains("vendors"), "got: {}", literal.content);
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
        tokio::fs::write(many.path().join(format!("f{i}.txt")), "needle\n").await.unwrap();
    }
    let searcher = SearchFilesTool::new(many.path().to_path_buf());

    let content = searcher
        .execute("m1", &json!({ "pattern": "needle" }), &cancel)
        .await
        .unwrap();
    assert!(content.content.contains("matching lines"), "got: {}", content.content);
    // The 'content' cap counts lines, and each file here has one match, so it is bounded
    // by the line ceiling's advice to switch modes.
    assert!(
        content.content.contains("files_only"),
        "content mode should point at the list modes: {}",
        content.content
    );

    let files_only = searcher
        .execute("m2", &json!({ "pattern": "needle", "output_mode": "files_only" }), &cancel)
        .await
        .unwrap();
    assert!(
        files_only.content.contains("files"),
        "files_only must announce its ceiling: {}",
        files_only.content
    );

    let count = searcher
        .execute("m3", &json!({ "pattern": "needle", "output_mode": "count" }), &cancel)
        .await
        .unwrap();
    assert!(count.content.contains("files"), "count must announce its ceiling: {}", count.content);
}

/// A search that finishes without hitting a ceiling must not claim truncation.
#[tokio::test]
async fn test_search_files_exhaustive_result_has_no_notice() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();
    tokio::fs::write(root.join("a.txt"), "needle\n").await.unwrap();

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
async fn test_registry_contains_all_six_tools() {
    let mut config = Config::default();
    let dir = tempdir().unwrap();
    config.paths.workspace_root = dir.path().to_path_buf();

    let registry: Arc<dyn ToolRegistry> = Arc::new(DefaultToolRegistry::new());
    let cache = ReadCache::new();

    register_core_tools(&registry, &config, cache).await.unwrap();

    let tools = registry.list_tools().await.unwrap();
    let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();

    assert_eq!(tool_names.len(), 6);
    assert!(tool_names.contains(&"read_file".to_string()));
    assert!(tool_names.contains(&"write_file".to_string()));
    assert!(tool_names.contains(&"patch".to_string()));
    assert!(tool_names.contains(&"search_files".to_string()));
    assert!(tool_names.contains(&"list_files".to_string()));
    assert!(tool_names.contains(&"terminal".to_string()));
}

/// `list_files` is the filename-discovery surface that previously forced a `terminal` call.
/// It must be non-mutating, so it never prompts under the HITL approval gate.
#[tokio::test]
async fn test_list_files_is_non_mutating() {
    let dir = tempdir().unwrap();
    let reader = ListFilesTool::new(dir.path().to_path_buf());
    assert!(!reader.is_mutating(), "list_files is read-only");
}

#[tokio::test]
async fn test_list_files_lists_tree_with_dir_markers() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    tokio::fs::write(root.join("main.rs"), "fn main() {}").await.unwrap();
    tokio::fs::write(root.join("notes.md"), "# notes").await.unwrap();
    tokio::fs::create_dir_all(root.join("src")).await.unwrap();
    tokio::fs::write(root.join("src/lib.rs"), "pub fn x() {}").await.unwrap();

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
    assert!(!res.content.contains("listing stopped"), "got: {}", res.content);
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
    tokio::fs::write(root.join("src/helper_test.rs"), "x").await.unwrap();

    let lister = ListFilesTool::new(root.clone());

    // Bare glob: matches entry names, and `cars` must not match `*.rs`.
    let by_ext = lister
        .execute("g1", &json!({ "glob": "*.rs" }), &cancel)
        .await
        .unwrap();
    assert!(by_ext.content.contains("agent.rs"), "got: {}", by_ext.content);
    assert!(by_ext.content.contains("src/helper_test.rs"));
    assert!(!by_ext.content.contains("cars"), "suffix matching must not creep in: {}", by_ext.content);

    // Path-shaped glob: matches the workspace-relative path.
    let by_path = lister
        .execute("g2", &json!({ "glob": "src/*.rs" }), &cancel)
        .await
        .unwrap();
    assert!(by_path.content.contains("src/helper_test.rs"), "got: {}", by_path.content);
    assert!(!by_path.content.contains("agent.rs"), "path glob must not match root files: {}", by_path.content);

    // A glob with no wildcard must equal the entry name.
    let literal = lister
        .execute("g3", &json!({ "glob": "cars" }), &cancel)
        .await
        .unwrap();
    assert!(literal.content.contains("cars"));
    assert!(!literal.content.contains("agent.rs"), "got: {}", literal.content);
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
    assert!(res.content.contains("Not a directory"), "got: {}", res.content);

    let res = ListFilesTool::new(root)
        .execute("e2", &json!({ "path": "../../etc" }), &cancel)
        .await
        .unwrap();
    assert!(res.is_error, "escaping the workspace must be refused");
    assert!(res.content.contains("escapes workspace root"), "got: {}", res.content);
}

/// A listing is read by a model rather than a pager, so it must be capped and must say so.
#[tokio::test]
async fn test_list_files_announces_its_ceiling() {
    let cancel = CancellationToken::new();

    let many = tempdir().unwrap();
    for i in 0..1_050 {
        tokio::fs::write(many.path().join(format!("f{i}.txt")), "x").await.unwrap();
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
    assert!(res.content.contains("glob"), "the advice names the narrowing lever: {}", res.content);
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
        tokio::fs::write(root.join(noise).join("hidden"), "x").await.unwrap();
    }

    let res = ListFilesTool::new(root)
        .execute("n1", &json!({}), &cancel)
        .await
        .unwrap();

    assert!(res.content.contains("keep.rs"), "got: {}", res.content);
    for noise in ["hidden", ".git/", "target/", "node_modules/"] {
        assert!(!res.content.contains(noise), "'{noise}' must be skipped: {}", res.content);
    }
}
