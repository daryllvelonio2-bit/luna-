//! Optional native desktop GUI (`luna-gui`) lifecycle: `luna gui [launch|install|update|remove|status]`.
//!
//! The default `luna` CLI remains lean and TUI-first; the native Tauri desktop app is distributed
//! separately via GitHub Releases / Actions (`gui-release.yml`) and installed on demand into
//! `paths.gui_dir` (`~/.luna/gui`).

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use clap::Subcommand;
use colored::Colorize;
use serde::{Deserialize, Serialize};
use zip::ZipArchive;

use harness_core::{Config, HarnessError};

const USER_AGENT: &str = concat!(
    "LunaHarness/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/luna-ai/rust-agent-harness)"
);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Subcommand, Debug)]
pub enum GuiCommand {
    /// Launch the installed Luna native desktop GUI (default when no subcommand is given)
    Launch,
    /// Download and install the optional Luna native desktop GUI into ~/.luna/gui
    Install {
        /// Optional local binary/archive path instead of downloading from GitHub
        #[arg(long)]
        source: Option<PathBuf>,
    },
    /// Update the installed Luna native desktop GUI to the latest release
    Update,
    /// Remove the optional Luna native desktop GUI without touching sessions, memory, or plugins
    Remove,
    /// Show installation status and resolved artifact target for this platform
    Status,
}

#[derive(Debug, Serialize, Deserialize)]
struct GuiInstallMeta {
    installed_at: String,
    target: String,
    source: String,
    binary: String,
}

pub fn platform_artifact_name() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "luna-gui-linux-x86_64.zip",
        ("linux", "aarch64") => "luna-gui-linux-aarch64.zip",
        ("macos", "aarch64") => "luna-gui-macos-aarch64.zip",
        ("macos", "x86_64") => "luna-gui-macos-x86_64.zip",
        ("windows", "x86_64") => "luna-gui-windows-x86_64.zip",
        _ => "luna-gui-unknown.zip",
    }
}

pub fn binary_filename() -> &'static str {
    if cfg!(windows) {
        "luna-gui.exe"
    } else {
        "luna-gui"
    }
}

pub fn installed_binary_path(config: &Config) -> Option<PathBuf> {
    let local = config.paths.gui_dir.join(binary_filename());
    if local.is_file() {
        return Some(local);
    }
    // Also check macOS .app bundle layout if extracted into gui_dir
    let mac_app = config
        .paths
        .gui_dir
        .join("Luna.app")
        .join("Contents")
        .join("MacOS")
        .join("luna-gui");
    if mac_app.is_file() {
        return Some(mac_app);
    }
    None
}

pub async fn run_gui_command(
    subcommand: Option<GuiCommand>,
    config: &Config,
) -> Result<(), HarnessError> {
    match subcommand.unwrap_or(GuiCommand::Launch) {
        GuiCommand::Launch => launch_gui(config),
        GuiCommand::Install { source } => install_gui(config, source.as_deref()).await,
        GuiCommand::Update => install_gui(config, None).await,
        GuiCommand::Remove => remove_gui(config),
        GuiCommand::Status => {
            show_status(config);
            Ok(())
        }
    }
}

fn show_status(config: &Config) {
    println!("GUI directory: {}", config.paths.gui_dir.display());
    println!("Platform artifact: {}", platform_artifact_name());
    match installed_binary_path(config) {
        Some(bin) => println!(
            "Status: {} ({})",
            "installed".green().bold(),
            bin.display()
        ),
        None => println!(
            "Status: {} (run `luna gui install` to download)",
            "not installed".yellow()
        ),
    }
}

fn launch_gui(config: &Config) -> Result<(), HarnessError> {
    let Some(bin) = installed_binary_path(config) else {
        return Err(HarnessError::ConfigurationError(format!(
            "Luna native GUI is not installed in {}. Run `luna gui install` first.",
            config.paths.gui_dir.display()
        )));
    };

    Command::new(&bin)
        .spawn()
        .map_err(|e| HarnessError::Io(format!("failed to launch {}: {}", bin.display(), e)))?;
    println!("{} Launched Luna Desktop ({})", "✔".green().bold(), bin.display());
    Ok(())
}

fn remove_gui(config: &Config) -> Result<(), HarnessError> {
    if config.paths.gui_dir.exists() {
        fs::remove_dir_all(&config.paths.gui_dir).map_err(|e| {
            HarnessError::Io(format!(
                "failed to remove {}: {}",
                config.paths.gui_dir.display(),
                e
            ))
        })?;
        println!(
            "{} Removed optional Luna GUI from {}",
            "✔".green().bold(),
            config.paths.gui_dir.display()
        );
    } else {
        println!(
            "{} Luna GUI is not installed in {}",
            "•".cyan(),
            config.paths.gui_dir.display()
        );
    }
    Ok(())
}

