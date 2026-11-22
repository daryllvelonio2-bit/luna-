//! Web command: launch the Luna web server and browser interface (`luna web`).
//!
//! By default `luna web` detaches cleanly into its own session (`setsid`) so
//! closing the launching terminal keeps the web server running. Pass
//! `--foreground` (`-f`) to stay attached to the terminal, `--status` to check
//! a running background instance, or `--stop` to shut it down.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clap::Args;
use colored::Colorize;
use harness_core::{Config, HarnessError};
use harness_web::{open_browser_detached, run_web_server, WebServerOptions};
use serde::{Deserialize, Serialize};

#[derive(Args, Debug, Clone)]
pub struct WebCommand {
    /// Host to bind to (e.g. 127.0.0.1; wildcards like 0.0.0.0 are refused for security)
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    /// Port to listen on (default: 3080)
    #[arg(short, long, default_value_t = 3080)]
    pub port: u16,

    /// Do not automatically open the default browser at startup
    #[arg(long)]
    pub no_open: bool,

    /// Advertised canonical public URL (e.g. https://luna.example.com/)
    #[arg(long)]
    pub public_url: Option<String>,

    /// Additional trusted host headers for remote proxy access
    #[arg(long = "trusted-host", value_name = "HOST")]
    pub trusted_hosts: Vec<String>,

    /// Path to TLS certificate chain PEM file
    #[arg(long = "tls-cert")]
    pub tls_cert: Option<PathBuf>,

    /// Path to TLS private key PEM file
    #[arg(long = "tls-key")]
    pub tls_key: Option<PathBuf>,

    /// Static auth token; if omitted, a secure one-time token is auto-generated
    #[arg(long)]
    pub token: Option<String>,

    /// Run in the foreground instead of detaching into the background
    #[arg(short, long)]
    pub foreground: bool,

    /// Stop the background Luna web server if running
    #[arg(long)]
    pub stop: bool,

    /// Show status of the background Luna web server
    #[arg(long)]
    pub status: bool,

    /// Internal flag used by the detached background worker process
    #[arg(long, hide = true)]
    pub daemon_child: bool,

    /// Internal readiness file written by the detached worker after binding
    #[arg(long, hide = true)]
    pub ready_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WebServerState {
    pid: u32,
    host: String,
    port: u16,
    launch_url: String,
    log_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct ReadyPayload {
    ok: bool,
    url: Option<String>,
    port: Option<u16>,
    error: Option<String>,
}

fn runtime_dir(config: &Config) -> PathBuf {
    config
        .paths
        .sessions_db
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| {
            std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(".luna")
        })
}

fn state_file_path(config: &Config) -> PathBuf {
    runtime_dir(config).join("web-server.json")
}

fn log_file_path(config: &Config) -> PathBuf {
    runtime_dir(config).join("web.log")
}

fn read_state(path: &Path) -> Option<WebServerState> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

fn write_state(path: &Path, state: &WebServerState) -> Result<(), HarnessError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| HarnessError::Io(e.to_string()))?;
    }
    let body = serde_json::to_string_pretty(state).map_err(|e| HarnessError::Io(e.to_string()))?;
    std::fs::write(path, body).map_err(|e| HarnessError::Io(e.to_string()))?;
    Ok(())
}

fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if rc == 0 {
            return true;
        }
        matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        )
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn stop_pid(pid: u32) -> bool {
    if !is_pid_alive(pid) {
        return false;
    }
    #[cfg(unix)]
    {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if !is_pid_alive(pid) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGKILL);
        }
        true
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

fn under_ssh() -> bool {
    std::env::var("SSH_CONNECTION").is_ok()
        || std::env::var("SSH_CLIENT").is_ok()
        || std::env::var("SSH_TTY").is_ok()
}

fn maybe_open_browser(url: &str, no_open: bool) {
    if !no_open && !under_ssh() {
        println!("luna web: opening the default browser; pass --no-open to disable");
        if let Err(err) = open_browser_detached(url) {
            tracing::warn!("could not open default browser: {}", err);
        }
    }
}

