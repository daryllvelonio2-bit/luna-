//! One answer to "how much room does this model have?".
//!
//! Several places need the number — the status line after every turn, the model picker, `/compact`,
//! `luna heartbeat` — and each used to compute it with its own `unwrap_or(128_000)`. That literal
//! is why a model the catalog had never heard of (any recent release) showed a 128k window whatever
//! it really had. The resolution order lives here instead, and callers just ask.
//!
//! First answer wins:
//!
//! 1. what the provider published for this model (`/models`, cached by discovery) — measured, which
//!    is why an asserted value below cannot beat it: over-stating a window fails a turn outright,
//!    while under-stating only folds the conversation earlier than it had to;
//! 2. `providers.<id>.context_window` — the operator's statement about a model this build predates;
//! 3. the static catalog tables and the model-family rules;
//! 4. `DEFAULT_CONTEXT_WINDOW`.

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

/// The same answer for a caller holding only the config. The cache path comes from
/// `paths.models_cache`, so the lookup is identical; the TTL is irrelevant because this only reads
/// the cache a real fetch already wrote.
pub fn context_window_for(config: &Config, provider: &str, model: &str) -> u32 {
    let discovery = ModelDiscovery::new(
        Some(config.paths.models_cache.clone()),
        0,
        config.providers.clone(),
    );
    context_window(config, &discovery, provider, model)
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_core::config::ProviderOverride;

    fn discovery_with(cache: &str) -> (tempfile::TempDir, ModelDiscovery) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("models_cache.json");
        std::fs::write(&path, cache).expect("write the cache");
        (dir, ModelDiscovery::new(Some(path), 24, Default::default()))
    }

    fn config() -> Config {
        Config::default()
    }

    /// The provider's own measurement is the answer, even when the catalog has an opinion.
    #[test]
    fn a_published_window_beats_every_guess() {
        let (_dir, discovery) = discovery_with(
            r#"{"providers":{"openrouter":{"models":["x/y"],"fetched_at":"2999-01-01T00:00:00Z",
                "context_windows":{"x/y":262144}}}}"#,
        );

        assert_eq!(
            context_window(&config(), &discovery, "openrouter", "x/y"),
            262_144
        );
    }

    /// A model nothing can describe falls to the documented default rather than to a model family
    /// it happens to look like, and the operator can raise it without a rebuild.
    #[test]
    fn an_unknown_model_uses_the_config_fallback() {
        let (_dir, discovery) = discovery_with("{}");
        let mut config = config();

        assert_eq!(
            context_window(&config, &discovery, "nvidia", "some/new-model"),
            128_000
        );

        config.providers.insert(
            "nvidia".to_string(),
            ProviderOverride {
                context_window: Some(1_000_000),
                ..Default::default()
            },
        );
        assert_eq!(
            context_window(&config, &discovery, "nvidia", "some/new-model"),
            1_000_000
        );
    }

    /// The config fallback also corrects a family rule the operator knows better, which is the
    /// whole point of having it.
    #[test]
    fn the_config_fallback_outranks_a_family_guess() {
        let (_dir, discovery) = discovery_with("{}");
        let mut config = config();
        config.providers.insert(
            "nvidia".to_string(),
            ProviderOverride {
                context_window: Some(262_144),
                ..Default::default()
            },
        );

        // The family rule alone would say 128k for an unrecognised model on this provider.
        assert_eq!(
            context_window(&config, &discovery, "nvidia", "vendor/brand-new"),
            262_144
        );
    }

    /// A known model with no published window still gets its family's real window, so the config
    /// fallback does not swallow the cases the catalog already answers correctly.
    #[test]
    fn a_known_family_keeps_its_catalog_window() {
        let (_dir, discovery) = discovery_with("{}");

        assert_eq!(
            context_window(&config(), &discovery, "nvidia", "z-ai/glm-5.3-flash"),
            1_000_000
        );
        assert_eq!(
            context_window(
                &config(),
                &discovery,
                "nvidia",
                "minimaxai/minimax-m3.1-flash"
            ),
            1_000_000
        );
    }

    #[test]
    fn cross_provider_discovery_cache_provides_real_context_window() {
        let (_dir, discovery) = discovery_with(
            r#"{"version":1,"providers":{"openrouter":{"models":["minimaxai/minimax-m3.1-flash"],"fetched_at":"2999-01-01T00:00:00Z","context_windows":{"minimaxai/minimax-m3.1-flash":1048576}}}}"#,
        );

        // Even though nvidia endpoint reports no context window, the real window from discovery cache is used
        assert_eq!(
            context_window(
                &config(),
                &discovery,
                "nvidia",
                "minimaxai/minimax-m3.1-flash"
            ),
            1_048_576
        );
    }

    /// An old cache file — written before windows were stored — must still parse, and simply
    /// contributes nothing rather than poisoning the lookup.
    #[test]
    fn a_cache_without_windows_falls_through_cleanly() {
        let (_dir, discovery) = discovery_with(
            r#"{"providers":{"openrouter":{"models":["x/y"],"fetched_at":"2999-01-01T00:00:00Z"}}}"#,
        );

        assert_eq!(discovery.context_window("openrouter", "x/y"), None);
        assert_eq!(
            context_window(
                &config(),
                &discovery,
                "openrouter",
                "anthropic/claude-3.5-sonnet"
            ),
            200_000
        );
    }
}