async fn install_gui(config: &Config, local_source: Option<&Path>) -> Result<(), HarnessError> {
    fs::create_dir_all(&config.paths.gui_dir).map_err(|e| {
        HarnessError::Io(format!(
            "failed to create {}: {}",
            config.paths.gui_dir.display(),
            e
        ))
    })?;

    let (bytes, source_desc) = if let Some(path) = local_source {
        if path.is_file() && path.extension().and_then(|e| e.to_str()) != Some("zip") {
            // Direct binary install
            let dest = config.paths.gui_dir.join(binary_filename());
            fs::copy(path, &dest)
                .map_err(|e| HarnessError::Io(format!("failed to copy binary: {}", e)))?;
            make_executable(&dest)?;
            write_meta(config, &dest, &path.display().to_string())?;
            println!(
                "{} Installed Luna GUI binary to {}",
                "✔".green().bold(),
                dest.display()
            );
            return Ok(());
        }
        let data = fs::read(path)
            .map_err(|e| HarnessError::Io(format!("failed to read {}: {}", path.display(), e)))?;
        (data, path.display().to_string())
    } else {
        let artifact = platform_artifact_name();
        let url = format!(
            "https://github.com/{}/releases/latest/download/{}",
            config.gui.repo, artifact
        );
        println!("{} Downloading Luna Desktop from {}...", "•".cyan(), url);

        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(DOWNLOAD_TIMEOUT)
            .build()
            .map_err(|e| HarnessError::Io(format!("http client init failed: {}", e)))?;

        let resp = client.get(&url).send().await.map_err(|e| {
            HarnessError::Io(format!("failed to download Luna GUI from {}: {}", url, e))
        })?;

        if !resp.status().is_success() {
            return Err(HarnessError::Io(format!(
                "GitHub download returned HTTP {} for {}. You can also pass `--source <local-zip-or-binary>`.",
                resp.status(),
                url
            )));
        }

        let data = resp
            .bytes()
            .await
            .map_err(|e| HarnessError::Io(format!("failed to read download stream: {}", e)))?
            .to_vec();
        (data, url)
    };

    extract_zip_into(&bytes, &config.paths.gui_dir)?;
    let bin = installed_binary_path(config).ok_or_else(|| {
        HarnessError::Io(format!(
            "archive extracted to {}, but '{}' was not found inside it",
            config.paths.gui_dir.display(),
            binary_filename()
        ))
    })?;
    make_executable(&bin)?;
    write_meta(config, &bin, &source_desc)?;
    println!(
        "{} Installed Luna Desktop to {} (run `luna gui` to launch)",
        "✔".green().bold(),
        bin.display()
    );
    Ok(())
}

fn extract_zip_into(bytes: &[u8], dest_dir: &Path) -> Result<(), HarnessError> {
    let cursor = std::io::Cursor::new(bytes);
    let mut archive = ZipArchive::new(cursor)
        .map_err(|e| HarnessError::Io(format!("invalid zip archive: {}", e)))?;

    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| HarnessError::Io(format!("zip entry error: {}", e)))?;
        let Some(rel_path) = entry.enclosed_name().map(|p| p.to_path_buf()) else {
            continue;
        };
        let out_path = dest_dir.join(rel_path);
        if entry.is_dir() {
            fs::create_dir_all(&out_path).map_err(|e| HarnessError::Io(e.to_string()))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent).map_err(|e| HarnessError::Io(e.to_string()))?;
        }
        let mut buf = Vec::new();
        entry
            .read_to_end(&mut buf)
            .map_err(|e| HarnessError::Io(e.to_string()))?;
        fs::write(&out_path, &buf).map_err(|e| HarnessError::Io(e.to_string()))?;
    }
    Ok(())
}

fn make_executable(path: &Path) -> Result<(), HarnessError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(path)
            .map_err(|e| HarnessError::Io(e.to_string()))?
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(path, perms).map_err(|e| HarnessError::Io(e.to_string()))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn write_meta(config: &Config, bin: &Path, source: &str) -> Result<(), HarnessError> {
    let meta = GuiInstallMeta {
        installed_at: chrono::Utc::now().to_rfc3339(),
        target: platform_artifact_name().to_string(),
        source: source.to_string(),
        binary: bin.display().to_string(),
    };
    let raw = serde_json::to_string_pretty(&meta).unwrap_or_default();
    fs::write(config.paths.gui_dir.join("gui-install.json"), raw)
        .map_err(|e| HarnessError::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn local_zip_install_and_remove_lifecycle() {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("luna-gui-test.zip");
        {
            let file = fs::File::create(&zip_path).unwrap();
            let mut zw = zip::ZipWriter::new(file);
            let options = zip::write::SimpleFileOptions::default();
            zw.start_file(binary_filename(), options).unwrap();
            zw.write_all(b"#!/bin/sh\necho ok\n").unwrap();
            zw.finish().unwrap();
        }

        let mut cfg = Config::default();
        cfg.paths.gui_dir = dir.path().join("gui-install");

        install_gui(&cfg, Some(&zip_path)).await.unwrap();
        let bin = installed_binary_path(&cfg).expect("binary should be installed");
        assert!(bin.is_file());

        remove_gui(&cfg).unwrap();
        assert!(installed_binary_path(&cfg).is_none());
    }
}
