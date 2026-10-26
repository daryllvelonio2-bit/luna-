use super::adapter::InboundMessage;
use super::discord::DiscordAdapter;
use super::telegram::TelegramAdapter;
use super::whatsapp::extract_query_param;
use harness_core::config::{self, Config, Layers};
use std::collections::BTreeMap;

#[test]
fn test_inbound_message_structure() {
    let msg = InboundMessage {
        platform: "telegram".to_string(),
        chat_id: "123456".to_string(),
        sender_id: "789".to_string(),
        sender_name: Some("@testuser".to_string()),
        text: "hello luna".to_string(),
        message_id: Some("1".to_string()),
    };

    assert_eq!(msg.platform, "telegram");
    assert_eq!(msg.chat_id, "123456");
    assert_eq!(msg.sender_id, "789");
    assert_eq!(msg.sender_name.as_deref(), Some("@testuser"));
    assert_eq!(msg.text, "hello luna");
}

#[test]
fn test_telegram_user_authorization() {
    let allowed = vec!["123456".to_string(), "janelle".to_string()];
    let adapter = TelegramAdapter::new("fake_token".to_string(), allowed);

    // Empty list allows all, but here allowed has members
    assert!(adapter.is_user_allowed(123456, Some("other_user")));
    assert!(adapter.is_user_allowed(999999, Some("janelle")));
    assert!(adapter.is_user_allowed(999999, Some("@janelle")));
    assert!(!adapter.is_user_allowed(999999, Some("stranger")));
}

#[test]
fn test_discord_user_authorization() {
    let allowed = vec!["112233".to_string(), "daryll".to_string()];
    let adapter = DiscordAdapter::new("fake_token".to_string(), "ch_1".to_string(), allowed);

    assert!(adapter.is_user_allowed("112233", Some("anon")));
    assert!(adapter.is_user_allowed("999999", Some("daryll")));
    assert!(adapter.is_user_allowed("999999", Some("@daryll")));
    assert!(!adapter.is_user_allowed("999999", Some("random_user")));
}

#[test]
fn test_whatsapp_extract_query_param() {
    let path =
        "/webhook?hub.mode=subscribe&hub.challenge=1158201444&hub.verify_token=my_secret_token";
    assert_eq!(
        extract_query_param(path, "hub.mode").as_deref(),
        Some("subscribe")
    );
    assert_eq!(
        extract_query_param(path, "hub.challenge").as_deref(),
        Some("1158201444")
    );
    assert_eq!(
        extract_query_param(path, "hub.verify_token").as_deref(),
        Some("my_secret_token")
    );
    assert_eq!(extract_query_param(path, "nonexistent"), None);
}

#[test]
fn test_gateway_config_persistence() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("config.yaml");

    let layers = Layers {
        defaults: Config::default(),
        file: Some(file.clone()),
        env: BTreeMap::new(),
        flags: BTreeMap::new(),
    };

    config::set_in_file(&layers, "gateway.enabled", "true").expect("set enabled");
    config::set_in_file(&layers, "gateway.platform", "telegram").expect("set platform");
    config::set_in_file(
        &layers,
        "gateway.telegram_bot_token",
        "123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11",
    )
    .expect("set token");

    let resolved = config::resolve(&layers)
        .expect("resolve config")
        .into_config();
    assert!(resolved.gateway.enabled);
    assert_eq!(resolved.gateway.platform, "telegram");
    assert_eq!(
        resolved.gateway.telegram_bot_token,
        "123456:ABC-DEF1234ghIkl-zyx57W2v1u123ew11"
    );
}
