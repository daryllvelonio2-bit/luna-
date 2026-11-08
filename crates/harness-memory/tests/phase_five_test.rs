use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use harness_memory::{
    all_mcp_tools, choose_write_route, handle_mcp_jsonrpc, parse_write_routing_policy,
    render_shared_brain_rules, run_hook, run_instructions, DaemonRuntime, MemoryStore,
    NativeDaemonClient, QueueStore, StoreConfig, WriteRoutingPolicy, WriteRoutingTarget,
    LOCK_REFUSAL_ERROR_CLASS, MAX_ATTEMPTS,
};
use serde_json::json;

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luna-phase5-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn test_write_routing_policy_logic() {
    assert_eq!(
        parse_write_routing_policy("direct", false).unwrap(),
        WriteRoutingPolicy::Direct
    );
    assert_eq!(
        parse_write_routing_policy("prefer", false).unwrap(),
        WriteRoutingPolicy::Prefer
    );
    assert_eq!(
        parse_write_routing_policy("require", false).unwrap(),
        WriteRoutingPolicy::Require
    );
    assert_eq!(
        parse_write_routing_policy("1", true).unwrap(),
        WriteRoutingPolicy::Prefer
    );
    assert_eq!(
        parse_write_routing_policy("0", true).unwrap(),
        WriteRoutingPolicy::Direct
    );

    // Direct
    let d1 = choose_write_route(WriteRoutingPolicy::Direct, true, true);
    assert_eq!(d1.target, WriteRoutingTarget::Direct);
    assert_eq!(d1.reason, "policy-direct");

    // Prefer with daemon available
    let d2 = choose_write_route(WriteRoutingPolicy::Prefer, true, false);
    assert_eq!(d2.target, WriteRoutingTarget::Daemon);
    assert_eq!(d2.reason, "daemon-available");

    // Prefer without daemon available but can start
    let d3 = choose_write_route(WriteRoutingPolicy::Prefer, false, true);
    assert_eq!(d3.target, WriteRoutingTarget::Daemon);
    assert!(d3.auto_start_daemon);
    assert_eq!(d3.reason, "daemon-auto-start");

    // Prefer without daemon and cannot start -> fallback to direct
    let d4 = choose_write_route(WriteRoutingPolicy::Prefer, false, false);
    assert_eq!(d4.target, WriteRoutingTarget::Direct);
    assert_eq!(d4.reason, "daemon-unavailable-fallback");

    // Require without daemon and cannot start -> blocked
    let d5 = choose_write_route(WriteRoutingPolicy::Require, false, false);
    assert_eq!(d5.target, WriteRoutingTarget::Blocked);
    assert_eq!(d5.reason, "daemon-required-unavailable");
}

