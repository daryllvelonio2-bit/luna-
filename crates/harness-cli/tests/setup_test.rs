//! Tests for targeted `luna setup <target> [value]` commands.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

fn run_luna(args: &[&str], config_path: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_luna"));
    command.env("LUNA_CONFIG", config_path).args(args);
    command.output().expect("luna runs")
}

fn read_config(file: &Path) -> serde_yaml::Value {
    let raw = fs::read_to_string(file).expect("config file exists");
    serde_yaml::from_str(&raw).expect("valid YAML config")
}

#[test]
fn setup_memory_direct_value_updates_config() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_path = tmp.path().join("config.yaml");

    let out = run_luna(&["setup", "memory", "background"], &config_path);
    assert!(
        out.status.success(),
        "stdout: {}, stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("memory.mode = background"));

    let cfg = read_config(&config_path);
    assert_eq!(cfg["memory"]["mode"].as_str(), Some("background"));
    assert_eq!(cfg["first_run"].as_bool(), Some(false));

    // Now switch back to session
    let out = run_luna(&["setup", "mem", "session"], &config_path);
    assert!(out.status.success());
    let cfg = read_config(&config_path);
    assert_eq!(cfg["memory"]["mode"].as_str(), Some("session"));
}

#[test]
fn setup_notes_direct_value_updates_config() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_path = tmp.path().join("config.yaml");
    let notes_path = tmp.path().join("my-notes");

    let out = run_luna(
        &["setup", "notes", notes_path.to_str().unwrap()],
        &config_path,
    );
    assert!(
        out.status.success(),
        "stdout: {}, stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let cfg = read_config(&config_path);
    assert_eq!(
        cfg["paths"]["notes_dir"].as_str(),
        Some(notes_path.to_str().unwrap())
    );
    assert_eq!(cfg["first_run"].as_bool(), Some(false));
}

#[test]
fn setup_compact_direct_value_updates_config() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_path = tmp.path().join("config.yaml");

    let out = run_luna(&["setup", "compact", "off"], &config_path);
    assert!(
        out.status.success(),
        "stdout: {}, stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let cfg = read_config(&config_path);
    assert_eq!(cfg["auto_compact_enabled"].as_bool(), Some(false));
    assert_eq!(cfg["first_run"].as_bool(), Some(false));

    let out = run_luna(&["setup", "compact", "on"], &config_path);
    assert!(out.status.success());
    let cfg = read_config(&config_path);
    assert_eq!(cfg["auto_compact_enabled"].as_bool(), Some(true));
}

#[test]
fn setup_web_direct_value_updates_config() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_path = tmp.path().join("config.yaml");

    let out = run_luna(&["setup", "web", "duckduckgo"], &config_path);
    assert!(
        out.status.success(),
        "stdout: {}, stderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let cfg = read_config(&config_path);
    assert_eq!(cfg["tools"]["web"]["enabled"].as_bool(), Some(true));
    assert_eq!(
        cfg["tools"]["web"]["search_backend"].as_str(),
        Some("duckduckgo")
    );
    assert_eq!(cfg["first_run"].as_bool(), Some(false));

    let out = run_luna(&["setup", "web", "off"], &config_path);
    assert!(out.status.success());
    let cfg = read_config(&config_path);
    assert_eq!(cfg["tools"]["web"]["enabled"].as_bool(), Some(false));
}

#[test]
fn setup_unknown_target_returns_error() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let config_path = tmp.path().join("config.yaml");

    let out = run_luna(&["setup", "invalid-subsystem"], &config_path);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("unknown setup target"));
}
