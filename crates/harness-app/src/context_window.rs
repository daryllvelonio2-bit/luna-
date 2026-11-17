//! Shared context-window resolution across CLI, TUI, and GUI.

use harness_core::config::Config;
use harness_providers::{get_context_window, ModelDiscovery, DEFAULT_CONTEXT_WINDOW};

/// The context window for `provider`/`model`.
pub fn context_window(
    config: &Config,
    discovery: &ModelDiscovery,
    provider: &str,
    model: &str,
) -> u32 {
    discovery
        .context_window(provider, model)
        .or_else(|| {
            config
                .providers
                .get(provider)
                .and_then(|over| over.context_window)
        })
        .or_else(|| discovery.cross_provider_context_window(model))
        .or_else(|| get_context_window(provider, model))
        .unwrap_or(DEFAULT_CONTEXT_WINDOW)
}

/// The same answer for a caller holding only the config.
pub fn context_window_for(config: &Config, provider: &str, model: &str) -> u32 {
    let discovery = ModelDiscovery::new(
        Some(config.paths.models_cache.clone()),
        0,
        config.providers.clone(),
    );
    context_window(config, &discovery, provider, model)
}
