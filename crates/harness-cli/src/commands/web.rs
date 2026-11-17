//! Web command: launch the Luna web server and browser interface (`luna web`).
//!
//! Follows the DeepSeek Harness web architecture (`dsh web`):
//! - Host/Origin trust fence
//! - Embedded React web application
//! - Real-time WebSocket event streaming and bidirectional control
//! - Full chat, session management, settings, memory/notes, and plugins

use std::path::PathBuf;

use clap::Args;
use harness_core::{Config, HarnessError};
use harness_web::{run_web_server, WebServerOptions};

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
}

pub async fn handle_web_command(cmd: WebCommand, config: Config) -> Result<(), HarnessError> {
    let options = WebServerOptions {
        host: cmd.host,
        port: cmd.port,
        no_open: cmd.no_open,
        public_url: cmd.public_url,
        trusted_hosts: cmd.trusted_hosts,
        tls_cert: cmd.tls_cert,
        tls_key: cmd.tls_key,
        static_token: cmd.token,
    };

    run_web_server(config, options).await
}
