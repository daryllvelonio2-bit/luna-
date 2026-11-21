//! Voice interface and speech tests.

use super::capture::*;
use super::catalog::*;
use super::playback::*;
use super::stt::*;
use super::summary::*;
use super::tts::*;
use super::*;
use std::sync::atomic::Ordering;

#[test]
fn test_build_gemini_tts_payload_includes_read_aloud_transcript_instruction() {
    let payload = build_gemini_tts_payload("Hello, how are you?", "");
    let sent = payload["contents"][0]["parts"][0]["text"].as_str().unwrap();
    assert!(sent.contains("Hello, how are you?"));
    assert!(sent.contains("read aloud"));
    assert_eq!(
        payload["generationConfig"]["speechConfig"]["voiceConfig"]["prebuiltVoiceConfig"]
            ["voiceName"]
            .as_str(),
        Some("Aoede")
    );
}

#[test]
fn test_clean_text_for_speech_strips_code_blocks_and_markdown() {
    let input = "Here is the result:\n```rust\nfn main() {}\n```\n| Col1 | Col2 |\n|---|---|\n- Fast and responsive.\n**Done!** Check [docs](https://example.com).";
    let out = clean_text_for_speech(input);
    assert!(!out.contains("fn main"));
    assert!(!out.contains("Col1"));
    assert!(!out.contains("https://"));
    assert!(out.contains("Here is the result:"));
    assert!(out.contains("Fast and responsive."));
    assert!(out.contains("Done!"));
    assert!(out.contains("Check docs."));
}

#[test]
fn test_extract_ready_speech_chunks_incremental_streaming() {
    let mut in_code = false;
    let (chunks1, c1) = extract_ready_speech_chunks("Hello! How are", &mut in_code, true);
    assert_eq!(chunks1, vec!["Hello!"]);
    assert_eq!(c1, 7);
    assert!(!in_code);

    let full2 = "Hello! How are you today?\n```rust\nlet x = 1;\n```\nAll done";
    let (chunks2, c2) = extract_ready_speech_chunks(&full2[c1..], &mut in_code, false);
    assert_eq!(chunks2, vec!["How are you today?"]);
    assert!(!in_code);
    assert_eq!(&full2[c1 + c2..], "All done");
}

#[test]
fn test_downsample_to_16khz_reduces_sample_count() {
    let src: Vec<i16> = (0..48_000).map(|i| (i % 1000) as i16).collect();
    let down = downsample_to_16khz(&src, 48_000);
    assert_eq!(down.len(), 16_000);
}

#[test]
fn test_parse_pcm_rate_from_mime() {
    assert_eq!(
        parse_pcm_rate_from_mime("audio/L16;codec=pcm;rate=24000"),
        Some(24000)
    );
    assert_eq!(parse_pcm_rate_from_mime("audio/wav"), None);
}

#[test]
fn test_is_blank_transcript() {
    assert!(is_blank_transcript(""));
    assert!(is_blank_transcript("   "));
    assert!(is_blank_transcript("[BLANK_AUDIO]"));
    assert!(is_blank_transcript("(silence)"));
    assert!(is_blank_transcript("0"));
    assert!(is_blank_transcript("00"));
    assert!(is_blank_transcript("tone"));
    assert!(!is_blank_transcript("Hello Luna"));
}

#[test]
fn test_parse_antigravity_stt_sse_filters_thoughts_and_signatures() {
    let sse_body = r#"data: {"response": {"candidates": [{"content": {"role": "model","parts": [{"thought": true,"text": "I am thinking about the audio"}]}}]}}
data: {"response": {"candidates": [{"content": {"role": "model","parts": [{"text": "Hello world"}]}}]}}
data: {"response": {"candidates": [{"content": {"role": "model","parts": [{"thoughtSignature": "abc123sig"}]}}]}}
"#;
    let transcript = super::stt::parse_antigravity_stt_sse(sse_body);
    assert_eq!(transcript, "Hello world");
}

