use colored::Colorize;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures_util::StreamExt;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
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

/// Shared HTTP client with connection pooling and TCP keep-alive for low-latency voice STT/TTS.
fn voice_http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .user_agent(concat!("luna/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(25))
            .connect_timeout(std::time::Duration::from_secs(8))
            .pool_idle_timeout(std::time::Duration::from_secs(90))
            .tcp_keepalive(std::time::Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new())
    })
}

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

enum AudioThreadCmd {
    AppendPcm { rate: u32, samples: Vec<i16> },
    AppendEncoded(Vec<u8>),
    ClearAndReset,
}

struct SpeechTask {
    text: String,
    config: Config,
    cred_manager: Arc<CredentialManager>,
    voice_tx: Option<tokio::sync::mpsc::UnboundedSender<VoiceEvent>>,
    generation: u64,
}

/// Persistent, interruptible, streaming TTS playback engine.
///
/// Keeps a warm `rodio::OutputStream` + `rodio::Sink` alive across sentences and turns so spoken
/// audio begins immediately as the first SSE chunk arrives with zero inter-sentence gaps.
#[derive(Clone)]
pub struct VoiceSpeechQueue {
    task_tx: tokio::sync::mpsc::UnboundedSender<SpeechTask>,
    audio_tx: std::sync::mpsc::Sender<AudioThreadCmd>,
    active_gen: Arc<AtomicU64>,
    is_playing: Arc<AtomicBool>,
    active_cancel: Arc<Mutex<CancellationToken>>,
}

