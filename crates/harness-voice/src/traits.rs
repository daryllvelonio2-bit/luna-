use async_trait::async_trait;
use futures_util::stream::BoxStream;
use tokio_util::sync::CancellationToken;

use harness_core::HarnessError;

#[async_trait]
pub trait WakeWordDetector: Send + Sync {
    /// Listens for the wake word "Luna" in background audio
    async fn wait_for_wake_word(&self, cancel: &CancellationToken) -> Result<(), HarnessError>;
}

#[async_trait]
pub trait SpeechToText: Send + Sync {
    /// Transcribes captured audio bytes into text
    async fn transcribe(&self, wav_bytes: &[u8]) -> Result<String, HarnessError>;
}

#[async_trait]
pub trait TextToSpeech: Send + Sync {
    /// Synthesizes text stream into audio sample chunks
    async fn synthesize_stream(
        &self,
        text_stream: BoxStream<'static, String>,
    ) -> Result<BoxStream<'static, Vec<f32>>, HarnessError>;
}