#[tokio::test]
async fn test_lookup_plain_api_key_skips_oauth_json() {
    let tmp = tempfile::tempdir().unwrap();
    let mgr = CredentialManager::new(Some(tmp.path().join("credentials.json"))).unwrap();
    mgr.add_credential(
        "test-voice-provider",
        "oauth-blob",
        r#"{"access_token":"ya29.abc","refresh_token":"1//xyz"}"#,
        true,
    )
    .await
    .unwrap();
    mgr.add_credential("test-voice-provider", "api-key", "AIzaTestKey123", false)
        .await
        .unwrap();
    let resolved = lookup_plain_api_key("test-voice-provider", &mgr).await;
    assert_eq!(resolved.as_deref(), Some("AIzaTestKey123"));
}

#[test]
fn test_voice_speech_queue_interrupt_and_turn_mute() {
    let queue = VoiceSpeechQueue::new();
    assert!(!queue.is_turn_muted());
    queue.set_turn_muted(true);
    assert!(queue.is_turn_muted());
    queue.interrupt();
    assert!(!queue.is_playing().load(Ordering::SeqCst));
    queue.set_turn_muted(false);
    assert!(!queue.is_turn_muted());
}

#[test]
fn test_available_voices_includes_gemini_openai_and_edge() {
    let voices = available_voices();
    assert!(voices
        .iter()
        .any(|v| v.id == "Aoede" && v.provider == "antigravity"));
    assert!(voices
        .iter()
        .any(|v| v.id == "Sulafat" && v.provider == "antigravity"));
    assert!(voices
        .iter()
        .any(|v| v.id == "alloy" && v.provider == "openai"));
    assert!(voices
        .iter()
        .any(|v| v.id == "marin" && v.provider == "openai"));
    assert!(voices
        .iter()
        .any(|v| v.id == "en-US-EmmaMultilingualNeural" && v.provider == "groq"));
    assert!(voices
        .iter()
        .all(|v| !v.id.is_empty() && !v.description.is_empty()));
    assert!(Config::default().voice.auto_send);
}

#[test]
fn test_needs_separate_voice_summary_detects_tables_code_and_long_replies() {
    assert!(needs_separate_voice_summary(
        "| File | Status |\n|---|---|\n| a.txt | done |"
    ));
    assert!(needs_separate_voice_summary(
        "Here is the code:\n```rust\nfn test() {}\n```"
    ));
    assert!(needs_separate_voice_summary(
        "Check this link: https://example.com/status"
    ));
    assert!(needs_separate_voice_summary(
        "- First step\n- Second step\n- Third step"
    ));
    assert!(!needs_separate_voice_summary(
        "All build artifacts have been cleaned up successfully."
    ));

    let fallback = fallback_voice_summary(
        "Cleaned up everything.\n| Path | Freed |\n|---|---|\n| target | 2 GB |",
    );
    assert!(fallback.contains("Cleaned up everything."));
    assert!(fallback.contains("full details in the chat"));
}

#[tokio::test]
#[ignore = "live audio hardware and API diagnostic"]
async fn live_dual_voice_diagnostic() {
    let layers = harness_core::config::process_layers(Config::default()).unwrap();
    let config = layers.defaults;
    let cred_manager = Arc::new(CredentialManager::new(None).unwrap());

    let start = std::time::Instant::now();
    let ack = generate_pre_task_voice_ack(
        "clean up build artifacts",
        Some("terminal rm -rf target"),
        &config,
        &cred_manager,
    )
    .await;
    eprintln!("ACK ({:?}): {:?}", start.elapsed(), ack);
    assert!(ack.is_some());

    let start2 = std::time::Instant::now();
    let summary = generate_separate_voice_summary(
        "list the crates in the workspace in a markdown table",
        "Here are the workspace crates:\n| Crate | Purpose |\n|---|---|\n| harness-core | core agent logic |\n| harness-cli | CLI interface |\n| harness-web | web server |",
        1,
        &config,
        &cred_manager,
    ).await;
    eprintln!("SUMMARY ({:?}): {:?}", start2.elapsed(), summary);
    assert!(!summary.is_empty());
    assert!(!summary.contains('|'));
}

