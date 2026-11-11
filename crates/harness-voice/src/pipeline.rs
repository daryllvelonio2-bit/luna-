use futures_util::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::traits::{SpeechToText, TextToSpeech, WakeWordDetector};
use harness_core::{Agent, HarnessError, MemoryEngine, ModelProvider, NotesEngine, ToolRegistry};

/// Pause before reopening the microphone after a detector error, so a broken device does not spin
/// the listening loop.
const WAKE_WORD_ERROR_BACKOFF: Duration = Duration::from_millis(500);

pub struct VoicePipelineConfig {
    pub wake_word: String,
    pub silence_threshold_ms: u64,
    pub enable_barge_in: bool,
}

impl Default for VoicePipelineConfig {
    fn default() -> Self {
        Self {
            wake_word: "Luna".to_string(),
            silence_threshold_ms: 800,
            enable_barge_in: true,
        }
    }
}

pub struct VoiceSession<W, S, T>
where
    W: WakeWordDetector + 'static,
    S: SpeechToText + 'static,
    T: TextToSpeech + 'static,
{
    detector: Arc<W>,
    stt: Arc<S>,
    tts: Arc<T>,
    config: VoicePipelineConfig,
    active_playback_cancel: Mutex<Option<CancellationToken>>,
}

impl<W, S, T> VoiceSession<W, S, T>
where
    W: WakeWordDetector + 'static,
    S: SpeechToText + 'static,
    T: TextToSpeech + 'static,
{
    pub fn new(detector: Arc<W>, stt: Arc<S>, tts: Arc<T>, config: VoicePipelineConfig) -> Self {
        Self {
            detector,
            stt,
            tts,
            config,
            active_playback_cancel: Mutex::new(None),
        }
    }

    /// Interrupt any ongoing assistant audio output (barge-in)
    pub async fn interrupt_playback(&self) {
        let mut lock = self.active_playback_cancel.lock().await;
        if let Some(cancel) = lock.take() {
            info!("Barge-in: Interrupting assistant audio playback!");
            cancel.cancel();
        }
    }

    /// Run the hands-free voice interaction loop
    pub async fn run_hands_free_loop<P, M, N, R>(
        &self,
        agent: Arc<Agent<P, M, N, R>>,
        cancel: CancellationToken,
    ) -> Result<(), HarnessError>
    where
        P: ModelProvider + 'static,
        M: MemoryEngine + 'static,
        N: NotesEngine + 'static,
        R: ToolRegistry + 'static,
    {
        info!(
            "Starting hands-free voice loop. Listening for wake word '{}'...",
            self.config.wake_word
        );

        while !cancel.is_cancelled() {
            // 1. Wait for wake word "Luna" in background
            debug!("Waiting for wake word '{}'...", self.config.wake_word);
            tokio::select! {
                _ = cancel.cancelled() => break,
                res = self.detector.wait_for_wake_word(&cancel) => {
                    if let Err(e) = res {
                        warn!("Wake word detection encountered error: {}", e);
                        tokio::time::sleep(WAKE_WORD_ERROR_BACKOFF).await;
                        continue;
                    }
                }
            }

            // If user utters wake word while audio is playing, interrupt immediately
            self.interrupt_playback().await;

            info!(
                "Wake word '{}' detected! Ingesting user speech via VAD...",
                self.config.wake_word
            );

            // 2. Transcribe speech bytes from input device
            // Mock or live captured WAV bytes
            let dummy_wav = vec![0u8; 1024];
            let user_text = match self.stt.transcribe(&dummy_wav).await {
                Ok(text) => text,
                Err(e) => {
                    warn!("STT transcription failed: {}", e);
                    continue;
                }
            };

            if user_text.trim().is_empty() {
                debug!("Empty transcription, returning to background listening.");
                continue;
            }

            info!("User spoken query: '{}'", user_text);

            // 3. Run Agent reasoning loop
            let turn_cancel = cancel.child_token();
            let agent_clone = agent.clone();
            let query = user_text.clone();

            let assistant_reply = agent_clone.run_task(&query, turn_cancel.clone()).await?;
            info!("Agent response: '{}'", assistant_reply);

            // 4. Stream TTS audio playback
            let playback_cancel = CancellationToken::new();
            {
                let mut lock = self.active_playback_cancel.lock().await;
                *lock = Some(playback_cancel.clone());
            }

            let text_stream = futures_util::stream::once(async move { assistant_reply });
            if let Ok(mut audio_stream) = self.tts.synthesize_stream(Box::pin(text_stream)).await {
                while let Some(chunk) = audio_stream.next().await {
                    if playback_cancel.is_cancelled() {
                        info!("Audio playback aborted due to barge-in.");
                        break;
                    }
                    debug!("Playing audio chunk of {} samples", chunk.len());
                }
            }

            {
                let mut lock = self.active_playback_cancel.lock().await;
                *lock = None;
            }
        }

        info!("Hands-free voice loop stopped.");
        Ok(())
    }
}
