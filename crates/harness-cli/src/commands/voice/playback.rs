//! Persistent, interruptible audio playback queue and streaming sink.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use harness_core::Config;
use harness_providers::CredentialManager;

use super::VoiceEvent;

pub enum AudioThreadCmd {
    AppendPcm {
        generation: u64,
        rate: u32,
        samples: Vec<i16>,
    },
    #[allow(dead_code)]
    AppendEncoded {
        generation: u64,
        bytes: Vec<u8>,
    },
    ClearAndReset,
}

#[derive(Clone, Copy)]
pub struct GenAudioSender<'a> {
    pub tx: &'a std::sync::mpsc::Sender<AudioThreadCmd>,
    pub generation: u64,
    pub cancel: &'a CancellationToken,
}

impl<'a> GenAudioSender<'a> {
    pub fn send_pcm(&self, rate: u32, samples: Vec<i16>) {
        if !self.cancel.is_cancelled() && !samples.is_empty() {
            let _ = self.tx.send(AudioThreadCmd::AppendPcm {
                generation: self.generation,
                rate,
                samples,
            });
        }
    }

    #[allow(dead_code)]
    pub fn send_encoded(&self, bytes: Vec<u8>) {
        if !self.cancel.is_cancelled() && !bytes.is_empty() {
            let _ = self.tx.send(AudioThreadCmd::AppendEncoded {
                generation: self.generation,
                bytes,
            });
        }
    }
}

pub struct SpeechTask {
    pub text: String,
    pub config: Config,
    pub cred_manager: Arc<CredentialManager>,
    pub voice_tx: Option<tokio::sync::mpsc::UnboundedSender<VoiceEvent>>,
    pub generation: u64,
}

/// Persistent, interruptible, streaming TTS playback engine.
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
                                    if let Some(ref s) = sink {
                                        s.stop();
                                    }
                                    sink = None;
                                    sink_busy_thread.store(false, Ordering::SeqCst);
                                    is_playing_audio.store(false, Ordering::SeqCst);
                                }
                                AudioThreadCmd::AppendPcm {
                                    generation,
                                    rate,
                                    samples,
                                } => {
                                    if generation != active_gen_audio.load(Ordering::SeqCst)
                                        || samples.is_empty()
                                    {
                                        continue;
                                    }
                                    if let Err(e) = ensure_sink(&mut output, &mut sink) {
                                        tracing::warn!("{e}");
                                        continue;
                                    }
                                    if let Some(ref s) = sink {
                                        let source =
                                            rodio::buffer::SamplesBuffer::new(1, rate, samples);
                                        s.append(source);
                                        if s.is_paused() {
                                            s.play();
                                        }
                                        sink_busy_thread.store(true, Ordering::SeqCst);
                                        is_playing_audio.store(true, Ordering::SeqCst);
                                    }
                                }
                                AudioThreadCmd::AppendEncoded { generation, bytes } => {
                                    if generation != active_gen_audio.load(Ordering::SeqCst)
                                        || bytes.is_empty()
                                    {
                                        continue;
                                    }
                                    if let Err(e) = ensure_sink(&mut output, &mut sink) {
                                        tracing::warn!("{e}");
                                        continue;
                                    }
                                    if let Some(ref s) = sink {
                                        let cursor = std::io::Cursor::new(bytes);
                                        if let Ok(source) = rodio::Decoder::new(cursor) {
                                            s.append(source);
                                            if s.is_paused() {
                                                s.play();
                                            }
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

                let in_flight = if let Some(ref s) = sink {
                    !s.empty()
                } else {
                    false
                };
                sink_busy_thread.store(in_flight, Ordering::SeqCst);
                let streaming = stream_busy_thread.load(Ordering::SeqCst);
                is_playing_audio.store(in_flight || streaming, Ordering::SeqCst);
            }
        });

        // Dedicated streaming worker: consumes queued sentence tasks in sequential order
        let active_gen_worker = active_gen.clone();
        let turn_muted_worker = turn_muted.clone();
        let stream_busy_worker = stream_busy.clone();
        let is_playing_worker = is_playing.clone();
        let audio_tx_worker = audio_tx.clone();
        let active_cancel_worker = active_cancel.clone();

        let stream_loop = async move {
            while let Some(task) = task_rx.recv().await {
                let cur_gen = active_gen_worker.load(Ordering::SeqCst);
                if task.generation != cur_gen || turn_muted_worker.load(Ordering::SeqCst) {
                    continue;
                }

                let cancel = {
                    let guard = active_cancel_worker
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    guard.clone()
                };
                if cancel.is_cancelled() {
                    continue;
                }

                stream_busy_worker.store(true, Ordering::SeqCst);
                is_playing_worker.store(true, Ordering::SeqCst);

                if let Some(ref tx) = task.voice_tx {
                    let _ = tx.send(VoiceEvent::Notice(format!("🔊 {}", task.text)));
                }

                let audio_sender = GenAudioSender {
                    tx: &audio_tx_worker,
                    generation: task.generation,
                    cancel: &cancel,
                };

                super::tts::stream_tts_sentence_to_audio(
                    &task.text,
                    &task.config,
                    &task.cred_manager,
                    task.voice_tx.as_ref(),
                    audio_sender,
                )
                .await;

                stream_busy_worker.store(false, Ordering::SeqCst);
            }
        };

        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(stream_loop);
        } else {
            std::thread::spawn(move || {
                if let Ok(rt) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    rt.block_on(stream_loop);
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
        if self.turn_muted.load(Ordering::SeqCst) {
            return;
        }
        let generation = self.active_gen.load(Ordering::SeqCst);
        let _ = self.task_tx.send(SpeechTask {
            text: text.to_string(),
            config: config.clone(),
            cred_manager: cred_manager.clone(),
            voice_tx,
            generation,
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
