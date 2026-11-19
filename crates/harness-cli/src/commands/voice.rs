use async_trait::async_trait;
use colored::Colorize;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures_util::stream::BoxStream;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use harness_core::{Agent, Config, DefaultToolRegistry, HarnessError, ToolRegistry};
use harness_memory::{
    build_memory_engine, ForgetMemoryTool, MemorySearchTool, SaveMemoryTool, UpdateMemoryTool,
};
use harness_notes::{
    DeleteNoteTool, FileNotesEngine, ListNotesTool, ReadNoteTool, SaveNoteTool, SearchNotesTool,
    UpdateNoteTool,
};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};
use harness_voice::{
    SpeechToText, TextToSpeech, VoicePipelineConfig, VoiceSession, WakeWordDetector,
};

use super::approval::{approval_settings, build_approver};

pub struct DefaultWakeWordDetector;

#[async_trait]
impl WakeWordDetector for DefaultWakeWordDetector {
    async fn wait_for_wake_word(&self, cancel: &CancellationToken) -> Result<(), HarnessError> {
        // Simulates background wake word listening
        tokio::select! {
            _ = cancel.cancelled() => Ok(()),
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(60)) => Ok(()),
        }
    }
}

/// The recogniser `luna voice` runs with, which is not a recogniser.
///
/// It answers every recording with the same sentence. The harness ships a voice loop and no speech
/// model, and a stub that invents words is worse than one that says it cannot: the loop announces
/// this at startup (see `run_voice_loop`) so nobody mistakes the demo for hearing them.
pub struct DefaultSpeechToText;

#[async_trait]
impl SpeechToText for DefaultSpeechToText {
    async fn transcribe(&self, _wav_bytes: &[u8]) -> Result<String, HarnessError> {
        Ok("Hello Luna, what are my persistent notes?".to_string())
    }
}

/// The synthesiser `luna voice` runs with, which produces silence: one block of zero samples per
/// chunk of text. It is here so the loop can be exercised end to end, not to be listened to.
pub struct DefaultTextToSpeech;

#[async_trait]
impl TextToSpeech for DefaultTextToSpeech {
    async fn synthesize_stream(
        &self,
        text_stream: BoxStream<'static, String>,
    ) -> Result<BoxStream<'static, Vec<f32>>, HarnessError> {
        let mapped = futures_util::StreamExt::map(text_stream, |_text| {
            // Generates synthetic audio sample chunk
            vec![0.0f32; 1024]
        });
        Ok(Box::pin(mapped))
    }
}

pub async fn run_voice_loop(
    config: Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
) -> Result<(), HarnessError> {
    let provider_id = config.model.provider.clone();

    print_voice_banner();

    let provider = build_voice_provider(&config, provider_id.clone(), cred_manager, discovery)?;

    let memory = build_memory_engine(&config)?;
    let notes = Arc::new(FileNotesEngine::new(
        config.paths.notes_dir.clone(),
        Some(config.paths.notes_db.clone()),
    )?);
    let registry = Arc::new(DefaultToolRegistry::new());

    register_voice_memory_tools(&registry, &config, &memory, &notes).await?;
    register_voice_core_tools(&registry, &config).await?;
    register_voice_skills(&registry, &config).await?;
    // The desktop tools, and the skill that documents them.
    crate::commands::desktop::tools::register(registry.as_ref()).await?;

    let agent = build_voice_agent(&config, provider, memory, notes, registry, &provider_id)?;

    let session = build_voice_session();

    let cancel = CancellationToken::new();
    session.run_hands_free_loop(agent, cancel).await
}

/// Print the hands-free header and the honest note about the stub recogniser.
fn print_voice_banner() {
    crate::commands::clear_screen();
    println!(
        "{}",
        "==================================================".cyan()
    );
    println!(
        "{}",
        "   Luna Hands-Free Voice Interface (\"Luna\")        "
            .bold()
            .cyan()
    );
    println!(
        "{}",
        "==================================================".cyan()
    );
    println!(
        "Wake word: {}\nListening on microphone...",
        "Luna".green().bold()
    );
    // Said before anything is listened to, because the alternative is a user talking to a program
    // that answers every recording with the same sentence and concluding it works.
    println!(
        "{}",
        "note: no speech model is wired into this build — every recording is answered with a fixed \
         sentence and the reply is not spoken. This is the loop's demo path, not recognition."
            .yellow()
    );
}

/// Build the provider client the voice agent talks through.
fn build_voice_provider(
    config: &Config,
    provider_id: String,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
) -> Result<Arc<GenericProviderClient>, HarnessError> {
    Ok(Arc::new(GenericProviderClient::new(
        provider_id.clone(),
        config.model.default.clone(),
        cred_manager,
        discovery,
        config.providers.get(&provider_id),
    )?))
}