#[test]
fn test_queue_store_lifecycle_and_dedupe() {
    let root = temp_root("queue-store");
    let queue_file = root.join("queue.sqlite3");
    let queue = QueueStore::new(queue_file).unwrap();

    // 1. Enqueue with deduplication key
    let job1 = queue
        .enqueue("sync", &json!({ "dry_run": false }), Some("dedupe-sync"), 10)
        .unwrap();
    assert_eq!(job1.kind, "sync");
    assert_eq!(job1.state, "queued");
    assert_eq!(job1.priority, 10);

    // Identical dedupe key while active returns the existing job
    let job1_dup = queue
        .enqueue("sync", &json!({ "dry_run": true }), Some("dedupe-sync"), 5)
        .unwrap();
    assert_eq!(job1.id, job1_dup.id);

    // 2. Claim next
    let exclude = HashSet::new();
    let claimed = queue.claim_next(&exclude).unwrap().unwrap();
    assert_eq!(claimed.id, job1.id);
    assert_eq!(claimed.state, "running");
    assert_eq!(claimed.attempts, 1);

    // 3. Defer on lock refusal
    let deferred = queue
        .defer(
            &claimed.id,
            claimed.started_at.as_deref(),
            Some(&json!({ "error_class": LOCK_REFUSAL_ERROR_CLASS })),
        )
        .unwrap();
    assert_eq!(deferred.state, "queued");
    assert_eq!(deferred.attempts, 0); // refund attempt on lock refusal

    // 4. Re-claim and finish
    let claimed_again = queue.claim_next(&exclude).unwrap().unwrap();
    assert_eq!(claimed_again.id, job1.id);
    let finished = queue
        .finish(
            &claimed_again.id,
            "succeeded",
            Some(&json!({ "success": true })),
            None,
            true,
        )
        .unwrap();
    assert_eq!(finished.state, "succeeded");

    let counts = queue.counts().unwrap();
    assert_eq!(*counts.get("succeeded").unwrap(), 1);

    // 5. Test recover_running dead-letters max attempts
    let doomed = queue
        .enqueue("diary_write", &json!({ "entry": "x" }), None, 0)
        .unwrap();
    // Simulate MAX_ATTEMPTS claims that crashed while running
    queue
        .set_job_state_for_test(&doomed.id, "running", MAX_ATTEMPTS)
        .unwrap();
    let re_queued = queue.recover_running().unwrap();
    assert_eq!(re_queued, 0); // not re-queued
    let after_doomed = queue.get(&doomed.id).unwrap();
    assert_eq!(after_doomed.state, "failed");
    assert_eq!(after_doomed.attempts, MAX_ATTEMPTS);
}

#[tokio::test]
async fn test_daemon_native_endpoint_and_client() {
    let root = temp_root("daemon-runtime");
    let port = 19876;
    let runtime = DaemonRuntime::new(root.clone()).unwrap();

    // Run daemon servers in background tokio task
    let runtime_handle = tokio::spawn(async move {
        let _ = runtime.run_servers(port).await;
    });

    // Wait briefly for daemon listener to bind
    tokio::time::sleep(Duration::from_millis(250)).await;

    // Connect via native Luna connection
    let client = NativeDaemonClient::connect_root(&root).unwrap();

    // 1. Native ping
    assert!(client.is_alive().await);
    let ping_val = client.ping().await.unwrap();
    assert_eq!(ping_val.get("ok").unwrap(), true);
    assert_eq!(ping_val.get("worker_alive").unwrap(), true);

    // 2. Enqueue job over native IPC
    let job = client
        .enqueue(
            "sync",
            json!({ "dry_run": false }),
            Some("test-sync-native"),
            5,
        )
        .await
        .unwrap();
    assert_eq!(job.kind, "sync");

    // 3. Wait for worker to execute and finish job
    let mut finished = false;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let j = client.get_job(&job.id).await.unwrap();
        if j.state == "succeeded" {
            finished = true;
            break;
        }
    }
    assert!(finished, "job should have finished as succeeded");

    // 4. Native status check
    let counts = client.counts().await.unwrap();
    assert!(counts.get("succeeded").copied().unwrap_or(0) >= 1);

    // 5. Native shutdown
    client.shutdown().await.unwrap();
    let _ = runtime_handle.await;
}

