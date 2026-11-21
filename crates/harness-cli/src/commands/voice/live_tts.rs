//! Persistent warm Gemini Live WebSocket TTS engine (`BidiGenerateContent`).
//! Keeps a pre-connected, pre-configured WebSocket session open across turns so
//! first-audio latency drops from ~3,500ms (REST) to ~400ms with true streaming PCM.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use harness_core::HarnessError;
use harness_providers::CredentialManager;
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use super::playback::GenAudioSender;
use super::tts::parse_pcm_rate_from_mime;

/// Close idle Live WebSocket connections after 3 minutes of inactivity.
pub const LIVE_WS_IDLE_TIMEOUT: Duration = Duration::from_secs(180);

/// Primary Live TTS model used across all turns to prevent acoustic drift or voice model swapping.
pub const LIVE_TTS_MODEL: &str = "gemini-3.8-live";

static LAST_WORKING_LIVE_KEY: AtomicUsize = AtomicUsize::new(0);

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct LiveWsSession {
    ws: WsStream,
    voice_name: String,
    model: String,
    key_idx: usize,
    last_active: Instant,
}

impl LiveWsSession {
    fn is_expired(&self) -> bool {
        self.last_active.elapsed() >= LIVE_WS_IDLE_TIMEOUT
    }
}

fn live_session_slot() -> &'static Mutex<Option<LiveWsSession>> {
    static SLOT: OnceLock<Mutex<Option<LiveWsSession>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Normalize any user input voice string (case-insensitively) to the exact canonical Antigravity/Gemini voice name.
pub fn normalize_gemini_voice(voice: &str) -> &'static str {
    let trimmed = voice.trim();
    for entry in super::catalog::available_voices() {
        if entry.id.eq_ignore_ascii_case(trimmed) {
            return entry.id;
        }
    }
    // Canonical 30 Antigravity voice catalog
    for v in [
        "Achernar", "Achird", "Algenib", "Algieba", "Alnilam", "Aoede", "Autonoe",
        "Callirrhoe", "Charon", "Despina", "Enceladus", "Erinome", "Fenrir", "Gacrux",
        "Iapetus", "Kore", "Laomedeia", "Leda", "Orus", "Puck", "Pulcherrima",
        "Rasalgethi", "Sadachbia", "Sadaltager", "Schedar", "Sulafat", "Umbriel",
        "Vindemiatrix", "Zephyr", "Zubenelgenubi",
    ] {
        if v.eq_ignore_ascii_case(trimmed) {
            return v;
        }
    }
    "Aoede"
}

/// Build the Gemini Live `setup` JSON payload configured as a strict verbatim TTS reader.
pub fn build_live_tts_setup(model: &str, voice: &str) -> serde_json::Value {
    let voice_name = normalize_gemini_voice(voice);
    serde_json::json!({
        "setup": {
            "model": format!("models/{model}"),
            "generationConfig": {
                "responseModalities": ["AUDIO"],
                "temperature": 0.0,
                "speechConfig": {
                    "voiceConfig": {
                        "prebuiltVoiceConfig": {
                            "voiceName": voice_name
                        }
                    }
                }
            },
            "systemInstruction": {
                "parts": [{
                    "text": "You are a strict text-to-speech reader. Speak the exact text from the user message aloud word for word. Never answer questions, never add commentary, never greet, and never say anything extra."
                }]
            }
        }
    })
}

/// Build a `clientContent` turn payload for speaking one text chunk on the open Live socket.
pub fn build_live_tts_turn(text: &str) -> serde_json::Value {
    serde_json::json!({
        "clientContent": {
            "turns": [{
                "role": "user",
                "parts": [{ "text": text }]
            }],
            "turnComplete": true
        }
    })
}

