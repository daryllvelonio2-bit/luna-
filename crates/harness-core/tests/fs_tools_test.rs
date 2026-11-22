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

#[cfg(unix)]
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

    // Everything `tools.enabled` names by default, plus nothing else: the opt-in `process` stays
    // out. The web pair and `view_image` are in that set: a registry that quietly dropped them is
    // a registry whose model reaches for `curl` or a terminal previewer instead.
    assert_eq!(tool_names.len(), 11, "{tool_names:?}");
    for expected in [
        "read_file",
        "view_image",
        "write_file",
        "patch",
        "search_files",
        "list_files",
        "todo_list",
        "terminal",
        "sleep",
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

#[tokio::test]
async fn write_file_preview_generates_diff_for_new_and_existing_files() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache = ReadCache::new();
    let writer = WriteFileTool::new(root.clone(), cache);

    // Brand-new file: diff shows every added line.
    let preview_new = writer
        .preview(&json!({ "path": "test.txt", "content": "hello\nworld\n" }))
        .await
        .unwrap();
    assert_eq!(preview_new.target.as_deref(), Some("test.txt"));
    let diff_new = preview_new.diff.expect("write_file preview must emit diff");
    assert!(diff_new.contains("+hello"), "{diff_new}");
    assert!(diff_new.contains("+world"), "{diff_new}");

    // Existing file: diff shows removed and added lines.
    tokio::fs::write(root.join("test.txt"), "hello\nworld\n")
        .await
        .unwrap();
    let preview_edit = writer
        .preview(&json!({ "path": "test.txt", "content": "hello\nrust\n" }))
        .await
        .unwrap();
    let diff_edit = preview_edit.diff.expect("overwrite preview must emit diff");
    assert!(diff_edit.contains("-world"), "{diff_edit}");
    assert!(diff_edit.contains("+rust"), "{diff_edit}");
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
    for noise in [".git", "target", "node_modules", "checkpoints", ".cache"] {
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
    for noise in [
        "hidden",
        ".git/",
        "target/",
        "node_modules/",
        "checkpoints/",
        ".cache/",
    ] {
        assert!(
            !res.content.contains(noise),
            "'{noise}' must be skipped: {}",
            res.content
        );
    }
}

#[tokio::test]
async fn test_search_files_truncates_long_matching_lines() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    let prefix = "a".repeat(25_000);
    let suffix = "b".repeat(25_000);
    let long_line = format!("{prefix}target_needle{suffix}\n");
    tokio::fs::write(root.join("huge_line.json"), long_line)
        .await
        .unwrap();

    let res = SearchFilesTool::new(root)
        .execute("c1", &json!({ "pattern": "target_needle" }), &cancel)
        .await
        .unwrap();

    assert!(res.content.contains("target_needle"));
    assert!(res.content.contains("[line truncated]"));
    // The truncated line should be roughly ~500 chars plus path/prefix/suffix, far below 50,000 chars
    assert!(
        res.content.len() < 1_000,
        "content was {} chars",
        res.content.len()
    );
}

#[tokio::test]
async fn test_search_files_skips_noise_directories() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    tokio::fs::write(root.join("src.rs"), "find_me")
        .await
        .unwrap();
    for noise in [".git", "target", "node_modules", "checkpoints", ".cache"] {
        tokio::fs::create_dir_all(root.join(noise)).await.unwrap();
        tokio::fs::write(root.join(noise).join("leak.txt"), "find_me")
            .await
            .unwrap();
    }

    let res = SearchFilesTool::new(root)
        .execute("c2", &json!({ "pattern": "find_me" }), &cancel)
        .await
        .unwrap();

    assert!(res.content.contains("src.rs:1: find_me"));
    for noise in [".git", "target", "node_modules", "checkpoints", ".cache"] {
        assert!(
            !res.content.contains(&format!("{noise}/leak.txt")),
            "'{noise}' directory must be skipped: {}",
            res.content
        );
    }
}

#[tokio::test]
async fn test_search_files_output_ceiling() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cancel = CancellationToken::new();

    // Create files with lines designed to exceed 100,000 characters before 200 lines
    for i in 0..250 {
        let line = format!("match_output_ceiling_token_{}: {}\n", i, "z".repeat(1000));
        tokio::fs::write(root.join(format!("file_{i}.txt")), line)
            .await
            .unwrap();
    }

    let res = SearchFilesTool::new(root)
        .execute(
            "c3",
            &json!({ "pattern": "match_output_ceiling_token" }),
            &cancel,
        )
        .await
        .unwrap();

    assert!(
        res.content.contains("characters of matches"),
        "must announce output character ceiling: {}",
        res.content
    );
}

#[tokio::test]
async fn patch_handles_crlf_and_trailing_whitespace_differences() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache = ReadCache::new();
    let cancel = CancellationToken::new();

    let patcher = PatchTool::new(root.clone(), cache.clone());
    let reader = ReadFileTool::new(root.clone(), 100_000, cache.clone());

    // File with trailing whitespace on line 2
    let file_path = root.join("whitespace.rs");
    tokio::fs::write(
        &file_path,
        "fn test() {\n    let val = 42;   \n    println!(\"{val}\");\n}\n",
    )
    .await
    .unwrap();

    // Read first
    reader
        .execute("r1", &json!({ "path": "whitespace.rs" }), &cancel)
        .await
        .unwrap();

    // Patch with old_string lacking trailing spaces on line 2
    let patch_res = patcher
        .execute(
            "p1",
            &json!({
                "path": "whitespace.rs",
                "old_string": "    let val = 42;\n    println!(\"{val}\");",
                "new_string": "    let val = 100;\n    println!(\"{val}\");"
            }),
            &cancel,
        )
        .await
        .unwrap();
    assert!(
        !patch_res.is_error,
        "Patch with trailing whitespace difference should succeed: {}",
        patch_res.content
    );

    let updated = tokio::fs::read_to_string(&file_path).await.unwrap();
    assert!(updated.contains("let val = 100;"));
}

#[tokio::test]
async fn file_tools_require_workspace_relative_paths() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache = ReadCache::new();
    let cancel = CancellationToken::new();

    let crates_dir = root.join("crates").join("my-crate");
    tokio::fs::create_dir_all(&crates_dir).await.unwrap();
    tokio::fs::write(crates_dir.join("lib.rs"), "pub fn hello() {}\n")
        .await
        .unwrap();

    let reader = ReadFileTool::new(root.clone(), 100_000, cache.clone());
    // Passing "my-crate/lib.rs" should fail because paths are strictly workspace-relative
    let res = reader
        .execute("r1", &json!({ "path": "my-crate/lib.rs" }), &cancel)
        .await
        .unwrap();
    assert!(
        res.is_error,
        "Should not guess subcrate path: {}",
        res.content
    );

    // Explicit workspace-relative path succeeds
    let res2 = reader
        .execute("r2", &json!({ "path": "crates/my-crate/lib.rs" }), &cancel)
        .await
        .unwrap();
    assert!(
        !res2.is_error,
        "Should resolve workspace-relative path: {}",
        res2.content
    );
    assert!(res2.content.contains("pub fn hello()"));
}
