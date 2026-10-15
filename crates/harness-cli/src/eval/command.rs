//! `luna eval` — the scripted end-to-end regression gate.
//!
//! `--offline` (the default) replays each case's canned model turns through the real agent loop, so
//! the suite runs in CI with no credential and no quota. `--live` drives the real provider instead
//! and skips every case when no credential is registered for it.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use clap::Subcommand;

use harness_core::config::Config;
use harness_core::{HarnessError, TokenUsage};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};

use crate::eval::runner::{run_suite, Backend, EvalReport, RunOptions};
use crate::eval::scripted::ScriptedProvider;

pub const DEFAULT_SUITE: &str = "tests/eval";
pub const DEFAULT_REPORT: &str = "eval-report.json";
/// The fleet's live model under test (`docs/AGENT_RULES.md` §2). Config, not a case file's business.
pub const DEFAULT_LIVE_PROVIDER: &str = "nvidia";
pub const DEFAULT_LIVE_MODEL: &str = "deepseek-ai/deepseek-v4.1-flash";

#[derive(Subcommand, Debug)]
pub enum EvalCommand {
    /// Run a scenario suite: one line per case, non-zero exit when any case fails
    Run {
        /// Suite directory holding one YAML file per case
        #[arg(long, default_value = DEFAULT_SUITE)]
        suite: PathBuf,
        /// Replay the case's scripted model turns (default; no credential, no quota)
        #[arg(long)]
        offline: bool,
        /// Drive the real provider (skipped, exit 0, when no credential is registered)
        #[arg(long)]
        live: bool,
        #[arg(long, default_value = DEFAULT_LIVE_PROVIDER)]
        provider: String,
        #[arg(long, default_value = DEFAULT_LIVE_MODEL)]
        model: String,
        /// Run only the cases whose name contains this string
        #[arg(long)]
        filter: Option<String>,
        /// Where the machine-readable report is written
        #[arg(long, default_value = DEFAULT_REPORT)]
        report: PathBuf,
    },
}

pub async fn handle_eval_command(
    cmd: EvalCommand,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
    config: &Config,
) -> Result<(), HarnessError> {
    let EvalCommand::Run {
        suite,
        offline,
        live,
        provider,
        model,
        filter,
        report,
    } = cmd;

    if offline && live {
        return Err(HarnessError::ConfigurationError(
            "--offline and --live are mutually exclusive".to_string(),
        ));
    }

    // Default to offline: the CI-safe backend that cannot spend quota by accident.
    let options = RunOptions {
        suite,
        backend: if live {
            Backend::Live
        } else {
            Backend::Offline
        },
        filter,
        provider_id: provider.clone(),
        model_id: model.clone(),
        report_path: report.clone(),
        skip_reason: "skipped: no credential".to_string(),
        usage: None,
    };
    let report_path = options.report_path.clone();

    let report = if live {
        if cred_manager.get_credentials(&provider).await.is_empty() {
            run_suite::<GenericProviderClient, _>(&options, |_| None).await?
        } else {
            let client = Arc::new(GenericProviderClient::new(
                provider.clone(),
                model,
                cred_manager,
                discovery,
                config.providers.get(&provider),
            )?);
            run_suite(&options, move |_| Some(client.clone())).await?
        }
    } else {
        let usage = Arc::new(Mutex::new(TokenUsage::default()));
        let counter = usage.clone();
        let mut options = options;
        options.usage = Some(usage);
        run_suite(&options, move |scenario| {
            Some(Arc::new(ScriptedProvider::new(
                &scenario.script,
                counter.clone(),
            )))
        })
        .await?
    };

    print_report(&report, &report_path);

    if !report.ok() {
        std::process::exit(1);
    }
    Ok(())
}

fn print_report(report: &EvalReport, report_path: &Path) {
    for case in &report.cases {
        println!(
            "{:<24} {:<4} {}",
            case.case,
            case.status.as_str(),
            case.reason
        );
    }
    println!(
        "{} case(s): {} pass, {} fail, {} skip ({}, {} / {}) -> {}",
        report.total,
        report.passed,
        report.failed,
        report.skipped,
        report.backend,
        report.provider,
        report.model,
        report_path.display()
    );
}
