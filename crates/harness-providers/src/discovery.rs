use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::PathBuf;
use chrono::{DateTime, Duration, Utc};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use tracing::{debug, info};

use harness_core::config::ProviderOverride;
use harness_core::HarnessError;
use crate::auth::CredentialManager;
use crate::catalog::{resolve_descriptor, AuthHeaderFormat};
use crate::client::build_http_client;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCacheEntry {
    pub models: Vec<String>,
    pub fetched_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelsCache {
    pub providers: HashMap<String, ModelCacheEntry>,
}

pub struct ModelDiscovery {
    cache_path: PathBuf,
    ttl: Duration,
    /// Resolved `providers.<id>.*` config, applied when a provider's models endpoint is fetched.
    overrides: BTreeMap<String, ProviderOverride>,
}

impl ModelDiscovery {
    pub fn new(
        cache_path: Option<PathBuf>,
        ttl_hours: i64,
        overrides: BTreeMap<String, ProviderOverride>,
    ) -> Self {
        let path = cache_path.unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".luna").join("models_cache.json")
        });

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
        if let Some(parent) = self.cache_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(data) = serde_json::to_string_pretty(cache) {
            let _ = fs::write(&self.cache_path, data);
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
            if let Some(entry) = cache.providers.get(provider_id) {
                if Utc::now() - entry.fetched_at < self.ttl {
                    debug!("Returning cached models for provider '{}'", provider_id);
                    return Ok(entry.models.clone());
                }
            }
        }

        info!("Fetching dynamic model list from provider '{}' endpoint", provider_id);
        let models = self.fetch_live_models(provider_id, cred_manager).await?;

        cache.providers.insert(
            provider_id.to_string(),
            ModelCacheEntry {
                models: models.clone(),
                fetched_at: Utc::now(),
            },
        );
        self.save_cache(&cache);

        Ok(models)
    }

    async fn fetch_live_models(
        &self,
        provider_id: &str,
        cred_manager: &CredentialManager,
    ) -> Result<Vec<String>, HarnessError> {
        let descriptor = resolve_descriptor(provider_id, self.overrides.get(provider_id))?;
        // Per-provider timeout, so a hanging `/models` call cannot outlive the config value.
        let http_client = build_http_client(descriptor.timeout_secs)?;

        let secret = cred_manager.acquire_credential(provider_id).await?;
        let api_key = secret.expose_secret();

        let mut url = format!("{}{}", descriptor.default_base_url, descriptor.models_endpoint);
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
            let body = resp.text().await.unwrap_or_default();
            return Err(HarnessError::AuthenticationFailed {
                provider: provider_id.to_string(),
                message: format!("HTTP {} from model discovery: {}", status, body),
            });
        }

        let body_json: serde_json::Value = resp.json().await.map_err(|e| HarnessError::ProviderError {
            provider: provider_id.to_string(),
            message: format!("Failed to parse JSON response: {}", e),
        })?;

        let mut model_names = Vec::new();

        // 1. OpenAI / Groq / DeepSeek / OpenRouter / Together style: { "data": [ { "id": "model_name" } ] }
        if let Some(data) = body_json.get("data").and_then(|d| d.as_array()) {
            for item in data {
                if let Some(id) = item.get("id").and_then(|id| id.as_str()) {
                    model_names.push(id.to_string());
                }
            }
        }
        // 2. Gemini style: { "models": [ { "name": "models/gemini-1.5-pro" } ] }
        else if let Some(models) = body_json.get("models").and_then(|m| m.as_array()) {
            for item in models {
                if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                    let cleaned = name.strip_prefix("models/").unwrap_or(name);
                    model_names.push(cleaned.to_string());
                }
            }
        }
        // 3. Ollama style: { "models": [ { "name": "llama3:latest" } ] }
        else if let Some(models) = body_json.get("models").and_then(|m| m.as_array()) {
            for item in models {
                if let Some(name) = item.get("name").and_then(|n| n.as_str()) {
                    model_names.push(name.to_string());
                }
            }
        }

        model_names.sort();
        Ok(model_names)
    }
}
