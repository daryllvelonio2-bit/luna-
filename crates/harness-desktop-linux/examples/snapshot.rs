//! Read a desktop application's tree and print the snapshot.
//!
//! Run it with `cargo run -p harness-desktop-linux --example snapshot -- Firefox`.
//! It only reads: no action is taken on the desktop.

use harness_desktop::commands;
use harness_desktop::tree::options::TreeOptions;
use harness_desktop::DesktopEngine;
use harness_desktop_linux::LinuxDesktopAdapter;
use std::sync::Arc;

fn main() {
    let app = std::env::args().nth(1).unwrap_or_else(|| "Firefox".to_string());
    let engine = DesktopEngine::new(Arc::new(LinuxDesktopAdapter::new()));
    let options = TreeOptions {
        interactive_only: true,
        compact: true,
        ..Default::default()
    };
    let response = commands::snapshot(&engine, &app, &options);
    println!("{}", response.to_json());
}
