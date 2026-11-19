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
    /// A non-empty user speech transcript ready to dispatch or stage in the composer.
    Transcript(String),
    /// Informational status notice (e.g. microphone ready).
    Notice(String),
    /// Actionable voice error surfaced in the UI instead of silently swallowed.
    Error(String),
    /// Immediate barge-in signal: user started speaking during TTS playback.
    /// Sent before full transcript to allow instant TTS interruption.
    BargeIn,
    /// User voice activity started; payload is normalized RMS energy `[0.0, 1.0]`.
    SpeechDetected(f32),
    /// Periodic live microphone energy level `[0.0, 1.0]` while user is speaking.
    SpeechLevel(f32),
    /// Utterance capture finished and speech-to-text transcription is in progress.
    Transcribing,
    /// Speech capture/transcription returned to idle listening.
    SpeechIdle,
}

/// Catalog entry for a built-in TTS voice across Antigravity/Gemini, OpenAI, and Edge TTS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceCatalogEntry {
    pub id: &'static str,
    pub provider: &'static str,
    pub provider_label: &'static str,
    pub description: &'static str,
}

/// Return all available built-in voices across Gemini/Antigravity, OpenAI, and Edge TTS.
pub fn available_voices() -> Vec<VoiceCatalogEntry> {
    vec![
        // Antigravity / Gemini Prebuilt Voices (30)
        VoiceCatalogEntry {
            id: "Aoede",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Breezy (default)",
        },
        VoiceCatalogEntry {
            id: "Zephyr",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Bright",
        },
        VoiceCatalogEntry {
            id: "Puck",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Upbeat",
        },
        VoiceCatalogEntry {
            id: "Charon",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Informative",
        },
        VoiceCatalogEntry {
            id: "Kore",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Firm",
        },
        VoiceCatalogEntry {
            id: "Fenrir",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Excitable",
        },
        VoiceCatalogEntry {
            id: "Leda",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Youthful",
        },
        VoiceCatalogEntry {
            id: "Orus",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Firm",
        },
        VoiceCatalogEntry {
            id: "Callirrhoe",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Easy-going",
        },
        VoiceCatalogEntry {
            id: "Autonoe",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Bright",
        },
        VoiceCatalogEntry {
            id: "Enceladus",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Breathy",
        },
        VoiceCatalogEntry {
            id: "Iapetus",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Clear",
        },
        VoiceCatalogEntry {
            id: "Umbriel",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Easy-going",
        },
        VoiceCatalogEntry {
            id: "Algieba",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Smooth",
        },
        VoiceCatalogEntry {
            id: "Despina",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Smooth",
        },
        VoiceCatalogEntry {
            id: "Erinome",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Clear",
        },
        VoiceCatalogEntry {
            id: "Algenib",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Gravelly",
        },
        VoiceCatalogEntry {
            id: "Rasalgethi",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Informative",
        },
        VoiceCatalogEntry {
            id: "Laomedeia",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Upbeat",
        },
        VoiceCatalogEntry {
            id: "Achernar",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Soft",
        },
        VoiceCatalogEntry {
            id: "Alnilam",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Firm",
        },
        VoiceCatalogEntry {
            id: "Schedar",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Even",
        },
        VoiceCatalogEntry {
            id: "Gacrux",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Mature",
        },
        VoiceCatalogEntry {
            id: "Pulcherrima",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Forward",
        },
        VoiceCatalogEntry {
            id: "Achird",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Friendly",
        },
        VoiceCatalogEntry {
            id: "Zubenelgenubi",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Casual",
        },
        VoiceCatalogEntry {
            id: "Vindemiatrix",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Gentle",
        },
        VoiceCatalogEntry {
            id: "Sadachbia",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Lively",
        },
        VoiceCatalogEntry {
            id: "Sadaltager",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Knowledgeable",
        },
        VoiceCatalogEntry {
            id: "Sulafat",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Warm",
        },
        // OpenAI Voices (13)
        VoiceCatalogEntry {
            id: "alloy",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Balanced, neutral",
        },
        VoiceCatalogEntry {
            id: "ash",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Clear, precise",
        },
        VoiceCatalogEntry {
            id: "ballad",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Melodic, warm",
        },
        VoiceCatalogEntry {
            id: "coral",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Expressive, bright",
        },
        VoiceCatalogEntry {
            id: "echo",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Smooth, male",
        },
        VoiceCatalogEntry {
            id: "fable",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Storytelling",
        },
        VoiceCatalogEntry {
            id: "nova",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Energetic, bright",
        },
        VoiceCatalogEntry {
            id: "onyx",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Deep, authoritative",
        },
        VoiceCatalogEntry {
            id: "sage",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Calm, thoughtful",
        },
        VoiceCatalogEntry {
            id: "shimmer",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Gentle, airy",
        },
        VoiceCatalogEntry {
            id: "verse",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Versatile, dynamic",
        },
        VoiceCatalogEntry {
            id: "marin",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Warm, natural",
        },
        VoiceCatalogEntry {
            id: "cedar",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Grounded, steady",
        },
        // Edge TTS / Groq Neural Voices (30)
        VoiceCatalogEntry {
            id: "en-US-EmmaMultilingualNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Female · Cheerful",
        },
        VoiceCatalogEntry {
            id: "en-US-AndrewMultilingualNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Male · Warm",
        },
        VoiceCatalogEntry {
            id: "en-US-AvaMultilingualNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Female · Expressive",
        },
        VoiceCatalogEntry {
            id: "en-US-BrianMultilingualNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Male · Casual",
        },
        VoiceCatalogEntry {
            id: "en-US-AriaNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Female · Positive",
        },
        VoiceCatalogEntry {
            id: "en-US-JennyNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Female · Clear",
        },
        VoiceCatalogEntry {
            id: "en-US-GuyNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Male · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-US-ChristopherNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Male · Reliable",
        },
        VoiceCatalogEntry {
            id: "en-US-EricNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Male · Rational",
        },
        VoiceCatalogEntry {
            id: "en-US-MichelleNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Female · Pleasant",
        },
        VoiceCatalogEntry {
            id: "en-US-RogerNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Male · Lively",
        },
        VoiceCatalogEntry {
            id: "en-US-SteffanNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Male · Mature",
        },
        VoiceCatalogEntry {
            id: "en-US-AnaNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Female · Cute",
        },
        VoiceCatalogEntry {
            id: "en-GB-SoniaNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "UK Female · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-GB-RyanNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "UK Male · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-GB-LibbyNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "UK Female · Bright",
        },
        VoiceCatalogEntry {
            id: "en-GB-MaisieNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "UK Female · Youthful",
        },
        VoiceCatalogEntry {
            id: "en-GB-ThomasNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "UK Male · Calm",
        },
        VoiceCatalogEntry {
            id: "en-AU-NatashaNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "AU Female · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-AU-WilliamMultilingualNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "AU Male · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-CA-ClaraNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "CA Female · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-CA-LiamNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "CA Male · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-IE-EmilyNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "IE Female · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-IE-ConnorNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "IE Male · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-IN-NeerjaExpressiveNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "IN Female · Expressive",
        },
        VoiceCatalogEntry {
            id: "en-IN-PrabhatNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "IN Male · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-PH-RosaNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "PH Female · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-PH-JamesNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "PH Male · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-SG-LunaNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "SG Female · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-SG-WayneNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "SG Male · Friendly",
        },
    ]
}

