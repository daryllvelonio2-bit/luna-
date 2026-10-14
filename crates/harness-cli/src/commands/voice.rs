use std::sync::Arc;
use async_trait::async_trait;
use colored::Colorize;
use futures_util::stream::BoxStream;
use tokio_util::sync::CancellationToken;

use harness_core::{Agent, AgentConfig, DefaultToolRegistry, HarnessError};
use harness_evolver::{ApplyUpdateTool, CodebaseEvolver, InspectSourceTool, ProposePatchTool};
use harness_memory::{
    ForgetMemoryTool, SaveMemoryTool, SqliteMemoryEngine, UpdateMemoryTool,
};
use harness_notes::{
    ReadNoteTool, SaveNoteTool, SearchNotesTool, SqliteNotesEngine, UpdateNoteTool,
};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};
use harness_voice::{
    SpeechToText, TextToSpeech, VoicePipelineConfig, VoiceSession, WakeWordDetector,
};

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

pub struct DefaultSpeechToText;

#[async_trait]
impl SpeechToText for DefaultSpeechToText {
    async fn transcribe(&self, _wav_bytes: &[u8]) -> Result<String, HarnessError> {
        Ok("Hello Luna, what are my persistent notes?".to_string())
    }
}

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
    provider_id: String,
    model: Option<String>,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
) -> Result<(), HarnessError> {
    println!("{}", "==================================================".cyan());
    println!("{}", "   Luna Hands-Free Voice Interface (\"Luna\")        ".bold().cyan());
    println!("{}", "==================================================".cyan());
    println!("Wake word: {}\nListening on microphone...", "Luna".green().bold());

    let provider = Arc::new(GenericProviderClient::new(
        provider_id,
        None,
        model.unwrap_or_else(|| "gpt-4o".to_string()),
        cred_manager,
        discovery,
    ));

    let memory = Arc::new(SqliteMemoryEngine::new(None)?);
    let notes = Arc::new(SqliteNotesEngine::new(None)?);
    let evolver = Arc::new(CodebaseEvolver::new(std::env::current_dir().unwrap_or_default()));
    let registry = Arc::new(DefaultToolRegistry::new());

    registry.register(Arc::new(SaveMemoryTool::new(memory.clone()))).await?;
    registry.register(Arc::new(UpdateMemoryTool::new(memory.clone()))).await?;
    registry.register(Arc::new(ForgetMemoryTool::new(memory.clone()))).await?;
    registry.register(Arc::new(SaveNoteTool::new(notes.clone()))).await?;
    registry.register(Arc::new(SearchNotesTool::new(notes.clone()))).await?;
    registry.register(Arc::new(ReadNoteTool::new(notes.clone()))).await?;
    registry.register(Arc::new(UpdateNoteTool::new(notes.clone()))).await?;
    registry.register(Arc::new(InspectSourceTool::new(evolver.clone()))).await?;
    registry.register(Arc::new(ProposePatchTool::new(evolver.clone()))).await?;
    registry.register(Arc::new(ApplyUpdateTool::new(evolver.clone()))).await?;

    let agent = Arc::new(Agent::new(
        provider,
        memory,
        notes,
        evolver,
        registry,
        AgentConfig::default(),
    ));

    let session = VoiceSession::new(
        Arc::new(DefaultWakeWordDetector),
        Arc::new(DefaultSpeechToText),
        Arc::new(DefaultTextToSpeech),
        VoicePipelineConfig::default(),
    );

    let cancel = CancellationToken::new();
    session.run_hands_free_loop(agent, cancel).await
}
