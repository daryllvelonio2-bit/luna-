//! End-to-end tests of the voice mode step: scripted answers against temp config files.

use std::collections::BTreeMap;
use std::io::Cursor;
use std::path::Path;

use harness_core::config::{Config, Layers};
use harness_onboarding::{run, run_voice, InstantPacer, Size};

fn layers(file: &Path) -> Layers {
    Layers {
        defaults: Config::default(),
        file: Some(file.to_path_buf()),
        profile: None,
        env: BTreeMap::new(),
        flags: BTreeMap::new(),
    }
}

fn columns() -> Size {
    Size::new(100, 40)
}

fn strip_escapes(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' && chars.peek() == Some(&'[') {
            chars.next();
            for escape in chars.by_ref() {
                if escape.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        out.push(ch);
    }
    out
}

fn read_config(file: &Path) -> serde_yaml::Value {
    let raw = std::fs::read_to_string(file).expect("the flow wrote a config file");
    serde_yaml::from_str(&raw).expect("the flow wrote valid YAML")
}

#[test]
fn default_voice_step_records_antigravity() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");
    let layers = layers(&file);

    let mut reader = Cursor::new(b"1\n\n".to_vec());
    let mut out = Vec::new();
    let mut pacer = InstantPacer;

    let (voice, written_path) =
        run_voice(&layers, &mut reader, &mut out, &mut pacer, &columns).expect("voice step runs");

    assert_eq!(written_path, file);
    assert!(voice.enabled);
    assert_eq!(voice.provider, "antigravity");
    assert_eq!(voice.voice_name, "Aoede");

    let config = read_config(&file);
    assert_eq!(config["voice"]["enabled"].as_bool(), Some(true));
    assert_eq!(config["voice"]["provider"].as_str(), Some("antigravity"));
    assert_eq!(config["voice"]["voice_name"].as_str(), Some("Aoede"));
}

#[test]
fn voice_step_custom_setup_records_user_overrides() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");
    let layers = layers(&file);

    // 4 = Custom, provider: custom-tts, wake: HeyLuna, voice: Fenrir, stt: whisper-v3, tts: piper, thresh: 1200, barge: y
    let script = "4\ncustom-tts\nHeyLuna\nFenrir\nwhisper-v3\npiper\n1200\ny\n";
    let mut reader = Cursor::new(script.as_bytes().to_vec());
    let mut out = Vec::new();
    let mut pacer = InstantPacer;

    let (voice, _) = run_voice(&layers, &mut reader, &mut out, &mut pacer, &columns)
        .expect("custom voice step runs");

    assert!(voice.enabled);
    assert_eq!(voice.provider, "custom-tts");
    assert_eq!(voice.wake_word, "HeyLuna");
    assert_eq!(voice.voice_name, "Fenrir");
    assert_eq!(voice.stt_model, "whisper-v3");
    assert_eq!(voice.tts_model, "piper");
    assert_eq!(voice.silence_threshold_ms, 1200);
    assert!(voice.enable_barge_in);

    let config = read_config(&file);
    assert_eq!(config["voice"]["provider"].as_str(), Some("custom-tts"));
    assert_eq!(config["voice"]["wake_word"].as_str(), Some("HeyLuna"));
    assert_eq!(config["voice"]["voice_name"].as_str(), Some("Fenrir"));
    assert_eq!(config["voice"]["silence_threshold_ms"].as_u64(), Some(1200));
}

#[test]
fn voice_step_disabled_records_disabled() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");
    let layers = layers(&file);

    let mut reader = Cursor::new(b"5\n".to_vec());
    let mut out = Vec::new();
    let mut pacer = InstantPacer;

    let (voice, _) = run_voice(&layers, &mut reader, &mut out, &mut pacer, &columns)
        .expect("disabled voice runs");

    assert!(!voice.enabled);
    assert_eq!(voice.provider, "none");

    let config = read_config(&file);
    assert_eq!(config["voice"]["enabled"].as_bool(), Some(false));
    assert_eq!(config["voice"]["provider"].as_str(), Some("none"));
}

#[test]
fn full_onboarding_flow_includes_voice_confirmation() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let file = tmp.path().join("config.yaml");
    let layers = layers(&file);

    // memory(1), compact(2), gateway(1), notes(blank), web(1), voice(1, blank)
    let script = "1\n2\n1\n\n1\n1\n\n";
    let mut reader = Cursor::new(script.as_bytes().to_vec());
    let mut out = Vec::new();
    let mut pacer = InstantPacer;

    let (outcome, rendered) = {
        let out_res = run(&layers, &mut reader, &mut out, &mut pacer, &columns)
            .expect("onboarding completes");
        (out_res, strip_escapes(&out))
    };

    assert!(outcome.voice.enabled);
    assert_eq!(outcome.voice.provider, "antigravity");
    assert!(rendered.contains("VOICE MODE"));
    assert!(rendered.contains("Voice mode"));
    assert!(rendered.contains("Antigravity"));
}
