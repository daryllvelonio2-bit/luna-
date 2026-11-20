//! Background microphone audio capture via native CLI (pw-record / parec / arecord) or CPAL.
//! Reconstructed from binary analysis of agy audio subsystem (`google3/third_party/jetski/cli/audio`).

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use harness_core::Config;
use harness_providers::{CredentialManager, ModelDiscovery};

use super::VoiceEvent;

/// Locate host audio recording CLI following `audio.selectTool` in agy architecture.
///
/// Order of evaluation:
/// 1. `pw-record` (PipeWire)
/// 2. `parec` (PulseAudio)
/// 3. `arecord` (ALSA fallback)
pub fn select_recorder_tool() -> Option<String> {
    for tool in ["pw-record", "parec", "arecord"] {
        if super::playback::find_in_path(tool).is_some() {
            return Some(tool.to_string());
        }
    }
    None
}

/// Run the continuous background voice listener.
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

    // Check for native recorder tool (pw-record, parec, arecord) matching agy hostCapture
    let recorder_tool = select_recorder_tool();

    std::thread::spawn(move || {
        if let Some(ref tool) = recorder_tool {
            let (prog, args): (&str, Vec<String>) = match tool.as_str() {
                "pw-record" => (
                    "pw-record",
                    vec![
                        "--rate".into(),
                        "16000".into(),
                        "--channels".into(),
                        "1".into(),
                        "--format".into(),
                        "s16".into(),
                        "--raw".into(),
                        "-".into(),
                    ],
                ),
                "parec" => (
                    "parec",
                    vec![
                        "--rate=16000".into(),
                        "--channels=1".into(),
                        "--format=s16le".into(),
                        "--raw".into(),
                    ],
                ),
                _ => (
                    "arecord",
                    vec![
                        "-t".into(),
                        "raw".into(),
                        "-f".into(),
                        "S16_LE".into(),
                        "-r".into(),
                        "16000".into(),
                        "-c".into(),
                        "1".into(),
                        "-q".into(),
                        "-".into(),
                    ],
                ),
            };

            let mut child = match std::process::Command::new(prog)
                .args(&args)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("Failed to spawn native recorder {prog}: {e}");
                    let _ = err_tx.send(VoiceEvent::Error(format!(
                        "Failed to start microphone ({prog}): {e}"
                    )));
                    return;
                }
            };

            let mut stdout = match child.stdout.take() {
                Some(s) => s,
                None => {
                    let _ = child.kill();
                    return;
                }
            };

            let _ = sample_rate_tx.send(16_000);
            let _ = err_tx.send(VoiceEvent::Notice(format!(
                "🎙 Microphone active ({tool} · 16000Hz)"
            )));

            let mut raw_buf = [0u8; 1024];
            let mut carry: Option<u8> = None;
            while !cancel_thread.is_cancelled() {
                match stdout.read(&mut raw_buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let mut buf = Vec::with_capacity(n + 1);
                        if let Some(b) = carry.take() {
                            buf.push(b);
                        }
                        buf.extend_from_slice(&raw_buf[..n]);
                        if buf.len() % 2 == 1 {
                            carry = buf.pop();
                        }
                        #[allow(clippy::chunks_exact_to_as_chunks)]
                        let samples: Vec<i16> = buf
                            .chunks_exact(2)
                            .map(|c| i16::from_le_bytes([c[0], c[1]]))
                            .collect();
                        if !samples.is_empty() {
                            let _ = audio_sample_tx.send(samples);
                        }
                    }
                    Err(_) => break,
                }
            }

            let _ = child.kill();
            let _ = child.wait();
            return;
        }

        // CPAL fallback if no native CLI recorder is found on host
        let host = cpal::default_host();
        let Some(device) = host.default_input_device() else {
            tracing::warn!("No default audio input device found for voice listener");
            let _ = err_tx.send(VoiceEvent::Error(
                "No microphone recorder found. Install one with: sudo apt-get install -y alsa-utils (arecord), or use PipeWire's pw-record.".to_string(),
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

    // Natural responsive end-of-utterance pause matching agy voiceTurn (default 650ms)
    let silence_ms = if config.voice.silence_threshold_ms == 800 {
        650
    } else {
        config.voice.silence_threshold_ms.max(300)
    };
    let silence_duration = std::time::Duration::from_millis(silence_ms);
    let min_samples = (sample_rate as usize) / 3; // ~330ms minimum utterance
    let max_samples = (sample_rate as usize) * 30; // 30s safety cap
    let pre_roll_max_samples = (sample_rate as usize) / 3; // ~330ms pre-roll

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
                    tts_cooldown_until = Some(std::time::Instant::now() + std::time::Duration::from_millis(60));
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
                            let trimmed_pcm = trim_trailing_silence(&speech_buffer, sample_rate, base_threshold * 0.4);
                            let resampled = downsample_to_16khz(trimmed_pcm, sample_rate);
                            let wav_res = harness_voice::AudioProcessor::samples_to_wav(&resampled, 16_000);
                            speech_buffer.clear();
                            if let Ok(wav_bytes) = wav_res {
                                match super::stt::transcribe_audio_bytes(&wav_bytes, &config, &cred_manager).await {
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
pub fn trim_trailing_silence(samples: &[i16], sample_rate: u32, threshold: f32) -> &[i16] {
    let win = ((sample_rate as usize) / 50).max(160);
    let keep_pad = ((sample_rate as usize) / 8).max(win);
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

pub fn is_blank_transcript(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    lower.is_empty()
        || lower == "[blank_audio]"
        || lower == "(silence)"
        || lower == "[silence]"
        || lower == "."
        || lower == "0"
        || lower == "00"
        || lower == "tone"
        || lower == "[tone]"
}
