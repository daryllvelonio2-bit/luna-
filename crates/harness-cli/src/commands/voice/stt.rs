//! Speech-to-text transcription engine (Antigravity, Gemini, Groq, OpenAI Whisper).
//! Strict credential isolation with zero fallbacks between providers.

use std::sync::Arc;

use harness_core::{Config, HarnessError};
use harness_providers::CredentialManager;

use super::voice_http_client;

pub async fn lookup_plain_api_keys(
    provider: &str,
    cred_manager: &CredentialManager,
) -> Vec<String> {
    let creds = cred_manager.get_credentials(provider).await;
    let mut out: Vec<String> = Vec::new();
    for priority_pass in [true, false] {
        for c in &creds {
            if c.is_priority != priority_pass {
                continue;
            }
            let trimmed = c.api_key.trim();
            if !trimmed.is_empty() && !trimmed.starts_with('{') && !out.iter().any(|k| k == trimmed)
            {
                out.push(trimmed.to_string());
            }
        }
    }
    out
}

pub async fn lookup_plain_api_key(
    provider: &str,
    cred_manager: &CredentialManager,
) -> Option<String> {
    lookup_plain_api_keys(provider, cred_manager)
        .await
        .into_iter()
        .next()
}

pub async fn lookup_raw_credential(
    provider: &str,
    cred_manager: &CredentialManager,
) -> Option<String> {
    let creds = cred_manager.get_credentials(provider).await;
    creds
        .iter()
        .find(|c| c.is_priority)
        .or_else(|| creds.first())
        .map(|c| c.api_key.clone())
}

pub async fn resolve_gemini_api_keys(cred_manager: &Arc<CredentialManager>) -> Vec<String> {
    let mut keys = lookup_plain_api_keys("gemini", cred_manager).await;
    for env_name in ["GEMINI_API_KEY", "GOOGLE_API_KEY"] {
        if let Ok(k) = std::env::var(env_name) {
            let trimmed = k.trim();
            if !trimmed.is_empty() && !keys.iter().any(|x| x == trimmed) {
                keys.push(trimmed.to_string());
            }
        }
    }
    keys
}

#[allow(dead_code)]
pub async fn resolve_gemini_api_key(cred_manager: &Arc<CredentialManager>) -> Option<String> {
    resolve_gemini_api_keys(cred_manager)
        .await
        .into_iter()
        .next()
}

pub async fn resolve_groq_api_key(cred_manager: &Arc<CredentialManager>) -> Option<String> {
    if let Some(k) = lookup_plain_api_key("groq", cred_manager).await {
        return Some(k);
    }
    std::env::var("GROQ_API_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

pub async fn resolve_openai_api_key(cred_manager: &Arc<CredentialManager>) -> Option<String> {
    if let Some(k) = lookup_plain_api_key("openai", cred_manager).await {
        return Some(k);
    }
    std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Transcribe recorded audio bytes into text with strict provider routing (zero fallbacks).
pub async fn transcribe_audio_bytes(
    wav_bytes: &[u8],
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
) -> Result<String, HarnessError> {
    let provider = config.voice.provider.to_ascii_lowercase();
    let stt_model = config.voice.stt_model.trim();

    match provider.as_str() {
        "antigravity" | "agy" => {
            transcribe_antigravity_audio(wav_bytes, cred_manager, stt_model).await
        }
        "gemini" => {
            let g_keys = resolve_gemini_api_keys(cred_manager).await;
            if g_keys.is_empty() {
                return Err(HarnessError::VoiceError(
                    "No Gemini API keys found for voice STT (check `luna auth list`)".into(),
                ));
            }
            transcribe_gemini_audio_with_keys(wav_bytes, &g_keys, stt_model).await
        }
        "groq" => {
            let key = resolve_groq_api_key(cred_manager).await.ok_or_else(|| {
                HarnessError::VoiceError(
                    "No Groq API key found for voice STT (check `luna auth list`)".into(),
                )
            })?;
            transcribe_groq_whisper(wav_bytes, &key, stt_model).await
        }
        "openai" => {
            let key = resolve_openai_api_key(cred_manager).await.ok_or_else(|| {
                HarnessError::VoiceError(
                    "No OpenAI API key found for voice STT (check `luna auth list`)".into(),
                )
            })?;
            transcribe_openai_whisper(wav_bytes, &key, stt_model).await
        }
        other => Err(HarnessError::VoiceError(format!(
            "Unsupported voice provider '{other}' for STT (use 'antigravity', 'gemini', 'groq', or 'openai')"
        ))),
    }
}

pub async fn transcribe_antigravity_audio(
    wav_bytes: &[u8],
    cred_manager: &Arc<CredentialManager>,
    model: &str,
) -> Result<String, HarnessError> {
    let raw_secret = match lookup_raw_credential("antigravity", cred_manager).await {
        Some(s) => s,
        None => lookup_raw_credential("agy", cred_manager)
            .await
            .ok_or_else(|| {
                HarnessError::VoiceError("No Antigravity credentials configured".into())
            })?,
    };

    let (mut bearer_token, project_opt) =
        harness_providers::antigravity::extract_bearer_token(&raw_secret);
    let project =
        project_opt.unwrap_or_else(|| harness_providers::antigravity::DEFAULT_PROJECT.to_string());
    let client = voice_http_client();

    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(wav_bytes);

    let chosen_model = if !model.is_empty() {
        model
    } else {
        "gemini-2.5-flash"
    };

    let payload = serde_json::json!({
        "project": project,
        "model": chosen_model,
        "request": {
            "contents": [{
                "parts": [
                    {
                        "text": "Transcribe the spoken words in this audio exactly. Output only the transcript text, nothing else. If there is no clear speech, output an empty string."
                    },
                    {
                        "inlineData": {
                            "mimeType": "audio/wav",
                            "data": b64
                        }
                    }
                ]
            }]
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
        .map_err(|e| HarnessError::VoiceError(format!("Antigravity STT request failed: {e}")))?;

    // Handle ordinary token expiry (401) by refreshing token in place and retrying once
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
                    HarnessError::VoiceError(format!("Antigravity STT retry request failed: {e}"))
                })?;
        }
    }

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(HarnessError::VoiceError(format!(
            "Antigravity STT HTTP {status}: {}",
            body.chars().take(160).collect::<String>()
        )));
    }

    let body = res
        .text()
        .await
        .map_err(|e| HarnessError::VoiceError(format!("Antigravity STT read failed: {e}")))?;
    let mut transcript = String::new();
    for line in body.lines() {
        if let Some(json_str) = line.strip_prefix("data: ") {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) {
                if let Some(parts) = val["response"]["candidates"][0]["content"]["parts"].as_array()
                {
                    for p in parts {
                        if let Some(t) = p["text"].as_str() {
                            transcript.push_str(t);
                        }
                    }
                }
            }
        }
    }
    Ok(transcript.trim().to_string())
}

