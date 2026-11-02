use colored::Colorize;
use std::process::Stdio;
use tokio::process::Command;
use tracing::{error, info};

use harness_core::HarnessError;

/// How many times a crashing child is restarted before the supervisor gives up.
const MAX_RESTARTS: usize = 5;

/// The delay before the first restart after a crash; it doubles per restart.
const FIRST_RESTART_DELAY: std::time::Duration = std::time::Duration::from_secs(3);

/// The longest that delay grows to. A child that crashes instantly is one whose next crash will
/// also be instant: waiting longer than a minute only delays telling the operator.
const MAX_RESTART_DELAY: std::time::Duration = std::time::Duration::from_secs(60);

pub async fn run_supervisor_loop(args: Vec<String>) -> Result<(), HarnessError> {
    crate::commands::clear_screen();
    println!(
        "{}",
        "==================================================".cyan()
    );
    println!(
        "{}",
        "        Luna Process Lifecycle Supervisor         "
            .bold()
            .cyan()
    );
    println!(
        "{}",
        "==================================================".cyan()
    );
    println!("Supervising autonomous agent process...\n");

    let current_exe = std::env::current_exe().map_err(|e| HarnessError::Io(e.to_string()))?;
    println!(
        "{}",
        "Restarting this binary on a crash. It does not rebuild it — run `cargo build --release` \
         and restart the supervisor to pick up a new build."
            .dimmed()
    );

    let mut restarts = 0usize;
    let mut delay = FIRST_RESTART_DELAY;

    loop {
        info!("Spawning child agent process: {:?}", current_exe);
        let mut child = Command::new(&current_exe)
            .args(&args)
            .env("LUNA_SUPERVISED", "1")
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| HarnessError::Io(format!("Failed to spawn child: {}", e)))?;

        let status = child
            .wait()
            .await
            .map_err(|e| HarnessError::Io(e.to_string()))?;

        if status.success() {
            info!("Agent exited cleanly. Shutting down supervisor.");
            break;
        }

        let code = status.code().unwrap_or(-1);
        restarts += 1;
        // Bounded on purpose. A binary that cannot start at all — a missing library, a config it
        // refuses, a port already taken — crashes in milliseconds, and a supervisor with no ceiling
        // turns that into a machine restarting it forever at the same rate, filling the log. The
        // operator wants to be told, not to have it kept secret by an infinite loop.
        if restarts >= MAX_RESTARTS {
            error!(
                "Agent exited with code {code} {restarts} times; giving up. Last output is above."
            );
            return Err(HarnessError::Io(format!(
                "the supervised process failed {restarts} times in a row (last exit code {code})"
            )));
        }

        error!(
            "Agent exited with error code {code}. Restart {restarts}/{MAX_RESTARTS} in {}s...",
            delay.as_secs()
        );
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(MAX_RESTART_DELAY);
    }

    Ok(())
}
