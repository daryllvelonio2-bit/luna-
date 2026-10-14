use harness_providers::*;
use secrecy::ExposeSecret;

#[tokio::test]
async fn test_luna_auth_rotation_strategies() {
    let temp_dir = tempfile::tempdir().unwrap();
    let cred_path = temp_dir.path().join("credentials.json");

    let manager = CredentialManager::new(Some(cred_path));

    // Register 2 credentials
    manager.add_credential("openai", "key-primary", "sk-11111", true).await.unwrap();
    manager.add_credential("openai", "key-backup", "sk-22222", false).await.unwrap();

    // 1. Test FillFirst: should always return primary key
    manager.set_rotation_strategy("openai", RotationStrategy::FillFirst).await.unwrap();
    let k1 = manager.acquire_credential("openai").await.unwrap();
    let k2 = manager.acquire_credential("openai").await.unwrap();
    assert_eq!(k1.expose_secret(), "sk-11111");
    assert_eq!(k2.expose_secret(), "sk-11111");

    // 2. Test RoundRobin: should cycle between keys
    manager.set_rotation_strategy("openai", RotationStrategy::RoundRobin).await.unwrap();
    let r1 = manager.acquire_credential("openai").await.unwrap();
    let r2 = manager.acquire_credential("openai").await.unwrap();
    let r3 = manager.acquire_credential("openai").await.unwrap();
    assert_ne!(r1.expose_secret(), r2.expose_secret());
    assert_eq!(r1.expose_secret(), r3.expose_secret());
}
