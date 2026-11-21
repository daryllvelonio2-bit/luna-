//! Web server, REST/WebSocket API, and embedded browser UI for Luna Harness.
//!
//! Follows the DeepSeek Harness web architecture (`dsh web`):
//! - Host / Origin trust fence and launch token exchange
//! - Embedded React / TypeScript single-page application
//! - Real-time duplex WebSocket stream for streaming turns, tool approvals, and clarifiers
//! - Proactive scheduled automations and TypeScript plugins management

pub mod auth;
pub mod extra_routes;
pub mod plugin_routes;
pub mod routes;
pub mod server;
pub mod state;

pub use server::{run_web_server, WebServerOptions};
pub use state::WebState;
