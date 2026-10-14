use std::io::IsTerminal;

use clap::Subcommand;

use harness_core::config::{self, Config, Layers, ProviderOverride};
use harness_core::HarnessError;
use harness_providers::get_provider_catalog;

#[derive(Subcommand, Debug)]
pub enum ConfigCommand {
    /// Print the effective value of one configuration key
    Get {
        /// Dotted key, e.g. model.default or paths.memory_db
        key: String,
        /// Also print the layer the value came from (default, file, env, flag)
        #[arg(long)]
        show_source: bool,
    },
    /// Print every effective key
    List {
        /// Also print the layer each value came from
        #[arg(long)]
        show_source: bool,
    },
    /// Write a key into the config file (prompts for the value when it is omitted)
    Set {
        /// Dotted key, e.g. agent.max_consecutive_failures
        key: String,
        /// New value; omit it to be prompted
        value: Option<String>,
    },
}

/// Catalog-derived `providers.<id>.*` defaults.
///
/// The provider catalog owns base URLs, models endpoints and timeouts, so the config file format
/// carries no vendor literals and `harness-core` needs no provider knowledge. Injecting this
/// before resolving is also what makes `luna config get providers.nvidia.base_url` and
/// `LUNA_PROVIDERS_NVIDIA_BASE_URL` work.
pub fn catalog_defaults() -> Config {
    let mut config = Config::default();
    for provider in get_provider_catalog() {
        config.providers.insert(
            provider.id,
            ProviderOverride {
                base_url: Some(provider.default_base_url),
                models_endpoint: Some(provider.models_endpoint),
                timeout_secs: Some(provider.timeout_secs),
            },
        );
    }
    config
}

pub fn handle_config_command(cmd: ConfigCommand, layers: &Layers) -> Result<(), HarnessError> {
    match cmd {
        ConfigCommand::Get { key, show_source } => {
            let resolved = config::resolve(layers)?;
            let value = resolved.get(&key)?;
            if show_source {
                println!("{} (source: {})", value, resolved.source(&key)?);
            } else {
                println!("{}", value);
            }
        }
        ConfigCommand::List { show_source } => {
            let resolved = config::resolve(layers)?;
            for (key, value, source) in resolved.iter() {
                if show_source {
                    println!("{} = {} (source: {})", key, value, source);
                } else {
                    println!("{} = {}", key, value);
                }
            }
        }
        ConfigCommand::Set { key, value } => {
            let raw = match value {
                Some(value) => value,
                None => prompt_for_value(layers, &key)?,
            };
            let path = config::set_in_file(layers, &key, &raw)?;
            // Report what the file now resolves to rather than what was typed.
            let resolved = config::resolve(layers)?;
            println!(
                "{} = {} ({}, source: {})",
                key,
                resolved.get(&key)?,
                path.display(),
                resolved.source(&key)?
            );
        }
    }
    Ok(())
}

/// Ask for the value instead of writing a key with an empty value. A key that is not settable is
/// rejected *before* the prompt, so a typo never costs an answer.
fn prompt_for_value(layers: &Layers, key: &str) -> Result<String, HarnessError> {
    let resolved = config::resolve(layers)?;
    if !resolved.knows(key) {
        // Reuse the unknown-key error (it names the nearest known key).
        resolved.get(key)?;
    }
    if !std::io::stdin().is_terminal() {
        return Err(HarnessError::ConfigurationError(format!(
            "no value given for '{key}' and stdin is not a terminal: use `luna config set {key} <value>`"
        )));
    }
    dialoguer::Input::<String>::new()
        .with_prompt(format!("Value for {key}"))
        .interact()
        .map_err(|error| HarnessError::Io(error.to_string()))
}