fn is_openai_voice(voice: &str) -> bool {
    matches!(
        voice.trim(),
        "alloy"
            | "ash"
            | "ballad"
            | "coral"
            | "echo"
            | "fable"
            | "nova"
            | "onyx"
            | "sage"
            | "shimmer"
            | "verse"
            | "marin"
            | "cedar"
    )
}

enum AudioThreadCmd {
    AppendPcm {
        generation: u64,
        rate: u32,
        samples: Vec<i16>,
    },
    AppendEncoded {
        generation: u64,
        bytes: Vec<u8>,
    },
    ClearAndReset,
}

#[derive(Clone, Copy)]
struct GenAudioSender<'a> {
    tx: &'a std::sync::mpsc::Sender<AudioThreadCmd>,
    generation: u64,
    cancel: &'a CancellationToken,
}

impl<'a> GenAudioSender<'a> {
    fn send_pcm(&self, rate: u32, samples: Vec<i16>) {
        if !self.cancel.is_cancelled() && !samples.is_empty() {
            let _ = self.tx.send(AudioThreadCmd::AppendPcm {
                generation: self.generation,
                rate,
                samples,
            });
        }
    }

    fn send_encoded(&self, bytes: Vec<u8>) {
        if !self.cancel.is_cancelled() && !bytes.is_empty() {
            let _ = self.tx.send(AudioThreadCmd::AppendEncoded {
                generation: self.generation,
                bytes,
            });
        }
    }
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
    turn_muted: Arc<AtomicBool>,
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
        let turn_muted = Arc::new(AtomicBool::new(false));
        let active_gen = Arc::new(AtomicU64::new(1));
        let active_cancel = Arc::new(Mutex::new(CancellationToken::new()));
        let (audio_tx, audio_rx) = std::sync::mpsc::channel::<AudioThreadCmd>();
        let (task_tx, mut task_rx) = tokio::sync::mpsc::unbounded_channel::<SpeechTask>();

