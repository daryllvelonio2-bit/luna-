use std::fs;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

use harness_core::config::Config;
use harness_core::traits::NotesEngine;
use harness_core::{
    DefaultToolRegistry, HarnessError, LocalAgentSubstrate, MockSubstrate, SubstrateOutcome,
    SurfaceSpec,
};
use harness_evolver::{RecursionMode, TaskHarnessRefiner};
use harness_memory::SqliteMemoryEngine;
use harness_notes::SqliteNotesEngine;
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};

pub async fn handle_evolve_command(
    task: &str,
    mode_str: &str,
    budget_k: usize,
    surface_path: Option<&str>,
    output_path: Option<&str>,
    offline: bool,
    config: &Config,
    cred_manager: &Arc<CredentialManager>,
    discovery: &Arc<ModelDiscovery>,
) -> Result<(), HarnessError> {
    let mode = match mode_str.to_lowercase().as_str() {
        "seq" | "sequential" => RecursionMode::Sequential,
        _ => RecursionMode::Parallel,
    };

    let base_surface = if let Some(path) = surface_path {
        let content = fs::read_to_string(path)
            .map_err(|e| HarnessError::Io(format!("failed to read surface file: {}", e)))?;
        serde_json::from_str::<SurfaceSpec>(&content)
            .map_err(|e| HarnessError::SerializationError(format!("invalid surface JSON: {}", e)))?
    } else {
        SurfaceSpec::default()
    };

    let mut refiner = TaskHarnessRefiner::new(budget_k, mode);
    let cancel_token = CancellationToken::new();

    println!("Refining harness for task: {}", task);
    println!(
        "Mode: {:?} | Budget k: {} (R={} rollouts, P={} stages) | Substrate: {}",
        mode,
        budget_k,
        refiner.ledger.max_rollouts,
        refiner.ledger.max_stages,
        if offline {
            "offline-mock"
        } else {
            "live-substrate"
        }
    );

    let refined_surface = if offline || config.model.provider.is_empty() {
        let substrate = MockSubstrate::new(
            "task_eval_runner",
            SubstrateOutcome {
                success: true,
                output: format!("Offline simulated probe for task: {}", task),
                tokens_used: 350,
                duration_ms: 45,
            },
        );
        refiner
            .run_recursion(&base_surface, task, &substrate, &cancel_token)
            .await?
    } else {
        match GenericProviderClient::new(
            config.model.provider.clone(),
            config.model.default.clone(),
            cred_manager.clone(),
            discovery.clone(),
            config.providers.get(&config.model.provider),
        ) {
            Ok(client) => {
                let memory = Arc::new(SqliteMemoryEngine::new(Some(
                    config.paths.memory_db.clone(),
                ))?);
                let notes = Arc::new(SqliteNotesEngine::new(Some(config.paths.notes_db.clone()))?);
                let evolver = Arc::new(harness_evolver::CodebaseEvolver::new(
                    config.paths.workspace_root.clone(),
                    config.paths.jobs_dir.clone(),
                ));
                let registry = Arc::new(DefaultToolRegistry::new());

                let local_substrate = LocalAgentSubstrate::new(
                    "luna_live_agent",
                    Arc::new(client),
                    memory,
                    notes,
                    evolver,
                    registry,
                    config.agent.clone(),
                );

                refiner
                    .run_recursion(&base_surface, task, &local_substrate, &cancel_token)
                    .await?
            }
            Err(_) => {
                println!(
                    "(No active credentials for '{}'; falling back to offline simulation)",
                    config.model.provider
                );
                let substrate = MockSubstrate::new(
                    "task_eval_runner",
                    SubstrateOutcome {
                        success: true,
                        output: format!("Simulated completion for task: {}", task),
                        tokens_used: 350,
                        duration_ms: 45,
                    },
                );
                refiner
                    .run_recursion(&base_surface, task, &substrate, &cancel_token)
                    .await?
            }
        }
    };

    println!("\nRefinement completed successfully:");
    println!(
        "  Rollouts executed:     {}",
        refiner.ledger.completed_rollouts.len()
    );
    println!(
        "  Stages executed:       {}",
        refiner.ledger.stages_executed
    );
    println!("  Total tokens:          {}", refiner.ledger.total_tokens());
    println!(
        "  Tool guidance entries: {}",
        refined_surface.c5_tool_guidance.len()
    );
    println!(
        "  Active skills:         {}",
        refined_surface.c4_skills.len()
    );
    println!(
        "  Max failure threshold: {}",
        refined_surface.c2_guardrails.max_consecutive_failures
    );

    let pretty_json = serde_json::to_string_pretty(&refined_surface)
        .map_err(|e| HarnessError::SerializationError(e.to_string()))?;

    // Level-2 Harnessing-level recursion: auto-accumulate adopted harness in persistent library
    let luna_dir = config
        .paths
        .sessions_db
        .parent()
        .unwrap_or(std::path::Path::new("."));
    let library_dir = luna_dir.join("harnesses");
    let _ = fs::create_dir_all(&library_dir);
    let task_slug: String = task
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '_' || *c == '-')
        .map(|c| c.to_ascii_lowercase())
        .take(32)
        .collect();
    let slug = if task_slug.is_empty() {
        "default".to_string()
    } else {
        task_slug
    };
    let library_path = library_dir.join(format!("{}.json", slug));
    let _ = fs::write(&library_path, &pretty_json);
    println!(
        "\nAdopted harness persisted to library: {}",
        library_path.display()
    );

    // Record an adopted knowledge note in notes.db
    if let Ok(notes_engine) = SqliteNotesEngine::new(Some(config.paths.notes_db.clone())) {
        let note_title = format!("[Harness²] Adopted harness: {}", task);
        let note_body = format!(
            "Adopted SurfaceSpec for task '{}'\nMode: {:?}\nStages: {}\nRollouts: {}\nTool guidance: {:?}",
            task, mode, refiner.ledger.stages_executed, refiner.ledger.completed_rollouts.len(), refined_surface.c5_tool_guidance
        );
        let _ = notes_engine
            .save_note(
                &note_title,
                &note_body,
                &["harness2".into(), "recursion".into()],
            )
            .await;
    }

    if let Some(out) = output_path {
        fs::write(out, pretty_json)
            .map_err(|e| HarnessError::Io(format!("failed to write output surface: {}", e)))?;
        println!("Saved requested SurfaceSpec to: {}", out);
    }

    Ok(())
}
