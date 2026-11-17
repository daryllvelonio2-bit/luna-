use harness_providers::*;
use secrecy::ExposeSecret;

#[tokio::test]
async fn test_luna_auth_rotation_strategies() {
    let temp_dir = tempfile::tempdir().unwrap();
    let cred_path = temp_dir.path().join("credentials.json");

    let manager = CredentialManager::new(Some(cred_path)).unwrap();

    // Register 2 credentials
    manager
        .add_credential("openai", "key-primary", "sk-11111", true)
        .await
        .unwrap();
    manager
        .add_credential("openai", "key-backup", "sk-22222", false)
        .await
        .unwrap();

    // 1. Test FillFirst: should always return primary key
    manager
        .set_rotation_strategy("openai", RotationStrategy::FillFirst)
        .await
        .unwrap();
    let k1 = manager.acquire_credential("openai").await.unwrap();
    let k2 = manager.acquire_credential("openai").await.unwrap();
    assert_eq!(k1.expose_secret(), "sk-11111");
    assert_eq!(k2.expose_secret(), "sk-11111");

    // 2. Test RoundRobin: should cycle between keys
    manager
        .set_rotation_strategy("openai", RotationStrategy::RoundRobin)
        .await
        .unwrap();
    let r1 = manager.acquire_credential("openai").await.unwrap();
    let r2 = manager.acquire_credential("openai").await.unwrap();
    let r3 = manager.acquire_credential("openai").await.unwrap();
    assert_ne!(r1.expose_secret(), r2.expose_secret());
    assert_eq!(r1.expose_secret(), r3.expose_secret());

    // 3. Test Never: strictly uses the priority key and does not failover/rotate
    manager
        .set_rotation_strategy("openai", RotationStrategy::Never)
        .await
        .unwrap();
    let n1 = manager.acquire_credential("openai").await.unwrap();
    let n2 = manager.acquire_credential("openai").await.unwrap();
    assert_eq!(n1.expose_secret(), "sk-11111");
    assert_eq!(n2.expose_secret(), "sk-11111");

    // Even if marked failed, Never refuses to rotate to backup
    let retired = manager.mark_credential_failed("openai", "sk-11111").await;
    assert!(!retired, "Never strategy must not retire the priority key");
    let n3 = manager.acquire_credential("openai").await.unwrap();
    assert_eq!(n3.expose_secret(), "sk-11111");

    // Switching priority updates the key used by Never
    manager
        .set_priority_credential("openai", "key-backup")
        .await
        .unwrap();
    let n4 = manager.acquire_credential("openai").await.unwrap();
    assert_eq!(n4.expose_secret(), "sk-22222");
}

#[tokio::test]
async fn test_session_scoped_priority_credentials() {
    let temp_dir = tempfile::tempdir().unwrap();
    let cred_path = temp_dir.path().join("credentials.json");

    let manager = CredentialManager::new(Some(cred_path)).unwrap();

    manager
        .add_credential("antigravity", "account-a", "token-aaa", true)
        .await
        .unwrap();
    manager
        .add_credential("antigravity", "account-b", "token-bbb", false)
        .await
        .unwrap();

    manager
        .set_rotation_strategy("antigravity", RotationStrategy::FillFirst)
        .await
        .unwrap();

    // Default global priority is account-a
    let global_key = manager.acquire_credential("antigravity").await.unwrap();
    assert_eq!(global_key.expose_secret(), "token-aaa");

    // Session 1 overrides to account-b
    manager.set_session_priority_credential("session-1", "antigravity", "account-b");

    let s1_key = manager
        .acquire_credential_for_session("antigravity", "session-1")
        .await
        .unwrap();
    assert_eq!(
        s1_key.expose_secret(),
        "token-bbb",
        "session-1 must acquire its session-scoped priority credential"
    );

    // Session 2 has no override, so it uses global priority account-a
    let s2_key = manager
        .acquire_credential_for_session("antigravity", "session-2")
        .await
        .unwrap();
    assert_eq!(
        s2_key.expose_secret(),
        "token-aaa",
        "session-2 must fall back to global priority"
    );

    // Clear session-1 override, now it uses global priority
    let cleared = manager.clear_session_priority_credential("session-1", "antigravity");
    assert!(cleared);

    let s1_after_clear = manager
        .acquire_credential_for_session("antigravity", "session-1")
        .await
        .unwrap();
    assert_eq!(
        s1_after_clear.expose_secret(),
        "token-aaa",
        "session-1 must use global priority after clearing override"
    );
}