impl Default for VoiceSpeechQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceSpeechQueue {
    pub fn new() -> Self {
        let is_playing = Arc::new(AtomicBool::new(false));
        let active_gen = Arc::new(AtomicU64::new(1));
        let active_cancel = Arc::new(Mutex::new(CancellationToken::new()));
        let (audio_tx, audio_rx) = std::sync::mpsc::channel::<AudioThreadCmd>();
        let (task_tx, mut task_rx) = tokio::sync::mpsc::unbounded_channel::<SpeechTask>();

        let sink_busy = Arc::new(AtomicBool::new(false));
        let stream_busy = Arc::new(AtomicBool::new(false));
        let is_playing_audio = is_playing.clone();
        let sink_busy_thread = sink_busy.clone();
        let stream_busy_thread = stream_busy.clone();

        // Dedicated non-blocking audio playback thread holding a persistent OutputStream + Sink.
        std::thread::spawn(move || {
            let mut output: Option<(rodio::OutputStream, rodio::OutputStreamHandle)> = None;
            let mut sink: Option<rodio::Sink> = None;

            let ensure_sink =
                |output: &mut Option<(rodio::OutputStream, rodio::OutputStreamHandle)>,
                 sink: &mut Option<rodio::Sink>|
                 -> Result<(), String> {
                    if output.is_none() {
                        let pair = rodio::OutputStream::try_default()
                            .map_err(|e| format!("Failed to open audio output stream: {e}"))?;
                        *output = Some(pair);
                    }
                    if sink.is_none() {
                        if let Some((_, ref handle)) = output {
                            let new_sink = rodio::Sink::try_new(handle)
                                .map_err(|e| format!("Failed to create audio sink: {e}"))?;
                            *sink = Some(new_sink);
                        }
                    }
                    Ok(())
                };

            loop {
                match audio_rx.recv_timeout(std::time::Duration::from_millis(30)) {
                    Ok(cmd) => {
                        let mut cmds = vec![cmd];
                        while let Ok(more) = audio_rx.try_recv() {
                            cmds.push(more);
                        }
                        for c in cmds {
                            match c {
                                AudioThreadCmd::ClearAndReset => {
                                    if let Some(s) = sink.take() {
                                        s.stop();
                                    }
                                    sink_busy_thread.store(false, Ordering::SeqCst);
                                    if !stream_busy_thread.load(Ordering::SeqCst) {
                                        is_playing_audio.store(false, Ordering::SeqCst);
                                    }
                                }
                                AudioThreadCmd::AppendPcm { rate, samples } => {
                                    if samples.is_empty() {
                                        continue;
                                    }
                                    if ensure_sink(&mut output, &mut sink).is_err() {
                                        // Retry once by reopening the output device
                                        sink = None;
                                        output = None;
                                        if ensure_sink(&mut output, &mut sink).is_err() {
                                            continue;
                                        }
                                    }
                                    if let Some(ref s) = sink {
                                        let buf =
                                            rodio::buffer::SamplesBuffer::new(1, rate, samples);
                                        s.append(buf);
                                        s.play();
                                        sink_busy_thread.store(true, Ordering::SeqCst);
                                        is_playing_audio.store(true, Ordering::SeqCst);
                                    }
                                }
                                AudioThreadCmd::AppendEncoded(bytes) => {
                                    if bytes.is_empty() {
                                        continue;
                                    }
                                    if ensure_sink(&mut output, &mut sink).is_err() {
                                        sink = None;
                                        output = None;
                                        if ensure_sink(&mut output, &mut sink).is_err() {
                                            continue;
                                        }
                                    }
                                    if let Some(ref s) = sink {
                                        let cursor = std::io::Cursor::new(bytes);
                                        if let Ok(source) = rodio::Decoder::new(cursor) {
                                            s.append(source);
                                            s.play();
                                            sink_busy_thread.store(true, Ordering::SeqCst);
                                            is_playing_audio.store(true, Ordering::SeqCst);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }

                let still_playing = sink.as_ref().is_some_and(|s| !s.empty());
                sink_busy_thread.store(still_playing, Ordering::SeqCst);
                let busy = still_playing || stream_busy_thread.load(Ordering::SeqCst);
                is_playing_audio.store(busy, Ordering::SeqCst);
            }
        });

        // Async sequential TTS stream fetcher worker (streams chunks immediately into audio_tx).
        let active_gen_worker = active_gen.clone();
        let active_cancel_worker = active_cancel.clone();
        let audio_tx_worker = audio_tx.clone();
        let stream_busy_worker = stream_busy.clone();
        let is_playing_worker = is_playing.clone();
        let sink_busy_worker = sink_busy.clone();

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                while let Some(task) = task_rx.recv().await {
                    if task.generation != active_gen_worker.load(Ordering::SeqCst) {
                        continue;
                    }
                    let cancel = {
                        let guard = active_cancel_worker
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        guard.clone()
                    };
                    if cancel.is_cancelled()
                        || task.generation != active_gen_worker.load(Ordering::SeqCst)
                    {
                        continue;
                    }

                    stream_busy_worker.store(true, Ordering::SeqCst);
                    is_playing_worker.store(true, Ordering::SeqCst);

                    stream_tts_sentence_to_audio(
                        &task.text,
                        &task.config,
                        &task.cred_manager,
                        task.voice_tx.as_ref(),
                        &audio_tx_worker,
                        &cancel,
                    )
                    .await;

                    stream_busy_worker.store(false, Ordering::SeqCst);
                    if !sink_busy_worker.load(Ordering::SeqCst) {
                        is_playing_worker.store(false, Ordering::SeqCst);
                    }
                }
            });
        }

        Self {
            task_tx,
            audio_tx,
            active_gen,
            is_playing,
            active_cancel,
        }
    }

    pub fn is_playing(&self) -> Arc<AtomicBool> {
        self.is_playing.clone()
    }

    pub fn enqueue(
        &self,
        text: &str,
        config: &Config,
        cred_manager: &Arc<CredentialManager>,
        voice_tx: Option<tokio::sync::mpsc::UnboundedSender<VoiceEvent>>,
    ) {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return;
        }
        let gen = self.active_gen.load(Ordering::SeqCst);
        let _ = self.task_tx.send(SpeechTask {
            text: trimmed.to_string(),
            config: config.clone(),
            cred_manager: cred_manager.clone(),
            voice_tx,
            generation: gen,
        });
    }

    pub fn interrupt(&self) {
        self.active_gen.fetch_add(1, Ordering::SeqCst);
        {
            let mut guard = self.active_cancel.lock().unwrap_or_else(|e| e.into_inner());
            guard.cancel();
            *guard = CancellationToken::new();
        }
        let _ = self.audio_tx.send(AudioThreadCmd::ClearAndReset);
        self.is_playing.store(false, Ordering::SeqCst);
    }
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
    crate::commands::desktop::tools::register(registry.as_ref()).await?;

    let mut agent_inst = Agent::new(provider, memory, notes, registry, {
        let mut agent_cfg = config.agent.clone();
        agent_cfg.workspace_root = Some(config.paths.workspace_root.clone());
        agent_cfg.memory_mode = config.memory.mode;
        agent_cfg.memory_enabled = config.memory_effective_enabled();
        agent_cfg.notes_enabled = config.notes_effective_enabled();
        agent_cfg.skills_enabled = config.skills_effective_enabled();
        agent_cfg.mcp_enabled = config.mcp_effective_enabled();
        agent_cfg.context_window = Some(super::context_window::context_window_for(
            &config,
            &provider_id,
            &config.model.default,
        ) as usize);
        agent_cfg
    })
    .with_permissions(harness_core::PermissionEngine::validate(
        config.tools.permissions.clone(),
    )?)
    .with_approval(build_approver(&config), approval_settings(&config));
    agent_inst.set_voice_mode(true);
    let agent = Arc::new(agent_inst);

    let cancel = CancellationToken::new();
    let speech_queue = VoiceSpeechQueue::new();
    let is_playing = speech_queue.is_playing();
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

    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                println!("\n{}", "Stopping voice session...".dimmed());
                cancel.cancel();
                speech_queue.interrupt();
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
                        speech_queue.interrupt();
                        println!("\n{} {}", "🎤 You:".bold().yellow(), query);
                        let turn_cancel = cancel.child_token();
                        match agent.run_task(&query, turn_cancel).await {
                            Ok(reply) => {
                                println!("\n{} {}\n", "Luna:".bold().green(), reply);
                                let cleaned = clean_text_for_speech(&reply);
                                if !cleaned.is_empty() {
                                    for sentence in split_sentences_for_tts(&cleaned) {
                                        speech_queue.enqueue(
                                            &sentence,
                                            &config,
                                            &cred_manager,
                                            Some(voice_tx.clone()),
                                        );
                                    }
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

/// Run the continuous background voice listener.
///
/// Captures speech from the default microphone using CPAL and adaptive RMS energy VAD,
/// downsamples to 16kHz, trims trailing silence, transcribes utterances, and sends `VoiceEvent`
/// messages to `voice_tx`.
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

    // Fast responsive end-of-utterance pause (cap default 800ms down to 480ms for real-time feel)
    let silence_ms = if config.voice.silence_threshold_ms == 800 {
        480
    } else {
        config.voice.silence_threshold_ms.max(250)
    };
    let silence_duration = std::time::Duration::from_millis(silence_ms);
    let min_samples = (sample_rate as usize) / 4; // 250ms minimum utterance
    let max_samples = (sample_rate as usize) * 30; // 30s safety cap
    let pre_roll_max_samples = (sample_rate as usize) / 4; // 250ms pre-roll

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
                    // Brief 200ms echo cooldown right after TTS playback finishes
                    tts_cooldown_until = Some(std::time::Instant::now() + std::time::Duration::from_millis(200));
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
                let base_threshold = (noise_floor * 2.6).clamp(130.0, 520.0);
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
                            let trimmed_pcm = trim_trailing_silence(&speech_buffer, sample_rate, base_threshold * 0.8);
                            let resampled = downsample_to_16khz(trimmed_pcm, sample_rate);
                            let wav_res = harness_voice::AudioProcessor::samples_to_wav(&resampled, 16_000);
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

/// Downsample mono PCM samples to 16,000 Hz using linear interpolation.
///
/// Cuts STT upload size by ~3x from 44.1kHz/48kHz microphones without degrading recognition.
pub fn downsample_to_16khz(samples: &[i16], src_rate: u32) -> Vec<i16> {
    if src_rate <= 16_000 || samples.is_empty() {
        return samples.to_vec();
    }
    let ratio = src_rate as f64 / 16_000.0;
    let out_len = ((samples.len() as f64) / ratio).floor() as usize;
    if out_len == 0 {
        return samples.to_vec();
    }
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = (i as f64) * ratio;
        let idx = pos as usize;
        let frac = pos - (idx as f64);
        let s0 = samples[idx] as f64;
        let s1 = samples.get(idx + 1).copied().unwrap_or(samples[idx]) as f64;
        let interp = s0 + (s1 - s0) * frac;
        out.push(interp.round().clamp(-32768.0, 32767.0) as i16);
    }
    out
}

/// Trim trailing silent windows beyond 120ms so STT doesn't process dead tail air.
fn trim_trailing_silence(samples: &[i16], sample_rate: u32, threshold: f32) -> &[i16] {
    let win = ((sample_rate as usize) / 50).max(160); // 20ms window
    let keep_pad = ((sample_rate as usize) / 8).max(win); // keep 125ms pad after speech
    let mut end = samples.len();
    while end > win {
        let start = end - win;
        let rms = harness_voice::AudioProcessor::calculate_rms_energy(&samples[start..end]);
        if rms > threshold {
            break;
        }
        end = start;
    }
    let final_end = (end + keep_pad).min(samples.len());
    &samples[..final_end]
}

fn is_blank_transcript(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    lower.is_empty()
        || lower == "[blank_audio]"
        || lower == "(silence)"
        || lower == "[silence]"
        || lower == "."
}

async fn lookup_plain_api_keys(provider: &str, cred_manager: &CredentialManager) -> Vec<String> {
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

async fn lookup_plain_api_key(provider: &str, cred_manager: &CredentialManager) -> Option<String> {
    lookup_plain_api_keys(provider, cred_manager)
        .await
        .into_iter()
        .next()
}

async fn lookup_raw_credential(provider: &str, cred_manager: &CredentialManager) -> Option<String> {
    let creds = cred_manager.get_credentials(provider).await;
    creds
        .iter()
        .find(|c| c.is_priority)
        .or_else(|| creds.first())
        .map(|c| c.api_key.clone())
}

async fn resolve_gemini_api_keys(cred_manager: &Arc<CredentialManager>) -> Vec<String> {
    let mut keys = lookup_plain_api_keys("gemini", cred_manager).await;
    for k in lookup_plain_api_keys("google", cred_manager).await {
        if !keys.contains(&k) {
            keys.push(k);
        }
    }
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

async fn resolve_gemini_api_key(cred_manager: &Arc<CredentialManager>) -> Option<String> {
    resolve_gemini_api_keys(cred_manager)
        .await
        .into_iter()
        .next()
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

/// Transcribe recorded audio bytes into text with ultra-low-latency routing + Antigravity/Gemini/Groq fallback.
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
            // "antigravity" | "agy" | "google" | "gemini" | fallback:
            // When no explicit Gemini STT model is forced, prefer Groq Whisper (~0.5-1.0s) when
            // available because it is 3-4x faster than multimodal upload, then Antigravity (agy)
            // SSE STT, then Gemini API STT, then OpenAI Whisper.
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

async fn transcribe_antigravity_audio(
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
    let target_model = if !model.is_empty() {
        model
    } else {
        "gemini-3.5-flash-lite"
    };

    let payload = serde_json::json!({
        "project": project,
        "model": target_model,
        "request": {
            "contents": [{
                "role": "user",
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
        .bearer_auth(&bearer_token)
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

async fn transcribe_gemini_audio_with_keys(
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

async fn transcribe_openai_whisper(
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

async fn transcribe_groq_whisper(
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

/// Stream a single sentence or reply segment to `audio_tx` as chunks arrive over SSE/HTTP.
async fn stream_tts_sentence_to_audio(
    text: &str,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    voice_tx: Option<&tokio::sync::mpsc::UnboundedSender<VoiceEvent>>,
    audio_tx: &std::sync::mpsc::Sender<AudioThreadCmd>,
    cancel: &CancellationToken,
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
    let mut streamed = false;

    match provider.as_str() {
        "openai" => {
            if let Some(key) = resolve_openai_api_key(cred_manager).await {
                match stream_openai_tts_pcm(&cleaned, &key, voice_name, tts_model, audio_tx, cancel)
                    .await
                {
                    Ok(()) => streamed = true,
                    Err(e) => {
                        tracing::warn!("OpenAI TTS failed, trying Edge TTS fallback: {e}");
                        errors.push(e.to_string());
                    }
                }
            }
            if !streamed && !cancel.is_cancelled() {
                match generate_edge_tts(&cleaned, voice_name).await {
                    Ok(bytes) => {
                        let _ = audio_tx.send(AudioThreadCmd::AppendEncoded(bytes));
                        streamed = true;
                    }
                    Err(e) => errors.push(e.to_string()),
                }
            }
        }
        "antigravity" | "agy" | "google" | "gemini" => {
            let g_keys = resolve_gemini_api_keys(cred_manager).await;
            if !g_keys.is_empty() {
                match stream_gemini_tts_sse(
                    &cleaned, &g_keys, voice_name, tts_model, audio_tx, cancel,
                )
                .await
                {
                    Ok(()) => streamed = true,
                    Err(e) => {
                        tracing::warn!("Gemini SSE TTS failed, trying Edge TTS fallback: {e}");
                        errors.push(e.to_string());
                    }
                }
            } else {
                errors.push("No Gemini API key configured for Gemini TTS".to_string());
            }
            if !streamed && !cancel.is_cancelled() {
                match generate_edge_tts(&cleaned, voice_name).await {
                    Ok(bytes) => {
                        let _ = audio_tx.send(AudioThreadCmd::AppendEncoded(bytes));
                        streamed = true;
                    }
                    Err(e) => errors.push(e.to_string()),
                }
            }
        }
        _ => {
            let g_keys = resolve_gemini_api_keys(cred_manager).await;
            if !g_keys.is_empty()
                && stream_gemini_tts_sse(&cleaned, &g_keys, voice_name, tts_model, audio_tx, cancel)
                    .await
                    .is_ok()
            {
                streamed = true;
            }
            if !streamed && !cancel.is_cancelled() {
                match generate_edge_tts(&cleaned, voice_name).await {
                    Ok(bytes) => {
                        let _ = audio_tx.send(AudioThreadCmd::AppendEncoded(bytes));
                        streamed = true;
                    }
                    Err(e) => errors.push(e.to_string()),
                }
            }
        }
    }

    if !streamed && !cancel.is_cancelled() {
        let msg = format!("Voice TTS unavailable: {}", errors.join("; "));
        tracing::warn!("{msg}");
        if let Some(tx) = voice_tx {
            let _ = tx.send(VoiceEvent::Error(msg));
        }
    }
}

async fn stream_openai_tts_pcm(
    text: &str,
    api_key: &str,
    voice: &str,
    model: &str,
    audio_tx: &std::sync::mpsc::Sender<AudioThreadCmd>,
    cancel: &CancellationToken,
) -> Result<(), HarnessError> {
    let client = voice_http_client();
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
        if cancel.is_cancelled() {
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
        if !samples.is_empty() {
            let _ = audio_tx.send(AudioThreadCmd::AppendPcm {
                rate: 24_000,
                samples,
            });
        }
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

/// Stream Gemini TTS via `streamGenerateContent?alt=sse`, pushing PCM chunks directly into `audio_tx`
/// as each SSE `data:` frame arrives (~0.8s time-to-first-audio), rotating across Gemini keys on
/// quota/rate limits.
async fn stream_gemini_tts_sse(
    text: &str,
    api_keys: &[String],
    voice: &str,
    model: &str,
    audio_tx: &std::sync::mpsc::Sender<AudioThreadCmd>,
    cancel: &CancellationToken,
) -> Result<(), HarnessError> {
    let client = voice_http_client();
    let payload = build_gemini_tts_payload(text, voice);

    // Fastest streaming-capable Gemini 3.x TTS models first (`gemini-3.8-flash-lite-tts` = ~0.8s TTFA).
    let candidates: Vec<&str> = if !model.is_empty() {
        vec![model]
    } else {
        vec![
            "gemini-3.8-flash-lite-tts",
            "gemini-3.1-flash-tts-preview",
            "gemini-3.8-flash-tts",
            "gemini-2.5-flash-preview-tts",
        ]
    };

    let mut last_err = String::new();
    for m in candidates {
        for api_key in api_keys {
            if cancel.is_cancelled() {
                return Ok(());
            }
            let url = format!(
                "https://generativelanguage.googleapis.com/v1beta/models/{m}:streamGenerateContent?alt=sse&key={api_key}"
            );
            let res = match client.post(&url).json(&payload).send().await {
                Ok(r) => r,
                Err(e) => {
                    last_err = format!("Gemini TTS ({m}) request failed: {e}");
                    break;
                }
            };

            if !res.status().is_success() {
                let status = res.status();
                let err_text = res.text().await.unwrap_or_default();
                last_err = format!(
                    "Gemini TTS ({m}) HTTP {status}: {}",
                    err_text.chars().take(160).collect::<String>()
                );
                if status.as_u16() == 429 || status.as_u16() == 403 {
                    // Fail over to next Gemini API key in pool before switching model
                    continue;
                }
                break;
            }

            let mut stream = res.bytes_stream();
            let mut line_buf: Vec<u8> = Vec::new();
            let mut got_audio = false;

            while let Some(chunk_res) = stream.next().await {
                if cancel.is_cancelled() {
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
                        if push_gemini_sse_audio_chunk(json_str, audio_tx) {
                            got_audio = true;
                        }
                    }
                }
            }

            if !line_buf.is_empty() && !cancel.is_cancelled() {
                let line_str = String::from_utf8_lossy(&line_buf);
                let trimmed = line_str.trim();
                if let Some(json_str) = trimmed.strip_prefix("data: ") {
                    if push_gemini_sse_audio_chunk(json_str, audio_tx) {
                        got_audio = true;
                    }
                }
            }

            if got_audio {
                return Ok(());
            }
            last_err = format!("No inline audio data in Gemini ({m}) SSE stream");
            break;
        }
    }

    Err(HarnessError::VoiceError(last_err))
}

fn push_gemini_sse_audio_chunk(
    json_str: &str,
    audio_tx: &std::sync::mpsc::Sender<AudioThreadCmd>,
) -> bool {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(json_str) else {
        return false;
    };
    let Some(parts) = val["candidates"][0]["content"]["parts"].as_array() else {
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
                let _ = audio_tx.send(AudioThreadCmd::AppendPcm {
                    rate: sample_rate,
                    samples,
                });
                sent_any = true;
            }
        }
    }
    sent_any
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

/// Extract ready-to-speak sentences or initial clauses from a growing streamed reply buffer.
///
/// Updates `in_code_block` across fenced blocks so code is skipped while conversational prose
/// starts speaking as soon as the first sentence or clause completes.
pub fn extract_ready_speech_chunks(
    pending: &str,
    in_code_block: &mut bool,
    first_chunk_of_turn: bool,
) -> (Vec<String>, usize) {
    let mut out = Vec::new();
    let mut consumed = 0usize;

    // 1. Process any completed newline-terminated lines first.
    if let Some(last_nl) = pending.rfind('\n') {
        let complete_lines = &pending[..=last_nl];
        for line in complete_lines.split_inclusive('\n') {
            consumed += line.len();
            let trimmed = line.trim();
            if trimmed.starts_with("```") {
                *in_code_block = !*in_code_block;
                continue;
            }
            if *in_code_block || trimmed.starts_with('|') || trimmed.starts_with("---") {
                continue;
            }
            let cleaned = clean_single_line_for_speech(trimmed);
            if !cleaned.is_empty() {
                for s in split_sentences_for_tts(&cleaned) {
                    out.push(s);
                }
            }
        }
    }

    // 2. Check the trailing partial line for completed sentences (or early first clause).
    let tail = &pending[consumed..];
    if !*in_code_block
        && !tail.trim_start().starts_with("```")
        && !tail.trim_start().starts_with('|')
    {
        let mut local_pos = 0usize;
        let bytes = tail.as_bytes();
        let mut is_first = first_chunk_of_turn && out.is_empty();

        while local_pos < tail.len() {
            let slice = &tail[local_pos..];
            let mut split_at: Option<usize> = None;

            for (i, ch) in slice.char_indices() {
                let next_idx = i + ch.len_utf8();
                let next_is_space = next_idx < slice.len() && bytes[local_pos + next_idx] == b' ';
                if !next_is_space {
                    continue;
                }
                if ch == '.' || ch == '!' || ch == '?' {
                    // Avoid splitting decimals like "3.14 " or short list markers like "1. "
                    let prev_char = slice[..i].chars().next_back();
                    let is_short_number_dot =
                        ch == '.' && prev_char.is_some_and(|c| c.is_ascii_digit()) && i < 6;
                    if !is_short_number_dot && i >= 4 {
                        split_at = Some(next_idx + 1);
                        break;
                    }
                } else if is_first && (ch == ',' || ch == ';' || ch == ':') && i >= 45 {
                    split_at = Some(next_idx + 1);
                    break;
                }
            }

            let Some(end_rel) = split_at else {
                break;
            };
            let segment = &slice[..end_rel];
            let cleaned = clean_single_line_for_speech(segment);
            if !cleaned.is_empty() {
                out.push(cleaned);
                is_first = false;
            }
            local_pos += end_rel;
            consumed += end_rel;
        }
    }

    (out, consumed)
}

pub fn split_sentences_for_tts(cleaned: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let mut start = 0usize;
    let bytes = cleaned.as_bytes();
    for (i, ch) in cleaned.char_indices() {
        let next = i + ch.len_utf8();
        if (ch == '.' || ch == '!' || ch == '?') && next < cleaned.len() && bytes[next] == b' ' {
            let chunk = cleaned[start..next].trim();
            if chunk.len() >= 12 {
                sentences.push(chunk.to_string());
                start = next + 1;
            }
        }
    }
    let rest = cleaned[start..].trim();
    if !rest.is_empty() {
        sentences.push(rest.to_string());
    }
    sentences
}

fn clean_single_line_for_speech(trimmed: &str) -> String {
    let stripped = trimmed
        .trim_start_matches(['#', '*', '-', '•'])
        .trim()
        .replace("**", "")
        .replace("__", "")
        .replace('`', "");
    strip_markdown_links(&stripped).trim().to_string()
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
        let stripped = clean_single_line_for_speech(trimmed);
        if !stripped.is_empty() {
            cleaned.push_str(&stripped);
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
    fn test_build_gemini_tts_payload_sends_bare_text_without_read_aloud_prefix() {
        let payload = build_gemini_tts_payload("Hello, how are you?", "");
        let sent = payload["contents"][0]["parts"][0]["text"].as_str().unwrap();
        assert_eq!(sent, "Hello, how are you?");
        assert!(!sent.to_lowercase().contains("read aloud"));
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

        let full2 = "Hello! How are you today?\n```rust\nlet x = 1;\n```\nAll done.";
        let (chunks2, c2) = extract_ready_speech_chunks(&full2[c1..], &mut in_code, false);
        assert_eq!(chunks2, vec!["How are you today?"]);
        assert!(!in_code);
        assert_eq!(&full2[c1 + c2..], "All done.");
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
        assert!(!is_blank_transcript("Hello Luna"));
    }

    #[tokio::test]
    async fn test_lookup_plain_api_key_skips_oauth_json() {
        let mgr = CredentialManager::new(None).unwrap();
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

    #[tokio::test]
    #[ignore = "live audio hardware and API diagnostic"]
    async fn live_voice_pipeline_diagnostic() {
        let layers = harness_core::config::process_layers(Config::default()).unwrap();
        let config = harness_core::config::resolve(&layers)
            .unwrap()
            .into_config();
        let cred_manager =
            Arc::new(CredentialManager::new(Some(config.paths.credentials.clone())).unwrap());

        let t0 = std::time::Instant::now();
        let queue = VoiceSpeechQueue::new();
        let playing = queue.is_playing();
        queue.enqueue(
            "Voice streaming is now active and fast.",
            &config,
            &cred_manager,
            None,
        );
        while !playing.load(Ordering::SeqCst) && t0.elapsed().as_secs() < 5 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        println!("Time to start streaming audio: {:?}", t0.elapsed());
        while playing.load(Ordering::SeqCst) && t0.elapsed().as_secs() < 15 {
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }
        println!("Total streaming audio completed in {:?}", t0.elapsed());
    }
}