#[test]
fn test_mcp_jsonrpc_protocol_tools() {
    let root = temp_root("mcp-server");
    let store = MemoryStore::open(StoreConfig::new(root)).unwrap();

    // 1. Tools list
    let tools = all_mcp_tools();
    assert!(tools.len() >= 40);
    assert!(tools.iter().any(|t| t.name == "memory_status"));
    assert!(tools.iter().any(|t| t.name == "memory_search"));
    assert!(tools.iter().any(|t| t.name == "memory_add_record"));
    assert!(tools.iter().any(|t| t.name == "memory_kg_add"));
    assert!(tools.iter().any(|t| t.name == "memory_event_append"));

    // 2. JSON-RPC initialize
    let init_req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize"
    });
    let init_resp = handle_mcp_jsonrpc(&store, &init_req);
    assert_eq!(init_resp["id"], 1);
    assert!(init_resp["result"]["capabilities"]["tools"].is_object());

    // 3. JSON-RPC tools/list
    let list_req = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list"
    });
    let list_resp = handle_mcp_jsonrpc(&store, &list_req);
    assert!(list_resp["result"]["tools"].as_array().unwrap().len() >= 40);

    // 4. JSON-RPC tools/call memory_add_record
    let add_req = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "memory_add_record",
            "arguments": {
                "domain": "projects",
                "topic": "luna",
                "body": "Luna autonomous agent architecture with native daemon endpoint.",
                "subject": "project.luna",
                "predicate": "architecture",
                "object": "native daemon"
            }
        }
    });
    let add_resp = handle_mcp_jsonrpc(&store, &add_req);
    assert!(add_resp["result"]["content"][0]["text"].as_str().unwrap().contains("success"));

    // 5. JSON-RPC tools/call memory_search
    let search_req = json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "memory_search",
            "arguments": {
                "query": "native daemon architecture"
            }
        }
    });
    let search_resp = handle_mcp_jsonrpc(&store, &search_req);
    let search_text = search_resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(search_text.contains("project.luna"));

    // 6. JSON-RPC tools/call memory_status
    let status_req = json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {
            "name": "memory_status",
            "arguments": {}
        }
    });
    let status_resp = handle_mcp_jsonrpc(&store, &status_req);
    let status_text = status_resp["result"]["content"][0]["text"].as_str().unwrap();
    assert!(status_text.contains("records"));
}

#[test]
fn test_semantic_lifecycle_hooks() {
    let root = temp_root("lifecycle-hooks");
    let store = MemoryStore::open(StoreConfig::new(root)).unwrap();

    // 1. Session start hook -> returns context addition
    let start_input = json!({
        "session_id": "session-123",
        "cwd": "/home/janelle"
    });
    let start_out = run_hook(
        "session-start",
        "claude-code",
        &start_input.to_string(),
        &store,
    )
    .unwrap();
    assert!(start_out.contains("system_prompt_addition"));

    // 2. Stop hook with low exchange count -> pass
    let stop_input_low = json!({
        "session_id": "session-123",
        "exchange_count": 3
    });
    let stop_out_low = run_hook("stop", "claude-code", &stop_input_low.to_string(), &store).unwrap();
    assert!(stop_out_low.contains("pass"));

    // 3. Stop hook exceeding save interval (>= 10) -> save_triggered
    let stop_input_high = json!({
        "session_id": "session-123",
        "exchange_count": 12
    });
    let stop_out_high =
        run_hook("stop", "claude-code", &stop_input_high.to_string(), &store).unwrap();
    assert!(stop_out_high.contains("save_triggered"));

    // 4. Session end hook -> cleans up session state and writes journal
    let end_input = json!({
        "session_id": "session-123"
    });
    let end_out = run_hook("session-end", "claude-code", &end_input.to_string(), &store).unwrap();
    assert!(end_out.contains("session_ended"));

    // 5. Precompact hook -> reindexes
    let precompact_out = run_hook("precompact", "claude-code", "{}", &store).unwrap();
    assert!(precompact_out.contains("precompact_mined"));
}

#[test]
fn test_instructions_and_shared_brain_rules() {
    let init_doc = run_instructions("init").unwrap();
    assert!(init_doc.contains("Luna Memory Init"));

    let search_doc = run_instructions("search").unwrap();
    assert!(search_doc.contains("Luna Memory Search"));

    let rules = render_shared_brain_rules("linux", "claude-code", "harness", "full").unwrap();
    assert!(rules.contains("linux:claude-code:harness"));
    assert!(rules.contains("memory_search"));

    let rules_light = render_shared_brain_rules("linux", "claude-code", "harness", "light").unwrap();
    assert!(rules_light.contains("memory_query FIND"));
}
