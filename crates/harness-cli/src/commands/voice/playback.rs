//! Persistent, interruptible audio playback queue and streaming sink.
//! Reconstructed from binary analysis of agy audio subsystem (`google3/third_party/jetski/cli/audio`).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

use harness_core::Config;
use harness_providers::CredentialManager;

use super::VoiceEvent;

/// Search for an executable binary in the system PATH.
pub fn find_in_path(cmd: &str) -> Option<std::path::PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for p in std::env::split_paths(&path_var) {
        let candidate = p.join(cmd);
        if candidate.is_file() {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = candidate.metadata() {
                    if meta.permissions().mode() & 0o111 != 0 {
                        return Some(candidate);
                    }
                }
            }
            #[cfg(not(unix))]
            return Some(candidate);
        }
    }
    None
}

/// Locate host audio player following `audio.selectPlayerTool` in agy architecture.
///
/// Order of evaluation:
/// 1. If `PULSE_SERVER` is set, prefers `paplay`. If absent, warns about pulseaudio-utils.
/// 2. Probes `$PATH` in order: `pw-cat` (PipeWire), `paplay` (PulseAudio), `aplay` (ALSA).
pub fn select_player_tool() -> Option<String> {
    if let Ok(server) = std::env::var("PULSE_SERVER") {
        if !server.trim().is_empty() {
            if find_in_path("paplay").is_some() {
                return Some("paplay".to_string());
            }
            tracing::warn!(
                "Audio forwarding is configured (PULSE_SERVER) but 'paplay' isn't installed. \
                 Install with: sudo apt-get install -y pulseaudio-utils"
            );
        }
    }

    for tool in ["pw-cat", "paplay", "aplay"] {
        if find_in_path(tool).is_some() {
            return Some(tool.to_string());
        }
    }
    None
}

/// Dedicated streaming process wrapper for low-latency playback via PipeWire/PulseAudio/ALSA.
pub struct NativePlayerProcess {
    #[allow(dead_code)]
    pub tool: String,
    pub rate: u32,
    child: std::process::Child,
    stdin: std::process::ChildStdin,
    last_write: std::time::Instant,
    play_until: std::time::Instant,
}

/// Idle timeout after which the warm audio player process is terminated to release audio hardware.
pub const DEFAULT_PLAYER_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(180); // 3 minutes

impl NativePlayerProcess {
    pub fn spawn(tool: &str, rate: u32) -> Result<Self, String> {
        let (prog, args): (&str, Vec<String>) = match tool {
            "pw-cat" => (
                "pw-cat",
                vec![
                    "--playback".into(),
                    "--rate".into(),
                    rate.to_string(),
                    "--channels".into(),
                    "1".into(),
                    "--format".into(),
                    "s16".into(),
                    "--latency=20ms".into(),
                    "--raw".into(),
                    "-".into(),
                ],
            ),
            "paplay" => (
                "paplay",
                vec![
                    format!("--rate={rate}"),
                    "--channels=1".into(),
                    "--format=s16le".into(),
                    "--latency-msec=20".into(),
                    "--raw".into(),
                ],
            ),
            _ => (
                "aplay",
                vec![
                    "-t".into(),
                    "raw".into(),
                    "-f".into(),
                    "S16_LE".into(),
                    "-r".into(),
                    rate.to_string(),
                    "-c".into(),
                    "1".into(),
                    "-B".into(),
                    "20000".into(),
                    "-q".into(),
                    "-".into(),
                ],
            ),
        };

        let mut child = std::process::Command::new(prog)
            .args(&args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start native audio player {prog}: {e}"))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Failed to capture stdin for audio player".to_string())?;

        let now = std::time::Instant::now();
        Ok(Self {
            tool: tool.to_string(),
            rate,
            child,
            stdin,
            last_write: now,
            play_until: now,
        })
    }

    pub fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub fn idle_elapsed(&self) -> std::time::Duration {
        self.last_write.elapsed()
    }

    pub fn is_playing(&self) -> bool {
        std::time::Instant::now() < self.play_until + std::time::Duration::from_millis(250)
    }