async fn connect_and_setup(
    api_key: &str,
    model: &str,
    voice_name: &str,
    key_idx: usize,
) -> Result<LiveWsSession, String> {
    let url = format!(
        "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent?key={api_key}"
    );

    let (mut ws, _) = tokio::time::timeout(Duration::from_secs(6), connect_async(&url))
        .await
        .map_err(|_| format!("Gemini Live WS ({model}) connect timed out"))?
        .map_err(|e| format!("Gemini Live WS ({model}) connect failed: {e}"))?;

    let setup_msg = build_live_tts_setup(model, voice_name).to_string();
    ws.send(Message::Text(setup_msg.into()))
        .await
        .map_err(|e| format!("Gemini Live WS ({model}) setup send failed: {e}"))?;

    // Wait for `setupComplete` acknowledgement
    let ack = tokio::time::timeout(Duration::from_secs(6), ws.next())
        .await
        .map_err(|_| format!("Gemini Live WS ({model}) setup timed out"))?;

    match ack {
        Some(Ok(Message::Text(_))) | Some(Ok(Message::Binary(_))) => Ok(LiveWsSession {
            ws,
            voice_name: voice_name.to_string(),
            model: model.to_string(),
            key_idx,
            last_active: Instant::now(),
        }),
        Some(Ok(Message::Close(frame))) => Err(format!(
            "Gemini Live WS ({model}) closed on setup: {:?}",
            frame.map(|f| format!("{} {}", f.code, f.reason))
        )),
        Some(Err(e)) => Err(format!("Gemini Live WS ({model}) setup error: {e}")),
        other => Err(format!(
            "Gemini Live WS ({model}) unexpected setup response: {other:?}"
        )),
    }
}

async fn acquire_or_connect(
    api_keys: &[String],
    voice_name: &str,
    slot: &mut Option<LiveWsSession>,
) -> Result<(), String> {
    if let Some(sess) = slot.as_ref() {
        if !sess.is_expired() && sess.voice_name == voice_name {
            return Ok(());
        }
    }
    // Tear down stale or mismatched session
    if let Some(mut old) = slot.take() {
        let _ = old.ws.close(None).await;
    }

    if api_keys.is_empty() {
        return Err("No Gemini API keys available for Live WS TTS".to_string());
    }

    let start_key = LAST_WORKING_LIVE_KEY.load(Ordering::Relaxed) % api_keys.len();
    let mut last_err = String::new();

    // Stick strictly to LIVE_TTS_MODEL so voice timbre and model never change mid-session
    for k_off in 0..api_keys.len() {
        let key_idx = (start_key + k_off) % api_keys.len();
        let key = &api_keys[key_idx];
        match connect_and_setup(key, LIVE_TTS_MODEL, voice_name, key_idx).await {
            Ok(sess) => {
                LAST_WORKING_LIVE_KEY.store(key_idx, Ordering::Relaxed);
                *slot = Some(sess);
                return Ok(());
            }
            Err(e) => {
                tracing::debug!("Live WS TTS candidate failed: {e}");
                last_err = e;
            }
        }
    }

    Err(last_err)
}

/// Pre-connect and warm the Gemini Live WebSocket in the background when voice mode starts.
pub fn spawn_warm_up_live_tts(cred_manager: Arc<CredentialManager>, voice_name: String) {
    tokio::spawn(async move {
        let api_keys = super::stt::resolve_gemini_api_keys(&cred_manager).await;
        if api_keys.is_empty() {
            return;
        }
        let voice = normalize_gemini_voice(&voice_name).to_string();
        let mut guard = live_session_slot().lock().await;
        if let Err(e) = acquire_or_connect(&api_keys, &voice, &mut guard).await {
            tracing::debug!("Live WS TTS pre-warm skipped: {e}");
        } else {
            tracing::debug!("Live WS TTS connection pre-warmed and ready");
        }
    });
}

/// Drop any active Live WebSocket connection (e.g., on voice mode deactivate or explicit reset).
pub fn drop_live_tts_session() {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        handle.spawn(async {
            if let Ok(mut guard) = live_session_slot().try_lock() {
                if let Some(mut old) = guard.take() {
                    let _ = old.ws.close(None).await;
                }
            }
        });
    }
}

