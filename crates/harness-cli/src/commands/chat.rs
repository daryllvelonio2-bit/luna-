use std::io::{self, Write};
use std::sync::Arc;
use colored::Colorize;
use tokio_util::sync::CancellationToken;

use harness_core::{Agent, Config, DefaultToolRegistry, HarnessError};
use harness_evolver::{ApplyUpdateTool, CodebaseEvolver, InspectSourceTool, ProposePatchTool};
use harness_memory::{
    ForgetMemoryTool, SaveMemoryTool, SqliteMemoryEngine, UpdateMemoryTool,
};
use harness_notes::{
    ReadNoteTool, SaveNoteTool, SearchNotesTool, SqliteNotesEngine, UpdateNoteTool,
};
use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};

/// `config` is already resolved (defaults → file → `LUNA_*` → flags); this function only reads it,
/// so the engines/filesystem paths come from one place instead of each engine guessing `~/.luna/…`.
pub async fn run_interactive_chat(
    config: Config,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
) -> Result<(), HarnessError> {
    let provider_id = config.model.provider.clone();
    let model = config.model.default.clone();

    println!("{}", "==================================================".cyan());
    println!("{}", "   Luna Autonomous AI Agent Harness (Interactive)  ".bold().cyan());
    println!("{}", "==================================================".cyan());
    println!("Provider: {}", provider_id.green());
    println!("Model:    {}", model.green());
    println!("Type 'exit' or 'quit' to terminate session.\n");

    let provider = Arc::new(GenericProviderClient::new(
        provider_id.clone(),
        model,
        cred_manager.clone(),
        discovery.clone(),
        config.providers.get(&provider_id),
    )?);

    let memory = Arc::new(SqliteMemoryEngine::new(Some(config.paths.memory_db.clone()))?);
    let notes = Arc::new(SqliteNotesEngine::new(Some(config.paths.notes_db.clone()))?);
    let evolver = Arc::new(CodebaseEvolver::new(config.paths.workspace_root.clone()));

    let registry = Arc::new(DefaultToolRegistry::new());

    // Register native memory tools
    registry.register(Arc::new(SaveMemoryTool::new(memory.clone()))).await?;
    registry.register(Arc::new(UpdateMemoryTool::new(memory.clone()))).await?;
    registry.register(Arc::new(ForgetMemoryTool::new(memory.clone()))).await?;

    // Register native note tools
    registry.register(Arc::new(SaveNoteTool::new(notes.clone()))).await?;
    registry.register(Arc::new(SearchNotesTool::new(notes.clone()))).await?;
    registry.register(Arc::new(ReadNoteTool::new(notes.clone()))).await?;
    registry.register(Arc::new(UpdateNoteTool::new(notes.clone()))).await?;

    // Register self-improvement evolver tools
    registry.register(Arc::new(InspectSourceTool::new(evolver.clone()))).await?;
    registry.register(Arc::new(ProposePatchTool::new(evolver.clone()))).await?;
    registry.register(Arc::new(ApplyUpdateTool::new(evolver.clone()))).await?;

    let agent = Agent::new(
        provider,
        memory,
        notes,
        evolver,
        registry,
        config.agent.clone(),
    );

    let cancel_token = CancellationToken::new();

    loop {
        print!("{}", "User > ".bold().yellow());
        io::stdout().flush().map_err(|e| HarnessError::Io(e.to_string()))?;

        let mut input = String::new();
        io::stdin().read_line(&mut input).map_err(|e| HarnessError::Io(e.to_string()))?;
        let query = input.trim();

        if query.eq_ignore_ascii_case("exit") || query.eq_ignore_ascii_case("quit") {
            println!("Goodbye!");
            break;
        }

        if query.is_empty() {
            continue;
        }

        println!("{}", "Luna is thinking...".dimmed());
        match agent.run_task(query, cancel_token.child_token()).await {
            Ok(reply) => {
                println!("\n{}\n{}\n", "Luna >".bold().green(), reply);
            }
            Err(e) => {
                eprintln!("\n{} Error: {}\n", "✖".red().bold(), e);
            }
        }
    }

    Ok(())
}
