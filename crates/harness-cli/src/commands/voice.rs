use colored::Colorize;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
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

use super::approval::{approval_settings, build_approver};

/// Events emitted by background voice capture, transcription, and playback tasks.
#[derive(Debug, Clone)]
pub enum VoiceEvent {
    /// A non-empty user speech transcript ready to dispatch as a turn.
    Transcript(String),
    /// Informational status notice (e.g. microphone ready).
    Notice(String),
    /// Actionable voice error surfaced in the UI instead of silently swallowed.
    Error(String),
}

pub async fn run_voice_loop(
    config: Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
) -> Result<(), HarnessError> {
    let provider_id = config.model.provider.clone();

    print_voice_banner(&config);

    let provider = build_voice_provider(
        &config,
        provider_id.clone(),
        cred_manager.clone(),
        discovery.clone(),
    )?;

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

    let cancel = CancellationToken::new();
    let is_playing = Arc::new(AtomicBool::new(false));
    let (voice_tx, mut voice_rx) = tokio::sync::mpsc::unbounded_channel::<VoiceEvent>();

    let listener_cfg = config.clone();
    let listener_creds = cred_manager.clone();
    let listener_disc = discovery.clone();
    let listener_playing = is_playing.clone();
    let listener_cancel = cancel.clone();
    let listener_tx = voice_tx.clone();
    tokio::spawn(async move {
        run_background_voice_listener(
            listener_cfg,
            listener_creds,
            listener_disc,
            listener_tx,
            listener_playing,
            listener_cancel,
        )
        .await;
    });

    let mut playback_cancel: Option<CancellationToken> = None;

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\n{}", "Stopping voice session...".dimmed());
                cancel.cancel();
                if let Some(pb) = playback_cancel.take() {
                    pb.cancel();
                }
                break;
            }
            maybe_evt = voice_rx.recv() => {
                let Some(evt) = maybe_evt else {
                    break;
                };
                match evt {
                    VoiceEvent::Notice(msg) => {
                        println!("{} {}", "•".cyan(), msg.dimmed());
                    }
                    VoiceEvent::Error(err) => {
                        eprintln!("{} {}", "✖".red().bold(), err);
                    }
                    VoiceEvent::Transcript(query) => {
                        if let Some(pb) = playback_cancel.take() {
                            pb.cancel();
                        }
                        println!("\n{} {}", "🎤 You:".bold().yellow(), query);
                        let voice_prompt = format!(
                            "[VOICE MODE ACTIVE: Output a concise, voice-compatible conversational spoken summary suitable for text-to-speech. Avoid raw Markdown tables, ASCII boxes, or unpronounceable formatting.]\n\n{query}"
                        );
                        let turn_cancel = cancel.child_token();
                        match agent.run_task(&voice_prompt, turn_cancel).await {
                            Ok(reply) => {
                                println!("\n{} {}\n", "Luna:".bold().green(), reply);
                                if !reply.trim().is_empty() {
                                    let pb = CancellationToken::new();
                                    playback_cancel = Some(pb.clone());
                                    let cfg_clone = config.clone();
                                    let creds_clone = cred_manager.clone();
                                    let tx_clone = voice_tx.clone();
                                    let playing_clone = is_playing.clone();
                                    tokio::spawn(async move {
                                        play_tts_response(
                                            &reply,
                                            &cfg_clone,
                                            &creds_clone,
                                            Some(tx_clone),
                                            playing_clone,
                                            pb,
                                        )
                                        .await;
                                    });
                                }
                            }
                            Err(e) => {
                                eprintln!("{} Agent error: {e}", "✖".red().bold());
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// Print the hands-free header.
fn print_voice_banner(config: &Config) {
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
        "Provider: {} | Voice: {} | Press Ctrl+C to exit.\nListening on microphone...",
        config.voice.provider.green().bold(),
        config.voice.voice_name.cyan(),
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

/// Run the continuous background voice listener.
///
/// Captures speech from the default microphone using CPAL and adaptive RMS energy VAD,
/// transcribes utterances, and sends `VoiceEvent` messages to `voice_tx` for the chat loop.
pub async fn run_background_voice_listener(
    config: Config,
    cred_manager: Arc<CredentialManager>,
    _discovery: Arc<ModelDiscovery>,
    voice_tx: tokio::sync::mpsc::UnboundedSender<VoiceEvent>,
    is_playing_tts: Arc<AtomicBool>,
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
    let err_tx = voice_tx.clone();
    std::thread::spawn(move || {
        let host = cpal::default_host();
        let Some(device) = host.default_input_device() else {
            tracing::warn!("No default audio input device found for voice listener");
            let _ = err_tx.send(VoiceEvent::Error(
                "No default microphone input device found.".to_string(),
            ));
            return;
        };

        let dev_name = device
            .name()
            .unwrap_or_else(|_| "default microphone".to_string());
        let supported_config = match device.default_input_config() {
            Ok(cfg) => cfg,
            Err(err) => {
                tracing::warn!("Failed to query default input config for {dev_name}: {err}");
                let _ = err_tx.send(VoiceEvent::Error(format!(
                    "Microphone '{dev_name}' configuration failed: {err}"
                )));
                return;
            }
        };

        let sample_rate = supported_config.sample_rate().0;
        let channels = supported_config.channels() as usize;
        let sample_format = supported_config.sample_format();
        let stream_config: cpal::StreamConfig = supported_config.into();

        let stream_result = match sample_format {
            cpal::SampleFormat::I16 => device.build_input_stream(
                &stream_config,
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
                &stream_config,
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
            cpal::SampleFormat::I32 => device.build_input_stream(
                &stream_config,
                move |data: &[i32], _: &cpal::InputCallbackInfo| {
                    let mono: Vec<i16> = if channels > 1 {
                        data.chunks_exact(channels)
                            .map(|c| (c[0] >> 16) as i16)
                            .collect()
                    } else {
                        data.iter().map(|&s| (s >> 16) as i16).collect()
                    };
                    let _ = audio_sample_tx.send(mono);
                },
                move |err| {
                    tracing::warn!("Audio input stream error: {}", err);
                },
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                &stream_config,
                move |data: &[u16], _: &cpal::InputCallbackInfo| {
                    let mono: Vec<i16> = if channels > 1 {
                        data.chunks_exact(channels)
                            .map(|c| (c[0] as i32 - 32768) as i16)
                            .collect()
                    } else {
                        data.iter().map(|&s| (s as i32 - 32768) as i16).collect()
                    };
                    let _ = audio_sample_tx.send(mono);
                },
                move |err| {
                    tracing::warn!("Audio input stream error: {}", err);
                },
                None,
            ),
            other => {
                tracing::warn!("Unsupported sample format for audio input: {other:?}");
                let _ = err_tx.send(VoiceEvent::Error(format!(
                    "Unsupported microphone sample format: {other:?}"
                )));
                return;
            }
        };

        let stream = match stream_result {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!("Failed to build audio input stream: {}", err);
                let _ = err_tx.send(VoiceEvent::Error(format!(
                    "Failed to open microphone '{dev_name}': {err}"
                )));
                return;
            }
        };

        if let Err(err) = stream.play() {
            tracing::warn!("Failed to start audio input stream: {}", err);
            let _ = err_tx.send(VoiceEvent::Error(format!(
                "Failed to start microphone '{dev_name}': {err}"
            )));
            return;
        }

        let _ = sample_rate_tx.send(sample_rate);
        let _ = err_tx.send(VoiceEvent::Notice(format!(
            "🎙 Microphone active ({dev_name} · {sample_rate}Hz)"
        )));

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
    let min_samples = (sample_rate as usize) / 3; // ~333ms minimum utterance
    let max_samples = (sample_rate as usize) * 30; // 30s safety cap
    let pre_roll_max_samples = (sample_rate as usize) / 3; // ~333ms pre-roll

    let mut pre_roll: VecDeque<Vec<i16>> = VecDeque::new();
    let mut pre_roll_len: usize = 0;
    let mut speech_buffer: Vec<i16> = Vec::new();
    let mut is_speaking = false;
    let mut noise_floor: f32 = 60.0;
    let mut last_speech_time = std::time::Instant::now();
    let mut was_playing_tts = false;
    let mut tts_cooldown_until: Option<std::time::Instant> = None;
    let mut last_stt_error_at: Option<std::time::Instant> = None;

    while !cancel.is_cancelled() {
        tokio::select! {
            _ = cancel.cancelled() => break,
            maybe_samples = audio_sample_rx.recv() => {
                let Some(samples) = maybe_samples else {
                    break;
                };
                if samples.is_empty() {
                    continue;
                }

                let playing = is_playing_tts.load(Ordering::SeqCst);
                if playing {
                    was_playing_tts = true;
                    if !config.voice.enable_barge_in {
                        is_speaking = false;
                        speech_buffer.clear();
                        pre_roll.clear();
                        pre_roll_len = 0;
                        continue;
                    }
                } else if was_playing_tts {
                    was_playing_tts = false;
                    // Brief 250ms echo cooldown right after TTS playback finishes
                    tts_cooldown_until = Some(std::time::Instant::now() + std::time::Duration::from_millis(250));
                    is_speaking = false;
                    speech_buffer.clear();
                    pre_roll.clear();
                    pre_roll_len = 0;
                    continue;
                }

                if let Some(until) = tts_cooldown_until {
                    if std::time::Instant::now() < until {
                        continue;
                    }
                    tts_cooldown_until = None;
                }

                let rms = harness_voice::AudioProcessor::calculate_rms_energy(&samples);
                let base_threshold = (noise_floor * 2.8).clamp(150.0, 550.0);
                let threshold = if playing {
                    // Require noticeably louder voice to barge in over speaker audio
                    (base_threshold * 2.5).max(900.0)
                } else {
                    base_threshold
                };

                if rms > threshold {
                    if !is_speaking {
                        is_speaking = true;
                        for chunk in pre_roll.drain(..) {
                            speech_buffer.extend_from_slice(&chunk);
                        }
                        pre_roll_len = 0;
                    }
                    last_speech_time = std::time::Instant::now();
                    speech_buffer.extend_from_slice(&samples);
                } else if is_speaking {
                    speech_buffer.extend_from_slice(&samples);
                    let timed_out = speech_buffer.len() >= max_samples;
                    if last_speech_time.elapsed() > silence_duration || timed_out {
                        is_speaking = false;
                        if speech_buffer.len() >= min_samples {
                            let wav_res = harness_voice::AudioProcessor::samples_to_wav(&speech_buffer, sample_rate);
                            speech_buffer.clear();
                            if let Ok(wav_bytes) = wav_res {
                                match transcribe_audio_bytes(&wav_bytes, &config, &cred_manager).await {
                                    Ok(transcript) => {
                                        let trimmed = transcript.trim();
                                        if !is_blank_transcript(trimmed) {
                                            let _ = voice_tx.send(VoiceEvent::Transcript(trimmed.to_string()));
                                        }
                                    }
                                    Err(e) => {
                                        tracing::warn!("Voice STT transcription error: {e}");
                                        let should_notify = last_stt_error_at
                                            .map(|t| t.elapsed() > std::time::Duration::from_secs(15))
                                            .unwrap_or(true);
                                        if should_notify {
                                            last_stt_error_at = Some(std::time::Instant::now());
                                            let _ = voice_tx.send(VoiceEvent::Error(format!("Voice STT failed: {e}")));
                                        }
                                    }
                                }
                            }
                        } else {
                            speech_buffer.clear();
                        }
                    }
                } else {
                    // Slowly adapt ambient noise floor while idle
                    if rms < 400.0 {
                        noise_floor = noise_floor * 0.98 + rms * 0.02;
                    }
                    pre_roll_len += samples.len();
                    pre_roll.push_back(samples);
                    while pre_roll_len > pre_roll_max_samples {
                        if let Some(front) = pre_roll.pop_front() {
                            pre_roll_len = pre_roll_len.saturating_sub(front.len());
                        } else {
                            break;
                        }
                    }
                }
            }
        }
    }
}

fn is_blank_transcript(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    lower.is_empty()
        || lower == "[blank_audio]"
        || lower == "(silence)"
        || lower == "[silence]"
        || lower == "."
}

async fn lookup_plain_api_key(provider: &str, cred_manager: &CredentialManager) -> Option<String> {
    let creds = cred_manager.get_credentials(provider).await;
    creds
        .iter()
        .filter(|c| {
            let trimmed = c.api_key.trim();
            !trimmed.is_empty() && !trimmed.starts_with('{')
        })
        .find(|c| c.is_priority)
        .or_else(|| {
            creds.iter().find(|c| {
                let trimmed = c.api_key.trim();
                !trimmed.is_empty() && !trimmed.starts_with('{')
            })
        })
        .map(|c| c.api_key.clone())
}

async fn resolve_gemini_api_key(cred_manager: &Arc<CredentialManager>) -> Option<String> {
    if let Some(k) = lookup_plain_api_key("gemini", cred_manager).await {
        return Some(k);
    }
    if let Some(k) = lookup_plain_api_key("google", cred_manager).await {
        return Some(k);
    }
    std::env::var("GEMINI_API_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("GOOGLE_API_KEY")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
}

async fn resolve_groq_api_key(cred_manager: &Arc<CredentialManager>) -> Option<String> {
    if let Some(k) = lookup_plain_api_key("groq", cred_manager).await {
        return Some(k);
    }
    std::env::var("GROQ_API_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

async fn resolve_openai_api_key(cred_manager: &Arc<CredentialManager>) -> Option<String> {
    if let Some(k) = lookup_plain_api_key("openai", cred_manager).await {
        return Some(k);
    }
    std::env::var("OPENAI_API_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty())
}

/// Transcribe recorded audio bytes into text with provider routing + cross-provider fallback.
async fn transcribe_audio_bytes(
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
            if let Some(key) = resolve_gemini_api_key(cred_manager).await {
                if let Ok(t) = transcribe_gemini_audio(wav_bytes, &key, "").await {
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
            // "antigravity" | "google" | "gemini" | fallback:
            // Prefer Gemini multimodal audio transcription, with Groq/OpenAI Whisper fallback.
            if let Some(key) = resolve_gemini_api_key(cred_manager).await {
                match transcribe_gemini_audio(wav_bytes, &key, stt_model).await {
                    Ok(t) => return Ok(t),
                    Err(e) => errors.push(format!("gemini: {e}")),
                }
            } else {
                errors.push("gemini: no Gemini API key configured".to_string());
            }

            if let Some(key) = resolve_groq_api_key(cred_manager).await {
                match transcribe_groq_whisper(wav_bytes, &key, "").await {
                    Ok(t) => return Ok(t),
                    Err(e) => errors.push(format!("groq fallback: {e}")),
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
            "No STT credentials available (configure gemini, groq, or openai key)".into(),
        ))
    } else {
        Err(HarnessError::VoiceError(errors.join("; ")))
    }
}

async fn transcribe_gemini_audio(
    wav_bytes: &[u8],
    api_key: &str,
    model: &str,
) -> Result<String, HarnessError> {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(wav_bytes);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());

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
            }
            Err(e) => {
                last_err = format!("{m}: {e}");
            }
        }
    }

    Err(HarnessError::VoiceError(last_err))
}

async fn transcribe_openai_whisper(
    wav_bytes: &[u8],
    api_key: &str,
    model: &str,
) -> Result<String, HarnessError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
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

async fn transcribe_groq_whisper(
    wav_bytes: &[u8],
    api_key: &str,
    model: &str,
) -> Result<String, HarnessError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
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

/// Synthesize and play text-to-speech audio for a turn reply.
pub async fn play_tts_response(
    text: &str,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    voice_tx: Option<tokio::sync::mpsc::UnboundedSender<VoiceEvent>>,
    is_playing_tts: Arc<AtomicBool>,
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
    let tts_model = config.voice.tts_model.trim();
    let voice_name = config.voice.voice_name.trim();
    let mut errors: Vec<String> = Vec::new();
    let mut audio_bytes: Option<Vec<u8>> = None;

    match provider.as_str() {
        "openai" => {
            if let Some(key) = resolve_openai_api_key(cred_manager).await {
                match generate_openai_tts(&cleaned, &key, voice_name, tts_model).await {
                    Ok(bytes) => audio_bytes = Some(bytes),
                    Err(e) => {
                        tracing::warn!("OpenAI TTS failed, trying Edge TTS fallback: {e}");
                        errors.push(e.to_string());
                    }
                }
            }
            if audio_bytes.is_none() {
                match generate_edge_tts(&cleaned, voice_name).await {
                    Ok(bytes) => audio_bytes = Some(bytes),
                    Err(e) => errors.push(e.to_string()),
                }
            }
        }
        "antigravity" | "google" | "gemini" => {
            if let Some(key) = resolve_gemini_api_key(cred_manager).await {
                match generate_gemini_tts(&cleaned, &key, voice_name, tts_model).await {
                    Ok(bytes) => audio_bytes = Some(bytes),
                    Err(e) => {
                        tracing::warn!("Gemini TTS failed, trying Edge TTS fallback: {e}");
                        errors.push(e.to_string());
                    }
                }
            } else {
                errors.push("No Gemini API key configured for Gemini TTS".to_string());
            }
            if audio_bytes.is_none() {
                match generate_edge_tts(&cleaned, voice_name).await {
                    Ok(bytes) => audio_bytes = Some(bytes),
                    Err(e) => errors.push(e.to_string()),
                }
            }
        }
        _ => {
            // "groq" | "edge" | "local": Edge TTS first, then Gemini/OpenAI fallback.
            match generate_edge_tts(&cleaned, voice_name).await {
                Ok(bytes) => audio_bytes = Some(bytes),
                Err(e) => {
                    tracing::warn!("Edge TTS failed: {e}");
                    errors.push(e.to_string());
                    if let Some(key) = resolve_gemini_api_key(cred_manager).await {
                        if let Ok(bytes) =
                            generate_gemini_tts(&cleaned, &key, voice_name, tts_model).await
                        {
                            audio_bytes = Some(bytes);
                        }
                    }
                }
            }
        }
    }

    if cancel.is_cancelled() {
        return;
    }

    let Some(bytes) = audio_bytes else {
        let msg = format!("Voice TTS unavailable: {}", errors.join("; "));
        tracing::warn!("{msg}");
        if let Some(ref tx) = voice_tx {
            let _ = tx.send(VoiceEvent::Error(msg));
        }
        return;
    };

    if let Err(err) = play_audio_stream(&bytes, is_playing_tts, cancel).await {
        tracing::warn!("Voice audio playback failed: {err}");
        if let Some(ref tx) = voice_tx {
            let _ = tx.send(VoiceEvent::Error(format!("Audio playback failed: {err}")));
        }
    }
}

async fn generate_openai_tts(
    text: &str,
    api_key: &str,
    voice: &str,
    model: &str,
) -> Result<Vec<u8>, HarnessError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(25))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let openai_voice = match voice {
        "alloy" | "ash" | "ballad" | "coral" | "echo" | "fable" | "nova" | "onyx" | "sage"
        | "shimmer" | "verse" => voice,
        _ => "alloy",
    };
    let res = client
        .post("https://api.openai.com/v1/audio/speech")
        .bearer_auth(api_key)
        .json(&serde_json::json!({
            "model": if model.is_empty() { "tts-1" } else { model },
            "input": text,
            "voice": openai_voice,
            "response_format": "mp3"
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
    model: &str,
) -> Result<Vec<u8>, HarnessError> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(25))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let voice_name = if voice.is_empty() || voice.contains("Neural") {
        "Aoede"
    } else {
        voice
    };
    // Gemini TTS models reject unframed questions/statements with HTTP 400
    // ("Model tried to generate text, but it should only be used for TTS").
    let framed_prompt = format!(
        "Read aloud the following text verbatim without adding or changing anything:\n\n{text}"
    );
    let payload = serde_json::json!({
        "contents": [{
            "parts": [{ "text": framed_prompt }]
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

    let candidates: Vec<&str> = if !model.is_empty() {
        vec![model]
    } else {
        vec!["gemini-2.5-flash-preview-tts", "gemini-3.8-flash-tts"]
    };

    let mut last_err = String::new();
    for m in candidates {
        let url = format!(
            "https://generativelanguage.googleapis.com/v1beta/models/{m}:generateContent?key={api_key}"
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
            continue;
        }

        let val: serde_json::Value = match res.json().await {
            Ok(v) => v,
            Err(e) => {
                last_err = format!("Failed to parse Gemini TTS json: {e}");
                continue;
            }
        };

        let inline = &val["candidates"][0]["content"]["parts"][0]["inlineData"];
        let Some(pcm_b64) = inline["data"].as_str() else {
            last_err = format!("No inline audio data in Gemini ({m}) reply");
            continue;
        };
        let sample_rate = inline["mimeType"]
            .as_str()
            .and_then(parse_pcm_rate_from_mime)
            .unwrap_or(24000);

        use base64::Engine;
        let pcm_bytes = base64::engine::general_purpose::STANDARD
            .decode(pcm_b64.trim())
            .map_err(|e| HarnessError::VoiceError(format!("Failed to decode base64 audio: {e}")))?;

        #[allow(clippy::chunks_exact_to_as_chunks)]
        let samples: Vec<i16> = pcm_bytes
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();

        return harness_voice::AudioProcessor::samples_to_wav(&samples, sample_rate);
    }

    Err(HarnessError::VoiceError(last_err))
}

fn parse_pcm_rate_from_mime(mime: &str) -> Option<u32> {
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

async fn generate_edge_tts(text: &str, voice: &str) -> Result<Vec<u8>, HarnessError> {
    let edge_voice = if voice.contains("Neural") {
        voice.to_string()
    } else {
        "en-US-EmmaMultilingualNeural".to_string()
    };

    let tmp_path = std::env::temp_dir().join(format!("luna_tts_{}.mp3", uuid::Uuid::new_v4()));
    let output = tokio::process::Command::new("edge-tts")
        .arg("--text")
        .arg(text)
        .arg("--voice")
        .arg(&edge_voice)
        .arg("--write-media")
        .arg(&tmp_path)
        .output()
        .await
        .map_err(|e| HarnessError::VoiceError(format!("edge-tts invocation failed: {e}")))?;

    if !output.status.success() {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(HarnessError::VoiceError(format!(
            "edge-tts failed: {}",
            stderr.trim().chars().take(160).collect::<String>()
        )));
    }

    let bytes = tokio::fs::read(&tmp_path).await.map_err(|e| {
        HarnessError::VoiceError(format!("Failed to read edge-tts output audio: {e}"))
    })?;
    let _ = tokio::fs::remove_file(&tmp_path).await;
    if bytes.is_empty() {
        return Err(HarnessError::VoiceError(
            "edge-tts produced empty audio file".into(),
        ));
    }
    Ok(bytes)
}

struct PlayingFlagGuard(Arc<AtomicBool>);

impl Drop for PlayingFlagGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

async fn play_audio_stream(
    audio_bytes: &[u8],
    is_playing_tts: Arc<AtomicBool>,
    cancel: CancellationToken,
) -> Result<(), String> {
    is_playing_tts.store(true, Ordering::SeqCst);
    let _guard = PlayingFlagGuard(is_playing_tts);

    let bytes_vec = audio_bytes.to_vec();
    let cancel_rodio = cancel.clone();
    let rodio_res = tokio::task::spawn_blocking(move || -> Result<(), String> {
        let (_stream, stream_handle) = rodio::OutputStream::try_default()
            .map_err(|e| format!("Failed to open audio output stream: {e}"))?;

        let sink = rodio::Sink::try_new(&stream_handle)
            .map_err(|e| format!("Failed to create audio sink: {e}"))?;

        let cursor = std::io::Cursor::new(bytes_vec);
        let source =
            rodio::Decoder::new(cursor).map_err(|e| format!("Failed to decode audio: {e}"))?;

        sink.append(source);
        while !sink.empty() {
            if cancel_rodio.is_cancelled() {
                sink.stop();
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(40));
        }
        Ok(())
    })
    .await
    .map_err(|e| format!("Audio playback task panicked: {e}"))?;

    if rodio_res.is_ok() || cancel.is_cancelled() {
        return rodio_res;
    }

    // Fallback: ffplay (handles MP3, WAV, etc. via PulseAudio/PipeWire)
    let err_msg = rodio_res.unwrap_err();
    tracing::debug!("rodio playback failed ({err_msg}), trying ffplay fallback");

    let ext = if audio_bytes.starts_with(b"RIFF") {
        "wav"
    } else {
        "mp3"
    };
    let tmp_path = std::env::temp_dir().join(format!("luna_play_{}.{ext}", uuid::Uuid::new_v4()));
    if tokio::fs::write(&tmp_path, audio_bytes).await.is_err() {
        return Err(err_msg);
    }

    let spawn_res = tokio::process::Command::new("ffplay")
        .args(["-nodisp", "-autoexit", "-loglevel", "quiet"])
        .arg(&tmp_path)
        .spawn();

    let Ok(mut child) = spawn_res else {
        let _ = tokio::fs::remove_file(&tmp_path).await;
        return Err(err_msg);
    };

    tokio::select! {
        _ = cancel.cancelled() => {
            let _ = child.kill().await;
        }
        status = child.wait() => {
            if !matches!(status, Ok(s) if s.success()) {
                let _ = tokio::fs::remove_file(&tmp_path).await;
                return Err(err_msg);
            }
        }
    }

    let _ = tokio::fs::remove_file(&tmp_path).await;
    Ok(())
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
        let stripped = trimmed
            .trim_start_matches(['#', '*', '-', '•'])
            .trim()
            .replace("**", "")
            .replace("__", "")
            .replace('`', "");
        let stripped = strip_markdown_links(&stripped);
        if !stripped.trim().is_empty() {
            cleaned.push_str(stripped.trim());
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

fn strip_markdown_links(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(open_bracket) = rest.find('[') {
        out.push_str(&rest[..open_bracket]);
        let after_bracket = &rest[open_bracket + 1..];
        if let Some(close_bracket) = after_bracket.find("](") {
            let label = &after_bracket[..close_bracket];
            let after_paren = &after_bracket[close_bracket + 2..];
            if let Some(close_paren) = after_paren.find(')') {
                out.push_str(label);
                rest = &after_paren[close_paren + 1..];
                continue;
            }
        }
        out.push('[');
        rest = after_bracket;
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(!is_blank_transcript("Hello Luna"));
    }

    #[tokio::test]
    async fn test_lookup_plain_api_key_skips_oauth_json() {
        let mgr = CredentialManager::new(None).unwrap();
        mgr.add_credential(
            "test-voice-provider",
            "oauth-blob",
            r#"{"access_token":"abc","refresh_token":"xyz"}"#,
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

    #[tokio::test]
    #[ignore = "live audio hardware and API diagnostic"]
    async fn live_voice_pipeline_diagnostic() {
        let layers = harness_core::config::process_layers(Config::default()).unwrap();
        let config = harness_core::config::resolve(&layers)
            .unwrap()
            .into_config();
        let cred_manager =
            Arc::new(CredentialManager::new(Some(config.paths.credentials.clone())).unwrap());

        // 1. Check microphone input opens and produces samples
        let host = cpal::default_host();
        let dev = host.default_input_device().expect("default input device");
        let dev_name = dev.name().unwrap_or_default();
        let cfg = dev.default_input_config().expect("default input config");
        println!(
            "Live mic: {} | rate={} | ch={} | fmt={:?}",
            dev_name,
            cfg.sample_rate().0,
            cfg.channels(),
            cfg.sample_format()
        );

        // 2. Test TTS synthesis + rodio audio playback + STT roundtrip
        let phrase = "Voice mode is now active and working.";
        let g_key = resolve_gemini_api_key(&cred_manager)
            .await
            .expect("gemini key");
        let wav_bytes = generate_gemini_tts(
            phrase,
            &g_key,
            &config.voice.voice_name,
            &config.voice.tts_model,
        )
        .await
        .expect("gemini tts");
        println!("Generated Gemini TTS WAV: {} bytes", wav_bytes.len());

        let transcript = transcribe_audio_bytes(&wav_bytes, &config, &cred_manager)
            .await
            .expect("stt roundtrip");
        println!("Transcribed roundtrip: {:?}", transcript);
        assert!(transcript.to_ascii_lowercase().contains("voice mode"));

        // 3. Test real speaker playback via play_audio_stream
        let is_playing = Arc::new(AtomicBool::new(false));
        let cancel = CancellationToken::new();
        play_audio_stream(&wav_bytes, is_playing.clone(), cancel)
            .await
            .expect("play_audio_stream");
        assert!(!is_playing.load(Ordering::SeqCst));
        println!("Live speaker playback succeeded!");
    }
}
