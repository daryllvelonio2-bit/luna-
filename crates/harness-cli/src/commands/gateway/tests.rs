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
    let adapter = DiscordAdapter::new("fake_token".to_string(), "ch_1".to_string(), allowed, None);

    assert!(adapter.is_user_allowed("112233", Some("anon")));
    assert!(adapter.is_user_allowed("999999", Some("daryll")));
    assert!(adapter.is_user_allowed("999999", Some("@daryll")));
    assert!(!adapter.is_user_allowed("999999", Some("random_user")));
}

/// The cursor is what makes a restart resume. Without it the adapter re-seeded from the newest
/// message and answered nothing, so every message sent while the gateway was down was skipped.
#[test]
fn test_discord_cursor_survives_a_restart() {
    use super::discord::DiscordAdapter;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("gateway-discord-ch_1.cursor");

    let first = DiscordAdapter::new(
        "fake_token".to_string(),
        "ch_1".to_string(),
        Vec::new(),
        Some(path.clone()),
    );
    assert!(
        !first.is_seeded(),
        "a first run has no cursor to resume from"
    );
    first.remember("4242");

    let restarted = DiscordAdapter::new(
        "fake_token".to_string(),
        "ch_1".to_string(),
        Vec::new(),
        Some(path.clone()),
    );
    assert!(
        restarted.is_seeded(),
        "a restarted adapter resumes from the recorded id"
    );
    // The id is on disk, so the next run asks for what came after it.
    assert_eq!(
        std::fs::read_to_string(&path).unwrap().trim(),
        "4242",
        "the cursor file holds the newest id seen"
    );
    // Written through a temporary file, so the directory holds only the cursor itself.
    let names: Vec<String> = std::fs::read_dir(dir.path())
        .expect("read dir")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(names, vec!["gateway-discord-ch_1.cursor".to_string()]);
}

/// The splitter is shared, and its two failure modes were a panic on a non-boundary byte and an
/// unbalanced code fence; both are asserted in `outbound`'s own tests.
/// A poll that keeps failing is retried on a widening delay rather than every 500 ms, which is how
/// an adapter being rate limited stays rate limited.
#[test]
fn test_a_failing_poll_is_retried_on_a_widening_delay() {
    use super::poll_backoff;
    use std::time::Duration;
    assert_eq!(poll_backoff(Duration::ZERO), Duration::from_secs(1));
    assert_eq!(poll_backoff(Duration::from_secs(1)), Duration::from_secs(2));
    assert_eq!(
        poll_backoff(Duration::from_secs(30)),
        Duration::from_secs(60)
    );
    assert_eq!(
        poll_backoff(Duration::from_secs(60)),
        Duration::from_secs(60)
    );
}

#[test]
fn test_outbound_chunking_is_shared_by_both_adapters() {
    use super::outbound::chunk_message;
    let text = "a".repeat(25);
    assert_eq!(chunk_message(&text, 10).len(), 3);
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
        profile: None,
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
