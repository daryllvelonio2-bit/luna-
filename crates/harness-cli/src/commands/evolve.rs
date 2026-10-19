use std::fs;
use tokio_util::sync::CancellationToken;

use harness_core::{HarnessError, MockSubstrate, SubstrateOutcome, SurfaceSpec};
use harness_evolver::{RecursionMode, TaskHarnessRefiner};

pub async fn handle_evolve_command(
    task: &str,
    mode_str: &str,
    budget_k: usize,
    surface_path: Option<&str>,
    output_path: Option<&str>,
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

    // Default substrate for task refinement
    let substrate = MockSubstrate::new(
        "task_eval_runner",
        SubstrateOutcome {
            success: true,
            output: format!("Simulated completion for task: {}", task),
            tokens_used: 350,
            duration_ms: 45,
        },
    );

    let cancel_token = CancellationToken::new();

    println!("Refining harness for task: {}", task);
    println!("Mode: {:?} | Budget k: {} (R={} rollouts, P={} stages)",
        mode, budget_k, refiner.ledger.max_rollouts, refiner.ledger.max_stages
    );

    let refined_surface = refiner
        .run_recursion(&base_surface, task, &substrate, &cancel_token)
        .await?;

    println!("\nRefinement completed successfully:");
    println!("  Rollouts executed: {}", refiner.ledger.completed_rollouts.len());
    println!("  Stages executed:   {}", refiner.ledger.stages_executed);
    println!("  Total tokens:      {}", refiner.ledger.total_tokens());
    println!("  Tool guidance entries: {}", refined_surface.c5_tool_guidance.len());
    println!("  Active skills:         {}", refined_surface.c4_skills.len());
    println!("  Max failure threshold: {}", refined_surface.c2_guardrails.max_consecutive_failures);

    if let Some(out) = output_path {
        let json = serde_json::to_string_pretty(&refined_surface)
            .map_err(|e| HarnessError::SerializationError(e.to_string()))?;
        fs::write(out, json)
            .map_err(|e| HarnessError::Io(format!("failed to write output surface: {}", e)))?;
        println!("\nSaved refined SurfaceSpec to: {}", out);
    }

    Ok(())
}
