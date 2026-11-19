//! Speech-to-text transcription engine (Groq, Antigravity, Gemini, OpenAI Whisper).

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
    for prov in ["google", "antigravity", "agy"] {
        for k in lookup_plain_api_keys(prov, cred_manager).await {
            if !keys.contains(&k) {
                keys.push(k);
            }
        }
    }
    for env_name in [
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "ANTIGRAVITY_API_KEY",
        "AGY_API_KEY",
    ] {
        if let Ok(k) = std::env::var(env_name) {
            let trimmed = k.trim();
            if !trimmed.is_empty() && !keys.iter().any(|x| x == trimmed) {
                keys.push(trimmed.to_string());
            }
        }
    }
    keys
}

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

/// Transcribe recorded audio bytes into text with ultra-low-latency routing + Antigravity/Gemini/Groq fallback.
pub async fn transcribe_audio_bytes(
    wav_bytes: &[u8],
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
) -> Result<String, HarnessError> {
    let provider = config.voice.provider.to_ascii_lowercase();
    let stt_model = config.voice.stt_model.trim();
    let mut errors: Vec<String> = Vec::new();

    match provider.as_str() {
        "groq" => {
            if let Some(key) = resolve_groq_api_key(cred_manager).await {
                match transcribe_groq_whisper(wav_bytes, &key, stt_model).await {
                    Ok(t) => return Ok(t),
                    Err(e) => errors.push(format!("groq: {e}")),
                }
            } else {
                errors.push("groq: no API key configured".to_string());
            }
            if let Ok(t) = transcribe_antigravity_audio(wav_bytes, cred_manager, "").await {
                return Ok(t);
            }
            if let Some(key) = resolve_gemini_api_key(cred_manager).await {
                let _ = key;
                let g_keys = resolve_gemini_api_keys(cred_manager).await;
                if let Ok(t) = transcribe_gemini_audio_with_keys(wav_bytes, &g_keys, "").await {
                    return Ok(t);
                }
            }
        }
        "openai" => {
            if let Some(key) = resolve_openai_api_key(cred_manager).await {
                match transcribe_openai_whisper(wav_bytes, &key, stt_model).await {
                    Ok(t) => return Ok(t),
                    Err(e) => errors.push(format!("openai: {e}")),
                }
            } else {
                errors.push("openai: no API key configured".to_string());
            }
            if let Some(key) = resolve_groq_api_key(cred_manager).await {
                if let Ok(t) = transcribe_groq_whisper(wav_bytes, &key, "").await {
                    return Ok(t);
                }
            }
        }
        _ => {
            if stt_model.is_empty() {
                if let Some(key) = resolve_groq_api_key(cred_manager).await {
                    match transcribe_groq_whisper(wav_bytes, &key, "").await {
                        Ok(t) => return Ok(t),
                        Err(e) => errors.push(format!("groq: {e}")),
                    }
                }
            }

            match transcribe_antigravity_audio(wav_bytes, cred_manager, stt_model).await {
                Ok(t) => return Ok(t),
                Err(e) => errors.push(format!("antigravity: {e}")),
            }

            let g_keys = resolve_gemini_api_keys(cred_manager).await;
            if !g_keys.is_empty() {
                match transcribe_gemini_audio_with_keys(wav_bytes, &g_keys, stt_model).await {
                    Ok(t) => return Ok(t),
                    Err(e) => errors.push(format!("gemini: {e}")),
                }
            } else {
                errors.push("gemini: no Gemini API key configured".to_string());
            }

            if !stt_model.is_empty() {
                if let Some(key) = resolve_groq_api_key(cred_manager).await {
                    match transcribe_groq_whisper(wav_bytes, &key, "").await {
                        Ok(t) => return Ok(t),
                        Err(e) => errors.push(format!("groq fallback: {e}")),
                    }
                }
            }
            if let Some(key) = resolve_openai_api_key(cred_manager).await {
                match transcribe_openai_whisper(wav_bytes, &key, "").await {
                    Ok(t) => return Ok(t),
                    Err(e) => errors.push(format!("openai fallback: {e}")),
                }
            }
        }
    }

    if errors.is_empty() {
        Err(HarnessError::VoiceError(
            "No STT credentials available (configure groq, antigravity, gemini, or openai key)"
                .into(),
        ))
    } else {
        Err(HarnessError::VoiceError(errors.join("; ")))
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

    let (bearer_token, project_opt) =
        harness_providers::antigravity::extract_bearer_token(&raw_secret);
    let project =
        project_opt.unwrap_or_else(|| harness_providers::antigravity::DEFAULT_PROJECT.to_string());
    let client = voice_http_client();

    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(wav_bytes);

    let candidates: Vec<&str> = if !model.is_empty() {
        vec![model]
    } else {
        vec![
            "gemini-3.1-flash-lite",
            "gemini-3-flash-preview",
            "gemini-3.8-flash",
        ]
    };

    let payload = serde_json::json!({
        "project": project,
        "model": candidates[0],
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

    let res = client
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
        vec![
            "gemini-3.1-flash-lite",
            "gemini-3-flash-preview",
            "gemini-3.8-flash",
        ]
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
            match client.post(&url).json(&payload).send().await {
                Ok(res) if res.status().is_success() => {
                    let val: serde_json::Value = res.json().await.unwrap_or_default();
                    let txt = val["candidates"][0]["content"]["parts"][0]["text"]
                        .as_str()
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    return Ok(txt);
                }
                Ok(res) => {
                    let status = res.status();
                    let body = res.text().await.unwrap_or_default();
                    last_err = format!(
                        "{m} HTTP {status}: {}",
                        body.chars().take(160).collect::<String>()
                    );
                    if status.as_u16() == 429 || status.as_u16() == 403 {
                        continue;
                    }
                    break;
                }
                Err(e) => {
                    last_err = format!("{m}: {e}");
                    break;
                }
            }
        }
    }

    Err(HarnessError::VoiceError(last_err))
}

pub async fn transcribe_openai_whisper(
    wav_bytes: &[u8],
    api_key: &str,
    model: &str,
) -> Result<String, HarnessError> {
    let client = voice_http_client();
    let part = reqwest::multipart::Part::bytes(wav_bytes.to_vec())
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| HarnessError::VoiceError(format!("Failed to create multipart: {e}")))?;

    let form = reqwest::multipart::Form::new().part("file", part).text(
        "model",
        if model.is_empty() {
            "whisper-1".to_string()
        } else {
            model.to_string()
        },
    );

    let res = client
        .post("https://api.openai.com/v1/audio/transcriptions")
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| HarnessError::VoiceError(format!("Whisper request failed: {e}")))?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(HarnessError::VoiceError(format!(
            "OpenAI Whisper HTTP {status}: {}",
            body.chars().take(160).collect::<String>()
        )));
    }

    let val: serde_json::Value = res.json().await.unwrap_or_default();
    Ok(val["text"].as_str().unwrap_or("").to_string())
}

pub async fn transcribe_groq_whisper(
    wav_bytes: &[u8],
    api_key: &str,
    model: &str,
) -> Result<String, HarnessError> {
    let client = voice_http_client();
    let part = reqwest::multipart::Part::bytes(wav_bytes.to_vec())
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| HarnessError::VoiceError(format!("Failed to create multipart: {e}")))?;

    let form = reqwest::multipart::Form::new().part("file", part).text(
        "model",
        if model.is_empty() {
            "whisper-large-v3-turbo".to_string()
        } else {
            model.to_string()
        },
    );

    let res = client
        .post("https://api.groq.com/openai/v1/audio/transcriptions")
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|e| HarnessError::VoiceError(format!("Groq Whisper request failed: {e}")))?;

    if !res.status().is_success() {
        let status = res.status();
        let body = res.text().await.unwrap_or_default();
        return Err(HarnessError::VoiceError(format!(
            "Groq Whisper HTTP {status}: {}",
            body.chars().take(160).collect::<String>()
        )));
    }

    let val: serde_json::Value = res.json().await.unwrap_or_default();
    Ok(val["text"].as_str().unwrap_or("").to_string())
}
