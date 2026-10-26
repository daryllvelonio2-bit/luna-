use colored::Colorize;
use std::process::Stdio;
use tokio::process::Command;
use tracing::{error, info};

use harness_core::HarnessError;

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
        } else {
            let code = status.code().unwrap_or(-1);
            error!(
                "Agent exited with error code {}. Restarting in 3 seconds...",
                code
            );
            tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;
        }
    }

    Ok(())
}
