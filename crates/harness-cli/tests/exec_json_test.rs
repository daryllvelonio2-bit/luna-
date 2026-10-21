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

    // A price for the mock model, so the ledger reports a dollar figure rather than "unpriced".
    let pricing = "usage:\n  prices:\n    openai/mock-1: { input_per_million: 1.0, output_per_million: 2.0 }\n";

    let hooks = "hooks:\n  enabled: true\n";

    let permissions = if allow_write {
        "tools:\n  permissions:\n    - action: allow\n      tool: write_file\n"
    } else {
        ""
    };

    let config = format!(
        "model:\n  provider: openai\n  default: mock-1\nproviders:\n  openai:\n    base_url: {base_url}\npaths:\n  sessions_db: {luna}/sessions.db\n  memory_db: {luna}/memory.db\n  notes_db: {luna}/notes.db\n  credentials: {luna}/credentials.json\n  models_cache: {luna}/models_cache.json\n  jobs_dir: {luna}/jobs\n  skills_dir: {luna}/skills\n  checkpoints_dir: {luna}/checkpoints\n  usage_db: {luna}/usage.db\n  workspace_root: {workspace}\n{pricing}{hooks}{permissions}",
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
        // A scratch `$HOME`, so the host's `~/.luna/hooks.json` and credentials can never decide
        // what a test does.
        .env("HOME", dir.join("home"))
        .env_remove("OPENAI_API_KEY")
        .args(args);
    command.output().expect("luna exec runs")
}

/// Run any `luna` subcommand against `dir`'s config and a scratch `$HOME`.
fn run_luna(dir: &Path, config: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_luna"))
        .current_dir(dir.join("ws"))
        .env("LUNA_CONFIG", config)
        .env("HOME", dir.join("home"))
        .args(args)
        .output()
        .expect("luna runs")
}

/// Write `$HOME/.luna/hooks.json` for the test's scratch home.
fn user_hooks(dir: &Path, body: serde_json::Value) {
    let hooks_dir = dir.join("home").join(".luna");
    fs::create_dir_all(&hooks_dir).expect("hooks dir");
    fs::write(
        hooks_dir.join("hooks.json"),
        serde_json::to_string_pretty(&body).expect("hooks json"),
    )
    .expect("hooks file");
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
        result["usage"]["total_tokens"], 30,
        "two model calls at 10 in / 5 out each"
    );

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

#[test]
fn the_turn_is_recorded_in_the_usage_ledger() {
    let dir = scratch("usage");
    let (base_url, _served) = start_mock(write_turn());
    let config = write_config(&dir, &base_url, true);

    let output = run_exec(&dir, &config, &["exec", "--json", "write the report"]);
    assert!(output.status.success());

    let usage = Command::new(env!("CARGO_BIN_EXE_luna"))
        .current_dir(dir.join("ws"))
        .env("LUNA_CONFIG", &config)
        .args(["usage", "--json"])
        .output()
        .expect("luna usage runs");
    assert!(
        usage.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&usage.stderr)
    );

    let report: serde_json::Value =
        serde_json::from_slice(&usage.stdout).expect("usage --json is one JSON object");
    assert_eq!(report["totals"]["turns"], 1, "one turn, one row: {report}");
    assert_eq!(
        report["totals"]["unpriced_turns"], 0,
        "usage.prices covers the mock model: {report}"
    );
    assert_eq!(
        report["totals"]["total_tokens"], 30,
        "the two model calls' tokens are summed: {report}"
    );
    // 20 in at $1/M + 10 out at $2/M
    let cost = report["totals"]["cost_usd"].as_f64().unwrap_or(-1.0);
    assert!((cost - 0.00004).abs() < 1e-9, "got {cost}");
    assert_eq!(report["by_model"][0]["key"], "openai/mock-1");

    let recent = Command::new(env!("CARGO_BIN_EXE_luna"))
        .current_dir(dir.join("ws"))
        .env("LUNA_CONFIG", &config)
        .args(["usage", "--recent", "--json", "--limit", "5"])
        .output()
        .expect("luna usage --recent runs");
    assert!(recent.status.success());
    let rows: Vec<serde_json::Value> = String::from_utf8_lossy(&recent.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("a JSON row"))
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["source"], "exec");
}

#[test]
fn exec_runs_the_same_hooks_the_repl_does() {
    let dir = scratch("hooks");
    let (base_url, _served) = start_mock(write_turn());
    let config = write_config(&dir, &base_url, true);
    let markers = dir.join("markers.log");

    // Three hooks, one per contract: the session brackets fire, and a `pre_tool_use` rewrite
    // changes what the tool actually does. Before parity, `luna exec` ran none of them.
    user_hooks(
        &dir,
        serde_json::json!({
            "hooks": {
                "session_start": [{"command": format!("echo start >> {}", markers.display())}],
                "pre_tool_use": [{
                    "matcher": "write_file",
                    // No `\n` in the content: `printf` would turn it into a real newline and the
                    // payload would stop being JSON — which is exactly the failure the runner now
                    // warns about.
                    "command": "printf '{\"updatedInput\":{\"path\":\"redirected.md\",\"content\":\"# redirected\"}}'"
                }],
                "session_end": [{"command": format!("echo end >> {}", markers.display())}]
            }
        }),
    );

    let output = run_exec(&dir, &config, &["exec", "write the report"]);
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let ws = dir.join("ws");
    assert!(
        ws.join("redirected.md").is_file(),
        "the hook rewrote the path\nstdout:\n{}\nstderr:\n{}\nws: {:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        fs::read_dir(&ws).map(|entries| entries.filter_map(|e| e.ok().map(|e| e.file_name())).collect::<Vec<_>>()),
    );
    assert!(!ws.join("report.md").exists(), "the original path was never written");

    let log = fs::read_to_string(&markers).expect("session hooks wrote markers");
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(
        lines,
        vec!["start", "end"],
        "session_start runs before the turn and session_end after it"
    );
}

