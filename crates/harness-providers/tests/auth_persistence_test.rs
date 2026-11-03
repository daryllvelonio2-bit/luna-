//! Rotation state must survive a process restart (t_29ea5d7c).
//!
//! `acquire_credential` used to bump `usage_count` and the round-robin position in memory only, so
//! the on-disk store never moved: `LeastUsed` restarted from the first key in every new process,
//! `current_round_robin_index` was dead weight, and `luna auth list` printed `Used: 0` forever.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use harness_providers::{CredentialManager, RotationStrategy};
use secrecy::ExposeSecret;

fn manager(path: &Path) -> Arc<CredentialManager> {
    CredentialManager::new(Some(path.to_path_buf())).expect("a fresh temp store path always loads")
}

/// Three acquires under `LeastUsed` in one process, then a fresh manager on the same file: the
/// persisted counts are [2, 1], so the next pick must be the second key. A process-local counter
/// restarts from all-zeros and picks the first key every time — the bug this pins.
#[tokio::test]
async fn least_used_spreads_load_across_processes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.json");

    {
        let mgr = manager(&path);
        mgr.add_credential("openai", "first", "sk-11111", false)
            .await
            .unwrap();
        mgr.add_credential("openai", "second", "sk-22222", false)
            .await
            .unwrap();
        mgr.set_rotation_strategy("openai", RotationStrategy::LeastUsed)
            .await
            .unwrap();

        for expected in ["sk-11111", "sk-22222", "sk-11111"] {
            let key = mgr.acquire_credential("openai").await.unwrap();
            assert_eq!(key.expose_secret(), expected);
        }
    } // the first "process" ends here

    let mgr = manager(&path);
    let next = mgr.acquire_credential("openai").await.unwrap();
    assert_eq!(
        next.expose_secret(),
        "sk-22222",
        "LeastUsed must continue from the persisted counts and hand out the least-used key"
    );
}

#[tokio::test]
async fn usage_counts_survive_a_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.json");

    {
        let mgr = manager(&path);
        mgr.add_credential("openai", "first", "sk-11111", false)
            .await
            .unwrap();
        mgr.add_credential("openai", "second", "sk-22222", false)
            .await
            .unwrap();
        // Default strategy is round_robin: three picks hit first, second, first.
        for _ in 0..3 {
            mgr.acquire_credential("openai").await.unwrap();
        }
    }

    let mgr = manager(&path);
    let counts: Vec<u64> = mgr
        .get_credentials("openai")
        .await
        .iter()
        .map(|c| c.usage_count)
        .collect();
    assert_eq!(
        counts,
        vec![2, 1],
        "`luna auth list` reads these from disk; they must survive a reload"
    );
}

#[tokio::test]
async fn round_robin_position_survives_a_reload() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.json");

    {
        let mgr = manager(&path);
        mgr.add_credential("openai", "first", "sk-11111", false)
            .await
            .unwrap();
        mgr.add_credential("openai", "second", "sk-22222", false)
            .await
            .unwrap();
        let key = mgr.acquire_credential("openai").await.unwrap();
        assert_eq!(key.expose_secret(), "sk-11111");
    }

    // The first process consumed pick #0; the next process must continue at pick #1, not restart.
    let mgr = manager(&path);
    let next = mgr.acquire_credential("openai").await.unwrap();
    assert_eq!(next.expose_secret(), "sk-22222");
}

/// Failover retirement is process-local by design (`mark_credential_failed`): a rate-limited key
/// must be retried on the next run, so the disabled flag must never ride along when an acquire
/// persists the store.
#[tokio::test]
async fn failover_flags_stay_out_of_the_persisted_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.json");
    let mgr = manager(&path);
    mgr.add_credential("openai", "first", "sk-11111", false)
        .await
        .unwrap();
    mgr.add_credential("openai", "second", "sk-22222", false)
        .await
        .unwrap();
    mgr.set_rotation_strategy("openai", RotationStrategy::FillFirst)
        .await
        .unwrap();

    assert!(mgr.mark_credential_failed("openai", "sk-11111").await);
    // This acquire persists the store — with the disabled flag set in memory.
    let key = mgr.acquire_credential("openai").await.unwrap();
    assert_eq!(key.expose_secret(), "sk-22222");

    let raw = fs::read_to_string(&path).unwrap();
    assert!(
        !raw.contains("\"is_temporarily_disabled\": true"),
        "process-local failover state reached the disk: {raw}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn credentials_file_has_0600_permissions_on_unix() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("credentials.json");
    let mgr = manager(&path);
    mgr.add_credential("openai", "key", "sk-secret-1234", false)
        .await
        .unwrap();

    let meta = fs::metadata(&path).unwrap();
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o600,
        "credentials.json must have 0600 permissions, found {:o}",
        mode
    );
}