#[tokio::test]
#[ignore = "live audio hardware and API diagnostic"]
async fn live_voice_pipeline_diagnostic() {
    let text = "Hello! Luna voice pipeline is working.";
    let payload = build_gemini_tts_payload(text, "Aoede");
    let json_str = serde_json::to_string(&payload).unwrap();
    assert!(json_str.contains("Aoede"));
}

#[test]
fn test_micwire_frame_and_hello_roundtrip() {
    use super::micserve::*;
    let hello = speaker_hello(16000);
    assert_eq!(hello, "SPEAKER rate=16000\n");
    assert_eq!(parse_speaker_hello(&hello), Some(16000));
    assert_eq!(parse_speaker_hello("SPEAKER rate=24000\n"), Some(24000));
    assert_eq!(parse_speaker_hello("INVALID\n"), None);

    let payload = b"voice-audio-pcm-data";
    let mut encoded = Vec::new();
    write_frame(&mut encoded, payload).unwrap();
    assert_eq!(encoded.len(), 4 + payload.len());

    let mut cursor = std::io::Cursor::new(encoded);
    let decoded = read_frame(&mut cursor).unwrap();
    assert_eq!(decoded, payload);
}

#[tokio::test]
async fn test_strict_voice_routing_zero_fallbacks() {
    let mut config = Config::default();
    config.voice.provider = "nonexistent_provider".to_string();
    let cred_manager = Arc::new(CredentialManager::new(None).unwrap());

    // Fails directly with provider-specific error instead of falling back
    let err = transcribe_audio_bytes(&[0u8; 100], &config, &cred_manager)
        .await
        .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("Unsupported voice provider 'nonexistent_provider'"));
}

#[test]
fn test_is_filler_speech_detects_chatter() {
    assert!(is_filler_speech("Alright, I'll check that now."));
    assert!(is_filler_speech("Alright, on it — working on that now."));
    assert!(is_filler_speech("Sure thing, let me look into this."));
    assert!(is_filler_speech("Working on that now."));
    assert!(is_filler_speech("Searching now."));
    assert!(!is_filler_speech("Found 3 open issues in the repository."));
    assert!(!is_filler_speech("The build passed with zero errors."));
    assert!(!is_filler_speech(
        "Error code 524 indicates you do not have permission to join this experience."
    ));
}

#[test]
fn test_generate_instant_voice_ack_matches_natural_conversational_patterns() {
    // 1. Wake word stripping ("hi luna", "luna check tests")
    let hi_luna = super::summary::generate_instant_voice_ack("hi luna");
    assert!(
        hi_luna.contains("Hi!") || hi_luna.contains("Hello!") || hi_luna.contains("Hey!"),
        "hi luna should yield a friendly greeting: {hi_luna}"
    );

    let hi = super::summary::generate_instant_voice_ack("hi");
    assert!(
        hi.contains("Hi!") || hi.contains("Hello!") || hi.contains("Hey!"),
        "hi should yield a friendly greeting: {hi}"
    );

    // 2. Identity queries
    let intro = super::summary::generate_instant_voice_ack("who are you?");
    assert!(intro.contains("Luna"), "intro should name Luna: {intro}");

    // 3. Affirmation / Thanks
    let thanks = super::summary::generate_instant_voice_ack("thanks luna");
    assert!(
        thanks.contains("got it") || thanks.contains("help") || thanks.contains("Sounds good") || thanks.contains("Understood"),
        "thanks should yield a natural closing/acknowledgment: {thanks}"
    );

    // 4. Action verbs & short topics
    let ipad = super::summary::generate_instant_voice_ack("Read on your iPad.");
    assert!(ipad.contains("reading on your iPad") || ipad.contains("iPad"));

    let server = super::summary::generate_instant_voice_ack("Check the status of the server.");
    assert!(server.contains("status of the server"));

    let challenger = super::summary::generate_instant_voice_ack("Challenger");
    assert!(challenger.contains("Challenger"));
}

