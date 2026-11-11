use chrono::{DateTime, Duration, Utc};
use futures_util::StreamExt as _;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;
use tracing::{debug, info, warn};

use crate::auth::CredentialManager;
use crate::catalog::{resolve_descriptor, AuthHeaderFormat};
use crate::client::build_http_client;
use harness_core::config::ProviderOverride;
use harness_core::HarnessError;

/// What a provider's `/models` listing gave back: the ids, and the context window for any model
/// that came with one.
///
/// Most OpenAI-compatible catalogs report only ids; the ones that do publish a window have it under
/// a different key each, so [`reported_context_window`] reads all the spellings rather than a
/// per-provider table. A model with no published window is simply absent from the map, and the
/// catalog's own answer stands.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelListing {
    pub models: Vec<String>,
    pub context_windows: HashMap<String, u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCacheEntry {
    pub models: Vec<String>,
    pub fetched_at: DateTime<Utc>,
    /// Windows the provider published, keyed by model id. `default` so a cache written before this
    /// existed still parses instead of being thrown away.
    #[serde(default)]
    pub context_windows: HashMap<String, u32>,
}

/// Cache file schema. Bumped when a change means entries written by an older build must be
/// refetched rather than trusted: version 1 is the build that started recording the context
/// windows providers publish, and version 2 is the build that corrected the Antigravity model
/// list, so a version-1 entry would keep serving the retired `gemini-2.5-pro`-era picker for a
/// day after the upgrade. Version 0 (written before the field existed) is refetched for the same
/// reason: it would answer "nothing published" forever and every model would keep its guessed
/// window until the TTL happened to expire.
pub const CACHE_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelsCache {
    /// `default` so a cache written before the field existed parses (as version 0) and is refetched.
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub providers: HashMap<String, ModelCacheEntry>,
}

pub struct ModelDiscovery {
    cache_path: PathBuf,
    ttl: Duration,
    /// Resolved `providers.<id>.*` config, applied when a provider's models endpoint is fetched.
    overrides: BTreeMap<String, ProviderOverride>,
}

/// How many providers are asked for their model list at once.
///
/// Four, the width the rest of the harness uses for independent calls: enough to hide the round-trip
/// latency of a catalog, few enough that twenty configured providers do not open twenty sockets and
/// collect twenty rate limits.
const MAX_CONCURRENT_PROVIDER_REFRESHES: usize = 4;

impl ModelDiscovery {
    pub fn new(
        cache_path: Option<PathBuf>,
        ttl_hours: i64,
        overrides: BTreeMap<String, ProviderOverride>,
    ) -> Self {
        let path = cache_path
            .unwrap_or_else(|| harness_core::config::PathsSettings::default().models_cache);

        Self {
            cache_path: path,
            ttl: Duration::hours(ttl_hours),
            overrides,
        }
    }

    fn load_cache(&self) -> ModelsCache {
        if !self.cache_path.exists() {
            return ModelsCache::default();
        }
        let data = fs::read_to_string(&self.cache_path).unwrap_or_default();
        serde_json::from_str(&data).unwrap_or_default()
    }