/// Register memory and note tools, plus the note-backed memory search.
async fn register_voice_memory_tools(
    registry: &Arc<DefaultToolRegistry>,
    config: &Config,
    memory: &Arc<harness_memory::MemoryEngineKind>,
    notes: &Arc<FileNotesEngine>,
) -> Result<(), HarnessError> {
    if !config.memory.mode.is_background() {
        registry
            .register(Arc::new(SaveMemoryTool::new(memory.clone())))
            .await?;
        registry
            .register(Arc::new(UpdateMemoryTool::new(memory.clone())))
            .await?;
        registry
            .register(Arc::new(ForgetMemoryTool::new(memory.clone())))
            .await?;
        registry
            .register(Arc::new(SaveNoteTool::new(notes.clone())))
            .await?;
        registry
            .register(Arc::new(UpdateNoteTool::new(notes.clone())))
            .await?;
        registry
            .register(Arc::new(DeleteNoteTool::new(notes.clone())))
            .await?;
    }
    registry
        .register(Arc::new(SearchNotesTool::new(notes.clone())))
        .await?;
    registry
        .register(Arc::new(ListNotesTool::new(notes.clone())))
        .await?;
    registry
        .register(Arc::new(ReadNoteTool::new(notes.clone())))
        .await?;
    registry
        .register(Arc::new(
            MemorySearchTool::new(memory.clone()).with_notes(notes.clone()),
        ))
        .await?;
    Ok(())
}

/// Register core filesystem and shell tools (§2.2).
async fn register_voice_core_tools(
    registry: &Arc<DefaultToolRegistry>,
    config: &Config,
) -> Result<(), HarnessError> {
    let read_cache = harness_core::tools::ReadCache::new();
    let reg_dyn: Arc<dyn ToolRegistry> = registry.clone();
    // A hands-free run is its own conversation, so it gets its own list.
    let todo_scope = harness_core::tools::TodoScope::new(
        config.paths.todos_dir.clone(),
        format!("voice_{}", chrono::Utc::now().format("%Y%m%d_%H%M%S")),
    );
    harness_core::tools::register_core_tools(&reg_dyn, config, read_cache, todo_scope).await
}

/// Load the global, workspace and builtin skills and register their tools.
async fn register_voice_skills(
    registry: &Arc<DefaultToolRegistry>,
    config: &Config,
) -> Result<(), HarnessError> {
    let mut all_skills = Vec::new();
    let _ = std::fs::create_dir_all(&config.paths.skills_dir);
    if let Ok(skills) = harness_plugins::SkillLoader::load_skills_from_dir(&config.paths.skills_dir)
    {
        all_skills.extend(skills);
    }
    let workspace_skills = config.paths.workspace_root.join(".harness/skills");
    if workspace_skills.exists() {
        if let Ok(skills) = harness_plugins::SkillLoader::load_skills_from_dir(&workspace_skills) {
            all_skills.extend(skills);
        }
    }
    // The skills the harness ships with, including the desktop skill.
    all_skills.extend(crate::commands::desktop::builtin_skills());

    if !all_skills.is_empty() {
        let skills_arc = Arc::new(all_skills);
        registry
            .register(Arc::new(harness_plugins::SkillViewTool::new(
                skills_arc.clone(),
            )))
            .await?;
        registry
            .register(Arc::new(harness_plugins::SkillsListTool::new(skills_arc)))
            .await?;
    }
    Ok(())
}

/// Assemble the live agent with the voice configuration and approver.
fn build_voice_agent(
    config: &Config,
    provider: Arc<GenericProviderClient>,
    memory: Arc<harness_memory::MemoryEngineKind>,
    notes: Arc<FileNotesEngine>,
    registry: Arc<DefaultToolRegistry>,
    provider_id: &str,
) -> Result<
    Arc<
        Agent<
            GenericProviderClient,
            harness_memory::MemoryEngineKind,
            FileNotesEngine,
            DefaultToolRegistry,
        >,
    >,
    HarnessError,
> {
    let mut agent_cfg = config.agent.clone();
    agent_cfg.workspace_root = Some(config.paths.workspace_root.clone());
    agent_cfg.memory_mode = config.memory.mode;
    agent_cfg.memory_enabled = config.memory_effective_enabled();
    agent_cfg.notes_enabled = config.notes_effective_enabled();
    agent_cfg.skills_enabled = config.skills_effective_enabled();
    agent_cfg.mcp_enabled = config.mcp_effective_enabled();
    agent_cfg.context_window = Some(super::context_window::context_window_for(
        config,
        provider_id,
        &config.model.default,
    ) as usize);

    Ok(Arc::new(
        Agent::new(provider, memory, notes, registry, agent_cfg)
            .with_permissions(harness_core::PermissionEngine::validate(
                config.tools.permissions.clone(),
            )?)
            .with_approval(build_approver(config), approval_settings(config)),
    ))
}

