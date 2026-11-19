//! Cloud streaming text-to-speech engine (Antigravity, Gemini, OpenAI).
//! Strict provider credential isolation with zero fallbacks between providers.

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use futures_util::StreamExt;
use harness_core::{Config, HarnessError};
use harness_providers::CredentialManager;

use super::playback::GenAudioSender;
use super::summary::clean_text_for_speech;
use super::{voice_http_client, VoiceEvent};

static LAST_WORKING_GEMINI_KEY: AtomicUsize = AtomicUsize::new(0);

pub async fn stream_tts_sentence_to_audio(
    text: &str,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    voice_tx: Option<&tokio::sync::mpsc::UnboundedSender<VoiceEvent>>,
    audio: GenAudioSender<'_>,
) {
    if audio.cancel.is_cancelled() {
        return;
    }

    let cleaned = clean_text_for_speech(text);
    if cleaned.trim().is_empty() {
        return;
    }

    let provider = config.voice.provider.to_ascii_lowercase();
    let tts_model = config.voice.tts_model.trim();
    let voice_name = config.voice.voice_name.trim();

    match provider.as_str() {
        "openai" => {
            let Some(key) = super::stt::resolve_openai_api_key(cred_manager).await else {
                let msg = "No OpenAI credentials configured for voice TTS (run `luna auth add openai`)".to_string();
                tracing::warn!("{msg}");
                if let Some(tx) = voice_tx {
                    let _ = tx.send(VoiceEvent::Error(msg));
                }
                return;
            };
            if let Err(e) =
                stream_openai_tts_pcm(&cleaned, &key, voice_name, tts_model, audio).await
            {
                tracing::warn!("OpenAI TTS failed: {e}");
                if let Some(tx) = voice_tx {
                    let _ = tx.send(VoiceEvent::Error(format!("OpenAI TTS error: {e}")));
                }
            }
        }
        "gemini" => {
            let g_keys = super::stt::lookup_plain_api_keys("gemini", cred_manager).await;
            if g_keys.is_empty() {
                let msg = "No Gemini credentials configured for voice TTS (run `luna auth add gemini`)".to_string();
                tracing::warn!("{msg}");
                if let Some(tx) = voice_tx {
                    let _ = tx.send(VoiceEvent::Error(msg));
                }
                return;
            }
            if let Err(e) =
                stream_gemini_tts_sse(&cleaned, &g_keys, voice_name, tts_model, audio).await
            {
                tracing::warn!("Gemini TTS error: {e}");
                if let Some(tx) = voice_tx {
                    let _ = tx.send(VoiceEvent::Error(format!("Gemini TTS error: {e}")));
                }
            }
        }
        "antigravity" | "agy" => {
            if let Err(e) =
                stream_antigravity_tts(&cleaned, cred_manager, voice_name, tts_model, audio).await
            {
                tracing::warn!("Antigravity TTS error: {e}");
                if let Some(tx) = voice_tx {
                    let _ = tx.send(VoiceEvent::Error(format!("Antigravity TTS error: {e}")));
                }
            }
        }
        other => {
            let msg = format!(
                "Unsupported voice provider '{other}' for TTS (supported: 'antigravity', 'gemini', 'openai')"
            );
            tracing::warn!("{msg}");
            if let Some(tx) = voice_tx {
                let _ = tx.send(VoiceEvent::Error(msg));
            }
        }
    }
}

pub async fn stream_openai_tts_pcm(
    text: &str,
    api_key: &str,
    voice: &str,
    model: &str,
    audio: GenAudioSender<'_>,
) -> Result<(), HarnessError> {
    let client = voice_http_client();
    let openai_voice = match voice {
        "alloy" | "ash" | "ballad" | "coral" | "echo" | "fable" | "nova" | "onyx" | "sage"
        | "shimmer" | "verse" | "marin" | "cedar" => voice,
        _ => "alloy",
    };
    let res = client
        .post("https://api.openai.com/v1/audio/speech")
        .bearer_auth(api_key)
        .json(&serde_json::json!({
            "model": if model.is_empty() { "tts-1" } else { model },
            "input": text,
            "voice": openai_voice,
            "response_format": "pcm"
        }))
        .send()
        .await
        .map_err(|e| HarnessError::VoiceError(format!("OpenAI TTS request failed: {e}")))?;

    if !res.status().is_success() {
        let err_text = res.text().await.unwrap_or_default();
        return Err(HarnessError::VoiceError(format!(
            "OpenAI TTS error: {}",
            err_text.chars().take(160).collect::<String>()
        )));
    }

    let mut stream = res.bytes_stream();
    let mut carry: Option<u8> = None;
    while let Some(chunk_res) = stream.next().await {
        if audio.cancel.is_cancelled() {
            return Ok(());
        }
        let chunk = chunk_res
            .map_err(|e| HarnessError::VoiceError(format!("OpenAI TTS stream read failed: {e}")))?;
        let mut buf = Vec::with_capacity(chunk.len() + 1);
        if let Some(b) = carry.take() {
            buf.push(b);
        }
        buf.extend_from_slice(&chunk);
        if buf.len() % 2 == 1 {
            carry = buf.pop();
        }
        #[allow(clippy::chunks_exact_to_as_chunks)]
        let samples: Vec<i16> = buf
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();
        audio.send_pcm(24_000, samples);
    }
    Ok(())
}

