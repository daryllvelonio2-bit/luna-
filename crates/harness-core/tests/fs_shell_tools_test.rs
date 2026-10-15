use std::sync::Arc;
use serde_json::json;
use tempfile::tempdir;
use tokio_util::sync::CancellationToken;

use harness_core::tools::{
    register_core_tools, PatchTool, ReadCache, ReadFileTool, SearchFilesTool, TerminalTool,
    WriteFileTool,
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
    assert!(
        res.content.contains("[Truncated: character limit reached]"),
        "Must append truncation notice"
    );
    assert!(res.content.len() <= 1_200);
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

#[tokio::test]
async fn test_mutating_declarations() {
    let dir = tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let cache = ReadCache::new();

    let reader = ReadFileTool::new(root.clone(), 100_000, cache.clone());
    let writer = WriteFileTool::new(root.clone(), cache.clone());
    let patcher = PatchTool::new(root.clone(), cache.clone());
    let searcher = SearchFilesTool::new(root.clone());
    let term = TerminalTool::new(root.clone(), 10, 10_000);

    assert!(!reader.is_mutating(), "read_file must be non-mutating");
    assert!(!searcher.is_mutating(), "search_files must be non-mutating");
    assert!(writer.is_mutating(), "write_file must be mutating");
    assert!(patcher.is_mutating(), "patch must be mutating");
    assert!(term.is_mutating(), "terminal must be mutating");
}

#[tokio::test]
async fn test_registry_contains_all_five_tools() {
    let mut config = Config::default();
    let dir = tempdir().unwrap();
    config.paths.workspace_root = dir.path().to_path_buf();

    let registry: Arc<dyn ToolRegistry> = Arc::new(DefaultToolRegistry::new());
    let cache = ReadCache::new();

    register_core_tools(&registry, &config, cache).await.unwrap();

    let tools = registry.list_tools().await.unwrap();
    let tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();

    assert_eq!(tool_names.len(), 5);
    assert!(tool_names.contains(&"read_file".to_string()));
    assert!(tool_names.contains(&"write_file".to_string()));
    assert!(tool_names.contains(&"patch".to_string()));
    assert!(tool_names.contains(&"search_files".to_string()));
    assert!(tool_names.contains(&"terminal".to_string()));
}
