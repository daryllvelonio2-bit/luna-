//! End-to-end tests for `luna exec`: the shipped binary, a local OpenAI-compatible endpoint, and
//! assertions on the NDJSON stream, the filesystem and the exit code.
//!
//! The mock server is a thread on 127.0.0.1 speaking just enough of the OpenAI shape for one tool
//! turn. That keeps the test hermetic — no credential, no network, no model — while still going
//! through clap, config resolution, the provider client, the gate and the tool registry.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luna-exec-test-{}-{}", std::process::id(), name));
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("wipe scratch dir");
    }
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

/// Serve `turns` responses in order: each request gets the next assistant message.
///
/// Deliberately ignores `stream: true` — the agent falls back to a buffered completion when a
/// stream yields nothing, which is exactly the path a plain JSON response exercises.
fn start_mock(script: Vec<serde_json::Value>) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
    let port = listener.local_addr().expect("local addr").port();
    let served = Arc::new(AtomicUsize::new(0));
    let counter = served.clone();

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));

            // Request line + headers.
            let mut content_length = 0usize;
            let mut is_post = false;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let trimmed = line.trim_end();
                if trimmed.is_empty() {
                    break;
                }
                if trimmed.starts_with("POST ") {
                    is_post = true;
                }
                if let Some(value) = trimmed.strip_prefix("content-length: ") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
                if let Some(value) = trimmed.strip_prefix("Content-Length: ") {
                    content_length = value.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            let _ = reader.read_exact(&mut body);

            let payload = if is_post {
                // The agent opens with a streaming request and falls back to a buffered one when
                // the stream yields nothing. A scripted turn must not be consumed by the throwaway
                // attempt, so only a non-streaming request advances the script.
                let streaming = serde_json::from_slice::<serde_json::Value>(&body)
                    .map(|request| request["stream"] == true)
                    .unwrap_or(false);
                let index = if streaming {
                    counter.load(Ordering::SeqCst)
                } else {
                    counter.fetch_add(1, Ordering::SeqCst)
                };
                let message = script
                    .get(index)
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({"role": "assistant", "content": "done"}));
                serde_json::json!({
                    "id": "chatcmpl-mock",
                    "object": "chat.completion",
                    "model": "mock-1",
                    "choices": [{"index": 0, "message": message, "finish_reason": "stop"}],
                    "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
                })
            } else {
                serde_json::json!({"data": [{"id": "mock-1"}]})
            };

            let body = serde_json::to_vec(&payload).expect("serialize payload");
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });

    (format!("http://127.0.0.1:{port}/v1"), served)
}

fn write_config(dir: &Path, base_url: &str, allow_write: bool) -> PathBuf {
    let luna = dir.join("luna");
    fs::create_dir_all(&luna).expect("luna dir");
    let workspace = dir.join("ws");
    fs::create_dir_all(&workspace).expect("workspace");

    fs::write(
        luna.join("credentials.json"),
        serde_json::json!({
            "providers": {
                "openai": {
                    "strategy": "round_robin",
                    "credentials": [{
                        "id": "c1",
                        "label": "mock",
                        "api_key": "sk-mock",
                        "is_priority": true,
                        "usage_count": 0,
                        "last_used": null,
                        "is_temporarily_disabled": false
                    }],
                    "current_round_robin_index": 0
                }
            }
        })
        .to_string(),
    )
    .expect("credentials");

    let permissions = if allow_write {
        "tools:\n  permissions:\n    - action: allow\n      tool: write_file\n"
    } else {
        ""
    };

    let config = format!(
        "model:\n  provider: openai\n  default: mock-1\nproviders:\n  openai:\n    base_url: {base_url}\npaths:\n  sessions_db: {luna}/sessions.db\n  memory_db: {luna}/memory.db\n  notes_db: {luna}/notes.db\n  credentials: {luna}/credentials.json\n  models_cache: {luna}/models_cache.json\n  jobs_dir: {luna}/jobs\n  skills_dir: {luna}/skills\n  checkpoints_dir: {luna}/checkpoints\n  workspace_root: {workspace}\n{permissions}",
        luna = luna.display(),
        workspace = workspace.display(),
    );
    let path = luna.join("config.yaml");
    fs::write(&path, config).expect("config");
    path
}

fn run_exec(dir: &Path, config: &Path, args: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_luna"));
    command
        .current_dir(dir.join("ws"))
        .env("LUNA_CONFIG", config)
        .env_remove("OPENAI_API_KEY")
        .args(args);
    command.output().expect("luna exec runs")
}

fn events(stdout: &[u8]) -> Vec<serde_json::Value> {
    String::from_utf8_lossy(stdout)
        .lines()
        .filter(|line| line.trim_start().starts_with('{'))
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{e}: {line}")))
        .collect()
}

fn write_turn() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "role": "assistant",
            "content": "writing",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "write_file",
                    "arguments": serde_json::to_string(&serde_json::json!({
                        "path": "report.md",
                        "content": "# report\n"
                    })).unwrap()
                }
            }]
        }),
        serde_json::json!({"role": "assistant", "content": "wrote report.md"}),
    ]
}