#[test]
fn test_default_player_idle_timeout_is_three_minutes() {
    assert_eq!(
        DEFAULT_PLAYER_IDLE_TIMEOUT,
        std::time::Duration::from_secs(180)
    );
}

#[test]
fn test_live_ws_tts_payloads_and_idle_timeout() {
    use super::live_tts::*;
    assert_eq!(LIVE_WS_IDLE_TIMEOUT, std::time::Duration::from_secs(180));
    assert_eq!(LIVE_TTS_MODEL, "gemini-3.8-live");

    let setup = build_live_tts_setup("gemini-3.8-live", "Aoede");
    assert_eq!(
        setup["setup"]["model"].as_str(),
        Some("models/gemini-3.8-live")
    );
    assert_eq!(
        setup["setup"]["generationConfig"]["speechConfig"]["voiceConfig"]["prebuiltVoiceConfig"]
            ["voiceName"]
            .as_str(),
        Some("Aoede")
    );

    let turn = build_live_tts_turn("The build passed.");
    assert_eq!(
        turn["clientContent"]["turns"][0]["parts"][0]["text"].as_str(),
        Some("The build passed.")
    );
    assert_eq!(turn["clientContent"]["turnComplete"].as_bool(), Some(true));
}

#[test]
fn test_normalize_gemini_voice_preserves_agy_voices_case_insensitively() {
    use super::live_tts::normalize_gemini_voice;
    assert_eq!(normalize_gemini_voice("Despina"), "Despina");
    assert_eq!(normalize_gemini_voice("despina"), "Despina");
    assert_eq!(normalize_gemini_voice("DESPINA"), "Despina");
    assert_eq!(normalize_gemini_voice("Puck"), "Puck");
    assert_eq!(normalize_gemini_voice("puck"), "Puck");
    assert_eq!(normalize_gemini_voice("Zephyr"), "Zephyr");
    assert_eq!(normalize_gemini_voice("zephyr"), "Zephyr");
    assert_eq!(normalize_gemini_voice("Fenrir"), "Fenrir");
    assert_eq!(normalize_gemini_voice("fenrir"), "Fenrir");
    assert_eq!(normalize_gemini_voice("Aoede"), "Aoede");
    assert_eq!(normalize_gemini_voice("aoede"), "Aoede");
    assert_eq!(normalize_gemini_voice(""), "Aoede");
    assert_eq!(normalize_gemini_voice("unknown_voice"), "Aoede");
}

#[test]
fn test_extract_ready_speech_chunks_sentence_and_word_boundary() {
    let mut in_code = false;
    let (chunks_dot, consumed_dot) = extract_ready_speech_chunks(
        "The server is ready, and all services are online. Next sentence here",
        &mut in_code,
        true,
    );
    assert_eq!(chunks_dot, vec!["The server is ready, and all services are online."]);
    assert_eq!(consumed_dot, 50);

    let (chunks_words, consumed_words) = extract_ready_speech_chunks(
        "The background worker finished compiling all workspace crates cleanly without stopping",
        &mut in_code,
        true,
    );
    assert_eq!(chunks_words.len(), 1);
    assert!(consumed_words >= 60);
    assert!(chunks_words[0].starts_with("The background worker"));
}

#[test]
fn test_split_sentences_multi_line_commas_and_dots() {
    let text = "Installed package X, version 1.2.3.\nConfigured settings in config.yaml.\nAll 1,494 workspace tests passed.";
    let sentences = super::summary::split_sentences_for_tts(text);
    assert_eq!(sentences.len(), 3);
    assert_eq!(sentences[0], "Installed package X, version 1.2.3.");
    assert_eq!(sentences[1], "Configured settings in config.yaml.");
    assert_eq!(sentences[2], "All 1,494 workspace tests passed.");

    let summary = super::summary::fallback_voice_summary(text);
    assert!(summary.contains("Installed package X, version 1.2.3."));
    assert!(summary.contains("Configured settings in config.yaml."));
    assert!(summary.contains("All 1,494 workspace tests passed."));
}