#[test]
fn hooks_command_reports_what_is_actually_loaded() {
    let dir = scratch("hooks-list");
    let (base_url, _served) = start_mock(vec![]);
    let config = write_config(&dir, &base_url, true);

    let empty = run_luna(&dir, &config, &["hooks", "--json"]);
    assert!(empty.status.success());
    let report: serde_json::Value = serde_json::from_slice(&empty.stdout).expect("json report");
    assert_eq!(report["hooks"].as_array().map(|hooks| hooks.len()), Some(0));

    user_hooks(
        &dir,
        serde_json::json!({
            "hooks": {
                "pre_tool_use": [{"matcher": "write_*", "command": "echo guarded"}],
                "stop": [{"command": "echo bye"}]
            }
        }),
    );

    let listed = run_luna(&dir, &config, &["hooks", "--json"]);
    assert!(listed.status.success());
    let report: serde_json::Value = serde_json::from_slice(&listed.stdout).expect("json report");
    let hooks = report["hooks"].as_array().expect("a hook array");
    assert_eq!(hooks.len(), 2);
    assert!(hooks.iter().any(|hook| hook["event"] == "pre_tool_use" && hook["matcher"] == "write_*"));
    assert!(hooks.iter().any(|hook| hook["event"] == "stop"));
    assert_eq!(report["enabled"], true);
}

fn read_turn(path: &str) -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "role": "assistant",
            "content": "reading",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {
                    "name": "read_file",
                    "arguments": serde_json::to_string(&serde_json::json!({"path": path})).unwrap()
                }
            }]
        }),
        serde_json::json!({"role": "assistant", "content": "done"}),
    ]
}

#[test]
fn the_workspace_is_a_boundary_even_when_the_gate_says_yes() {
    let dir = scratch("sandbox");
    let (base_url, _served) = start_mock(read_turn("/etc/passwd"));
    let config = write_config(&dir, &base_url, true);

    // `--yes` approves every mutation. It must not widen what a tool can reach: the sandbox is a
    // property of the path, not a human decision.
    let output = run_exec(&dir, &config, &["exec", "--json", "--yes", "read /etc/passwd"]);
    let escape_events = events(&output.stdout);
    let tool_result = escape_events
        .iter()
        .find(|event| event["type"] == "tool_result")
        .expect("the call ran and reported");
    assert_eq!(tool_result["ok"], false, "escaping the workspace is an error");
    assert!(
        tool_result["output"]
            .as_str()
            .unwrap_or_default()
            .contains("escapes workspace root"),
        "got: {}",
        tool_result["output"]
    );

    // A symlinked parent cannot smuggle a path out either.
    std::os::unix::fs::symlink("/etc", dir.join("ws").join("etc-link")).expect("symlink");
    let (base_url, _served) = start_mock(read_turn("etc-link/passwd"));
    let config = write_config(&dir, &base_url, true);
    let output = run_exec(&dir, &config, &["exec", "--json", "--yes", "read the link"]);
    let link_events = events(&output.stdout);
    let tool_result = link_events
        .iter()
        .find(|event| event["type"] == "tool_result")
        .expect("the call ran and reported");
    assert_eq!(tool_result["ok"], false, "a symlinked parent must not escape");
    assert!(
        tool_result["output"]
            .as_str()
            .unwrap_or_default()
            .contains("escapes workspace root"),
        "got: {}",
        tool_result["output"]
    );
}

#[test]
fn paths_extra_roots_widens_the_sandbox_deliberately() {
    let dir = scratch("extra-roots");
    let shared = dir.join("shared");
    fs::create_dir_all(&shared).expect("shared dir");
    let outside = shared.join("allowed.txt");
    fs::write(&outside, "shared notes").expect("write shared file");

    // Without the extra root: refused, because the workspace is the sandbox.
    let (base_url, _served) = start_mock(read_turn(outside.to_str().unwrap()));
    let config = write_config(&dir, &base_url, true);
    let refused = run_exec(&dir, &config, &["exec", "--json", "read the shared file"]);
    let refused_events = events(&refused.stdout);
    let result = refused_events
        .iter()
        .find(|event| event["type"] == "tool_result")
        .expect("reported");
    assert_eq!(result["ok"], false, "outside the workspace and not opted in");
    assert!(result["output"]
        .as_str()
        .unwrap_or_default()
        .contains("escapes workspace root"));

    // With it: readable, because the operator said so under `paths`, not under `tools`.
    let (base_url, _served) = start_mock(read_turn(outside.to_str().unwrap()));
    let config = write_config(&dir, &base_url, true);
    let text = fs::read_to_string(&config).expect("config");
    let with_root = text.replace(
        &format!("  workspace_root: {}", dir.join("ws").display()),
        &format!(
            "  workspace_root: {}\n  extra_roots:\n    - {}",
            dir.join("ws").display(),
            shared.display()
        ),
    );
    assert_ne!(with_root, text, "the config rewrite must have found the paths block");
    fs::write(&config, with_root).expect("config with extra roots");

    let allowed = run_exec(&dir, &config, &["exec", "--json", "read the shared file"]);
    let allowed_events = events(&allowed.stdout);
    let result = allowed_events
        .iter()
        .find(|event| event["type"] == "tool_result")
        .expect("reported");
    assert_eq!(
        result["ok"], true,
        "an opted-in root is reachable: {}",
        result["output"]
    );
    assert!(result["output"]
        .as_str()
        .unwrap_or_default()
        .contains("shared notes"));
}
