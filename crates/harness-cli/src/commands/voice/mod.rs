//! Hands-free voice interface, continuous microphone listener, STT, and streaming TTS.

use colored::Colorize;
use std::sync::atomic::{AtomicBool, Ordering};
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

pub mod capture;
pub mod catalog;
pub mod playback;
pub mod stt;
pub mod summary;
pub mod tts;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use capture::{downsample_to_16khz, run_background_voice_listener};
#[allow(unused_imports)]
pub use catalog::{available_voices, VoiceCatalogEntry};
pub use playback::VoiceSpeechQueue;
#[allow(unused_imports)]
pub use summary::{
    clean_text_for_speech, extract_ready_speech_chunks, fallback_voice_summary,
    generate_pre_task_voice_ack, generate_separate_voice_summary, needs_separate_voice_summary,
    spawn_pre_task_voice_ack, spawn_separate_voice_reply, split_sentences_for_tts,
};
#[allow(unused_imports)]
pub use tts::build_gemini_tts_payload;

/// Shared HTTP client with connection pooling and TCP keep-alive for low-latency voice STT/TTS.
pub(crate) fn voice_http_client() -> &'static reqwest::Client {
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
    Transcript(String),
    Notice(String),
    Error(String),
    BargeIn,
    SpeechDetected(f32),
    SpeechLevel(f32),
    Transcribing,
    SpeechIdle,
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