    fn save_cache(&self, cache: &ModelsCache) {
        // The cache is best-effort: a failed write just means the next lookup hits the network,
        // but say so instead of dropping the error silently.
        if let Some(parent) = self.cache_path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                tracing::warn!(
                    "cannot create model cache dir '{}': {error}",
                    parent.display()
                );
                return;
            }
        }
        match serde_json::to_string_pretty(cache) {
            Ok(data) => {
                if let Err(error) = fs::write(&self.cache_path, data) {
                    tracing::warn!(
                        "cannot write model cache '{}': {error}",
                        self.cache_path.display()
                    );
                }
            }
            Err(error) => tracing::warn!("cannot serialize model cache: {error}"),
        }
    }

    pub async fn get_models(
        &self,
        provider_id: &str,
        cred_manager: &CredentialManager,
        force_refresh: bool,
    ) -> Result<Vec<String>, HarnessError> {
        let mut cache = self.load_cache();

        if !force_refresh {
            if let Some(models) = serve_from_cache(&cache, provider_id, self.ttl, Utc::now()) {
                debug!("Returning cached models for provider '{}'", provider_id);
                return Ok(models);
            }
        }

        info!(
            "Fetching dynamic model list from provider '{}' endpoint",
            provider_id
        );
        let listing = self.fetch_live_models(provider_id, cred_manager).await?;

        cache.version = CACHE_SCHEMA_VERSION;
        cache.providers.insert(
            provider_id.to_string(),
            ModelCacheEntry {
                models: listing.models.clone(),
                fetched_at: Utc::now(),
                context_windows: listing.context_windows,
            },
        );
        self.save_cache(&cache);

        Ok(listing.models)
    }

    /// The context window `provider_id` published for `model`, if its catalog stated one.
    ///
    /// This is the authoritative answer when it exists: the provider measures its own models, while
    /// everything the catalog crate holds is either a hand-written table or a family guess. Read
    /// from the discovery cache, so it costs a file read and no network call, and returns `None`
    /// (rather than guessing) when the provider published nothing or has not been fetched yet.
    pub fn context_window(&self, provider_id: &str, model: &str) -> Option<u32> {
        let cache = self.load_cache();
        let entry = cache.providers.get(provider_id)?;
        entry
            .context_windows
            .get(model)
            .or_else(|| {
                // Some catalogs key the entry by a different prefix than the chat route uses.
                let leaf = model.rsplit('/').next().unwrap_or(model);
                entry
                    .context_windows
                    .iter()
                    .find(|(id, _)| id.rsplit('/').next() == Some(leaf))
                    .map(|(_, window)| window)
            })
            .copied()
            .filter(|window| *window > 0)
    }

    /// Search across all providers in the discovery cache for a published context window
    /// matching the given model ID or its trailing leaf name.
    pub fn cross_provider_context_window(&self, model: &str) -> Option<u32> {
        let cache = self.load_cache();
        let leaf = model.rsplit('/').next().unwrap_or(model);
        let mut best: Option<u32> = None;
        for entry in cache.providers.values() {
            if let Some(&window) = entry.context_windows.get(model) {
                if window > 0 {
                    best = Some(best.map_or(window, |current| current.max(window)));
                }
            }
            if let Some((_, &window)) = entry
                .context_windows
                .iter()
                .find(|(id, _)| id.rsplit('/').next() == Some(leaf))
            {
                if window > 0 {
                    best = Some(best.map_or(window, |current| current.max(window)));
                }
            }
        }
        best
    }

    pub async fn get_all_target_providers(&self, cred_manager: &CredentialManager) -> Vec<String> {
        let mut providers = Vec::new();
        for p in cred_manager.get_loaded_providers().await {
            if !providers.contains(&p) {
                providers.push(p);
            }
        }
        for p in self.overrides.keys() {
            if !providers.contains(p) {
                providers.push(p.clone());
            }
        }
        for p in [
            "opencode",
            "opencode-cli",
            "opencode-zen",
            "antigravity",
            "kiro",
            "kiro-cli",
            "supercode",
        ] {
            let s = p.to_string();
            if !providers.contains(&s) {
                providers.push(s);
            }
        }
        for p in crate::catalog::known_provider_ids() {
            if !providers.contains(&p) {
                providers.push(p);
            }
        }
        providers
    }

    /// Every provider's model list, refreshed.
    ///
    /// A provider that cannot be reached keeps its entry, carrying its error: a refresh that quietly
    /// shortens the catalog is worse than one that names what it could not read.
    pub async fn refresh_all_models(
        &self,
        cred_manager: &CredentialManager,
    ) -> Vec<(String, Result<Vec<String>, HarnessError>)> {
        // The list is settled first: which providers are worth asking is a decision, and asking them
        // is a set of independent HTTPS calls. Reading them one after another means the catalog
        // takes as long as the sum of every provider's `/models` — seconds each, a minute or more
        // for a machine with every provider configured.
        let mut targets = Vec::new();
        let mut seen_canonical = std::collections::HashSet::new();
        for provider in self.get_all_target_providers(cred_manager).await {
            let canonical = crate::catalog::canonicalize_provider_id(&provider);
            if !seen_canonical.insert(canonical.to_string()) {
                continue;
            }
            if crate::catalog::find_provider(&provider).is_none() {
                continue;
            }
            targets.push(provider);
        }

        // Bounded, not unbounded: a machine with twenty providers configured should not open twenty
        // sockets at once, and the providers themselves rate limit.
        futures_util::stream::iter(targets)
            .map(|provider| async move {
                if provider == "kiro" || provider == "kiro-cli" {
                    if let Err(error) =
                        crate::kiro::sync_kiro_credential_if_needed(cred_manager).await
                    {
                        warn!("could not sync the Kiro credential before discovery: {error}");
                    }
                }

                if provider == "supercode" {
                    if let Err(error) =
                        crate::supercode::sync_supercode_credential_if_needed(cred_manager).await
                    {
                        warn!("could not sync the Supercode credential before discovery: {error}");
                    }
                }

                let has_creds = cred_manager.acquire_credential(&provider).await.is_ok();
                let is_auto = matches!(
                    provider.as_str(),
                    "opencode"
                        | "opencode-cli"
                        | "opencode-zen"
                        | "antigravity"
                        | "agy"
                        | "kiro"
                        | "kiro-cli"
                        | "supercode"
                );

                if !has_creds && !is_auto {
                    return None;
                }

                let res = self.get_models(&provider, cred_manager, true).await;
                Some((provider, res))
            })
            .buffered(MAX_CONCURRENT_PROVIDER_REFRESHES)
            .filter_map(|result| async move { result })
            .collect()
            .await
    }

    async fn fetch_live_models(
        &self,
        provider_id: &str,
        cred_manager: &CredentialManager,
    ) -> Result<ModelListing, HarnessError> {
        let descriptor = resolve_descriptor(provider_id, self.overrides.get(provider_id))?;
        // Per-provider timeout, so a hanging `/models` call cannot outlive the config value.
        let http_client = build_http_client(descriptor.timeout_secs)?;

        if provider_id == "supercode" {
            if let Err(error) =
                crate::supercode::sync_supercode_credential_if_needed(cred_manager).await
            {
                warn!("could not sync the Supercode credential before discovery: {error}");
            }
            return crate::supercode::fetch_supercode_models()
                .await
                .map(names_only);
        }

        if provider_id == "kiro" || provider_id == "kiro-cli" {
            if let Err(error) = crate::kiro::sync_kiro_credential_if_needed(cred_manager).await {
                warn!("could not sync the Kiro credential before discovery: {error}");
            }
            let secret = cred_manager.acquire_credential(provider_id).await?;
            return crate::kiro::fetch_kiro_models(&http_client, secret.expose_secret())
                .await
                .map(names_only);
        }

        let secret = cred_manager.acquire_credential(provider_id).await?;
        let api_key = secret.expose_secret();

        if provider_id == "antigravity" || provider_id == "agy" {
            let (bearer, _) = crate::antigravity::extract_bearer_token(api_key);
            return crate::antigravity::fetch_antigravity_models(&http_client, &bearer)
                .await
                .map(names_only);
        }

        if provider_id == "opencode-cli" || provider_id == "opencode-headless" {
            return crate::opencode::fetch_opencode_cli_models()
                .await
                .map(names_only);
        }

        if provider_id == "opencode" || provider_id == "opencode-zen" {
            return crate::opencode::fetch_opencode_models()
                .await
                .map(names_only);
        }

        let mut url = format!(
            "{}{}",
            descriptor.default_base_url, descriptor.models_endpoint
        );
        let mut headers = HeaderMap::new();

        match descriptor.auth_header_format {
            AuthHeaderFormat::Bearer => {
                headers.insert(
                    AUTHORIZATION,
                    HeaderValue::from_str(&format!("Bearer {}", api_key))
                        .map_err(|e| HarnessError::ConfigurationError(e.to_string()))?,
                );
            }
            AuthHeaderFormat::AnthropicApiKey => {
                headers.insert(
                    "x-api-key",
                    HeaderValue::from_str(api_key)
                        .map_err(|e| HarnessError::ConfigurationError(e.to_string()))?,
                );
                headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
            }
            AuthHeaderFormat::QueryParam(ref param) => {
                url = format!("{}?{}={}", url, param, api_key);
            }
            AuthHeaderFormat::None => {}
        }

        let resp = http_client
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| HarnessError::ProviderError {
                provider: provider_id.to_string(),
                message: format!("HTTP request failed: {}", e),
            })?;

        if !resp.status().is_success() {
            let status = resp.status();
            // Model discovery is the first call a session makes, so this is where a dead key
            // usually shows up: retire it rather than reporting the same 401 on every run.
            if matches!(status.as_u16(), 401 | 403 | 429) {
                let retired = cred_manager
                    .mark_credential_failed(provider_id, secret.expose_secret())
                    .await;
                if retired {
                    warn!(
                        "Model discovery rejected the selected credential for '{}'; retired",
                        provider_id
                    );
                }
            }
            let body = resp.text().await.unwrap_or_default();
            return Err(HarnessError::AuthenticationFailed {
                provider: provider_id.to_string(),
                message: format!("HTTP {} from model discovery: {}", status, body),
            });
        }

        let body_json: serde_json::Value =
            resp.json().await.map_err(|e| HarnessError::ProviderError {
                provider: provider_id.to_string(),
                message: format!("Failed to parse JSON response: {}", e),
            })?;

        let mut listing = listing_from_catalog(&body_json);
        listing.models.sort();
        listing.models.dedup();
        Ok(listing)
    }
}