    pub fn write_samples(&mut self, samples: &[i16]) -> std::io::Result<()> {
        use std::io::Write;
        if samples.is_empty() {
            return Ok(());
        }
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for &s in samples {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        self.stdin.write_all(&bytes)?;
        self.stdin.flush()?;
        let now = std::time::Instant::now();
        let dur = std::time::Duration::from_secs_f64(samples.len() as f64 / self.rate as f64);
        self.play_until = self.play_until.max(now) + dur;
        self.last_write = now;
        Ok(())
    }

    pub fn kill(&mut self) {
        self.play_until = std::time::Instant::now();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub enum AudioThreadCmd {
    WarmUp {
        rate: u32,
    },
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
    pub tx: &'a tokio::sync::mpsc::UnboundedSender<AudioThreadCmd>,
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

        let stream_busy = Arc::new(AtomicBool::new(false));
        let is_playing_audio = is_playing.clone();
        let active_gen_audio = active_gen.clone();
        let stream_busy_thread = stream_busy.clone();

        // Dedicated low-latency playback thread matching agy streamPlayer architecture:
        // Prefers native CLI pipeline (pw-cat / paplay / aplay) with instant kill on barge-in.
        std::thread::spawn(move || {
            let player_tool = select_player_tool();
            let mut native_player: Option<NativePlayerProcess> = None;
            let mut last_sample_time: Option<std::time::Instant> = None;

            // Rodio fallback only if no host CLI audio tool is available
            let mut rodio_output: Option<(rodio::OutputStream, rodio::OutputStreamHandle)> = None;
            let mut rodio_sink: Option<rodio::Sink> = None;

            let ensure_rodio_sink =
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
                match audio_rx.recv_timeout(std::time::Duration::from_millis(25)) {
                    Ok(cmd) => {
                        let mut cmds = vec![cmd];
                        while let Ok(more) = audio_rx.try_recv() {
                            cmds.push(more);
                        }
                        for c in cmds {
                            match c {
                                AudioThreadCmd::WarmUp { rate } => {
                                    if let Some(ref tool) = player_tool {
                                        let needs_new = match native_player {
                                            Some(ref mut p) => p.rate != rate || !p.is_alive(),
                                            None => true,
                                        };
                                        if needs_new {
                                            if let Some(mut old) = native_player.take() {
                                                old.kill();
                                            }
                                            match NativePlayerProcess::spawn(tool, rate) {
                                                Ok(p) => native_player = Some(p),
                                                Err(e) => tracing::warn!("{e}"),
                                            }
                                        }
                                    }
                                }
                                AudioThreadCmd::ClearAndReset => {
                                    if let Some(mut p) = native_player.take() {
                                        p.kill();
                                    }
                                    if let Some(ref s) = rodio_sink {
                                        s.stop();
                                    }
                                    rodio_sink = None;
                                    last_sample_time = None;
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

                                    if let Some(ref tool) = player_tool {
                                        let needs_new = match native_player {
                                            Some(ref mut p) => p.rate != rate || !p.is_alive(),
                                            None => true,
                                        };
                                        if needs_new {
                                            if let Some(mut old) = native_player.take() {
                                                old.kill();
                                            }
                                            match NativePlayerProcess::spawn(tool, rate) {
                                                Ok(p) => native_player = Some(p),
                                                Err(e) => tracing::warn!("{e}"),
                                            }
                                        }

                                        if let Some(ref mut p) = native_player {
                                            if let Err(e) = p.write_samples(&samples) {
                                                tracing::warn!("Failed writing samples to player, respawning: {e}");
                                                p.kill();
                                                native_player = None;
                                                if let Ok(mut new_p) =
                                                    NativePlayerProcess::spawn(tool, rate)
                                                {
                                                    let _ = new_p.write_samples(&samples);
                                                    native_player = Some(new_p);
                                                }
                                            }
                                            last_sample_time = Some(std::time::Instant::now());
                                            is_playing_audio.store(true, Ordering::SeqCst);
                                        }
                                    } else {
                                        if let Err(e) =
                                            ensure_rodio_sink(&mut rodio_output, &mut rodio_sink)
                                        {
                                            tracing::warn!("{e}");
                                            continue;
                                        }
                                        if let Some(ref s) = rodio_sink {
                                            let source =
                                                rodio::buffer::SamplesBuffer::new(1, rate, samples);
                                            s.append(source);
                                            if s.is_paused() {
                                                s.play();
                                            }
                                            last_sample_time = Some(std::time::Instant::now());
                                            is_playing_audio.store(true, Ordering::SeqCst);
                                        }
                                    }
                                }
                                AudioThreadCmd::AppendEncoded { generation, bytes } => {
                                    if generation != active_gen_audio.load(Ordering::SeqCst)
                                        || bytes.is_empty()
                                    {
                                        continue;
                                    }
                                    if let Err(e) =
                                        ensure_rodio_sink(&mut rodio_output, &mut rodio_sink)
                                    {
                                        tracing::warn!("{e}");
                                        continue;
                                    }
                                    if let Some(ref s) = rodio_sink {
                                        let cursor = std::io::Cursor::new(bytes);
                                        if let Ok(source) = rodio::Decoder::new(cursor) {
                                            s.append(source);
                                            if s.is_paused() {
                                                s.play();
                                            }
                                            last_sample_time = Some(std::time::Instant::now());
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

                // Check whether playback has settled
                let streaming = stream_busy_thread.load(Ordering::SeqCst);
                let audio_busy = if let Some(ref mut p) = native_player {
                    p.is_playing()
                } else if let Some(ref s) = rodio_sink {
                    !s.empty()
                } else {
                    false
                };

                is_playing_audio.store(streaming || audio_busy, Ordering::SeqCst);

                // Idle check: terminate warm native player process after 3 minutes of inactivity
                if let Some(ref mut p) = native_player {
                    if p.idle_elapsed() >= DEFAULT_PLAYER_IDLE_TIMEOUT {
                        tracing::debug!("Audio player idle for 3 minutes; terminating process to release audio hardware.");
                        p.kill();
                        native_player = None;
                    }
                }
            }
        });

        // Dedicated concurrent pipelining worker:
        // Spawns up to 3 lookahead synthesis streams in parallel and drains them in strict FIFO order
        // directly to the warm audio player so inter-sentence gaps drop to ~0ms.
        let active_gen_worker = active_gen.clone();
        let turn_muted_worker = turn_muted.clone();
        let stream_busy_worker = stream_busy.clone();
        let is_playing_worker = is_playing.clone();
        let audio_tx_worker = audio_tx.clone();
        let active_cancel_worker = active_cancel.clone();

        struct OrderedAudioSlot {
            generation: u64,
            text: String,
            voice_tx: Option<tokio::sync::mpsc::UnboundedSender<VoiceEvent>>,
            rx: tokio::sync::mpsc::UnboundedReceiver<AudioThreadCmd>,
        }

        let stream_loop = async move {
            let (ordered_tx, mut ordered_rx) =
                tokio::sync::mpsc::unbounded_channel::<OrderedAudioSlot>();
            let prefetch_sem = Arc::new(tokio::sync::Semaphore::new(3));

            let active_gen_disp = active_gen_worker.clone();
            let turn_muted_disp = turn_muted_worker.clone();
            let active_cancel_disp = active_cancel_worker.clone();

            // Stage 1: Dispatcher — starts synthesizing queued chunks concurrently
            tokio::spawn(async move {
                while let Some(task) = task_rx.recv().await {
                    let cur_gen = active_gen_disp.load(Ordering::SeqCst);
                    if task.generation != cur_gen || turn_muted_disp.load(Ordering::SeqCst) {
                        continue;
                    }
                    let cancel = {
                        let guard = active_cancel_disp.lock().unwrap_or_else(|e| e.into_inner());
                        guard.clone()
                    };
                    if cancel.is_cancelled() {
                        continue;
                    }

                    let Ok(permit) = prefetch_sem.clone().acquire_owned().await else {
                        break;
                    };
                    let cur_gen_after = active_gen_disp.load(Ordering::SeqCst);
                    if task.generation != cur_gen_after || cancel.is_cancelled() {
                        drop(permit);
                        continue;
                    }

                    let (chunk_tx, chunk_rx) =
                        tokio::sync::mpsc::unbounded_channel::<AudioThreadCmd>();
                    if ordered_tx
                        .send(OrderedAudioSlot {
                            generation: task.generation,
                            text: task.text.clone(),
                            voice_tx: task.voice_tx.clone(),
                            rx: chunk_rx,
                        })
                        .is_err()
                    {
                        drop(permit);
                        break;
                    }

                    tokio::spawn(async move {
                        let _permit = permit;
                        let audio_sender = GenAudioSender {
                            tx: &chunk_tx,
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
                    });
                }
            });

            // Stage 2: Ordered playback drainer — forwards prefetched & live PCM chunks strictly in order
            while let Some(mut slot) = ordered_rx.recv().await {
                let cur_gen = active_gen_worker.load(Ordering::SeqCst);
                if slot.generation != cur_gen || turn_muted_worker.load(Ordering::SeqCst) {
                    continue;
                }

                stream_busy_worker.store(true, Ordering::SeqCst);
                is_playing_worker.store(true, Ordering::SeqCst);

                if let Some(ref tx) = slot.voice_tx {
                    let _ = tx.send(VoiceEvent::Notice(format!("🔊 {}", slot.text)));
                }

                while let Some(cmd) = slot.rx.recv().await {
                    if slot.generation != active_gen_worker.load(Ordering::SeqCst)
                        || turn_muted_worker.load(Ordering::SeqCst)
                    {
                        break;
                    }
                    let _ = audio_tx_worker.send(cmd);
                }

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

        let queue = Self {
            task_tx,
            audio_tx,
            active_gen,
            is_playing,
            turn_muted,
            active_cancel,
        };
        queue.warm_up(24_000);
        queue
    }

    /// Pre-spawn and warm up the native audio player process to eliminate playback latency.
    pub fn warm_up(&self, rate: u32) {
        let _ = self.audio_tx.send(AudioThreadCmd::WarmUp { rate });
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
        super::live_tts::drop_live_tts_session();
        let _ = self.audio_tx.send(AudioThreadCmd::ClearAndReset);
        self.is_playing.store(false, Ordering::SeqCst);
    }
}
