use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use chrono::{DateTime, Utc};
use rand::Rng;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::warn;

use harness_core::HarnessError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RotationStrategy {
    #[default]
    RoundRobin,
    FillFirst,
    Random,
    LeastUsed,
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

/// File name of the credential store, under `$HOME/.luna`.
const CREDENTIALS_FILE: &str = "credentials.json";

pub struct CredentialManager {
    storage_path: PathBuf,
    state: RwLock<LunaAuthStorage>,
}

impl CredentialManager {
    /// Load the credential store, failing loudly if it exists but cannot be read.
    ///
    /// A corrupt store used to be `unwrap_or_default()`'d into "the user has no credentials",
    /// and the next write persisted that emptiness over every registered key (t_24375a7e).
    pub fn new(storage_path: Option<PathBuf>) -> Result<Arc<Self>, HarnessError> {
        let path = match storage_path {
            Some(path) => path,
            None => Self::default_storage_path(std::env::var("HOME").ok())?,
        };

        Ok(Arc::new(Self {
            storage_path: path.clone(),
            state: RwLock::new(Self::load_from_disk(&path)?),
        }))
    }

    /// `$HOME/.luna/credentials.json`. A missing `HOME` is an error rather than a `"."`
    /// fallback, which silently relocated every credential to the current directory.
    fn default_storage_path(home: Option<String>) -> Result<PathBuf, HarnessError> {
        let home = home.filter(|h| !h.is_empty()).ok_or_else(|| {
            HarnessError::ConfigurationError(format!(
                "HOME is not set: cannot locate the credential store ($HOME/.luna/{CREDENTIALS_FILE})"
            ))
        })?;
        Ok(PathBuf::from(home).join(".luna").join(CREDENTIALS_FILE))
    }

    fn load_from_disk(path: &Path) -> Result<LunaAuthStorage, HarnessError> {
        let mut storage = if !path.exists() {
            LunaAuthStorage::default()
        } else {
            let data = fs::read_to_string(path).map_err(|e| {
                HarnessError::Io(format!(
                    "cannot read credential store {}: {e}",
                    path.display()
                ))
            })?;
            serde_json::from_str(&data).map_err(|e| {
                HarnessError::SerializationError(format!(
                    "credential store {} is not valid JSON ({e}); refusing to start from an empty \
                     credential set, which the next write would persist over your keys — repair the \
                     file or move it aside",
                    path.display()
                ))
            })?
        };

        Self::load_fallback_credentials(&mut storage);
        Ok(storage)
    }

    fn load_fallback_credentials(storage: &mut LunaAuthStorage) {
        let mut env_map: HashMap<String, Vec<(String, String)>> = HashMap::new();

        let check_env_vars = [
            ("openrouter", "OPENROUTER_API_KEY"),
            ("mistral", "MISTRAL_API_KEY"),
            ("codestral", "CODESTRAL_API_KEY"),
            ("nvidia", "NVIDIA_API_KEY"),
            ("openai", "OPENAI_API_KEY"),
            ("anthropic", "ANTHROPIC_API_KEY"),
            ("gemini", "GEMINI_API_KEY"),
            ("gemini", "GOOGLE_API_KEY"),
            ("deepseek", "DEEPSEEK_API_KEY"),
            ("groq", "GROQ_API_KEY"),
            ("together", "TOGETHER_API_KEY"),
        ];

        for (p_id, env_var) in check_env_vars {
            if let Ok(val) = std::env::var(env_var) {
                let val = val.trim();
                if !val.is_empty() {
                    env_map
                        .entry(p_id.to_string())
                        .or_default()
                        .push((format!("env-{}", env_var.to_lowercase()), val.to_string()));
                }
            }
        }

        if let Ok(home) = std::env::var("HOME") {
            let shiina_env = PathBuf::from(home).join(".shiina/.env");
            if let Ok(content) = fs::read_to_string(&shiina_env) {
                for line in content.lines() {
                    let trimmed = line.trim();
                    if trimmed.is_empty() || trimmed.starts_with('#') || !trimmed.contains('=') {
                        continue;
                    }
                    if let Some((k, v)) = trimmed.split_once('=') {
                        let k = k.trim();
                        let v = v.trim();
                        if v.is_empty() {
                            continue;
                        }
                        for (p_id, env_var) in check_env_vars {
                            if k == env_var {
                                env_map
                                    .entry(p_id.to_string())
                                    .or_default()
                                    .push((format!("shiina-{}", k.to_lowercase()), v.to_string()));
                            }
                        }
                    }
                }
            }
        }

        for (p_id, keys) in env_map {
            let pool = storage.providers.entry(p_id).or_insert_with(|| ProviderAuthState {
                strategy: RotationStrategy::FillFirst,
                credentials: Vec::new(),
                current_round_robin_index: 0,
            });

            for (lbl, key) in keys {
                if !pool.credentials.iter().any(|c| c.api_key == key) {
                    pool.credentials.push(CredentialEntry {
                        id: uuid::Uuid::new_v4().to_string(),
                        label: lbl,
                        api_key: key,
                        is_priority: pool.credentials.is_empty(),
                        usage_count: 0,
                        last_used: None,
                        is_temporarily_disabled: false,
                    });
                }
            }
        }
    }

    /// Rewrite the store file from `bytes`. Shared tail of every persist path.
    fn write_store(&self, bytes: &str) -> Result<(), HarnessError> {
        if let Some(parent) = self.storage_path.parent() {
            fs::create_dir_all(parent).map_err(|e| HarnessError::Io(e.to_string()))?;
        }
        fs::write(&self.storage_path, bytes).map_err(|e| HarnessError::Io(e.to_string()))
    }

    fn merge_from_disk(to_write: &mut LunaAuthStorage, path: &Path) {
        if let Ok(disk) = Self::load_from_disk(path) {
            for (p_id, p_state) in disk.providers {
                if !to_write.providers.contains_key(&p_id) {
                    to_write.providers.insert(p_id, p_state);
                } else if let Some(local_p) = to_write.providers.get_mut(&p_id) {
                    for disk_cred in p_state.credentials {
                        if !local_p
                            .credentials
                            .iter()
                            .any(|c| c.id == disk_cred.id || c.api_key == disk_cred.api_key)
                        {
                            local_p.credentials.push(disk_cred);
                        }
                    }
                }
            }
        }
    }

    /// Persist the rotation state after an acquire: `usage_count`, `last_used` and the per-provider
    /// round-robin position must survive a restart (t_29ea5d7c) or `LeastUsed` restarts from the
    /// same key in every new process and `luna auth list` shows `Used: 0` forever.
    ///
    /// Deliberately NOT persisted (process-local by design, per t_d79b9293): the
    /// `is_temporarily_disabled` failover flag. A key retired because it was rate-limited must be
    /// retried on the next run, so `persist_state` serializes a copy of the state with those flags
    /// cleared instead of writing what is in memory. Persistence failures are logged, not returned:
    /// a full disk must not turn a working API key into a failed request.
    async fn persist_state(&self) {
        let mut to_write = self.state.read().await.clone();
        Self::merge_from_disk(&mut to_write, &self.storage_path);
        for p in to_write.providers.values_mut() {
            for cred in &mut p.credentials {
                cred.is_temporarily_disabled = false;
            }
        }
        let bytes = match serde_json::to_string_pretty(&to_write) {
            Ok(bytes) => bytes,
            Err(e) => {
                warn!(error = %e, "cannot serialize credential rotation state; skipping persist");
                return;
            }
        };
        if let Err(e) = self.write_store(&bytes) {
            warn!(
                path = %self.storage_path.display(),
                error = %e,
                "cannot persist credential rotation state; usage counts may reset on restart"
            );
        }
    }

    pub async fn save_to_disk(&self) -> Result<(), HarnessError> {
        let mut to_write = self.state.read().await.clone();
        if self.storage_path.exists() {
            let disk = Self::load_from_disk(&self.storage_path)?;
            for (p_id, p_state) in disk.providers {
                if !to_write.providers.contains_key(&p_id) {
                    to_write.providers.insert(p_id, p_state);
                } else if let Some(local_p) = to_write.providers.get_mut(&p_id) {
                    for disk_cred in p_state.credentials {
                        if !local_p
                            .credentials
                            .iter()
                            .any(|c| c.id == disk_cred.id || c.api_key == disk_cred.api_key)
                        {
                            local_p.credentials.push(disk_cred);
                        }
                    }
                }
            }
        }
        let bytes = serde_json::to_string_pretty(&to_write)
            .map_err(|e| HarnessError::SerializationError(e.to_string()))?;
        self.write_store(&bytes)
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

    /// Atomically updates a credential's secret (used for OAuth token rotation).
    pub async fn update_credential_key(
        &self,
        provider: &str,
        old_key: &str,
        new_key: &str,
    ) -> Result<bool, HarnessError> {
        let mut state = self.state.write().await;
        if let Some(pool) = state.providers.get_mut(&provider.to_lowercase()) {
            let mut found = false;
            for cred in &mut pool.credentials {
                if cred.api_key == old_key {
                    cred.api_key = new_key.to_string();
                    found = true;
                    break;
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
        let p_lower = provider.to_lowercase();
        {
            let state = self.state.read().await;
            if let Some(p) = state.providers.get(&p_lower) {
                if !p.credentials.is_empty() {
                    return p.credentials.clone();
                }
            }
        }
        if let Ok(disk) = Self::load_from_disk(&self.storage_path) {
            let mut state = self.state.write().await;
            for (p_id, p_state) in disk.providers {
                state.providers.entry(p_id).or_insert(p_state);
            }
            return state
                .providers
                .get(&p_lower)
                .map(|p| p.credentials.clone())
                .unwrap_or_default();
        }
        Vec::new()
    }

    pub async fn acquire_credential(&self, provider: &str) -> Result<SecretString, HarnessError> {
        let p_lower = provider.to_lowercase();
        let mut state = self.state.write().await;
        if !state.providers.contains_key(&p_lower) {
            if let Ok(disk) = Self::load_from_disk(&self.storage_path) {
                for (p_id, p_state) in disk.providers {
                    state.providers.entry(p_id).or_insert(p_state);
                }
            }
        }
        let provider_state = state
            .providers
            .get_mut(&p_lower)
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
            // Recovery hatch, kept deliberately: `is_temporarily_disabled` is part of the persisted
            // credential file, so a file whose keys were all retired (by an older build, or by
            // hand) would otherwise leave the provider permanently unusable with no way back from
            // `luna auth`. The failover path itself never gets here — `mark_credential_failed`
            // refuses to retire the last active key.
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
                // Per provider, and persisted: the position survives a restart (t_29ea5d7c). A
                // process-local counter restarted from 0 in every new process, so round_robin
                // always opened on the same key, and `current_round_robin_index` sat dead in the
                // file. The counter advances even when the provider later rejects the pick —
                // which is fine: it only decides *who goes next*, unlike LeastUsed, which must
                // count real traffic.
                let consumed = provider_state.current_round_robin_index;
                provider_state.current_round_robin_index = consumed.wrapping_add(1) % count.max(1);
                active_credentials[consumed % count]
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

        // The write lock guards the whole read-modify-write, so acquires cannot interleave.
        // Persist after dropping it: the snapshot on disk reflects this selection (or a newer one
        // from a concurrent acquire), and `luna auth list` reads exactly these fields (t_29ea5d7c).
        drop(state);
        self.persist_state().await;

        Ok(SecretString::new(raw_key))
    }

    /// Retire the credential that just failed so the next [`acquire_credential`] fails over.
    ///
    /// The caller only ever holds the secret it was handed, so the credential is matched by key
    /// value — not by position, and not by strategy: a revoked or rate-limited key must be
    /// retired whatever the rotation strategy is.
    ///
    /// Returns `true` when a credential was retired. The last active credential is never retired:
    /// retiring it would leave the provider with nothing to hand out, which is worse than a key
    /// that keeps failing. The flag is process-local (never written back to disk), so a key that
    /// was only rate-limited is retried on the next run.
    ///
    /// [`acquire_credential`]: CredentialManager::acquire_credential
    pub async fn mark_credential_failed(&self, provider: &str, api_key: &str) -> bool {
        let mut state = self.state.write().await;
        let Some(p) = state.providers.get_mut(&provider.to_lowercase()) else {
            return false;
        };

        let active_count = p
            .credentials
            .iter()
            .filter(|c| !c.is_temporarily_disabled)
            .count();
        if active_count < 2 {
            warn!(
                provider = %provider,
                "Sole active credential failed; keeping it selected instead of retiring it"
            );
            return false;
        }

        match p
            .credentials
            .iter_mut()
            .find(|c| !c.is_temporarily_disabled && c.api_key == api_key)
        {
            Some(cred) => {
                cred.is_temporarily_disabled = true;
                warn!(
                    provider = %provider,
                    label = %cred.label,
                    "Credential retired after a provider error; failing over to another key"
                );
                true
            }
            None => false,
        }
    }

    /// Get the configured rotation strategy for a provider.
    pub async fn get_rotation_strategy(&self, provider: &str) -> RotationStrategy {
        let state = self.state.read().await;
        state
            .providers
            .get(&provider.to_lowercase())
            .map(|p| p.strategy)
            .unwrap_or_default()
    }

    /// Reset cooldowns / temporary disabled flags for a provider, or for all providers if None.
    pub async fn reset_cooldowns(&self, provider: Option<&str>) -> Result<usize, HarnessError> {
        let mut state = self.state.write().await;
        let mut reset_count = 0;

        let targets: Vec<String> = if let Some(p) = provider {
            vec![p.to_lowercase()]
        } else {
            state.providers.keys().cloned().collect()
        };

        for p in targets {
            if let Some(p_state) = state.providers.get_mut(&p) {
                for c in &mut p_state.credentials {
                    if c.is_temporarily_disabled {
                        c.is_temporarily_disabled = false;
                        reset_count += 1;
                    }
                }
            }
        }

        drop(state);
        if reset_count > 0 {
            self.save_to_disk().await?;
        }
        Ok(reset_count)
    }

    /// Returns provider IDs that have registered credentials, sorted with active keys first.
    pub async fn get_loaded_providers(&self) -> Vec<String> {
        if let Ok(disk) = Self::load_from_disk(&self.storage_path) {
            let mut state = self.state.write().await;
            for (p_id, p_state) in disk.providers {
                if !state.providers.contains_key(&p_id) {
                    state.providers.insert(p_id, p_state);
                } else if let Some(local_p) = state.providers.get_mut(&p_id) {
                    for disk_cred in p_state.credentials {
                        if !local_p
                            .credentials
                            .iter()
                            .any(|c| c.id == disk_cred.id || c.api_key == disk_cred.api_key)
                        {
                            local_p.credentials.push(disk_cred);
                        }
                    }
                }
            }
        }
        let state = self.state.read().await;
        let mut list: Vec<(String, u64, bool)> = state
            .providers
            .iter()
            .filter(|(_, p)| !p.credentials.is_empty())
            .map(|(id, p)| {
                let total_used: u64 = p.credentials.iter().map(|c| c.usage_count).sum();
                let has_active = p.credentials.iter().any(|c| !c.is_temporarily_disabled);
                (id.clone(), total_used, has_active)
            })
            .collect();

        // Sort active first, then most used
        list.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| b.1.cmp(&a.1)));
        list.into_iter().map(|(id, _, _)| id).collect()
    }

    /// Returns the primary loaded provider from the credential pool, if any.
    pub async fn get_default_loaded_provider(&self) -> Option<String> {
        self.get_loaded_providers().await.into_iter().next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_home_is_an_error_not_a_cwd_relocation() {
        let err = CredentialManager::default_storage_path(None).unwrap_err();
        assert!(err.to_string().contains("HOME is not set"), "{err}");
        // An empty HOME is the same failure, not a path under the empty string.
        assert!(CredentialManager::default_storage_path(Some(String::new())).is_err());
    }

    #[test]
    fn the_default_store_is_absolute_under_home() {
        let path = CredentialManager::default_storage_path(Some("/home/tester".to_string()))
            .expect("HOME set");
        assert_eq!(path, PathBuf::from("/home/tester/.luna/credentials.json"));
    }
}