/// Build the Gemini TTS payload sending bare text directly so the model never speaks
/// instruction prefixes like "Read aloud...".
pub fn build_gemini_tts_payload(text: &str, voice: &str) -> serde_json::Value {
    let voice_name = if voice.is_empty() || voice.contains("Neural") {
        "Aoede"
    } else {
        voice
    };
    serde_json::json!({
        "contents": [{
            "parts": [{ "text": text }]
        }],
        "generationConfig": {
            "responseModalities": ["AUDIO"],
            "speechConfig": {
                "voiceConfig": {
                    "prebuiltVoiceConfig": {
                        "voiceName": voice_name
                    }
                }
            }
        }
    })
}

/// Stream Gemini TTS via `streamGenerateContent?alt=sse`.
pub async fn stream_gemini_tts_sse(
    text: &str,
    api_keys: &[String],
    voice: &str,
    model: &str,
    audio: GenAudioSender<'_>,
) -> Result<(), HarnessError> {
    if api_keys.is_empty() {
        return Err(HarnessError::VoiceError(
            "No Gemini API keys available for TTS".into(),
        ));
    }
    let client = voice_http_client();
    let payload = build_gemini_tts_payload(text, voice);

    let candidates: Vec<&str> = if !model.is_empty() {
        vec![model]
    } else {
        vec!["gemini-2.5-flash-preview-tts", "gemini-2.5-pro-preview-tts"]
    };

    let start_idx =
        LAST_WORKING_GEMINI_KEY.load(std::sync::atomic::Ordering::Relaxed) % api_keys.len();
    let mut last_err = String::new();

    for m in candidates {
        for offset in 0..api_keys.len() {
            let key_idx = (start_idx + offset) % api_keys.len();
            let api_key = &api_keys[key_idx];
            if audio.cancel.is_cancelled() {
                return Ok(());
            }
            let url = format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{m}:streamGenerateContent?alt=sse&key={api_key}"
            );
            let res = match client.post(&url).json(&payload).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = format!("Gemini TTS ({m}) request failed: {e}");
                    continue;
                }
            };

            if !res.status().is_success() {
                let status = res.status();
                let err_text = res.text().await.unwrap_or_default();
                last_err = format!(
                    "Gemini TTS ({m}) HTTP {status}: {}",
                    err_text.chars().take(160).collect::<String>()
                );
                if status.as_u16() == 429 || status.as_u16() == 403 || status.as_u16() == 401 {
                    continue;
                }
                break;
            }

            let mut stream = res.bytes_stream();
            let mut line_buf: Vec<u8> = Vec::new();
            let mut got_audio = false;

            while let Some(chunk_res) = stream.next().await {
                if audio.cancel.is_cancelled() {
                    return Ok(());
                }
                let Ok(chunk) = chunk_res else {
                    break;
                };
                line_buf.extend_from_slice(&chunk);

                while let Some(nl_pos) = line_buf.iter().position(|&b| b == b'\n') {
                    let raw_line: Vec<u8> = line_buf.drain(..=nl_pos).collect();
                    let line_str = String::from_utf8_lossy(&raw_line);
                    let trimmed = line_str.trim();
                    if let Some(json_str) = trimmed.strip_prefix("data: ") {
                        if push_gemini_sse_audio_chunk(json_str, audio) {
                            got_audio = true;
                        }
                    }
                }
            }

            if !line_buf.is_empty() && !audio.cancel.is_cancelled() {
                let line_str = String::from_utf8_lossy(&line_buf);
                let trimmed = line_str.trim();
                if let Some(json_str) = trimmed.strip_prefix("data: ") {
                    if push_gemini_sse_audio_chunk(json_str, audio) {
                        got_audio = true;
                    }
                }
            }

            if got_audio {
                LAST_WORKING_GEMINI_KEY.store(key_idx, std::sync::atomic::Ordering::Relaxed);
                return Ok(());
            }
            last_err = format!("No inline audio data in Gemini ({m}) SSE stream");
            break;
        }
    }

    Err(HarnessError::VoiceError(last_err))
}

