//! Print what the Linux adapter can do on this machine.
//!
//! Run it with `cargo run -p harness-desktop-linux --example status`. It
//! answers the same question `desktop_status` answers, without a harness.

use harness_desktop::commands;
use harness_desktop::DesktopEngine;
use harness_desktop_linux::LinuxDesktopAdapter;
use std::sync::Arc;

fn main() {
    let engine = DesktopEngine::new(Arc::new(LinuxDesktopAdapter::new()));
    println!("{}", commands::status(&engine).to_json());
    println!("{}", commands::list_apps(&engine).to_json());
}
