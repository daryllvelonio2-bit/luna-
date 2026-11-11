use async_trait::async_trait;
use colored::Colorize;
use futures_util::stream::BoxStream;
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

/// The recogniser `luna voice` runs with, which is not a recogniser.
///
/// It answers every recording with the same sentence. The harness ships a voice loop and no speech
/// model, and a stub that invents words is worse than one that says it cannot: the loop announces
/// this at startup (see `run_voice_loop`) so nobody mistakes the demo for hearing them.
pub struct DefaultSpeechToText;

#[async_trait]
impl SpeechToText for DefaultSpeechToText {
    async fn transcribe(&self, _wav_bytes: &[u8]) -> Result<String, HarnessError> {
        Ok("Hello Luna, what are my persistent notes?".to_string())
    }
}

/// The synthesiser `luna voice` runs with, which produces silence: one block of zero samples per
/// chunk of text. It is here so the loop can be exercised end to end, not to be listened to.
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

    print_voice_banner();

    let provider = build_voice_provider(&config, provider_id.clone(), cred_manager, discovery)?;

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

    let session = build_voice_session();

    let cancel = CancellationToken::new();
    session.run_hands_free_loop(agent, cancel).await
}

/// Print the hands-free header and the honest note about the stub recogniser.
fn print_voice_banner() {
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
        "Wake word: {}\nListening on microphone...",
        "Luna".green().bold()
    );
    // Said before anything is listened to, because the alternative is a user talking to a program
    // that answers every recording with the same sentence and concluding it works.
    println!(
        "{}",
        "note: no speech model is wired into this build — every recording is answered with a fixed \
         sentence and the reply is not spoken. This is the loop's demo path, not recognition."
            .yellow()
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

/// The demo pipeline: stub wake word, fixed transcription, silent synthesis.
fn build_voice_session(
) -> VoiceSession<DefaultWakeWordDetector, DefaultSpeechToText, DefaultTextToSpeech> {
    VoiceSession::new(
        Arc::new(DefaultWakeWordDetector),
        Arc::new(DefaultSpeechToText),
        Arc::new(DefaultTextToSpeech),
        VoicePipelineConfig::default(),
    )
}
