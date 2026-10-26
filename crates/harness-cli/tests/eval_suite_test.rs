//! End-to-end tests for the `luna eval` regression gate.
//!
//! These drive the real binary (the shipped entry point) against the committed suite in
//! `tests/eval`, so `cargo test --workspace` covers the offline suite without a credential, a
//! network call or a model.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("workspace root resolves")
}

fn suite_dir() -> PathBuf {
    repo_root().join("tests").join("eval")
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("luna-eval-test-{}-{}", std::process::id(), name));
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("wipe scratch dir");
    }
    dir
}

fn run_eval(args: &[&str], home: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_luna"));
    command.current_dir(repo_root()).args(args);
    if let Some(home) = home {
        command.env("HOME", home);
    }
    command.output().expect("luna eval runs")
}

fn report_at(path: &Path) -> serde_json::Value {
    let raw = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {}", path.display(), e));
    serde_json::from_str(&raw).expect("eval-report.json parses")
}

const REQUIRED_CASES: [&str; 12] = [
    "read-only-turn",
    "write-then-read",
    "denied-approval",
    "failing-tool-recovery",
    "malformed-tool-arguments",
    "permission-rule-deny",
    "permission-rule-allow",
    "missing-file-read-recovery",
    "failure-counter-reset",
    "denied-call-not-a-failure",
    "sandbox-path-escape",
    "multi-call-single-turn",
];

#[test]
fn offline_suite_passes_and_the_report_identifies_the_backend() {
    let scratch = scratch("offline");
    fs::create_dir_all(&scratch).expect("scratch");
    let report_path = scratch.join("eval-report.json");

    let output = run_eval(
        &[
            "eval",
            "run",
            "--suite",
            suite_dir().to_str().expect("utf-8 suite path"),
            "--offline",
            "--report",
            report_path.to_str().expect("utf-8 report path"),
        ],
        None,
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    assert!(
        output.status.success(),
        "offline suite must exit 0\nstdout:\n{}\nstderr:\n{}",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );

    let report = report_at(&report_path);
    assert_eq!(report["backend"], "offline");
    assert_eq!(report["provider"], "offline");
    assert_eq!(report["model"], "scripted-v1");
    assert_eq!(report["total"], REQUIRED_CASES.len());
    assert_eq!(report["failed"], 0);
    assert_eq!(report["skipped"], 0);

    let cases = report["cases"].as_array().expect("cases array");
    for required in REQUIRED_CASES {
        let case = cases
            .iter()
            .find(|c| c["case"] == required)
            .unwrap_or_else(|| panic!("case '{}' is missing from the report", required));
        assert_eq!(
            case["status"], "pass",
            "case '{}' failed: {}",
            required, case["reason"]
        );
        assert!(
            stdout.contains(required),
            "stdout must print one line per case, '{}' is absent:\n{}",
            required,
            stdout
        );
    }

    // The report has to carry what the card asks for: observed calls, tokens, wall time and identity.
    let write_then_read = cases
        .iter()
        .find(|c| c["case"] == "write-then-read")
        .expect("write-then-read case");
    let observed = write_then_read["tool_calls"]
        .as_array()
        .expect("tool calls");
    assert_eq!(
        observed.len(),
        2,
        "write-then-read issues exactly two calls"
    );
    assert_eq!(observed[0]["name"], "eval_write_file");
    assert_eq!(observed[1]["name"], "eval_read_file");
    assert_eq!(observed[0]["error"], false);
    assert!(
        write_then_read["tokens"]["total_tokens"]
            .as_u64()
            .unwrap_or(0)
            > 0,
        "offline accounting must report tokens"
    );
    assert!(write_then_read["wall_time_ms"].is_u64());
    assert_eq!(write_then_read["provider"], "offline");
    assert_eq!(write_then_read["model"], "scripted-v1");

    let read_only = cases
        .iter()
        .find(|c| c["case"] == "read-only-turn")
        .expect("read-only-turn case");
    assert_eq!(
        read_only["tool_calls"].as_array().expect("calls").len(),
        0,
        "a read-only turn dispatches nothing"
    );

    let denied = cases
        .iter()
        .find(|c| c["case"] == "denied-approval")
        .expect("denied-approval case");
    assert_eq!(denied["tool_calls"][0]["denied"], true);

    fs::remove_dir_all(&scratch).expect("clean scratch");
}

#[test]
fn a_broken_expectation_fails_the_suite_and_exits_non_zero() {
    let scratch = scratch("broken");
    let suite = scratch.join("suite");
    let report_path = scratch.join("eval-report.json");
    fs::create_dir_all(&suite).expect("suite dir");
    fs::write(
        suite.join("broken.yaml"),
        "case: deliberately-broken\n\
         prompt: Answer without calling a tool.\n\
         script:\n\
         \x20 - answer: The sky is blue.\n\
         expect:\n\
         \x20 final_answer_contains:\n\
         \x20   - The sky is green.\n",
    )
    .expect("write case");

    let output = run_eval(
        &[
            "eval",
            "run",
            "--suite",
            suite.to_str().expect("utf-8"),
            "--offline",
            "--report",
            report_path.to_str().expect("utf-8"),
        ],
        None,
    );

    assert_eq!(
        output.status.code(),
        Some(1),
        "a failing case must exit non-zero\nstdout:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let report = report_at(&report_path);
    assert_eq!(report["failed"], 1);
    assert_eq!(report["cases"][0]["status"], "fail");
    assert!(
        report["cases"][0]["reason"]
            .as_str()
            .unwrap_or_default()
            .contains("The sky is green"),
        "the report must name the assertion that failed: {}",
        report["cases"][0]["reason"]
    );

    fs::remove_dir_all(&scratch).expect("clean scratch");
}

#[test]
fn a_live_run_without_a_credential_is_skipped_not_failed() {
    let scratch = scratch("no-credential");
    fs::create_dir_all(&scratch).expect("scratch");
    let report_path = scratch.join("eval-report.json");

    let output = run_eval(
        &[
            "eval",
            "run",
            "--suite",
            suite_dir().to_str().expect("utf-8"),
            "--live",
            "--report",
            report_path.to_str().expect("utf-8"),
        ],
        Some(&scratch),
    );
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();

    assert!(
        output.status.success(),
        "a skipped live backend still exits 0\nstdout:\n{}",
        stdout
    );
    assert!(
        stdout.contains("skipped: no credential"),
        "stdout must say why the cases were skipped:\n{}",
        stdout
    );
    let report = report_at(&report_path);
    assert_eq!(report["backend"], "live");
    assert_eq!(report["failed"], 0);
    assert_eq!(report["skipped"], report["total"]);

    fs::remove_dir_all(&scratch).expect("clean scratch");
}