fn spawn_detached_web(cmd: &WebCommand, config: &Config) -> Result<(), HarnessError> {
    let state_path = state_file_path(config);
    let log_path = log_file_path(config);

    // If an existing background instance is already alive on the same host & port (and no
    // explicit new static token was requested), surface its URL instead of failing bind.
    if cmd.token.is_none() {
        if let Some(existing) = read_state(&state_path) {
            if is_pid_alive(existing.pid) && existing.host == cmd.host && existing.port == cmd.port
            {
                println!(
                    "{} {} (already running, pid {})",
                    "luna web:".green().bold(),
                    existing.launch_url,
                    existing.pid
                );
                maybe_open_browser(&existing.launch_url, cmd.no_open);
                return Ok(());
            }
        }
    }

    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| HarnessError::Io(e.to_string()))?;
    }
    let log_out = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .map_err(|e| {
            HarnessError::Io(format!(
                "failed to open web log '{}': {}",
                log_path.display(),
                e
            ))
        })?;
    let log_err = log_out
        .try_clone()
        .map_err(|e| HarnessError::Io(format!("failed to clone web log handle: {}", e)))?;

    let ready_path =
        std::env::temp_dir().join(format!("luna-web-ready-{}.json", std::process::id()));
    let _ = std::fs::remove_file(&ready_path);

    let exe = std::env::current_exe().map_err(|e| HarnessError::Io(e.to_string()))?;
    let mut child_cmd = Command::new(exe);
    child_cmd
        .arg("web")
        .arg("--daemon-child")
        .arg("--no-open")
        .arg("--host")
        .arg(&cmd.host)
        .arg("--port")
        .arg(cmd.port.to_string())
        .arg("--ready-file")
        .arg(&ready_path);

    if let Some(public_url) = &cmd.public_url {
        child_cmd.arg("--public-url").arg(public_url);
    }
    for th in &cmd.trusted_hosts {
        child_cmd.arg("--trusted-host").arg(th);
    }
    if let Some(cert) = &cmd.tls_cert {
        child_cmd.arg("--tls-cert").arg(cert);
    }
    if let Some(key) = &cmd.tls_key {
        child_cmd.arg("--tls-key").arg(key);
    }
    if let Some(token) = &cmd.token {
        child_cmd.arg("--token").arg(token);
    }

    child_cmd
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_out))
        .stderr(Stdio::from(log_err));

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            child_cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::signal(libc::SIGHUP, libc::SIG_IGN);
                Ok(())
            });
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        child_cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    let mut child = child_cmd
        .spawn()
        .map_err(|e| HarnessError::Io(format!("failed to start background luna web: {}", e)))?;

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Ok(raw) = std::fs::read_to_string(&ready_path) {
            if let Ok(ready) = serde_json::from_str::<ReadyPayload>(&raw) {
                let _ = std::fs::remove_file(&ready_path);
                if ready.ok {
                    let launch_url = ready.url.unwrap_or_default();
                    let bound_port = ready.port.unwrap_or(cmd.port);
                    let pid = child.id();
                    let _ = write_state(
                        &state_path,
                        &WebServerState {
                            pid,
                            host: cmd.host.clone(),
                            port: bound_port,
                            launch_url: launch_url.clone(),
                            log_path: log_path.clone(),
                        },
                    );
                    println!("{} {}", "luna web:".green().bold(), launch_url);
                    println!(
                        "luna web: running in background (pid {} · log {} · stop with `luna web --stop`)",
                        pid,
                        log_path.display()
                    );
                    maybe_open_browser(&launch_url, cmd.no_open);
                    return Ok(());
                } else {
                    let err_msg = ready
                        .error
                        .unwrap_or_else(|| "luna web failed to start".to_string());
                    return Err(HarnessError::Io(err_msg));
                }
            }
        }

        if let Ok(Some(status)) = child.try_wait() {
            let _ = std::fs::remove_file(&ready_path);
            let log_tail = std::fs::read_to_string(&log_path)
                .ok()
                .map(|s| {
                    s.lines()
                        .rev()
                        .take(6)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .unwrap_or_default();
            return Err(HarnessError::Io(format!(
                "luna web exited early ({status}){}",
                if log_tail.trim().is_empty() {
                    String::new()
                } else {
                    format!(":\n{log_tail}")
                }
            )));
        }

        if Instant::now() >= deadline {
            let _ = std::fs::remove_file(&ready_path);
            return Err(HarnessError::Io(format!(
                "timed out waiting for luna web to bind (check {})",
                log_path.display()
            )));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

pub async fn handle_web_command(cmd: WebCommand, config: Config) -> Result<(), HarnessError> {
    let state_path = state_file_path(&config);

    if cmd.status {
        match read_state(&state_path) {
            Some(st) if is_pid_alive(st.pid) => {
                println!(
                    "luna web: running (pid {} · {} · log {})",
                    st.pid,
                    st.launch_url,
                    st.log_path.display()
                );
            }
            _ => {
                let _ = std::fs::remove_file(&state_path);
                println!("luna web: not running");
            }
        }
        return Ok(());
    }

    if cmd.stop {
        match read_state(&state_path) {
            Some(st) if stop_pid(st.pid) => {
                let _ = std::fs::remove_file(&state_path);
                println!("luna web: stopped (pid {})", st.pid);
            }
            _ => {
                let _ = std::fs::remove_file(&state_path);
                println!("luna web: not running");
            }
        }
        return Ok(());
    }

    if !cmd.foreground && !cmd.daemon_child {
        return spawn_detached_web(&cmd, &config);
    }

    #[cfg(unix)]
    if cmd.daemon_child {
        unsafe {
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
        }
    }

    let options = WebServerOptions {
        host: cmd.host,
        port: cmd.port,
        no_open: cmd.no_open,
        public_url: cmd.public_url,
        trusted_hosts: cmd.trusted_hosts,
        tls_cert: cmd.tls_cert,
        tls_key: cmd.tls_key,
        static_token: cmd.token,
        ready_file: cmd.ready_file,
    };

    run_web_server(config, options).await
}