/// The cached model list for `provider_id`, when it is both fresh and written by this schema.
///
/// The schema check is what lets a change to what discovery records (the context windows) take
/// effect on a machine that already has a cache: an older version 0 file is refetched once instead
/// of being served for the rest of its 24h TTL, which would otherwise leave every model on its
/// guessed window for a day after the upgrade.
fn serve_from_cache(
    cache: &ModelsCache,
    provider_id: &str,
    ttl: Duration,
    now: DateTime<Utc>,
) -> Option<Vec<String>> {
    if cache.version != CACHE_SCHEMA_VERSION {
        return None;
    }
    let entry = cache.providers.get(provider_id)?;
    (now - entry.fetched_at < ttl).then(|| entry.models.clone())
}

/// The ids and published windows in a `/models` body.
///
/// Two shapes cover every provider in the catalog: `{ "data": [{ "id": … }] }` (OpenAI, Groq,
/// DeepSeek, OpenRouter, Together, NVIDIA, Mistral) and `{ "models": [{ "name": … }] }` (Gemini,
/// which prefixes its names with `models/`, and Ollama, which does not — stripping the prefix is a
/// no-op for the latter, so one branch reads both).
fn listing_from_catalog(body: &serde_json::Value) -> ModelListing {
    let mut listing = ModelListing::default();
    let entries = body
        .get("data")
        .or_else(|| body.get("models"))
        .and_then(|value| value.as_array());

    for item in entries.into_iter().flatten() {
        let id = item
            .get("id")
            .or_else(|| item.get("name"))
            .and_then(|value| value.as_str())
            .map(|name| name.strip_prefix("models/").unwrap_or(name));
        let Some(id) = id else { continue };

        listing.models.push(id.to_string());
        if let Some(window) = reported_context_window(item) {
            listing.context_windows.insert(id.to_string(), window);
        }
    }

    listing
}

