use std::path::PathBuf;

use colored::Colorize;
use harness_core::config::Config;
use harness_core::{HarnessError, SessionStore};

use crate::auth::{is_loopback_host, is_wildcard_host};
use crate::routes::create_router;
use crate::state::WebState;

#[derive(Debug, Clone)]
pub struct WebServerOptions {
    pub host: String,
    pub port: u16,
    pub no_open: bool,
    pub public_url: Option<String>,
    pub trusted_hosts: Vec<String>,
    pub tls_cert: Option<PathBuf>,
    pub tls_key: Option<PathBuf>,
    pub static_token: Option<String>,
    pub ready_file: Option<PathBuf>,
}

impl Default for WebServerOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_string(),
            port: 3080,
            no_open: false,
            public_url: None,
            trusted_hosts: Vec::new(),
            tls_cert: None,
            tls_key: None,
            static_token: None,
            ready_file: None,
        }
    }
}

pub fn open_browser_detached(url: &str) -> Result<(), String> {
    open::that_detached(url).map_err(|e| e.to_string())
}

fn write_ready_file(path: Option<&PathBuf>, payload: &serde_json::Value) {
    if let Some(p) = path {
        if let Some(parent) = p.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = p.with_extension("tmp");
        if std::fs::write(&tmp, payload.to_string()).is_ok() {
            let _ = harness_core::fsutil::replace_file(&tmp, p);
        } else {
            let _ = std::fs::write(p, payload.to_string());
        }
    }
}

pub async fn run_web_server(config: Config, options: WebServerOptions) -> Result<(), HarnessError> {
    let ready_file = options.ready_file.clone();

    // Wildcard host rejection
    if is_wildcard_host(&options.host) {
        let msg = format!(
            "error: --host {} is an unspecified (wildcard) address, which is not supported: \
             binding every interface would expose remote code execution to the network; \
             bind one concrete IPv4 or IPv6 address instead",
            options.host
        );
        write_ready_file(
            ready_file.as_ref(),
            &serde_json::json!({ "ok": false, "error": msg }),
        );
        return Err(HarnessError::ConfigurationError(msg));
    }

    // TLS pair validation
    if (options.tls_cert.is_some() && options.tls_key.is_none())
        || (options.tls_cert.is_none() && options.tls_key.is_some())
    {
        let msg = "error: --tls-cert and --tls-key must be supplied together".to_string();
        write_ready_file(
            ready_file.as_ref(),
            &serde_json::json!({ "ok": false, "error": msg }),
        );
        return Err(HarnessError::ConfigurationError(msg));
    }

    let bind_addr = format!("{}:{}", options.host, options.port);
    let listener = match tokio::net::TcpListener::bind(&bind_addr).await {
        Ok(l) => l,
        Err(e) => {
            let msg = format!("failed to bind web server to {}: {}", bind_addr, e);
            write_ready_file(
                ready_file.as_ref(),
                &serde_json::json!({ "ok": false, "error": msg }),
            );
            return Err(HarnessError::Io(msg));
        }
    };

    let local_addr = listener
        .local_addr()
        .map_err(|e| HarnessError::Io(format!("failed to get listener address: {}", e)))?;
    let bound_port = local_addr.port();

    let state = match WebState::new(
        config,
        options.host.clone(),
        options.trusted_hosts.clone(),
        options.static_token.clone(),
    )
    .await
    {
        Ok(s) => s,
        Err(e) => {
            write_ready_file(
                ready_file.as_ref(),
                &serde_json::json!({ "ok": false, "error": e.to_string() }),
            );
            return Err(e);
        }
    };

    // Advertised root URL
    let advertised_url = options.public_url.clone().unwrap_or_else(|| {
        let proto = if options.tls_cert.is_some() {
            "https"
        } else {
            "http"
        };
        format!("{}://{}:{}", proto, options.host, bound_port)
    });
    let advertised_url = advertised_url.trim_end_matches('/').to_string();

    // Set LUNA_WEB_URL environment variable for child tasks/processes
    std::env::set_var("LUNA_WEB_URL", &advertised_url);

    // Warning for plain HTTP on non-loopback
    if !is_loopback_host(&options.host) && options.tls_cert.is_none() {
        println!(
            "{} listening on {} over plain HTTP; restrict this port to a trusted proxy or network",
            "luna web:".yellow().bold(),
            options.host
        );
    }

    let launch_url = format!("{}/?token={}", advertised_url, state.auth.launch_token);
    println!("{} {}", "luna web:".green().bold(), launch_url);

    // Prune any abandoned empty sessions from previous runs
    let _ = state.store.prune_empty_sessions(None).await;

    // Start background scheduler loop so due automations execute automatically while luna web runs
    let sched_handler = state.cancellable_schedule_delivery_handler();
    let _scheduler_handle = state.scheduler.clone().spawn_loop_cancellable(
        sched_handler,
        std::time::Duration::from_secs(5),
        state.shutdown_token.clone(),
    );

    let router = create_router(state);

    // Signal readiness after bind, state initialization, and router setup are complete.
    write_ready_file(
        ready_file.as_ref(),
        &serde_json::json!({
            "ok": true,
            "url": launch_url,
            "port": bound_port,
        }),
    );

    // Browser handoff in background task so axum::serve starts accepting requests immediately
    let under_ssh = std::env::var("SSH_CONNECTION").is_ok()
        || std::env::var("SSH_CLIENT").is_ok()
        || std::env::var("SSH_TTY").is_ok();

    if !options.no_open && !under_ssh {
        println!("luna web: opening the default browser; pass --no-open to disable");
        let url = launch_url.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if let Err(err) = open::that_detached(&url) {
                tracing::warn!("could not open default browser: {}", err);
            }
        });
    }

    axum::serve(listener, router)
        .await
        .map_err(|e| HarnessError::Io(format!("web server error: {}", e)))?;

    Ok(())
}