#[test]
fn json_mode_emits_an_event_stream_and_the_answer() {
    let dir = scratch("json");
    let (base_url, _served) = start_mock(write_turn());
    let config = write_config(&dir, &base_url, true);

    let output = run_exec(&dir, &config, &["exec", "--json", "write the report"]);
    assert!(
        output.status.success(),
        "exec must exit 0\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let events = events(&output.stdout);
    let kinds: Vec<&str> = events
        .iter()
        .map(|event| event["type"].as_str().unwrap_or("?"))
        .collect();
    assert_eq!(kinds.first(), Some(&"start"), "the stream opens with start");
    assert_eq!(kinds.last(), Some(&"result"), "and closes with result");
    assert!(
        kinds.contains(&"tool_call") && kinds.contains(&"tool_result"),
        "the tool turn must be visible in the stream: {kinds:?}"
    );

    let start = &events[0];
    assert_eq!(start["provider"], "openai");
    assert!(start["session"].as_str().unwrap_or("").starts_with('s'));

    let result = events.last().unwrap();
    assert_eq!(result["ok"], true);
    assert_eq!(result["answer"], "wrote report.md");
    assert_eq!(result["tool_calls"][0]["name"], "write_file");
    assert_eq!(result["tool_calls"][0]["denied"], false);

    assert_eq!(
        fs::read_to_string(dir.join("ws").join("report.md")).expect("the file was written"),
        "# report\n"
    );
}

#[test]
fn plain_mode_puts_only_the_answer_on_stdout() {
    let dir = scratch("plain");
    let (base_url, _served) = start_mock(write_turn());
    let config = write_config(&dir, &base_url, true);

    let output = run_exec(&dir, &config, &["exec", "write the report"]);
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "wrote report.md",
        "stdout must be the answer alone, so $(luna exec ...) is usable"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("write_file"),
        "the tool trace belongs on stderr"
    );
}

#[test]
fn a_denied_write_fails_the_run_and_leaves_no_file() {
    let dir = scratch("denied");
    let (base_url, _served) = start_mock(write_turn());
    // No `allow` rule and no --yes: headless is fail-closed.
    let config = write_config(&dir, &base_url, false);

    let output = run_exec(&dir, &config, &["exec", "--json", "write the report"]);
    assert_eq!(
        output.status.code(),
        Some(1),
        "a denial must not look like success\nstdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );

    let events = events(&output.stdout);
    let result = events.last().expect("a result event");
    assert_eq!(result["type"], "result");
    assert_eq!(result["ok"], false);
    assert_eq!(result["tool_calls"][0]["denied"], true);
    assert!(!dir.join("ws").join("report.md").exists());
}

#[test]
fn yes_approves_mutations_without_a_rule() {
    let dir = scratch("yes");
    let (base_url, _served) = start_mock(write_turn());
    let config = write_config(&dir, &base_url, false);

    let output = run_exec(&dir, &config, &["exec", "--json", "--yes", "write the report"]);
    assert!(
        output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read_to_string(dir.join("ws").join("report.md")).expect("written"),
        "# report\n"
    );
}

#[test]
fn the_prompt_can_come_from_stdin() {
    let dir = scratch("stdin");
    let (base_url, served) = start_mock(write_turn());
    let config = write_config(&dir, &base_url, true);

    let mut child = Command::new(env!("CARGO_BIN_EXE_luna"))
        .current_dir(dir.join("ws"))
        .env("LUNA_CONFIG", &config)
        .args(["exec", "--json", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn exec");

    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b"write the report\n")
        .expect("write prompt");
    let output = child.wait_with_output().expect("wait");

    assert!(output.status.success());
    let events = events(&output.stdout);
    assert_eq!(events[0]["prompt"], "write the report\n");
    assert!(served.load(Ordering::SeqCst) > 0, "the model was called");
}

#[test]
fn a_session_can_be_resumed_and_replays_its_history() {
    let dir = scratch("session");
    let (base_url, _served) = start_mock(write_turn());
    let config = write_config(&dir, &base_url, true);

    let first = run_exec(&dir, &config, &["exec", "--json", "write the report"]);
    assert!(first.status.success());
    let session = events(&first.stdout)[0]["session"]
        .as_str()
        .expect("session id")
        .to_string();

    let second = run_exec(
        &dir,
        &config,
        &["exec", "--json", "--session", &session, "and summarise it"],
    );
    assert!(second.status.success());
    let events = events(&second.stdout);
    assert_eq!(events[0]["session"], session, "same session id");
    let result = events.last().expect("result");
    assert!(
        result["replayed"].as_u64().unwrap_or(0) > 0,
        "a resumed session replays its history: {result}"
    );
}

#[test]
fn an_empty_prompt_is_refused() {
    let dir = scratch("empty");
    let (_base_url, _served) = start_mock(vec![]);
    let config = write_config(&dir, "http://127.0.0.1:9/v1", true);

    let mut child = Command::new(env!("CARGO_BIN_EXE_luna"))
        .current_dir(dir.join("ws"))
        .env("LUNA_CONFIG", &config)
        .args(["exec", "--json", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn exec");
    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("empty prompt"), "stderr: {stderr}");
}