/// The context window a model entry published, if it published one.
///
/// Each catalog spells it differently — OpenRouter `context_length` (nested under `top_provider`),
/// Mistral `max_context_length`, Gemini `inputTokenLimit`, various gateways `max_input_tokens` —
/// and most spell nothing at all (NVIDIA, Groq, the OpenAI-compatible Gemini route), which is why
/// the caller treats an absent value as "unknown" rather than as a number to invent.
fn reported_context_window(item: &serde_json::Value) -> Option<u32> {
    const KEYS: [&str; 6] = [
        "context_length",
        "max_context_length",
        "max_input_tokens",
        "inputTokenLimit",
        "input_token_limit",
        "context_window",
    ];

    let nested = item
        .get("top_provider")
        .and_then(|top| KEYS.iter().find_map(|key| top.get(*key)));
    let flat = KEYS.iter().find_map(|key| item.get(*key));

    [nested, flat]
        .into_iter()
        .flatten()
        .find_map(|value| value.as_u64())
        .and_then(|tokens| u32::try_from(tokens).ok())
        .filter(|tokens| *tokens > 0)
}

/// A model list from a route that reports ids only.
fn names_only(models: Vec<String>) -> ModelListing {
    ModelListing {
        models,
        context_windows: HashMap::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_discovery(cache: &str) -> (tempfile::TempDir, ModelDiscovery) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("models_cache.json");
        std::fs::write(&path, cache).expect("write the cache");
        (dir, ModelDiscovery::new(Some(path), 24, Default::default()))
    }

    #[test]
    fn a_listing_keeps_the_ids_and_the_windows_a_catalog_publishes() {
        let listing = listing_from_catalog(&json!({
            "data": [
                { "id": "a/one", "context_length": 1_048_576 },
                { "id": "b/two", "top_provider": { "context_length": 262_144 } },
                { "id": "c/three", "max_context_length": 32_768 },
                { "id": "d/four" }
            ]
        }));

        assert_eq!(listing.models.len(), 4);
        assert_eq!(listing.context_windows.get("a/one"), Some(&1_048_576));
        assert_eq!(listing.context_windows.get("b/two"), Some(&262_144));
        assert_eq!(listing.context_windows.get("c/three"), Some(&32_768));
        assert_eq!(
            listing.context_windows.get("d/four"),
            None,
            "a catalog that states no window must not have one invented for it"
        );
    }

    /// Gemini prefixes its ids with `models/` and Ollama does not; both arrive under `models`.
    #[test]
    fn both_catalog_shapes_are_read_and_the_prefix_is_stripped() {
        let gemini = listing_from_catalog(&json!({
            "models": [{ "name": "models/gemini-2.5-flash", "inputTokenLimit": 1_048_576 }]
        }));
        assert_eq!(gemini.models, vec!["gemini-2.5-flash"]);
        assert_eq!(
            gemini.context_windows.get("gemini-2.5-flash"),
            Some(&1_048_576),
            "the id in the map has to be the one the chat route uses"
        );

        let ollama = listing_from_catalog(&json!({ "models": [{ "name": "llama3.2" }] }));
        assert_eq!(ollama.models, vec!["llama3.2"]);
    }

    #[test]
    fn a_window_that_is_not_a_positive_count_is_ignored() {
        assert_eq!(
            reported_context_window(&json!({ "context_length": 0 })),
            None
        );
        assert_eq!(
            reported_context_window(&json!({ "context_length": -1 })),
            None
        );
        assert_eq!(
            reported_context_window(&json!({ "context_length": "128k" })),
            None
        );
        assert_eq!(reported_context_window(&json!({ "id": "x" })), None);
    }

    /// The migration that makes the fix land on a machine that already has a cache: a file written
    /// before windows were recorded is refetched instead of being served for the rest of its TTL.
    #[test]
    fn a_cache_from_an_older_schema_is_refetched_and_a_fresh_one_is_served() {
        let now = Utc::now();
        let entry = ModelCacheEntry {
            models: vec!["vendor/model".to_string()],
            fetched_at: now,
            context_windows: HashMap::from([("vendor/model".to_string(), 1_048_576)]),
        };
        let ttl = Duration::hours(24);

        let current = ModelsCache {
            version: CACHE_SCHEMA_VERSION,
            providers: HashMap::from([("openrouter".to_string(), entry.clone())]),
        };
        assert_eq!(
            serve_from_cache(&current, "openrouter", ttl, now),
            Some(vec!["vendor/model".to_string()])
        );

        let old_schema = ModelsCache {
            version: 0,
            providers: HashMap::from([("openrouter".to_string(), entry.clone())]),
        };
        assert_eq!(
            serve_from_cache(&old_schema, "openrouter", ttl, now),
            None,
            "a version-0 cache predates the recorded windows and must not be served"
        );

        let expired = ModelsCache {
            version: CACHE_SCHEMA_VERSION,
            providers: HashMap::from([(
                "openrouter".to_string(),
                ModelCacheEntry {
                    fetched_at: now - Duration::hours(25),
                    ..entry
                },
            )]),
        };
        assert_eq!(serve_from_cache(&expired, "openrouter", ttl, now), None);
    }

    #[test]
    fn a_window_is_looked_up_by_the_full_id_or_by_its_leaf() {
        let (_dir, discovery) = temp_discovery(&format!(
            r#"{{"version":{},"providers":{{"openrouter":{{"models":["vendor/model-a"],
                "fetched_at":"2999-01-01T00:00:00Z",
                "context_windows":{{"vendor/model-a":1048576}}}}}}}}"#,
            CACHE_SCHEMA_VERSION
        ));

        assert_eq!(
            discovery.context_window("openrouter", "vendor/model-a"),
            Some(1_048_576)
        );
        assert_eq!(
            discovery.context_window("openrouter", "model-a"),
            Some(1_048_576)
        );
        assert_eq!(discovery.context_window("openrouter", "vendor/other"), None);
        assert_eq!(discovery.context_window("nvidia", "vendor/model-a"), None);
        assert_eq!(
            discovery.cross_provider_context_window("vendor/model-a"),
            Some(1_048_576)
        );
        assert_eq!(
            discovery.cross_provider_context_window("model-a"),
            Some(1_048_576)
        );
        assert_eq!(discovery.cross_provider_context_window("unknown"), None);
    }
}