/// Stream a single text chunk over the persistent warm Gemini Live WebSocket into `audio`.
/// Automatically retries once with a fresh key/connection if the warm socket died or hit quota.
pub async fn stream_gemini_live_ws_tts(
    text: &str,
    api_keys: &[String],
    voice: &str,
    audio: GenAudioSender<'_>,
) -> Result<(), HarnessError> {
    if api_keys.is_empty() {
        return Err(HarnessError::VoiceError(
            "No Gemini API keys available for Live WS TTS".into(),
        ));
    }

    let voice_name = normalize_gemini_voice(voice);
    let turn_payload = build_live_tts_turn(text).to_string();
    let mut guard = live_session_slot().lock().await;

    let max_attempts = api_keys.len().clamp(1, 4);
    let mut last_err = String::new();

    for attempt in 0..max_attempts {
        if audio.cancel.is_cancelled() {
            return Ok(());
        }

        if let Err(e) = acquire_or_connect(api_keys, voice_name, &mut guard).await {
            return Err(HarnessError::VoiceError(e));
        }

        let Some(sess) = guard.as_mut() else {
            return Err(HarnessError::VoiceError(
                "Live WS TTS session unavailable".into(),
            ));
        };

        if let Err(e) = sess
            .ws
            .send(Message::Text(turn_payload.clone().into()))
            .await
        {
            last_err = format!("Live WS send failed: {e}");
            let bad_key = sess.key_idx;
            *guard = None;
            LAST_WORKING_LIVE_KEY.store((bad_key + 1) % api_keys.len(), Ordering::Relaxed);
            continue;
        }

        let mut got_audio = false;
        let mut stream_err: Option<String> = None;

        loop {
            let next_msg = tokio::select! {
                _ = audio.cancel.cancelled() => {
                    // Drop the socket on barge-in so stale server frames never leak into the next turn
                    if let Some(mut old) = guard.take() {
                        let _ = old.ws.close(None).await;
                    }
                    return Ok(());
                }
                msg = tokio::time::timeout(Duration::from_secs(10), sess.ws.next()) => msg,
            };

            let frame = match next_msg {
                Ok(Some(Ok(m))) => m,
                Ok(Some(Err(e))) => {
                    stream_err = Some(format!("Live WS read error: {e}"));
                    break;
                }
                Ok(None) => {
                    stream_err = Some("Live WS closed by server".to_string());
                    break;
                }
                Err(_) => {
                    stream_err = Some("Live WS audio chunk timed out".to_string());
                    break;
                }
            };

            let raw_text = match frame {
                Message::Text(t) => t.to_string(),
                Message::Binary(b) => String::from_utf8_lossy(&b).into_owned(),
                Message::Ping(p) => {
                    let _ = sess.ws.send(Message::Pong(p)).await;
                    continue;
                }
                Message::Pong(_) => continue,
                Message::Close(c) => {
                    stream_err = Some(format!(
                        "Live WS closed: {:?}",
                        c.map(|f| format!("{} {}", f.code, f.reason))
                    ));
                    break;
                }
                Message::Frame(_) => continue,
            };

            let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw_text) else {
                continue;
            };

            let sc = &val["serverContent"];
            if let Some(parts) = sc["modelTurn"]["parts"].as_array() {
                for part in parts {
                    let inline = &part["inlineData"];
                    let Some(pcm_b64) = inline["data"].as_str() else {
                        continue;
                    };
                    let sample_rate = inline["mimeType"]
                        .as_str()
                        .and_then(parse_pcm_rate_from_mime)
                        .unwrap_or(24_000);
                    use base64::Engine;
                    if let Ok(pcm_bytes) =
                        base64::engine::general_purpose::STANDARD.decode(pcm_b64.trim())
                    {
                        #[allow(clippy::chunks_exact_to_as_chunks)]
                        let samples: Vec<i16> = pcm_bytes
                            .chunks_exact(2)
                            .map(|c| i16::from_le_bytes([c[0], c[1]]))
                            .collect();
                        if !samples.is_empty() {
                            audio.send_pcm(sample_rate, samples);
                            got_audio = true;
                        }
                    }
                }
            }

            if sc["turnComplete"].as_bool() == Some(true)
                || sc["generationComplete"].as_bool() == Some(true)
            {
                sess.last_active = Instant::now();
                if got_audio {
                    return Ok(());
                }
                break;
            }
        }

        if got_audio {
            if let Some(s) = guard.as_mut() {
                s.last_active = Instant::now();
            }
            return Ok(());
        }

        // Connection failed or hit quota: drop socket, advance sticky key, retry
        let failed_key = guard.as_ref().map(|s| s.key_idx).unwrap_or(0);
        let failed_model = guard
            .as_ref()
            .map(|s| s.model.clone())
            .unwrap_or_else(|| "live".to_string());
        *guard = None;
        LAST_WORKING_LIVE_KEY.store((failed_key + 1) % api_keys.len(), Ordering::Relaxed);
        last_err = stream_err
            .unwrap_or_else(|| format!("Live WS ({failed_model}) produced no audio data"));
        tracing::debug!(
            attempt = attempt + 1,
            "Live WS TTS failed, rotating key: {last_err}"
        );
    }

    Err(HarnessError::VoiceError(last_err))
}
