use harness_core::HarnessError;
use std::path::{Path, PathBuf};
use tokio::process::Command;

/// A tokio command for the OpenCode CLI binary.
///
/// npm and yarn install the CLI as a `.cmd` shim on Windows, and `CreateProcess` cannot execute
/// that directly. Those go through `cmd /C`; a native `.exe` is spawned as-is.
pub(super) fn opencode_command(bin: &Path) -> Command {
    #[cfg(windows)]
    {
        let is_script = bin
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
        if is_script {
            let mut cmd = Command::new("cmd");
            cmd.arg("/C").arg(bin);
            return cmd;
        }
    }
    Command::new(bin)
}

/// The first candidate that exists on disk.
fn first_existing(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates.into_iter().find(|candidate| candidate.is_file())
}

/// Env-overridden binary paths, checked before every install location.
fn opencode_binary_from_env() -> Option<PathBuf> {
    ["OPENCODE_BIN", "SHIINA_OPENCODE_COMMAND"]
        .iter()
        .filter_map(|env_var| std::env::var(env_var).ok())
        .map(|bin_str| PathBuf::from(bin_str.trim()))
        .find(|path| path.is_file())
}

/// Standard per-user install locations.
fn opencode_binary_in_home() -> Option<PathBuf> {
    let home = harness_core::config::home_dir();
    if let Some(cand) = first_existing([
        home.join(".opencode/bin/opencode"),
        home.join(".local/bin/opencode"),
        home.join(".npm-global/bin/opencode"),
    ]) {
        return Some(cand);
    }
    // npm/yarn install the CLI as an extension-carrying shim on Windows, so the Unix name
    // alone never matches.
    #[cfg(windows)]
    let windows_candidates = [
        home.join(".opencode").join("bin").join("opencode.exe"),
        home.join("AppData")
            .join("Roaming")
            .join("npm")
            .join("opencode.cmd"),
    ];
    #[cfg(windows)]
    if let Some(cand) = first_existing(windows_candidates) {
        return Some(cand);
    }
    None
}

/// System-wide install locations.
fn opencode_binary_in_system_dirs() -> Option<PathBuf> {
    first_existing([
        PathBuf::from("/usr/local/bin/opencode"),
        PathBuf::from("/usr/bin/opencode"),
    ])
}

/// Installer locations advertised by the Windows environment.
fn opencode_binary_in_windows_env() -> Option<PathBuf> {
    if let Some(cand) = std::env::var("LOCALAPPDATA")
        .ok()
        .map(|local_app| PathBuf::from(local_app).join("opencode/bin/opencode.exe"))
        .filter(|cand| cand.is_file())
    {
        return Some(cand);
    }
    std::env::var("APPDATA")
        .ok()
        .map(|app_data| PathBuf::from(app_data).join("npm/opencode.cmd"))
        .filter(|cand| cand.is_file())
}

/// Every directory on PATH.
fn opencode_binary_in_path() -> Option<PathBuf> {
    let path_var = std::env::var("PATH").ok()?;
    for dir in std::env::split_paths(&path_var) {
        let cand = dir.join("opencode");
        if cand.is_file() {
            return Some(cand);
        }
        #[cfg(windows)]
        {
            let cand_exe = dir.join("opencode.exe");
            if cand_exe.is_file() {
                return Some(cand_exe);
            }
            for ext in ["cmd", "bat", "ps1"] {
                let shim = dir.join(format!("opencode.{ext}"));
                if shim.is_file() {
                    return Some(shim);
                }
            }
        }
    }
    None
}

/// Discover the installed OpenCode binary across standard platform locations or env.
pub fn find_opencode_binary() -> Result<PathBuf, HarnessError> {
    if let Some(path) = opencode_binary_from_env() {
        return Ok(path);
    }
    if let Some(path) = opencode_binary_in_home() {
        return Ok(path);
    }
    if let Some(path) = opencode_binary_in_system_dirs() {
        return Ok(path);
    }
    if let Some(path) = opencode_binary_in_windows_env() {
        return Ok(path);
    }
    if let Some(path) = opencode_binary_in_path() {
        return Ok(path);
    }

    Err(HarnessError::ProviderError {
        provider: "opencode".to_string(),
        message: "OpenCode binary not found. Please install opencode or set OPENCODE_BIN."
            .to_string(),
    })
}