        let sink_busy = Arc::new(AtomicBool::new(false));
        let stream_busy = Arc::new(AtomicBool::new(false));
        let is_playing_audio = is_playing.clone();
        let active_gen_audio = active_gen.clone();
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
                                AudioThreadCmd::AppendPcm {
                                    generation,
                                    rate,
                                    samples,
                                } => {
                                    if samples.is_empty()
                                        || generation != active_gen_audio.load(Ordering::SeqCst)
                                    {
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
                                AudioThreadCmd::AppendEncoded { generation, bytes } => {
                                    if bytes.is_empty()
                                        || generation != active_gen_audio.load(Ordering::SeqCst)
                                    {
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

                    let sender = GenAudioSender {
                        tx: &audio_tx_worker,
                        generation: task.generation,
                        cancel: &cancel,
                    };
                    stream_tts_sentence_to_audio(
                        &task.text,
                        &task.config,
                        &task.cred_manager,
                        task.voice_tx.as_ref(),
                        sender,
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
            turn_muted,
            active_cancel,
        }
    }

    pub fn is_playing(&self) -> Arc<AtomicBool> {
        self.is_playing.clone()
    }

    pub fn set_turn_muted(&self, muted: bool) {
        self.turn_muted.store(muted, Ordering::SeqCst);
    }

    pub fn is_turn_muted(&self) -> bool {
        self.turn_muted.load(Ordering::SeqCst)
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

    let cancel = CancellationToken::new();
    let speech_queue = VoiceSpeechQueue::new();
    let is_playing = speech_queue.is_playing();
    let (voice_tx, mut voice_rx) = tokio::sync::mpsc::unbounded_channel::<VoiceEvent>();

    let ack_sent_flag = Arc::new(AtomicBool::new(false));
    let current_query_shared: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
    let turn_voice_cancel: Arc<Mutex<CancellationToken>> =
        Arc::new(Mutex::new(CancellationToken::new()));
    let obs_cfg = config.clone();
    let obs_creds = cred_manager.clone();
    let obs_queue = speech_queue.clone();
    let obs_tx = voice_tx.clone();
    let obs_ack = ack_sent_flag.clone();
    let obs_query = current_query_shared.clone();
    let obs_cancel = turn_voice_cancel.clone();
    agent_inst = agent_inst.with_observer(Arc::new(move |ev| {
        if let harness_core::AgentEvent::ToolExecuting { name, arguments } = ev {
            if !obs_ack.swap(true, Ordering::SeqCst) {
                let q = obs_query.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let c = obs_cancel.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let hint = format!("{name} {arguments}");
                spawn_pre_task_voice_ack(
                    q,
                    Some(hint),
                    obs_cfg.clone(),
                    obs_creds.clone(),
                    obs_queue.clone(),
                    Some(obs_tx.clone()),
                    c,
                );
            }
        }
    }));
    let agent = Arc::new(agent_inst);

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
                    VoiceEvent::BargeIn => {
                        speech_queue.interrupt();
                        println!("\n{} Barge-in detected — interrupting...", "🎤".bold().yellow());
                    }
                    VoiceEvent::SpeechDetected(_)
                    | VoiceEvent::SpeechLevel(_)
                    | VoiceEvent::Transcribing
                    | VoiceEvent::SpeechIdle => {}
                    VoiceEvent::Transcript(query) => {
                        speech_queue.interrupt();
                        speech_queue.set_turn_muted(false);
                        ack_sent_flag.store(false, Ordering::SeqCst);
                        let per_turn_cancel = cancel.child_token();
                        {
                            let mut guard = turn_voice_cancel
                                .lock()
                                .unwrap_or_else(|e| e.into_inner());
                            guard.cancel();
                            *guard = per_turn_cancel.clone();
                        }
                        *current_query_shared
                            .lock()
                            .unwrap_or_else(|e| e.into_inner()) = query.clone();
                        println!("\n{} {}", "🎤 You:".bold().yellow(), query);
                        let turn_cancel = per_turn_cancel.clone();
                        match agent.run_task(&query, turn_cancel).await {
                            Ok(reply) => {
                                println!("\n{} {}\n", "Luna:".bold().green(), reply);
                                spawn_separate_voice_reply(
                                    query,
                                    reply,
                                    0,
                                    config.clone(),
                                    cred_manager.clone(),
                                    speech_queue.clone(),
                                    Some(voice_tx.clone()),
                                    per_turn_cancel,
                                );
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
    let mut barge_in_hits: u8 = 0;
    let mut tts_cooldown_until: Option<std::time::Instant> = None;
    let mut last_stt_error_at: Option<std::time::Instant> = None;
    let mut last_level_sent_at: Option<std::time::Instant> = None;

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
                    // Brief 100ms echo cooldown right after TTS playback finishes
                    tts_cooldown_until = Some(std::time::Instant::now() + std::time::Duration::from_millis(100));
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
                    // Require moderately louder voice to barge in over speaker audio
                    (base_threshold * 1.5).max(600.0)
                } else {
                    base_threshold
                };
                let norm_level = ((rms - base_threshold * 0.5) / 2200.0).clamp(0.05, 1.0);

                if rms > threshold {
                    if !is_speaking {
                        if playing {
                            barge_in_hits = barge_in_hits.saturating_add(1);
                            if barge_in_hits < 2 {
                                pre_roll_len += samples.len();
                                pre_roll.push_back(samples);
                                while pre_roll_len > pre_roll_max_samples {
                                    if let Some(front) = pre_roll.pop_front() {
                                        pre_roll_len = pre_roll_len.saturating_sub(front.len());
                                    } else {
                                        break;
                                    }
                                }
                                continue;
                            }
                            let _ = voice_tx.send(VoiceEvent::BargeIn);
                            was_playing_tts = false;
                        }
                        barge_in_hits = 0;
                        is_speaking = true;
                        last_level_sent_at = Some(std::time::Instant::now());
                        let _ = voice_tx.send(VoiceEvent::SpeechDetected(norm_level));
                        for chunk in pre_roll.drain(..) {
                            speech_buffer.extend_from_slice(&chunk);
                        }
                        pre_roll_len = 0;
                    } else if last_level_sent_at
                        .map(|t| t.elapsed() >= std::time::Duration::from_millis(80))
                        .unwrap_or(true)
                    {
                        last_level_sent_at = Some(std::time::Instant::now());
                        let _ = voice_tx.send(VoiceEvent::SpeechLevel(norm_level));
                    }
                    last_speech_time = std::time::Instant::now();
                    speech_buffer.extend_from_slice(&samples);
                } else if is_speaking {
                    speech_buffer.extend_from_slice(&samples);
                    if last_level_sent_at
                        .map(|t| t.elapsed() >= std::time::Duration::from_millis(80))
                        .unwrap_or(true)
                    {
                        last_level_sent_at = Some(std::time::Instant::now());
                        let _ = voice_tx.send(VoiceEvent::SpeechLevel(norm_level * 0.6));
                    }
                    let timed_out = speech_buffer.len() >= max_samples;
                    if last_speech_time.elapsed() > silence_duration || timed_out {
                        is_speaking = false;
                        last_level_sent_at = None;
                        if speech_buffer.len() >= min_samples {
                            let _ = voice_tx.send(VoiceEvent::Transcribing);
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
                            let _ = voice_tx.send(VoiceEvent::SpeechIdle);
                        } else {
                            speech_buffer.clear();
                            let _ = voice_tx.send(VoiceEvent::SpeechIdle);
                        }
                    }
                } else {
                    barge_in_hits = 0;
                    if !playing && rms < 400.0 {
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

/// Stream a single sentence or reply segment to `audio` as chunks arrive over SSE/HTTP.
async fn stream_tts_sentence_to_audio(
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
    let mut errors: Vec<String> = Vec::new();
    let mut streamed = false;

    if voice_name.contains("Neural") {
        match generate_edge_tts(&cleaned, voice_name).await {
            Ok(bytes) => {
                audio.send_encoded(bytes);
                streamed = true;
            }
            Err(e) => errors.push(e.to_string()),
        }
    } else if is_openai_voice(voice_name) && provider != "openai" {
        if let Some(key) = resolve_openai_api_key(cred_manager).await {
            match stream_openai_tts_pcm(&cleaned, &key, voice_name, tts_model, audio).await {
                Ok(()) => streamed = true,
                Err(e) => {
                    tracing::warn!("OpenAI TTS failed, trying Edge TTS fallback: {e}");
                    errors.push(e.to_string());
                }
            }
        }
        if !streamed && !audio.cancel.is_cancelled() {
            match generate_edge_tts(&cleaned, voice_name).await {
                Ok(bytes) => {
                    audio.send_encoded(bytes);
                    streamed = true;
                }
                Err(e) => errors.push(e.to_string()),
            }
        }
    } else {
        match provider.as_str() {
            "openai" => {
                if let Some(key) = resolve_openai_api_key(cred_manager).await {
                    match stream_openai_tts_pcm(&cleaned, &key, voice_name, tts_model, audio).await
                    {
                        Ok(()) => streamed = true,
                        Err(e) => {
                            tracing::warn!("OpenAI TTS failed, trying Edge TTS fallback: {e}");
                            errors.push(e.to_string());
                        }
                    }
                }
                if !streamed && !audio.cancel.is_cancelled() {
                    match generate_edge_tts(&cleaned, voice_name).await {
                        Ok(bytes) => {
                            audio.send_encoded(bytes);
                            streamed = true;
                        }
                        Err(e) => errors.push(e.to_string()),
                    }
                }
            }
            "antigravity" | "agy" | "google" | "gemini" => {
                let g_keys = resolve_gemini_api_keys(cred_manager).await;
                if !g_keys.is_empty() {
                    match stream_gemini_tts_sse(&cleaned, &g_keys, voice_name, tts_model, audio)
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
                if !streamed && !audio.cancel.is_cancelled() {
                    match generate_edge_tts(&cleaned, voice_name).await {
                        Ok(bytes) => {
                            audio.send_encoded(bytes);
                            streamed = true;
                        }
                        Err(e) => errors.push(e.to_string()),
                    }
                }
            }
            _ => {
                let g_keys = resolve_gemini_api_keys(cred_manager).await;
                if !g_keys.is_empty()
                    && stream_gemini_tts_sse(&cleaned, &g_keys, voice_name, tts_model, audio)
                        .await
                        .is_ok()
                {
                    streamed = true;
                }
                if !streamed && !audio.cancel.is_cancelled() {
                    match generate_edge_tts(&cleaned, voice_name).await {
                        Ok(bytes) => {
                            audio.send_encoded(bytes);
                            streamed = true;
                        }
                        Err(e) => errors.push(e.to_string()),
                    }
                }
            }
        }
    }

    if !streamed && !audio.cancel.is_cancelled() {
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

/// Stream Gemini TTS via `streamGenerateContent?alt=sse`, pushing PCM chunks directly into `audio_tx`
/// as each SSE `data:` frame arrives (~0.8s time-to-first-audio), rotating across Gemini keys on
/// quota/rate limits.
async fn stream_gemini_tts_sse(
    text: &str,
    api_keys: &[String],
    voice: &str,
    model: &str,
    audio: GenAudioSender<'_>,
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
            last_err = format!("No inline audio data in Gemini ({m}) SSE stream");
            break;
        }
    }

    Err(HarnessError::VoiceError(last_err))
}

fn push_gemini_sse_audio_chunk(json_str: &str, audio: GenAudioSender<'_>) -> bool {
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
                audio.send_pcm(sample_rate, samples);
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
#[cfg(test)]
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

/// Returns `true` when the full written reply contains structured formatting (Markdown tables,
/// code blocks, headings, lists, raw URLs) or exceeds short conversational length, so a
/// separate spoken response should be generated rather than reading the raw reply verbatim.
pub fn needs_separate_voice_summary(reply: &str) -> bool {
    let trimmed = reply.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.contains("```") || trimmed.contains("http://") || trimmed.contains("https://") {
        return true;
    }
    let mut table_rows = 0usize;
    let mut list_rows = 0usize;
    for line in trimmed.lines() {
        let lt = line.trim_start();
        if lt.starts_with('|') && lt.matches('|').count() >= 2 {
            table_rows += 1;
            if table_rows >= 2 {
                return true;
            }
        }
        if lt.starts_with("# ") || lt.starts_with("## ") || lt.starts_with("### ") {
            return true;
        }
        if lt.starts_with("- ") || lt.starts_with("* ") || lt.starts_with("• ") {
            list_rows += 1;
            if list_rows >= 2 {
                return true;
            }
        } else if let Some(dot_pos) = lt.find(". ") {
            if dot_pos > 0 && dot_pos <= 3 && lt[..dot_pos].chars().all(|c| c.is_ascii_digit()) {
                list_rows += 1;
                if list_rows >= 2 {
                    return true;
                }
            }
        }
    }
    let cleaned = clean_text_for_speech(trimmed);
    if cleaned.chars().count() > 280 {
        return true;
    }
    split_sentences_for_tts(&cleaned).len() > 3
}

/// Deterministic fallback spoken summary when an LLM summary call is unavailable.
pub fn fallback_voice_summary(reply: &str) -> String {
    let cleaned = clean_text_for_speech(reply);
    if cleaned.is_empty() {
        return "Done! I've posted the full details in the chat.".to_string();
    }
    let sentences = split_sentences_for_tts(&cleaned);
    let mut out = String::new();
    for s in sentences.iter().take(2) {
        if out.len() + s.len() > 240 && !out.is_empty() {
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(s);
    }
    if out.is_empty() {
        out = cleaned.chars().take(220).collect();
    }
    if reply.contains('|') || reply.contains("```") {
        if !out.ends_with(['.', '!', '?']) {
            out.push('.');
        }
        out.push_str(" I've put the full details in the chat.");
    }
    out
}

/// Fast lightweight text completion helper for pre-task voice acknowledgments and separate
/// spoken summaries using `GenericProviderClient` (handles OAuth refresh, key rotation, and
/// provider routing cleanly) with a tight 5-second timeout per candidate.
async fn generate_fast_voice_text(
    system: &str,
    user_prompt: &str,
    max_tokens: u32,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
) -> Result<String, HarnessError> {
    use harness_core::traits::ModelProvider;
    use harness_core::types::{CompletionOptions, Message, NO_TEXT_RESPONSE};

    let discovery = Arc::new(ModelDiscovery::new(
        Some(config.paths.models_cache.clone()),
        24,
        config.providers.clone(),
    ));

    let fast_model_for = |prov: &str, active_default: &str| -> String {
        match prov.to_ascii_lowercase().as_str() {
            "antigravity" | "agy" => "gemini-3-flash".to_string(),
            "gemini" => "gemini-3-flash-preview".to_string(),
            "openai" => "gpt-4o-mini".to_string(),
            "groq" => "llama-3.1-8b-instant".to_string(),
            _ if !active_default.trim().is_empty() => active_default.trim().to_string(),
            _ => "gpt-4o-mini".to_string(),
        }
    };

    let mut candidates: Vec<(String, String)> = Vec::new();
    let active_prov = config.model.provider.trim().to_ascii_lowercase();
    if !active_prov.is_empty() {
        candidates.push((
            active_prov.clone(),
            fast_model_for(&active_prov, &config.model.default),
        ));
    }
    let voice_prov = config.voice.provider.trim().to_ascii_lowercase();
    if !voice_prov.is_empty() && voice_prov != "none" && voice_prov != active_prov {
        candidates.push((
            voice_prov.clone(),
            fast_model_for(&voice_prov, &config.model.default),
        ));
    }
    for (prov, model) in [
        ("groq", "llama-3.1-8b-instant"),
        ("antigravity", "gemini-3-flash"),
        ("openai", "gpt-4o-mini"),
        ("gemini", "gemini-3-flash-preview"),
    ] {
        if !candidates.iter().any(|(p, _)| p == prov) {
            candidates.push((prov.to_string(), model.to_string()));
        }
    }

    let messages = vec![Message::system(system), Message::user(user_prompt)];
    let mut last_err = String::new();

    for (prov, model) in candidates {
        let has_creds = !cred_manager.get_credentials(&prov).await.is_empty()
            || (prov == "antigravity" && !cred_manager.get_credentials("agy").await.is_empty());
        if !has_creds {
            continue;
        }
        let Ok(client) = GenericProviderClient::new(
            prov.clone(),
            model.clone(),
            cred_manager.clone(),
            discovery.clone(),
            config.providers.get(&prov),
        ) else {
            continue;
        };
        let options = CompletionOptions {
            model: Some(model.clone()),
            temperature: Some(0.4),
            max_tokens: Some(max_tokens.max(512)),
            reasoning_effort: None,
            ..Default::default()
        };
        match tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.complete(&messages, &[], &options),
        )
        .await
        {
            Ok(Ok(msg)) => {
                if let Some(content) = msg.content {
                    let trimmed = content.trim();
                    if !trimmed.is_empty() && trimmed != NO_TEXT_RESPONSE {
                        return Ok(trimmed.to_string());
                    }
                }
                last_err = format!("{prov}/{model} returned empty text");
            }
            Ok(Err(e)) => {
                tracing::debug!(provider = %prov, model = %model, "fast voice text failed: {e}");
                last_err = format!("{prov}/{model}: {e}");
            }
            Err(_) => {
                tracing::debug!(provider = %prov, model = %model, "fast voice text timed out");
                last_err = format!("{prov}/{model} timed out");
            }
        }
    }

    Err(HarnessError::VoiceError(if last_err.is_empty() {
        "No fast voice model credentials available".to_string()
    } else {
        last_err
    }))
}

/// Generate a brief, natural spoken acknowledgment before or as Luna begins executing tools.
pub async fn generate_pre_task_voice_ack(
    user_query: &str,
    tool_hint: Option<&str>,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
) -> Option<String> {
    let trimmed_query = user_query.trim();
    if trimmed_query.is_empty() {
        return None;
    }

    let (system, prompt) = if let Some(hint) = tool_hint {
        (
            "You are Luna, a helpful AI voice assistant. Generate ONE ultra-brief, natural spoken acknowledgment (4 to 10 words) saying you're starting the user's task, like \"Alright, on it — cleaning up the build artifacts now.\" or \"Sure thing, let me check those files.\" Output ONLY the spoken line with no quotes, no markdown, no preamble.",
            format!("User request: {trimmed_query}\nFirst tool starting: {hint}"),
        )
    } else {
        (
            "You are Luna, a helpful AI voice assistant. If the user's message is an action, command, coding, file, or multi-step task request that takes a moment to execute, output ONE ultra-brief spoken acknowledgment (4 to 10 words, e.g. \"Alright, on it — checking that now.\"). If it is a simple greeting, chat, or direct factual question that doesn't run a task, output exactly SKIP. Output nothing else.",
            format!("User message: {trimmed_query}"),
        )
    };

    match generate_fast_voice_text(system, &prompt, 48, config, cred_manager).await {
        Ok(raw) => {
            let cleaned = clean_text_for_speech(raw.trim().trim_matches('"'));
            if cleaned.is_empty() || cleaned.eq_ignore_ascii_case("skip") {
                if tool_hint.is_some() {
                    Some("Alright, on it — working on that now.".to_string())
                } else {
                    None
                }
            } else {
                Some(cleaned)
            }
        }
        Err(_) => tool_hint.map(|_| "Alright, on it — working on that now.".to_string()),
    }
}

/// Generate a separate conversational spoken response for rich/long/tabular written answers.
pub async fn generate_separate_voice_summary(
    user_query: &str,
    full_reply: &str,
    tool_count: usize,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
) -> String {
    let truncated_reply: String = full_reply.chars().take(5000).collect();
    let system = "You are Luna's spoken voice output. Luna's full written response (which may include Markdown tables, code blocks, lists, or long technical details) is already displayed in the chat UI. Write a SEPARATE, natural 1 to 3 sentence spoken response to say out loud to the user. Summarize the outcome, key answer, or main numbers conversationally using contractions. Never read raw tables, code syntax, file trees, or URLs aloud. If the written reply has a table, code, or detailed breakdown, mention briefly that the full details are in the chat. Output ONLY the spoken text.";
    let prompt = format!(
        "User asked: {}\nTools run: {}\n\nFull written reply shown in chat:\n{}",
        user_query.trim(),
        tool_count,
        truncated_reply
    );

    match generate_fast_voice_text(system, &prompt, 160, config, cred_manager).await {
        Ok(raw) => {
            let cleaned = clean_text_for_speech(&raw);
            if cleaned.is_empty() {
                fallback_voice_summary(full_reply)
            } else {
                cleaned
            }
        }
        Err(_) => fallback_voice_summary(full_reply),
    }
}

/// Spawn an immediate pre-task spoken acknowledgment without blocking the turn or UI loop.
pub fn spawn_pre_task_voice_ack(
    user_query: String,
    tool_hint: Option<String>,
    config: Config,
    cred_manager: Arc<CredentialManager>,
    speech_queue: VoiceSpeechQueue,
    voice_tx: Option<tokio::sync::mpsc::UnboundedSender<VoiceEvent>>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        if cancel.is_cancelled() || speech_queue.is_turn_muted() {
            return;
        }
        let Some(ack) =
            generate_pre_task_voice_ack(&user_query, tool_hint.as_deref(), &config, &cred_manager)
                .await
        else {
            return;
        };
        if cancel.is_cancelled() || speech_queue.is_turn_muted() {
            return;
        }
        if let Some(ref tx) = voice_tx {
            let _ = tx.send(VoiceEvent::Notice(format!("🔊 {ack}")));
        }
        speech_queue.enqueue(&ack, &config, &cred_manager, voice_tx);
    });
}

/// Speak Luna's final turn response: direct TTS for short conversational prose, or a separate
/// spoken summary when the written response is long or contains tables/code/lists.
#[allow(clippy::too_many_arguments)]
pub fn spawn_separate_voice_reply(
    user_query: String,
    full_reply: String,
    tool_count: usize,
    config: Config,
    cred_manager: Arc<CredentialManager>,
    speech_queue: VoiceSpeechQueue,
    voice_tx: Option<tokio::sync::mpsc::UnboundedSender<VoiceEvent>>,
    cancel: CancellationToken,
) {
    if full_reply.trim().is_empty() || cancel.is_cancelled() || speech_queue.is_turn_muted() {
        return;
    }

    if !needs_separate_voice_summary(&full_reply) {
        let cleaned = clean_text_for_speech(&full_reply);
        for sentence in split_sentences_for_tts(&cleaned) {
            speech_queue.enqueue(&sentence, &config, &cred_manager, voice_tx.clone());
        }
        return;
    }

    tokio::spawn(async move {
        if cancel.is_cancelled() || speech_queue.is_turn_muted() {
            return;
        }
        let spoken = generate_separate_voice_summary(
            &user_query,
            &full_reply,
            tool_count,
            &config,
            &cred_manager,
        )
        .await;
        if cancel.is_cancelled() || speech_queue.is_turn_muted() || spoken.trim().is_empty() {
            return;
        }
        if let Some(ref tx) = voice_tx {
            let _ = tx.send(VoiceEvent::Notice(format!("🔊 {spoken}")));
        }
        for sentence in split_sentences_for_tts(&spoken) {
            if cancel.is_cancelled() || speech_queue.is_turn_muted() {
                break;
            }
            speech_queue.enqueue(&sentence, &config, &cred_manager, voice_tx.clone());
        }
    });
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
        assert!(!needs_separate_voice_summary(
            "Twelve times three is thirty-six."
        ));
        assert!(!needs_separate_voice_summary(
            "Hey! I'm doing great, how can I help today?"
        ));
        assert!(needs_separate_voice_summary(
            "Done:\n| Path | Size |\n|---|---|\n| target | 1.2 GB |"
        ));
        assert!(needs_separate_voice_summary(
            "Here is the code:\n```rust\nfn main() {}\n```"
        ));
        assert!(needs_separate_voice_summary(
            "Results:\n- Item one\n- Item two"
        ));
        let long = "Sentence one is moderately detailed here. ".repeat(10);
        assert!(needs_separate_voice_summary(&long));
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
        let config = harness_core::config::resolve(&layers)
            .unwrap()
            .into_config();
        let cred_manager =
            Arc::new(CredentialManager::new(Some(config.paths.credentials.clone())).unwrap());

        let t0 = std::time::Instant::now();
        let ack = generate_pre_task_voice_ack(
            "clean up build artifacts",
            Some("Run terminal: cargo clean"),
            &config,
            &cred_manager,
        )
        .await;
        println!("ACK ({:?}): {:?}", t0.elapsed(), ack);
        assert!(ack.is_some());

        let t1 = std::time::Instant::now();
        let sample_table = "Here are the workspace crates:\n\n| Crate | Purpose |\n|---|---|\n| harness-core | Agent loop & tools |\n| harness-cli | TUI & CLI |\n| harness-web | Web server |\n";
        let summary = generate_separate_voice_summary(
            "List the workspace crates in a table",
            sample_table,
            1,
            &config,
            &cred_manager,
        )
        .await;
        println!("SUMMARY ({:?}): {:?}", t1.elapsed(), summary);
        assert!(!summary.is_empty());
        assert!(!summary.contains('|'));
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