/// Stream Antigravity TTS using Antigravity OAuth credentials.
pub async fn stream_antigravity_tts(
    text: &str,
    cred_manager: &Arc<CredentialManager>,
    voice: &str,
    model: &str,
    audio: GenAudioSender<'_>,
) -> Result<(), HarnessError> {
    let raw_secret = match super::stt::lookup_raw_credential("antigravity", cred_manager).await {
        Some(s) => s,
        None => super::stt::lookup_raw_credential("agy", cred_manager)
            .await
            .ok_or_else(|| {
                HarnessError::VoiceError(
                    "No Antigravity credentials configured (run `luna auth add antigravity`)".into(),
                )
            })?,
    };

    let (mut bearer_token, project_opt) =
        harness_providers::antigravity::extract_bearer_token(&raw_secret);
    let project =
        project_opt.unwrap_or_else(|| harness_providers::antigravity::DEFAULT_PROJECT.to_string());
    let client = voice_http_client();

    let voice_name = if voice.is_empty() || voice.contains("Neural") {
        "Aoede"
    } else {
        voice
    };

    let chosen_model = if !model.is_empty() {
        model
    } else {
        "gemini-2.5-flash-preview-tts"
    };

    let payload = serde_json::json!({
        "project": project,
        "model": chosen_model,
        "request": {
            "contents": [{
                "parts": [{ "text": text }]
            }],
            "generationConfig": {
                "responseModalities": ["AUDIO"],
                "speechConfig": {
                    "voiceConfig": {
                        "prebuiltVoiceConfig": {
                            "voiceName": voice_name
                        }
                    }
                }
            }
        }
    });

    let mut res = client
        .post(harness_providers::antigravity::CODE_ASSIST_ENDPOINT)
        .header(
            reqwest::header::AUTHORIZATION,
            format!("Bearer {bearer_token}"),
        )
        .header(
            reqwest::header::USER_AGENT,
            harness_providers::antigravity::user_agent(),
        )
        .json(&payload)
        .send()
        .await
        .map_err(|e| HarnessError::VoiceError(format!("Antigravity TTS request failed: {e}")))?;

    if res.status().as_u16() == 401 {
        if let Ok(new_token) =
            harness_providers::antigravity::refresh_and_store_antigravity_credential(
                client,
                &raw_secret,
                Some(cred_manager),
            )
            .await
        {
            bearer_token = new_token;
            res = client
                .post(harness_providers::antigravity::CODE_ASSIST_ENDPOINT)
                .header(
                    reqwest::header::AUTHORIZATION,
                    format!("Bearer {bearer_token}"),
                )
                .header(
                    reqwest::header::USER_AGENT,
                    harness_providers::antigravity::user_agent(),
                )
                .json(&payload)
                .send()
                .await
                .map_err(|e| {
                    HarnessError::VoiceError(format!("Antigravity TTS retry request failed: {e}"))
                })?;
        }
    }

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(HarnessError::VoiceError(format!(
            "Antigravity TTS HTTP {status}: {}",
            body.chars().take(160).collect::<String>()
        )));
    }

    let mut stream = res.bytes_stream();
    let mut line_buf: Vec<u8> = Vec::new();
    let mut got_audio = false;

    while let Some(chunk_res) = stream.next().await {
        if audio.cancel.is_cancelled() {
            return Ok(());
        }
        let Ok(chunk) = chunk_res else {
            break;
        };
        line_buf.extend_from_slice(&chunk);

        while let Some(nl_pos) = line_buf.iter().position(|&b| b == b'\n') {
            let raw_line: Vec<u8> = line_buf.drain(..=nl_pos).collect();
            let line_str = String::from_utf8_lossy(&raw_line);
            let trimmed = line_str.trim();
            if let Some(json_str) = trimmed.strip_prefix("data: ") {
                if push_gemini_sse_audio_chunk(json_str, audio) {
                    got_audio = true;
                }
            }
        }
    }

    if !line_buf.is_empty() && !audio.cancel.is_cancelled() {
        let line_str = String::from_utf8_lossy(&line_buf);
        let trimmed = line_str.trim();
        if let Some(json_str) = trimmed.strip_prefix("data: ") {
            if push_gemini_sse_audio_chunk(json_str, audio) {
                got_audio = true;
            }
        }
    }

    if got_audio {
        return Ok(());
    }

    Err(HarnessError::VoiceError(
        "No inline audio received in Antigravity TTS stream".into(),
    ))
}

pub fn push_gemini_sse_audio_chunk(json_str: &str, audio: GenAudioSender<'_>) -> bool {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) else {
        return false;
    };
    let candidates = val["candidates"]
        .as_array()
        .or_else(|| val["response"]["candidates"].as_array());
    let Some(candidates) = candidates else {
        return false;
    };
    let Some(parts) = candidates.first().and_then(|c| c["content"]["parts"].as_array()) else {
        return false;
    };
    let mut sent_any = false;
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
        if let Ok(pcm_bytes) = base64::engine::general_purpose::STANDARD.decode(pcm_b64.trim()) {
            #[allow(clippy::chunks_exact_to_as_chunks)]
            let samples: Vec<i16> = pcm_bytes
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();
            if !samples.is_empty() {
                audio.send_pcm(sample_rate, samples);
                sent_any = true;
            }
        }
    }
    sent_any
}

pub fn parse_pcm_rate_from_mime(mime: &str) -> Option<u32> {
    for part in mime.split(';') {
        let trimmed = part.trim();
        if let Some(val) = trimmed.strip_prefix("rate=") {
            if let Ok(rate) = val.trim().parse::<u32>() {
                return Some(rate);
            }
        }
    }
    None
}
