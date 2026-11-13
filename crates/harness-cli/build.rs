//! Stamp the binary with the commit it was built from.
//!
//! The launch-time update check compares that commit with the newest successful release-build
//! workflow run, so `luna` needs to know its own provenance. `git rev-parse HEAD` is available in
//! the checkout both locally and on the CI runner; a source tarball without `.git` simply gets
//! `unknown`, and the check then does nothing rather than guess.
//!
//! The build script re-runs when the checked-out commit moves. Watching `.git/HEAD` alone is not
//! enough: HEAD keeps naming the branch, which is why the branch ref and `packed-refs` are watched
//! too.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    let repo = manifest.join("../..");
    let git_dir = repo.join(".git");

    println!("cargo:rerun-if-changed=build.rs");
    for watched in ["HEAD", "packed-refs", "refs/heads/main"] {
        println!("cargo:rerun-if-changed={}", git_dir.join(watched).display());
    }

    let sha = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|sha| !sha.is_empty())
        .unwrap_or_else(|| "unknown".to_string());

    let commit_ts = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["show", "-s", "--format=%ct", "HEAD"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|ts| !ts.is_empty() && ts.chars().all(|ch| ch.is_ascii_digit()))
        .unwrap_or_else(|| "0".to_string());

    let short: String = sha.chars().take(8).collect();
    let package_version =
        std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let version = format!("{package_version} ({short})");
    println!("cargo:rustc-env=LUNA_BUILD_SHA={sha}");
    println!("cargo:rustc-env=LUNA_BUILD_COMMIT_TS={commit_ts}");
    println!("cargo:rustc-env=LUNA_VERSION={version}");
}