pub async fn transcribe_gemini_audio_with_keys(
    wav_bytes: &[u8],
    api_keys: &[String],
    model: &str,
) -> Result<String, HarnessError> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(wav_bytes);
    let client = voice_http_client();

    let candidates: Vec<&str> = if !model.is_empty() {
        vec![model]
    } else {
        vec!["gemini-2.5-flash", "gemini-2.0-flash", "gemini-1.5-flash"]
    };

    let payload = serde_json::json!({
        "contents": [{
            "parts": [
                {
                    "text": "Transcribe the spoken words in this audio exactly. Output only the transcript text, nothing else. If there is no clear speech, output an empty string."
                },
                {
                    "inlineData": {
                        "mimeType": "audio/wav",
                        "data": b64
                    }
                }
            ]
        }]
    });

    let mut last_err = String::new();
    for m in candidates {
        for api_key in api_keys {
            let url = format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{m}:generateContent?key={api_key}"
            );
            let res = match client.post(&url).json(&payload).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = format!("Gemini STT ({m}) request failed: {e}");
                    continue;
                }
            };

            if !res.status().is_success() {
                let status = res.status();
                let err_text = res.text().await.unwrap_or_default();
                last_err = format!(
                    "Gemini STT ({m}) HTTP {status}: {}",
                    err_text.chars().take(160).collect::<String>()
                );
                if status.as_u16() == 429 || status.as_u16() == 403 || status.as_u16() == 401 {
                    continue;
                }
                break;
            }

            let val: serde_json::Value = match res.json().await {
                Ok(v) => v,
                Err(e) => {
                    last_err = format!("Gemini STT ({m}) JSON parse failed: {e}");
                    continue;
                }
            };

            let mut transcript = String::new();
            if let Some(parts) = val["candidates"][0]["content"]["parts"].as_array() {
                for p in parts {
                    if let Some(t) = p["text"].as_str() {
                        transcript.push_str(t);
                    }
                }
            }
            return Ok(transcript.trim().to_string());
        }
    }

    Err(HarnessError::VoiceError(last_err))
}

pub async fn transcribe_groq_whisper(
    wav_bytes: &[u8],
    api_key: &str,
    model: &str,
) -> Result<String, HarnessError> {
    let client = voice_http_client();
    let chosen_model = if model.is_empty() {
        "whisper-large-v3-turbo"
    } else {
        model
    };

    let part = reqwest::multipart::Part::bytes(wav_bytes.to_vec())
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| HarnessError::VoiceError(format!("Multipart part error: {e}")))?;

    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", chosen_model.to_string())
        .text("response_format", "json");

    let res = client
        .post("https://api.groq.com/openai/v1/audio/transcriptions")
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| HarnessError::VoiceError(format!("Groq Whisper request failed: {e}")))?;

    if !res.status().is_success() {
        let status = res.status();
        let err_text = res.text().await.unwrap_or_default();
        return Err(HarnessError::VoiceError(format!(
            "Groq Whisper HTTP {status}: {}",
            err_text.chars().take(160).collect::<String>()
        )));
    }

    let val: serde_json::Value = res
        .json()
        .await
        .map_err(|e| HarnessError::VoiceError(format!("Groq response JSON error: {e}")))?;

    let text = val["text"].as_str().unwrap_or_default().trim().to_string();
    Ok(text)
}

pub async fn transcribe_openai_whisper(
    wav_bytes: &[u8],
    api_key: &str,
    model: &str,
) -> Result<String, HarnessError> {
    let client = voice_http_client();
    let chosen_model = if model.is_empty() { "whisper-1" } else { model };

    let part = reqwest::multipart::Part::bytes(wav_bytes.to_vec())
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| HarnessError::VoiceError(format!("Multipart part error: {e}")))?;

    let form = reqwest::multipart::Form::new()
        .part("file", part)
        .text("model", chosen_model.to_string())
        .text("response_format", "json");

    let res = client
        .post("https://api.openai.com/v1/audio/transcriptions")
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| HarnessError::VoiceError(format!("OpenAI Whisper request failed: {e}")))?;

    if !res.status().is_success() {
        let status = res.status();
        let err_text = res.text().await.unwrap_or_default();
        return Err(HarnessError::VoiceError(format!(
            "OpenAI Whisper HTTP {status}: {}",
            err_text.chars().take(160).collect::<String>()
        )));
    }

    let val: serde_json::Value = res
        .json()
        .await
        .map_err(|e| HarnessError::VoiceError(format!("OpenAI Whisper JSON error: {e}")))?;

    let text = val["text"].as_str().unwrap_or_default().trim().to_string();
    Ok(text)
}
