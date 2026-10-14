use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use chrono::{DateTime, Utc};
use rand::Rng;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use harness_core::HarnessError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RotationStrategy {
    RoundRobin,
    FillFirst,
    Random,
    LeastUsed,
}

impl Default for RotationStrategy {
    fn default() -> Self {
        Self::RoundRobin
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CredentialEntry {
    pub id: String,
    pub label: String,
    pub api_key: String, // Stored encrypted or in OS keyring in production
    pub is_priority: bool,
    pub usage_count: u64,
    pub last_used: Option<DateTime<Utc>>,
    pub is_temporarily_disabled: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProviderAuthState {
    pub strategy: RotationStrategy,
    pub credentials: Vec<CredentialEntry>,
    pub current_round_robin_index: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LunaAuthStorage {
    pub providers: HashMap<String, ProviderAuthState>,
}

pub struct CredentialManager {
    storage_path: PathBuf,
    state: RwLock<LunaAuthStorage>,
    round_robin_counter: AtomicUsize,
}

impl CredentialManager {
    pub fn new(storage_path: Option<PathBuf>) -> Arc<Self> {
        let path = storage_path.unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".luna").join("credentials.json")
        });

        let loaded = Self::load_from_disk(&path).unwrap_or_default();

        Arc::new(Self {
            storage_path: path,
            state: RwLock::new(loaded),
            round_robin_counter: AtomicUsize::new(0),
        })
    }

    fn load_from_disk(path: &PathBuf) -> Result<LunaAuthStorage, HarnessError> {
        if !path.exists() {
            return Ok(LunaAuthStorage::default());
        }
        let data = fs::read_to_string(path).map_err(|e| HarnessError::Io(e.to_string()))?;
        serde_json::from_str(&data).map_err(|e| HarnessError::SerializationError(e.to_string()))
    }

    pub async fn save_to_disk(&self) -> Result<(), HarnessError> {
        let state = self.state.read().await;
        if let Some(parent) = self.storage_path.parent() {
            fs::create_dir_all(parent).map_err(|e| HarnessError::Io(e.to_string()))?;
        }
        let serialized = serde_json::to_string_pretty(&*state)
            .map_err(|e| HarnessError::SerializationError(e.to_string()))?;
        fs::write(&self.storage_path, serialized).map_err(|e| HarnessError::Io(e.to_string()))?;
        Ok(())
    }

    pub async fn add_credential(
        &self,
        provider: &str,
        label: &str,
        key: &str,
        is_priority: bool,
    ) -> Result<String, HarnessError> {
        let mut state = self.state.write().await;
        let provider_state = state.providers.entry(provider.to_lowercase()).or_default();

        let id = uuid::Uuid::new_v4().to_string();
        if is_priority {
            for cred in &mut provider_state.credentials {
                cred.is_priority = false;
            }
        }

        let entry = CredentialEntry {
            id: id.clone(),
            label: label.to_string(),
            api_key: key.to_string(),
            is_priority,
            usage_count: 0,
            last_used: None,
            is_temporarily_disabled: false,
        };

        provider_state.credentials.push(entry);
        drop(state);
        self.save_to_disk().await?;
        Ok(id)
    }

    pub async fn remove_credential(&self, provider: &str, label_or_id: &str) -> Result<bool, HarnessError> {
        let mut state = self.state.write().await;
        if let Some(provider_state) = state.providers.get_mut(&provider.to_lowercase()) {
            let initial_len = provider_state.credentials.len();
            provider_state.credentials.retain(|c| c.id != label_or_id && c.label != label_or_id);
            let removed = provider_state.credentials.len() < initial_len;
            drop(state);
            if removed {
                self.save_to_disk().await?;
            }
            Ok(removed)
        } else {
            Ok(false)
        }
    }

    pub async fn set_rotation_strategy(
        &self,
        provider: &str,
        strategy: RotationStrategy,
    ) -> Result<(), HarnessError> {
        let mut state = self.state.write().await;
        let provider_state = state.providers.entry(provider.to_lowercase()).or_default();
        provider_state.strategy = strategy;
        drop(state);
        self.save_to_disk().await?;
        Ok(())
    }

    pub async fn set_priority_credential(
        &self,
        provider: &str,
        label_or_id: &str,
    ) -> Result<bool, HarnessError> {
        let mut state = self.state.write().await;
        if let Some(provider_state) = state.providers.get_mut(&provider.to_lowercase()) {
            let mut found = false;
            for cred in &mut provider_state.credentials {
                if cred.id == label_or_id || cred.label == label_or_id {
                    cred.is_priority = true;
                    found = true;
                } else {
                    cred.is_priority = false;
                }
            }
            drop(state);
            if found {
                self.save_to_disk().await?;
            }
            Ok(found)
        } else {
            Ok(false)
        }
    }

    pub async fn get_credentials(&self, provider: &str) -> Vec<CredentialEntry> {
        let state = self.state.read().await;
        state
            .providers
            .get(&provider.to_lowercase())
            .map(|p| p.credentials.clone())
            .unwrap_or_default()
    }

    pub async fn acquire_credential(&self, provider: &str) -> Result<SecretString, HarnessError> {
        let mut state = self.state.write().await;
        let provider_state = state
            .providers
            .get_mut(&provider.to_lowercase())
            .ok_or_else(|| HarnessError::AuthenticationFailed {
                provider: provider.to_string(),
                message: "No credentials registered for provider in Luna Auth".to_string(),
            })?;

        let active_credentials: Vec<usize> = provider_state
            .credentials
            .iter()
            .enumerate()
            .filter(|(_, c)| !c.is_temporarily_disabled)
            .map(|(i, _)| i)
            .collect();

        if active_credentials.is_empty() {
            // Re-enable if all were temporarily disabled
            for c in &mut provider_state.credentials {
                c.is_temporarily_disabled = false;
            }
            return Err(HarnessError::AuthenticationFailed {
                provider: provider.to_string(),
                message: "All credentials for provider are exhausted or disabled".to_string(),
            });
        }

        let selected_index = match provider_state.strategy {
            RotationStrategy::FillFirst => {
                // Priority credential first, otherwise first active credential
                if let Some(idx) = active_credentials
                    .iter()
                    .copied()
                    .find(|&i| provider_state.credentials[i].is_priority)
                {
                    idx
                } else {
                    active_credentials[0]
                }
            }
            RotationStrategy::RoundRobin => {
                let count = active_credentials.len();
                let next = self.round_robin_counter.fetch_add(1, Ordering::Relaxed) % count;
                active_credentials[next]
            }
            RotationStrategy::Random => {
                let mut rng = rand::thread_rng();
                let idx = rng.gen_range(0..active_credentials.len());
                active_credentials[idx]
            }
            RotationStrategy::LeastUsed => {
                let mut min_idx = active_credentials[0];
                let mut min_usage = provider_state.credentials[min_idx].usage_count;
                for &idx in &active_credentials[1..] {
                    if provider_state.credentials[idx].usage_count < min_usage {
                        min_usage = provider_state.credentials[idx].usage_count;
                        min_idx = idx;
                    }
                }
                min_idx
            }
        };

        let cred = &mut provider_state.credentials[selected_index];
        cred.usage_count += 1;
        cred.last_used = Some(Utc::now());
        let raw_key = cred.api_key.clone();

        Ok(SecretString::new(raw_key))
    }

    pub async fn mark_credential_failed(&self, provider: &str, _api_key: &str) {
        let mut state = self.state.write().await;
        if let Some(p) = state.providers.get_mut(&provider.to_lowercase()) {
            if p.strategy == RotationStrategy::FillFirst && p.credentials.len() > 1 {
                // Temporarily disable the first/priority one to failover
                if let Some(first) = p.credentials.iter_mut().find(|c| !c.is_temporarily_disabled) {
                    first.is_temporarily_disabled = true;
                }
            }
        }
    }
}