/// The demo pipeline: stub wake word, fixed transcription, silent synthesis.
fn build_voice_session(
) -> VoiceSession<DefaultWakeWordDetector, DefaultSpeechToText, DefaultTextToSpeech> {
    VoiceSession::new(
        Arc::new(DefaultWakeWordDetector),
        Arc::new(DefaultSpeechToText),
        Arc::new(DefaultTextToSpeech),
        VoicePipelineConfig::default(),
    )
}

/// Run the continuous background voice listener.
///
/// Captures speech from the default microphone using CPAL and RMS energy VAD,
/// transcribes utterances, and sends them to `voice_tx` for the chat loop.
pub async fn run_background_voice_listener(
    config: Config,
    cred_manager: Arc<CredentialManager>,
    _discovery: Arc<ModelDiscovery>,
    voice_tx: tokio::sync::mpsc::UnboundedSender<String>,
    cancel: CancellationToken,
) {
    tracing::info!(
        "Starting background voice listener for wake word '{}' (provider: {})...",
        config.voice.wake_word,
        config.voice.provider
    );

    let (sample_rate_tx, sample_rate_rx) = tokio::sync::oneshot::channel::<u32>();
    let (audio_sample_tx, mut audio_sample_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();

    let cancel_thread = cancel.clone();
    std::thread::spawn(move || {
        let host = cpal::default_host();
        let Some(device) = host.default_input_device() else {
            tracing::debug!("No default audio input device found for voice listener");
            return;
        };

        let Ok(supported_config) = device.default_input_config() else {
            tracing::debug!("Failed to query default input config");
            return;
        };

        let sample_rate = supported_config.sample_rate().0;
        let _ = sample_rate_tx.send(sample_rate);
        let channels = supported_config.channels() as usize;

        let stream_result = match supported_config.sample_format() {
            cpal::SampleFormat::I16 => device.build_input_stream(
                &supported_config.into(),
                move |data: &[i16], _: &cpal::InputCallbackInfo| {
                    let mono: Vec<i16> = if channels > 1 {
                        data.chunks_exact(channels).map(|c| c[0]).collect()
                    } else {
                        data.to_vec()
                    };
                    let _ = audio_sample_tx.send(mono);
                },
                move |err| {
                    tracing::warn!("Audio input stream error: {}", err);
                },
                None,
            ),
            cpal::SampleFormat::F32 => device.build_input_stream(
                &supported_config.into(),
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    let mono: Vec<i16> = if channels > 1 {
                        data.chunks_exact(channels)
                            .map(|c| (c[0].clamp(-1.0, 1.0) * 32767.0) as i16)
                            .collect()
                    } else {
                        data.iter()
                            .map(|&s| (s.clamp(-1.0, 1.0) * 32767.0) as i16)
                            .collect()
                    };
                    let _ = audio_sample_tx.send(mono);
                },
                move |err| {
                    tracing::warn!("Audio input stream error: {}", err);
                },
                None,
            ),
            _ => {
                tracing::debug!("Unsupported sample format for audio input");
                return;
            }
        };

        let stream = match stream_result {
            Ok(s) => s,
            Err(err) => {
                tracing::debug!("Failed to build audio input stream: {}", err);
                return;
            }
        };

        if let Err(err) = stream.play() {
            tracing::debug!("Failed to start audio input stream: {}", err);
            return;
        }

        while !cancel_thread.is_cancelled() {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        drop(stream);
    });

    let sample_rate = match sample_rate_rx.await {
        Ok(sr) => sr,
        Err(_) => {
            let _ = cancel.cancelled().await;
            return;
        }
    };

    let silence_duration =
        std::time::Duration::from_millis(config.voice.silence_threshold_ms.max(300));
    let mut speech_buffer: Vec<i16> = Vec::new();
    let mut is_speaking = false;
    let mut last_speech_time = std::time::Instant::now();

    while !cancel.is_cancelled() {
        tokio::select! {
            _ = cancel.cancelled() => break,
            Some(samples) = audio_sample_rx.recv() => {
                let rms = harness_voice::AudioProcessor::calculate_rms_energy(&samples);
                let threshold = 350.0f32;

                if rms > threshold {
                    is_speaking = true;
                    last_speech_time = std::time::Instant::now();
                    speech_buffer.extend_from_slice(&samples);
                } else if is_speaking {
                    speech_buffer.extend_from_slice(&samples);
                    if last_speech_time.elapsed() > silence_duration {
                        is_speaking = false;
                        if speech_buffer.len() >= (sample_rate as usize) / 2 {
                            let wav_res = harness_voice::AudioProcessor::samples_to_wav(&speech_buffer, sample_rate);
                            speech_buffer.clear();
                            if let Ok(wav_bytes) = wav_res {
                                if let Ok(transcript) = transcribe_audio_bytes(&wav_bytes, &config, &cred_manager).await {
                                    let trimmed = transcript.trim();
                                    if !trimmed.is_empty() {
                                        let _ = voice_tx.send(trimmed.to_string());
                                    }
                                }
                            }
                        } else {
                            speech_buffer.clear();
                        }
                    }
                }
            }
        }
    }
}

async fn resolve_voice_api_key(
    provider: &str,
    cred_manager: &Arc<CredentialManager>,
) -> Option<String> {
    let p_lower = provider.to_lowercase();
    let creds = cred_manager.get_credentials(&p_lower).await;
    if let Some(c) = creds
        .iter()
        .find(|c| c.is_priority)
        .or_else(|| creds.first())
    {
        return Some(c.api_key.clone());
    }
    match p_lower.as_str() {
        "openai" => std::env::var("OPENAI_API_KEY").ok(),
        "groq" => std::env::var("GROQ_API_KEY").ok(),
        "antigravity" | "google" => std::env::var("GEMINI_API_KEY")
            .ok()
            .or_else(|| std::env::var("GOOGLE_API_KEY").ok()),
        _ => None,
    }
}

/// Transcribe recorded audio bytes into text.
async fn transcribe_audio_bytes(
    wav_bytes: &[u8],
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
) -> Result<String, HarnessError> {
    let provider = config.voice.provider.to_ascii_lowercase();
    let api_key = resolve_voice_api_key(&provider, cred_manager).await;

    if provider == "groq" {
        if let Some(key) = api_key {
            return transcribe_groq_whisper(wav_bytes, &key, &config.voice.stt_model).await;
        }
    } else if provider == "openai" {
        if let Some(key) = api_key {
            return transcribe_openai_whisper(wav_bytes, &key, &config.voice.stt_model).await;
        }
    }

    Ok(String::new())
}

async fn transcribe_openai_whisper(
    wav_bytes: &[u8],
    api_key: &str,
    model: &str,
) -> Result<String, HarnessError> {
    let client = reqwest::Client::new();
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
        return Ok(String::new());
    }

    let val: serde_json::Value = res.json().await.unwrap_or_default();
    Ok(val["text"].as_str().unwrap_or("").to_string())
}

