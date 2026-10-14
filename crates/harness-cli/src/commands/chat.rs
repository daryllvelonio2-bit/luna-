use std::io::{self, Write};
use std::sync::Arc;
use colored::Colorize;
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

pub async fn run_interactive_chat(
    provider_id: String,
    model: Option<String>,
    cred_manager: Arc<CredentialManager>,
    discovery: Arc<ModelDiscovery>,
) -> Result<(), HarnessError> {
    println!("{}", "==================================================".cyan());
    println!("{}", "   Luna Autonomous AI Agent Harness (Interactive)  ".bold().cyan());
    println!("{}", "==================================================".cyan());
    println!("Provider: {}", provider_id.green());
    println!("Model:    {}", model.as_deref().unwrap_or("default").green());
    println!("Type 'exit' or 'quit' to terminate session.\n");

    let provider = Arc::new(GenericProviderClient::new(
        provider_id.clone(),
        None,
        model.unwrap_or_else(|| "gpt-4o".to_string()),
        cred_manager.clone(),
        discovery.clone(),
    ));

    let memory = Arc::new(SqliteMemoryEngine::new(None)?);
    let notes = Arc::new(SqliteNotesEngine::new(None)?);
    let evolver = Arc::new(CodebaseEvolver::new(std::env::current_dir().unwrap_or_default()));

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
        AgentConfig::default(),
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
