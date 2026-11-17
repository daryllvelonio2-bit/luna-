use std::path::PathBuf;

use colored::Colorize;
use harness_core::config::Config;
use harness_core::HarnessError;

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
        }
    }
}

pub async fn run_web_server(config: Config, options: WebServerOptions) -> Result<(), HarnessError> {
    // Wildcard host rejection
    if is_wildcard_host(&options.host) {
        return Err(HarnessError::ConfigurationError(format!(
            "error: --host {} is an unspecified (wildcard) address, which is not supported: \
             binding every interface would expose remote code execution to the network; \
             bind one concrete IPv4 or IPv6 address instead",
            options.host
        )));
    }

    // TLS pair validation
    if (options.tls_cert.is_some() && options.tls_key.is_none())
        || (options.tls_cert.is_none() && options.tls_key.is_some())
    {
        return Err(HarnessError::ConfigurationError(
            "error: --tls-cert and --tls-key must be supplied together".to_string(),
        ));
    }

    let bind_addr = format!("{}:{}", options.host, options.port);
    let listener = tokio::net::TcpListener::bind(&bind_addr)
        .await
        .map_err(|e| {
            HarnessError::Io(format!("failed to bind web server to {}: {}", bind_addr, e))
        })?;

    let local_addr = listener
        .local_addr()
        .map_err(|e| HarnessError::Io(format!("failed to get listener address: {}", e)))?;
    let bound_port = local_addr.port();

    let state = WebState::new(
        config,
        options.host.clone(),
        options.trusted_hosts.clone(),
        options.static_token.clone(),
    )
    .await?;

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

    // Browser handoff
    let under_ssh = std::env::var("SSH_CONNECTION").is_ok()
        || std::env::var("SSH_CLIENT").is_ok()
        || std::env::var("SSH_TTY").is_ok();

    if !options.no_open && !under_ssh {
        println!("luna web: opening the default browser; pass --no-open to disable");
        if let Err(err) = open::that(&launch_url) {
            tracing::warn!("could not open default browser: {}", err);
        }
    }

    let router = create_router(state);

    axum::serve(listener, router)
        .await
        .map_err(|e| HarnessError::Io(format!("web server error: {}", e)))?;

    Ok(())
}