async fn transcribe_groq_whisper(
    wav_bytes: &[u8],
    api_key: &str,
    model: &str,
) -> Result<String, HarnessError> {
    let client = reqwest::Client::new();
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
        return Ok(String::new());
    }

    let val: serde_json::Value = res.json().await.unwrap_or_default();
    Ok(val["text"].as_str().unwrap_or("").to_string())
}

/// Synthesize and play text-to-speech audio for a turn reply.
pub async fn play_tts_response(
    text: &str,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    cancel: CancellationToken,
) {
    if cancel.is_cancelled() {
        return;
    }

    let cleaned = clean_text_for_speech(text);
    if cleaned.trim().is_empty() {
        return;
    }

    let provider = config.voice.provider.to_ascii_lowercase();
    let api_key = resolve_voice_api_key(&provider, cred_manager).await;

    let audio_bytes = if provider == "openai" {
        if let Some(key) = api_key {
            generate_openai_tts(
                &cleaned,
                &key,
                &config.voice.voice_name,
                &config.voice.tts_model,
            )
            .await
            .ok()
        } else {
            None
        }
    } else if provider == "antigravity" || provider == "google" {
        if let Some(key) = api_key {
            generate_gemini_tts(&cleaned, &key, &config.voice.voice_name)
                .await
                .ok()
        } else {
            None
        }
    } else {
        None
    };

    if let Some(bytes) = audio_bytes {
        if cancel.is_cancelled() {
            return;
        }
        play_audio_stream(&bytes, cancel).await;
    }
}

