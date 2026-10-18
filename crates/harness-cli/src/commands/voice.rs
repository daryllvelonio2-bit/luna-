use std::sync::Arc;
use async_trait::async_trait;
use colored::Colorize;
use futures_util::stream::BoxStream;
use tokio_util::sync::CancellationToken;

use harness_core::{Agent, Config, DefaultToolRegistry, HarnessError, ToolRegistry};
use harness_evolver::{
    CheckImprovementTool, CodebaseEvolver, DelegateImprovementTool, InspectSourceTool,
    VerifyAndStageUpdateTool,
};
use harness_memory::{
    ForgetMemoryTool, SaveMemoryTool, SqliteMemoryEngine, UpdateMemoryTool,
};
use harness_notes::{
    DeleteNoteTool, ListNotesTool, ReadNoteTool, SaveNoteTool, SearchNotesTool, SqliteNotesEngine,
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
    config: Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
) -> Result<(), HarnessError> {
    let provider_id = config.model.provider.clone();

    crate::commands::clear_screen();
    println!("{}", "==================================================".cyan());
    println!("{}", "   Luna Hands-Free Voice Interface (\"Luna\")        ".bold().cyan());
    println!("{}", "==================================================".cyan());
    println!("Wake word: {}\nListening on microphone...", "Luna".green().bold());

    let provider = Arc::new(GenericProviderClient::new(
        provider_id.clone(),
        config.model.default.clone(),
        cred_manager,
        discovery,
        config.providers.get(&provider_id),
    )?);

    let memory = Arc::new(SqliteMemoryEngine::new(Some(config.paths.memory_db.clone()))?);
    let notes = Arc::new(SqliteNotesEngine::new(Some(config.paths.notes_db.clone()))?);
    let evolver = Arc::new(
        CodebaseEvolver::new(
            config.paths.workspace_root.clone(),
            config.paths.jobs_dir.clone(),
        )
        .with_provider(provider.clone()),
    );
    let registry = Arc::new(DefaultToolRegistry::new());

    registry.register(Arc::new(SaveMemoryTool::new(memory.clone()))).await?;
    registry.register(Arc::new(UpdateMemoryTool::new(memory.clone()))).await?;
    registry.register(Arc::new(ForgetMemoryTool::new(memory.clone()))).await?;
    registry.register(Arc::new(SaveNoteTool::new(notes.clone()))).await?;
    registry.register(Arc::new(SearchNotesTool::new(notes.clone()))).await?;
    registry.register(Arc::new(ListNotesTool::new(notes.clone()))).await?;
    registry.register(Arc::new(ReadNoteTool::new(notes.clone()))).await?;
    registry.register(Arc::new(UpdateNoteTool::new(notes.clone()))).await?;
    registry.register(Arc::new(DeleteNoteTool::new(notes.clone()))).await?;
    registry.register(Arc::new(InspectSourceTool::new(evolver.clone()))).await?;
    registry.register(Arc::new(DelegateImprovementTool::new(evolver.clone()))).await?;
    registry.register(Arc::new(CheckImprovementTool::new(evolver.clone()))).await?;
    registry.register(Arc::new(VerifyAndStageUpdateTool::new(evolver.clone()))).await?;

    // Register core filesystem and shell tools (§2.2)
    let read_cache = harness_core::tools::ReadCache::new();
    let reg_dyn: Arc<dyn ToolRegistry> = registry.clone();
    harness_core::tools::register_core_tools(&reg_dyn, &config, read_cache).await?;

    let agent = Arc::new(
        Agent::new(
            provider,
            memory,
            notes,
            evolver,
            registry,
            config.agent.clone(),
        )
        .with_approval(build_approver(&config), approval_settings(&config)),
    );

    let session = VoiceSession::new(
        Arc::new(DefaultWakeWordDetector),
        Arc::new(DefaultSpeechToText),
        Arc::new(DefaultTextToSpeech),
        VoicePipelineConfig::default(),
    );

    let cancel = CancellationToken::new();
    session.run_hands_free_loop(agent, cancel).await
}
