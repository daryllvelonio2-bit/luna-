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
    /// API key stored encrypted in the file. In production builds, encryption should use OS keyring.
    pub api_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_in_keyring: Option<bool>,
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
        if !path.exists() {
            return Ok(LunaAuthStorage::default());
        }
        let data = fs::read_to_string(path).map_err(|e| {
            HarnessError::Io(format!(
                "cannot read credential store {}: {e}",
                path.display()
            ))
        })?;
        let mut state: LunaAuthStorage = serde_json::from_str(&data).map_err(|e| {
            HarnessError::SerializationError(format!(
                "credential store {} is not valid JSON ({e}); refusing to start from an empty \
                 credential set, which the next write would persist over your keys — repair the \
                 file or move it aside",
                path.display()
            ))
        })?;
        // Every row the store hands out comes through here, so this is the single place the
        // one-account-one-row invariant holds. A store written by an older build already has a
        // row per token refresh, and the load path is otherwise a plain deserialize: without
        // this, the in-memory state starts stacked and the fast path in `get_credentials`
        // returns the stack unexamined.
        for p in state.providers.values_mut() {
            Self::dedupe_by_label(p);
        }
        Ok(state)
    }

    /// Rewrite the store file from `bytes`. Shared tail of every persist path.
    fn write_store(&self, bytes: &str) -> Result<(), HarnessError> {
        if let Some(parent) = self.storage_path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                HarnessError::Io(format!(
                    "cannot create credential store directory {}: {e}",
                    parent.display()
                ))
            })?;
        }
        if let Ok(meta) = self.storage_path.metadata() {
            if meta.is_dir() {
                return Err(HarnessError::Io(format!(
                    "credential store path {} is a directory; a file is expected there",
                    self.storage_path.display()
                )));
            }
        }
        fs::write(&self.storage_path, bytes).map_err(|e| {
            HarnessError::Io(format!(
                "cannot write credential store {}: {e}",
                self.storage_path.display()
            ))
        })
    }

    /// Fold the on-disk store into an in-memory one: a provider missing locally is taken whole,
    /// a provider present locally absorbs the disk's credentials that are not there yet.
    ///
    /// Every merge is deduplicated afterwards. The per-credential `id`/`api_key` check cannot
    /// catch the stack, because the rows being stacked share nothing on id or key — only the
    /// label — so without the dedupe pass every read pulls the stack back in again.
    /// `luna auth list` reads what lands on disk, so this is where the list gets its shape.
    fn merge_from_disk(to_write: &mut LunaAuthStorage, path: &Path) -> Result<(), HarnessError> {
        // A store that cannot be read means "cannot merge", not "there is nothing to merge": the
        // error goes back to the caller, which decides whether that is fatal (`save_to_disk`) or
        // merely a lost merge (`persist_state`, which must not fail a working request).
        let disk = Self::load_from_disk(path)?;
        for (p_id, disk_state) in disk.providers {
            if !to_write.providers.contains_key(&p_id) {
                to_write.providers.insert(p_id, disk_state);
                continue;
            }
            let local = to_write.providers.get_mut(&p_id).expect("checked above");
            for disk_cred in disk_state.credentials {
                let already = local.credentials.iter().any(|c| {
                    c.id == disk_cred.id || c.api_key == disk_cred.api_key
                });
                if !already {
                    local.credentials.push(disk_cred);
                }
            }
        }
        // One pass over everything that ended up in the store, local rows included: the
        // in-memory stack predates the label-keyed `add_credential`, so it needs the same
        // collapse the merged rows do, or `luna auth list` still shows the duplicates.
        for p in to_write.providers.values_mut() {
            Self::dedupe_by_label(p);
        }
        Ok(())
    }

    /// Collapse rows that describe the same account, keeping the one that carries the most
    /// history.
    ///
    /// `add_credential` now keys an account by its label, but that reached the code after stores
    /// had already stacked a row per token refresh: the label stayed the same while the key
    /// rotated, so a single Gmail account had as many rows as it had refreshes. Dedupe by label
    /// on the way to disk, otherwise every write re-persists the rows forever.
    ///
    /// The winner is the most recently used row, which for an OAuth account holds the live
    /// token; chronological ties break on `usage_count`, and the surviving row keeps its own `id`
    /// so the rotation code's `current_round_robin_index` still points at something real.
    /// Returns how many rows were dropped.
    fn dedupe_by_label(p: &mut ProviderAuthState) -> usize {
        let before = p.credentials.len();
        let mut deduped: Vec<CredentialEntry> = Vec::with_capacity(before);
        for cred in std::mem::take(&mut p.credentials) {
            if let Some(kept) = deduped.iter_mut().find(|k| k.label == cred.label) {
                // Most recently used wins first. For an OAuth account that is the only correct
                // rule: the stacked rows are the same account after each token refresh, so the
                // newest row holds the live token and the older ones are already expired.
                // `usage_count` alone would keep the oldest row, which is exactly backwards.
                let newer = cred.last_used.is_some()
                    && (kept.last_used.is_none() || cred.last_used > kept.last_used);
                let more_used =
                    cred.last_used.is_none() == kept.last_used.is_none() && cred.usage_count > kept.usage_count;
                if newer || more_used {
                    *kept = cred;
                }
            } else {
                deduped.push(cred);
            }
        }
        p.credentials = deduped;
        before - p.credentials.len()
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
        // A store that cannot be read is logged, not returned: this runs after a successful
        // acquire, and a full disk or a transient read failure must not fail the request itself.
        if let Err(e) = Self::merge_from_disk(&mut to_write, &self.storage_path) {
            warn!(error = %e, "cannot merge on-disk credential state; persisting in-memory view");
        }
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
        // Propagating matters: `load_from_disk` refuses a corrupt store on purpose, and silently
        // treating a corrupt file as "nothing there" is how an empty state gets persisted over
        // every registered key. The merge helper takes a Result for exactly this reason.
        Self::merge_from_disk(&mut to_write, &self.storage_path)?;
        let bytes = serde_json::to_string_pretty(&to_write)
            .map_err(|e| HarnessError::SerializationError(e.to_string()))?;
        self.write_store(&bytes)
    }

    /// Persist the in-memory state directly without merging from disk.
    /// Used when mutating operations (such as credential removal) must not resurrect
    /// keys that were explicitly deleted.
    pub async fn save_exact(&self) -> Result<(), HarnessError> {
        let mut to_write = self.state.read().await.clone();
        for p in to_write.providers.values_mut() {
            Self::dedupe_by_label(p);
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

        // One account is one entry, keyed by the label the account is known by. Re-registering
        // an account — `luna auth antigravity copy` after a token refresh, or re-adding the same
        // provider/label — used to append a second row with a fresh UUID, so a single Gmail
        // account accumulated a row per refresh. The token itself rotates, so neither `id` nor
        // `api_key` could identify the account; the label is the only stable handle on it.
        let existing = provider_state
            .credentials
            .iter_mut()
            .find(|c| c.label == label);

        let id = match existing {
            Some(cred) => {
                // Refresh in place: keep the identity, the usage history and the priority flag,
                // so a token rotation does not reset `luna auth list` counters or drop the
                // account back out of the rotation order.
                cred.api_key = key.to_string();
                if is_priority {
                    cred.is_priority = true;
                }
                cred.is_temporarily_disabled = false;
                cred.id.clone()
            }
            None => {
                let id = uuid::Uuid::new_v4().to_string();
                let entry = CredentialEntry {
                    id: id.clone(),
                    label: label.to_string(),
                    api_key: key.to_string(),
                    encrypted_in_keyring: None,
                    is_priority,
                    usage_count: 0,
                    last_used: None,
                    is_temporarily_disabled: false,
                };
                provider_state.credentials.push(entry);
                id
            }
        };

        if is_priority {
            for cred in &mut provider_state.credentials {
                cred.is_priority = false;
            }
            if let Some(cred) = provider_state
                .credentials
                .iter_mut()
                .find(|c| c.id == id)
            {
                cred.is_priority = true;
            }
        }

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
                self.save_exact().await?;
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

    /// Fold the on-disk store into the in-memory one, in place. Every load path funnels through
    /// this so the two never disagree about which rows exist.
    async fn absorb_disk_state(&self) -> Result<(), HarnessError> {
        let mut merged = self.state.read().await.clone();
        Self::merge_from_disk(&mut merged, &self.storage_path)?;
        let mut state = self.state.write().await;
        for (p_id, p_state) in merged.providers {
            state.providers.insert(p_id, p_state);
        }
        Ok(())
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
        if let Err(e) = self.absorb_disk_state().await {
            tracing::warn!("Failed to absorb credentials from disk: {}", e);
        }
        let state = self.state.read().await;
        state
            .providers
            .get(&p_lower)
            .map(|p| p.credentials.clone())
            .unwrap_or_default()
    }

    pub async fn acquire_credential(&self, provider: &str) -> Result<SecretString, HarnessError> {
        let p_lower = provider.to_lowercase();
        let mut state = self.state.write().await;
        if !state.providers.contains_key(&p_lower) {
            // Merge under no lock, then take the lock again: holding the write lock across a
            // blocking read makes every other acquirer wait on two filesystem reads.
            let mut merged = state.clone();
            drop(state);
            // Nothing on disk to offer: the provider must already be in memory, or the request
            // is about to fail with its own "no credentials" error, which names the provider
            // more usefully than a read failure would.
            let _ = Self::merge_from_disk(&mut merged, &self.storage_path);
            state = self.state.write().await;
            for (p_id, p_state) in merged.providers {
                state.providers.entry(p_id).or_insert(p_state);
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
        if let Err(e) = self.absorb_disk_state().await {
            tracing::warn!("Failed to absorb credentials from disk: {}", e);
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

    fn cred(label: &str, key: &str, used: u64) -> CredentialEntry {
        CredentialEntry {
            id: uuid::Uuid::new_v4().to_string(),
            label: label.to_string(),
            api_key: key.to_string(),
            encrypted_in_keyring: None,
            is_priority: false,
            usage_count: used,
            last_used: None,
            is_temporarily_disabled: false,
        }
    }

    /// The reported bug: `luna auth antigravity copy` re-registers the same Gmail account after
    /// each token refresh. The label is identical, the key is not, so every stacked row survived
    /// both the id and the api_key dedupe in the merge path and landed in `luna auth list`.
    #[tokio::test]
    async fn re_registering_one_account_does_not_stack_a_row_per_refresh() {
        let mgr = CredentialManager::new(Some(temp_store_path())).expect("manager");
        for key in ["token-a", "token-b", "token-c"] {
            mgr.add_credential("antigravity", "daryllvelonio9@gmail.com", key, false)
                .await
                .expect("add");
        }
        let rows = mgr.get_credentials("antigravity").await;
        assert_eq!(
            rows.len(),
            1,
            "one Gmail account must be one list entry, got {} rows: {:?}",
            rows.len(),
            rows.iter().map(|c| c.label.as_str()).collect::<Vec<_>>()
        );
        // The refresh must take effect on the surviving row, not be dropped alongside it.
        assert_eq!(rows[0].api_key, "token-c");
    }

    /// A store that accumulated the duplicates before the fix must heal on read, not only on
    /// write: `luna auth list` reads what is merged out of the file.
    #[tokio::test]
    async fn a_store_that_already_stacked_the_rows_collapses_on_read() {
        let path = temp_store_path();
        let mut stacked = LunaAuthStorage::default();
        stacked.providers.insert(
            "antigravity".to_string(),
            ProviderAuthState {
                strategy: RotationStrategy::default(),
                credentials: vec![
                    CredentialEntry {
                        last_used: Some(
                            "2026-10-18T16:07:35Z".parse().expect("timestamp"),
                        ),
                        ..cred("daryllvelonio9@gmail.com", "token-old", 88)
                    },
                    CredentialEntry {
                        last_used: Some(
                            "2026-10-18T17:45:41Z".parse().expect("timestamp"),
                        ),
                        ..cred("daryllvelonio9@gmail.com", "token-fresh", 87)
                    },
                ],
                current_round_robin_index: 0,
            },
        );
        std::fs::write(&path, serde_json::to_string_pretty(&stacked).expect("serialize")).unwrap();

        let mgr = CredentialManager::new(Some(path)).expect("manager");
        let rows = mgr.get_credentials("antigravity").await;
        assert_eq!(
            rows.len(),
            1,
            "an already-stacked store must collapse to one entry, got {}",
            rows.len()
        );
        // The most recently used row survives: it carries the live OAuth token. Picking by
        // `usage_count` would keep the older token, which the provider has already rotated away.
        assert_eq!(rows[0].api_key, "token-fresh");
        assert_eq!(rows[0].usage_count, 87);
    }

    /// Two genuinely different accounts must both survive — the collapse is per label, not a
    /// "keep only the first row of the provider".
    #[tokio::test]
    async fn two_distinct_accounts_both_survive_the_collapse() {
        let mgr = CredentialManager::new(Some(temp_store_path())).expect("manager");
        mgr.add_credential("antigravity", "one@gmail.com", "key-1", false)
            .await
            .expect("add");
        mgr.add_credential("antigravity", "two@gmail.com", "key-2", false)
            .await
            .expect("add");
        assert_eq!(mgr.get_credentials("antigravity").await.len(), 2);
    }

    #[tokio::test]
    async fn remove_credential_persists_deletion_without_resurrecting_from_disk() {
        let path = temp_store_path();
        let mgr = CredentialManager::new(Some(path.clone())).expect("manager");
        mgr.add_credential("antigravity", "keep@gmail.com", "key-keep", false)
            .await
            .expect("add 1");
        mgr.add_credential("antigravity", "delete-me", "key-del", false)
            .await
            .expect("add 2");

        assert_eq!(mgr.get_credentials("antigravity").await.len(), 2);

        let removed = mgr
            .remove_credential("antigravity", "delete-me")
            .await
            .expect("remove");
        assert!(removed, "credential should be reported removed");

        // Create a completely new manager reading the disk file to ensure it wasn't resurrected
        let fresh_mgr = CredentialManager::new(Some(path)).expect("fresh manager");
        let remaining = fresh_mgr.get_credentials("antigravity").await;
        assert_eq!(remaining.len(), 1, "only 1 credential should survive on disk");
        assert_eq!(remaining[0].label, "keep@gmail.com");
    }

    fn temp_store_path() -> std::path::PathBuf {
        let path = std::env::temp_dir()
            .join(format!("luna-test-{}.json", uuid::Uuid::new_v4()));
        let _ = std::fs::remove_file(&path);
        path
    }
}