async fn generate_openai_tts(
    text: &str,
    api_key: &str,
    voice: &str,
    model: &str,
) -> Result<Vec<u8>, HarnessError> {
    let client = reqwest::Client::new();
    let res = client
        .post("https://api.openai.com/v1/audio/speech")
        .bearer_auth(api_key)
        .json(&serde_json::json!({
            "model": if model.is_empty() { "tts-1" } else { model },
            "input": text,
            "voice": if voice.is_empty() { "alloy" } else { voice },
            "response_format": "mp3"
        }))
        .send()
        .await
        .map_err(|e| HarnessError::VoiceError(format!("OpenAI TTS request failed: {e}")))?;

    if !res.status().is_success() {
        let err_text = res.text().await.unwrap_or_default();
        return Err(HarnessError::VoiceError(format!(
            "OpenAI TTS error: {err_text}"
        )));
    }

    let bytes = res
        .bytes()
        .await
        .map_err(|e| HarnessError::VoiceError(format!("Failed to read OpenAI TTS bytes: {e}")))?;
    Ok(bytes.to_vec())
}

async fn generate_gemini_tts(
    text: &str,
    api_key: &str,
    voice: &str,
) -> Result<Vec<u8>, HarnessError> {
    let client = reqwest::Client::new();
    let url = format!(
        "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.0-flash:generateContent?key={}",
        api_key
    );
    let voice_name = if voice.is_empty() { "Puck" } else { voice };
    let payload = serde_json::json!({
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
    });

    let res = client
        .post(&url)
        .json(&payload)
        .send()
        .await
        .map_err(|e| HarnessError::VoiceError(format!("Gemini TTS request failed: {e}")))?;

    if !res.status().is_success() {
        let err_text = res.text().await.unwrap_or_default();
        return Err(HarnessError::VoiceError(format!(
            "Gemini TTS error: {err_text}"
        )));
    }

    let val: serde_json::Value = res
        .json()
        .await
        .map_err(|e| HarnessError::VoiceError(format!("Failed to parse Gemini TTS json: {e}")))?;

    let pcm_b64 = val["candidates"][0]["content"]["parts"][0]["inlineData"]["data"]
        .as_str()
        .ok_or_else(|| HarnessError::VoiceError("No inline audio data in Gemini reply".into()))?;

    use base64::Engine;
    let pcm_bytes = base64::engine::general_purpose::STANDARD
        .decode(pcm_b64.trim())
        .map_err(|e| HarnessError::VoiceError(format!("Failed to decode base64 audio: {e}")))?;

    #[allow(clippy::chunks_exact_to_as_chunks)]
    let samples: Vec<i16> = pcm_bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();

    harness_voice::AudioProcessor::samples_to_wav(&samples, 24000)
}

async fn play_audio_stream(audio_bytes: &[u8], cancel: CancellationToken) {
    let bytes_vec = audio_bytes.to_vec();
    tokio::task::spawn_blocking(move || {
        let (_stream, stream_handle) = match rodio::OutputStream::try_default() {
            Ok(pair) => pair,
            Err(e) => {
                tracing::debug!("Failed to open audio output stream: {}", e);
                return;
            }
        };

        let sink = match rodio::Sink::try_new(&stream_handle) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("Failed to create audio sink: {}", e);
                return;
            }
        };

        let cursor = std::io::Cursor::new(bytes_vec);
        if let Ok(source) = rodio::Decoder::new(cursor) {
            sink.append(source);
            while !sink.empty() {
                if cancel.is_cancelled() {
                    sink.stop();
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    })
    .await
    .ok();
}

/// Clean conversational response text for speech synthesis.
pub fn clean_text_for_speech(text: &str) -> String {
    let mut cleaned = String::new();
    let mut in_code_block = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }
        if in_code_block {
            continue;
        }
        if trimmed.starts_with('|') || trimmed.starts_with("---") {
            continue;
        }
        let stripped = trimmed.trim_start_matches(['#', '*', '-', '•']).trim();
        if !stripped.is_empty() {
            cleaned.push_str(stripped);
            cleaned.push(' ');
        }
    }

    if cleaned.len() > 800 {
        if let Some(pos) = cleaned[..800].rfind(['.', '!', '?']) {
            cleaned.truncate(pos + 1);
        } else {
            cleaned.truncate(800);
        }
    }

    cleaned.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_text_for_speech_strips_code_blocks_and_markdown() {
        let input = "Here is the result:\n```rust\nfn main() {}\n```\n| Col1 | Col2 |\n|---|---|\n- Fast and responsive.\n**Done!**";
        let out = clean_text_for_speech(input);
        assert!(!out.contains("fn main"));
        assert!(!out.contains("Col1"));
        assert!(out.contains("Here is the result:"));
        assert!(out.contains("Fast and responsive."));
        assert!(out.contains("Done!"));
    }
}
