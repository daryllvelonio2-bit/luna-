use super::commands::SlashCommand;
use super::turn::{
    can_auto_resume, composer_command, submission, turn_records, ComposerCommand, InterruptNotice,
};
use std::time::{Duration, Instant};

/// The commands the composer owns are recognised by their whole name, and their argument is
/// kept verbatim: guidance is prose, not a token.
#[test]
fn the_composer_commands_are_parsed_from_the_typed_line() {
    assert_eq!(
        composer_command("/queue run the tests after this"),
        Some(ComposerCommand::Queue(
            "run the tests after this".to_string()
        ))
    );
    assert_eq!(
        composer_command("/steer  keep the patch minimal "),
        Some(ComposerCommand::Steer("keep the patch minimal".to_string()))
    );
    assert_eq!(
        composer_command("/interrupt"),
        Some(ComposerCommand::Interrupt)
    );
    // No argument: the answer is how to use it, not a silent no-op.
    assert_eq!(
        composer_command("/queue"),
        Some(ComposerCommand::QueueUsage)
    );
    assert_eq!(
        composer_command("/queue    "),
        Some(ComposerCommand::QueueUsage)
    );
    assert_eq!(
        composer_command("/steer"),
        Some(ComposerCommand::SteerUsage)
    );
}

/// Everything else stays an ordinary prompt, including the near-misses of these three names.
#[test]
fn anything_else_stays_a_prompt() {
    for line in [
        "",
        "hello",
        "/help",
        "/queued up work",
        "/steering wheel",
        "/interrupts",
        "/queue-now do it",
        "queue this",
    ] {
        assert_eq!(composer_command(line), None, "line: {line:?}");
    }
}

/// `/interrupt` takes no argument: words after it are not guidance for it to carry.
#[test]
fn interrupt_ignores_anything_after_it() {
    assert_eq!(
        composer_command("/interrupt now please"),
        Some(ComposerCommand::Interrupt)
    );
}

/// The three states a submit can meet: idle takes the message, a running turn is interrupted by
/// it, and while the transcript is being folded, everything waits in the queue.
#[test]
fn a_free_agent_takes_the_message_and_a_busy_one_is_interrupted() {
    assert_eq!(
        submission("run the tests", false, false),
        super::Submission::Turn
    );
    assert_eq!(
        submission("run the tests", true, false),
        super::Submission::Interrupt
    );
}

#[test]
fn a_fold_holds_everything_back() {
    assert_eq!(
        submission("run the tests", false, true),
        super::Submission::Queue
    );
    assert_eq!(
        submission("run the tests", true, true),
        super::Submission::Queue
    );
}

/// A turn that was redirected mid-reply becomes two records, cut exactly where the user spoke:
/// the partial answer stays under the question it was answering, and the redirect starts the next
/// turn. This is the shape a resumed transcript replays, so the live view must match it.
#[test]
fn an_interrupted_turn_is_split_at_the_message_that_cut_it() {
    let start = Instant::now();
    let records = turn_records(
        "write the parser",
        vec![InterruptNotice {
            text: "actually fix the lexer".to_string(),
            reasoning: "weighing two designs".to_string(),
            partial: "half an answer\n\n[interrupted by your next message]".to_string(),
            tool_count: 0,
            at: start + Duration::from_secs(2),
        }],
        start,
        Duration::from_secs(5),
        Some("final thought".to_string()),
        Vec::new(),
        Vec::new(),
        "final answer".to_string(),
    );

    assert_eq!(records.len(), 2);
    assert_eq!(records[0].user_query, "write the parser");
    assert_eq!(
        records[0].reasoning.as_deref(),
        Some("weighing two designs")
    );
    assert!(records[0].reply.contains("half an answer"), "{records:?}");
    assert!(
        records[0]
            .reply
            .contains("interrupted by your next message"),
        "the partial reply has to be marked: {records:?}"
    );
    assert_eq!(records[1].user_query, "actually fix the lexer");
    assert_eq!(records[1].reply, "final answer");
    assert_eq!(records[1].duration, Duration::from_secs(3));
}

/// With no interruption the builder is the old one-record turn, duration and all.
#[test]
fn an_untouched_turn_is_one_record() {
    let start = Instant::now();
    let records = turn_records(
        "run the tests",
        Vec::new(),
        start,
        Duration::from_secs(4),
        Some("thought".to_string()),
        Vec::new(),
        Vec::new(),
        "all green".to_string(),
    );

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].user_query, "run the tests");
    assert_eq!(records[0].duration, Duration::from_secs(4));
    assert_eq!(records[0].reply, "all green");
}

#[test]
fn slash_commands_includes_default_model() {
    use super::commands::SLASH_COMMANDS;
    assert!(
        SLASH_COMMANDS
            .iter()
            .any(|cmd| cmd.name == "/default_model"),
        "/default_model command must be registered in SLASH_COMMANDS"
    );
}

#[test]
fn slash_commands_includes_workers() {
    use super::commands::SLASH_COMMANDS;
    assert!(
        SLASH_COMMANDS.iter().any(|cmd| cmd.name == "/worker"),
        "/worker command must be registered in SLASH_COMMANDS"
    );
}

#[test]
fn command_entries_lists_configured_workers_as_invokable_commands() {
    use super::commands::command_entries;
    use harness_core::{Config, WorkerProfile};

    let mut config = Config::default();
    config.workers.insert(
        "carbon".to_string(),
        WorkerProfile {
            provider: "nvidia".to_string(),
            model: "tiny-model".to_string(),
            reasoning_effort: None,
        },
    );

    let entries = command_entries(&config, &[], true);
    let carbon = entries
        .iter()
        .find(|entry| entry.name == "/carbon")
        .expect("a worker shows up as its own slash command");
    assert!(carbon.details.contains("tiny-model"), "{}", carbon.details);
    assert!(entries.iter().any(|entry| entry.name == "/worker"));
}

#[test]
fn command_entries_lists_installed_skills_as_invokable_commands() {
    use super::commands::command_entries;
    use harness_core::Config;
    use harness_plugins::Skill;

    let config = Config::default();
    let skill = |name: &str, description: &str| Skill {
        name: name.to_string(),
        description: description.to_string(),
        instructions: "Do the thing.".to_string(),
        file_path: std::path::PathBuf::from(format!("{name}/SKILL.md")),
        command: None,
        timeout_secs: None,
        parameters: None,
    };
    let skills = vec![
        skill("review", "Review a diff carefully."),
        // A skill cannot steal a built-in command name.
        skill("model", "Not a real command."),
        // Nor one with whitespace, which cannot be a slash command.
        skill("two words", "Unreachable."),
    ];

    let entries = command_entries(&config, &skills, true);
    let review = entries
        .iter()
        .find(|entry| entry.name == "/review")
        .expect("an installed skill shows up as its own slash command");
    assert!(review.details.contains("Skill"), "{}", review.details);
    assert!(review.details.contains("Review a diff carefully."));
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.name == "/model")
            .count(),
        1,
        "the built-in /model must not be duplicated by a skill"
    );
    assert!(!entries.iter().any(|entry| entry.name == "/two words"));
}

#[test]
fn built_in_commands_shadow_a_worker_with_the_same_name() {
    use super::commands::command_entries;
    use harness_core::{Config, WorkerProfile};

    let mut config = Config::default();
    config.workers.insert(
        "model".to_string(),
        WorkerProfile {
            provider: "nvidia".to_string(),
            model: "tiny-model".to_string(),
            reasoning_effort: None,
        },
    );

    let entries = command_entries(&config, &[], true);
    assert!(
        entries
            .iter()
            .filter(|entry| entry.name == "/model")
            .count()
            == 1,
        "the built-in /model must not be duplicated by a worker"
    );

    config.workers.insert(
        "provider".to_string(),
        WorkerProfile {
            provider: "nvidia".to_string(),
            model: "tiny-model".to_string(),
            reasoning_effort: None,
        },
    );
    let entries = command_entries(&config, &[], true);
    assert!(
        !entries.iter().any(|entry| entry.name == "/provider"),
        "an alias of a built-in command must not be listed as a worker"
    );
}

#[test]
fn slash_commands_includes_sessions_picker() {
    use super::commands::SLASH_COMMANDS;
    assert!(
        SLASH_COMMANDS.iter().any(|cmd| cmd.name == "/sessions"),
        "/sessions command must be registered in SLASH_COMMANDS"
    );
}

/// The usage report is a command of its own: it reads the ledger, so it has to be reachable by
/// name and not only through the terminal's `luna usage`.
#[test]
fn slash_commands_includes_usage() {
    use super::commands::SLASH_COMMANDS;
    assert!(
        SLASH_COMMANDS.iter().any(|cmd| cmd.name == "/usage"),
        "/usage command must be registered in SLASH_COMMANDS"
    );
}

#[test]
fn slash_commands_includes_mcp() {
    use super::commands::SLASH_COMMANDS;
    assert!(
        SLASH_COMMANDS.iter().any(|cmd| cmd.name == "/mcp"),
        "/mcp command must be registered in SLASH_COMMANDS"
    );
}

#[test]
fn slash_commands_includes_remember() {
    use super::commands::SLASH_COMMANDS;
    assert!(
        SLASH_COMMANDS.iter().any(|cmd| cmd.name == "/remember"),
        "/remember must be registered so the user can force a pass"
    );
}

#[test]
fn slash_commands_includes_thinking() {
    use super::commands::SLASH_COMMANDS;
    assert!(
        SLASH_COMMANDS.iter().any(|cmd| cmd.name == "/thinking"),
        "/thinking command must be registered in SLASH_COMMANDS"
    );
    assert!(
        SLASH_COMMANDS.iter().any(|cmd| cmd.name == "/think"),
        "/think command must be registered in SLASH_COMMANDS"
    );
    assert!(
        SLASH_COMMANDS.iter().any(|cmd| cmd.name == "/reasoning"),
        "/reasoning command must be registered in SLASH_COMMANDS"
    );
}

#[test]
fn slash_commands_includes_dir_and_workspace() {
    use super::commands::SLASH_COMMANDS;
    assert!(
        SLASH_COMMANDS.iter().any(|cmd| cmd.name == "/dir"),
        "/dir command must be registered in SLASH_COMMANDS"
    );
    assert!(
        SLASH_COMMANDS.iter().any(|cmd| cmd.name == "/workspace"),
        "/workspace command must be registered in SLASH_COMMANDS"
    );
}

/// The slash command filter prioritizes prefix matches: typing "/s" should show /sessions,
/// /summarize, /skills, etc. before /model (which merely contains "s").
#[test]
fn slash_command_filter_prioritizes_prefix() {
    use super::commands::SLASH_COMMANDS;

    // Simulate the filter logic from picker.rs
    let filter = |f: &str| -> Vec<&'static str> {
        let f = f.strip_prefix('/').unwrap_or(f).trim().to_lowercase();
        if f.is_empty() {
            return SLASH_COMMANDS.iter().map(|c| c.name).collect();
        }
        let mut matches: Vec<&'static SlashCommand> = SLASH_COMMANDS
            .iter()
            .filter(|cmd| {
                let name = cmd.name.to_lowercase();
                let details = cmd.details.to_lowercase();
                name.contains(&f) || details.contains(&f)
            })
            .collect();
        matches.sort_by(|a, b| {
            let a_name = a.name.to_lowercase();
            let b_name = b.name.to_lowercase();
            let a_prefix = a_name.starts_with(&format!("/{}", f));
            let b_prefix = b_name.starts_with(&format!("/{}", f));
            match (a_prefix, b_prefix) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => {
                    let a_exact = a_name == format!("/{}", f);
                    let b_exact = b_name == format!("/{}", f);
                    match (a_exact, b_exact) {
                        (true, false) => std::cmp::Ordering::Less,
                        (false, true) => std::cmp::Ordering::Greater,
                        _ => std::cmp::Ordering::Equal,
                    }
                }
            }
        });
        matches.into_iter().map(|c| c.name).collect()
    };

    let results = filter("s");
    // First results should be /s* commands, not /model
    let first_s_prefix = results
        .iter()
        .position(|n| n.starts_with("/s"))
        .unwrap_or(usize::MAX);
    let first_model = results
        .iter()
        .position(|n| *n == "/model")
        .unwrap_or(usize::MAX);
    assert!(
        first_s_prefix < first_model,
        "/s* commands should appear before /model when filtering 's', got: {:?}",
        results
    );
}

/// Exact match comes first within its prefix group.
#[test]
fn slash_command_filter_exact_match_first() {
    use super::commands::SLASH_COMMANDS;

    let filter = |f: &str| -> Vec<&'static str> {
        let f = f.strip_prefix('/').unwrap_or(f).trim().to_lowercase();
        if f.is_empty() {
            return SLASH_COMMANDS.iter().map(|c| c.name).collect();
        }
        let mut matches: Vec<&'static SlashCommand> = SLASH_COMMANDS
            .iter()
            .filter(|cmd| {
                let name = cmd.name.to_lowercase();
                let details = cmd.details.to_lowercase();
                name.contains(&f) || details.contains(&f)
            })
            .collect();
        matches.sort_by(|a, b| {
            let a_name = a.name.to_lowercase();
            let b_name = b.name.to_lowercase();
            let a_prefix = a_name.starts_with(&format!("/{}", f));
            let b_prefix = b_name.starts_with(&format!("/{}", f));
            match (a_prefix, b_prefix) {
                (true, false) => std::cmp::Ordering::Less,
                (false, true) => std::cmp::Ordering::Greater,
                _ => {
                    let a_exact = a_name == format!("/{}", f);
                    let b_exact = b_name == format!("/{}", f);
                    match (a_exact, b_exact) {
                        (true, false) => std::cmp::Ordering::Less,
                        (false, true) => std::cmp::Ordering::Greater,
                        _ => std::cmp::Ordering::Equal,
                    }
                }
            }
        });
        matches.into_iter().map(|c| c.name).collect()
    };

    // Exact match "/sessions" should come first when filtering "sessions"
    let results = filter("sessions");
    assert_eq!(results.first(), Some(&"/sessions"));

    // Prefix match "/think" should bring /thinking and /think first
    let results = filter("think");
    assert!(
        results.starts_with(&["/think", "/thinking"])
            || results.starts_with(&["/thinking", "/think"])
    );
}

#[tokio::test]
async fn handle_thinking_switch_sets_effort() {
    use super::commands::handle_thinking_switch;
    use super::ui::ChatUi;
    use harness_core::{Agent, AgentConfig, DefaultToolRegistry, ReasoningEffort};
    use harness_memory::SqliteMemoryEngine;
    use harness_notes::FileNotesEngine;
    use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};
    use std::sync::Arc;

    let tmp = tempfile::tempdir().unwrap();
    let memory = Arc::new(SqliteMemoryEngine::in_memory().unwrap());
    let notes = Arc::new(FileNotesEngine::new(tmp.path().join("notes"), None).unwrap());
    let registry = Arc::new(DefaultToolRegistry::new());
    let creds = CredentialManager::new(None).unwrap();
    let discovery = Arc::new(ModelDiscovery::new(None, 0, Default::default()));
    let provider = Arc::new(
        GenericProviderClient::new(
            "openai".to_string(),
            "gpt-4o".to_string(),
            creds,
            discovery,
            None,
        )
        .unwrap(),
    );
    let agent = Arc::new(tokio::sync::Mutex::new(Agent::new(
        provider,
        memory,
        notes,
        registry,
        AgentConfig::default(),
    )));
    let ui = ChatUi::new(false);

    // Initial effort is None (off)
    assert_eq!(agent.lock().await.reasoning_effort(), None);

    // Set to high
    handle_thinking_switch(None, &ui, &agent, Some("high"))
        .await
        .unwrap();
    assert_eq!(
        agent.lock().await.reasoning_effort(),
        Some(ReasoningEffort::High)
    );

    // Set to low
    handle_thinking_switch(None, &ui, &agent, Some("low"))
        .await
        .unwrap();
    assert_eq!(
        agent.lock().await.reasoning_effort(),
        Some(ReasoningEffort::Low)
    );

    // Set to off
    handle_thinking_switch(None, &ui, &agent, Some("off"))
        .await
        .unwrap();
    assert_eq!(agent.lock().await.reasoning_effort(), None);

    // Set to 0 (alias for off)
    handle_thinking_switch(None, &ui, &agent, Some("medium"))
        .await
        .unwrap();
    assert_eq!(
        agent.lock().await.reasoning_effort(),
        Some(ReasoningEffort::Medium)
    );
    handle_thinking_switch(None, &ui, &agent, Some("0"))
        .await
        .unwrap();
    assert_eq!(agent.lock().await.reasoning_effort(), None);

    // Unknown effort does not change setting
    handle_thinking_switch(None, &ui, &agent, Some("high"))
        .await
        .unwrap();
    handle_thinking_switch(None, &ui, &agent, Some("invalid_effort"))
        .await
        .unwrap();
    assert_eq!(
        agent.lock().await.reasoning_effort(),
        Some(ReasoningEffort::High)
    );

    // Non-interactive with no arg leaves setting unchanged
    handle_thinking_switch(None, &ui, &agent, None)
        .await
        .unwrap();
    assert_eq!(
        agent.lock().await.reasoning_effort(),
        Some(ReasoningEffort::High)
    );
}

#[tokio::test]
async fn execute_slash_command_plan_toggle_and_unrecognized() {
    use super::commands::{execute_slash_command, Feature, FeatureToggles, SlashOutcome};
    use super::ui::ChatUi;
    use harness_core::{
        tools::TodoScope, Agent, AgentConfig, Config, DefaultToolRegistry, SqliteSessionStore,
    };
    use harness_memory::SqliteMemoryEngine;
    use harness_notes::FileNotesEngine;
    use harness_providers::{CredentialManager, GenericProviderClient, ModelDiscovery};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use tokio::sync::mpsc;

    let tmp = tempfile::tempdir().unwrap();
    let memory = Arc::new(SqliteMemoryEngine::in_memory().unwrap());
    let notes = Arc::new(FileNotesEngine::new(tmp.path().join("notes"), None).unwrap());
    let registry = Arc::new(DefaultToolRegistry::new());
    let creds = CredentialManager::new(None).unwrap();
    let discovery = Arc::new(ModelDiscovery::new(None, 0, Default::default()));
    let provider = Arc::new(
        GenericProviderClient::new(
            "openai".to_string(),
            "gpt-4o".to_string(),
            creds.clone(),
            discovery.clone(),
            None,
        )
        .unwrap(),
    );
    let agent = Arc::new(tokio::sync::Mutex::new(Agent::new(
        provider,
        memory,
        notes,
        registry,
        AgentConfig::default(),
    )));
    let ui = ChatUi::new(false);
    let mut config = Config::default();
    let cred_manager = Arc::new(creds);
    let store = Arc::new(SqliteSessionStore::in_memory().unwrap());
    let mut session_id = "test-session".to_string();
    let mut current_provider = "openai".to_string();
    let mut current_model = "gpt-4o".to_string();
    let mut compact_provider = None;
    let mut compact_model = None;
    let mut turns = Vec::new();
    let mut process_expanded = false;
    let todo_scope = TodoScope::new(tmp.path().join("todos"), &session_id);
    let plan_mode = Arc::new(AtomicBool::new(false));
    let feature_toggles = FeatureToggles::default();
    let (_tx, mut rx) = mpsc::unbounded_channel();

    // 1. /plan toggles plan mode
    assert!(!plan_mode.load(Ordering::SeqCst));
    let res = execute_slash_command(
        "/plan",
        &mut rx,
        &ui,
        &mut config,
        &cred_manager,
        &discovery,
        &agent,
        &store,
        &mut session_id,
        &mut current_provider,
        &mut current_model,
        &mut compact_provider,
        &mut compact_model,
        &mut turns,
        &mut process_expanded,
        &todo_scope,
        None,
        &[],
        &feature_toggles,
        &plan_mode,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(res, SlashOutcome::Continue));
    assert!(plan_mode.load(Ordering::SeqCst));

    // /plan again toggles it off
    let _ = execute_slash_command(
        "/plan",
        &mut rx,
        &ui,
        &mut config,
        &cred_manager,
        &discovery,
        &agent,
        &store,
        &mut session_id,
        &mut current_provider,
        &mut current_model,
        &mut compact_provider,
        &mut compact_model,
        &mut turns,
        &mut process_expanded,
        &todo_scope,
        None,
        &[],
        &feature_toggles,
        &plan_mode,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(!plan_mode.load(Ordering::SeqCst));

    // 2. /help returns Ok(false)
    let help_res = execute_slash_command(
        "/help",
        &mut rx,
        &ui,
        &mut config,
        &cred_manager,
        &discovery,
        &agent,
        &store,
        &mut session_id,
        &mut current_provider,
        &mut current_model,
        &mut compact_provider,
        &mut compact_model,
        &mut turns,
        &mut process_expanded,
        &todo_scope,
        None,
        &[],
        &feature_toggles,
        &plan_mode,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(help_res, SlashOutcome::Continue));

    // 3. Unrecognized command returns Ok(false) without breaking
    let unrec_res = execute_slash_command(
        "/unrecognized_foo_command",
        &mut rx,
        &ui,
        &mut config,
        &cred_manager,
        &discovery,
        &agent,
        &store,
        &mut session_id,
        &mut current_provider,
        &mut current_model,
        &mut compact_provider,
        &mut compact_model,
        &mut turns,
        &mut process_expanded,
        &todo_scope,
        None,
        &[],
        &feature_toggles,
        &plan_mode,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(unrec_res, SlashOutcome::Continue));

    // 4. A feature command reaches both the CLI's menu state and the live agent's config.
    assert!(feature_toggles.enabled(Feature::Memory));
    let memory_res = execute_slash_command(
        "/memory off",
        &mut rx,
        &ui,
        &mut config,
        &cred_manager,
        &discovery,
        &agent,
        &store,
        &mut session_id,
        &mut current_provider,
        &mut current_model,
        &mut compact_provider,
        &mut compact_model,
        &mut turns,
        &mut process_expanded,
        &todo_scope,
        None,
        &[],
        &feature_toggles,
        &plan_mode,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(matches!(memory_res, SlashOutcome::Continue));
    assert!(!feature_toggles.enabled(Feature::Memory));
    assert!(!agent.lock().await.config().memory_enabled);

    // And the same command turns it back on.
    let _ = execute_slash_command(
        "/memory on",
        &mut rx,
        &ui,
        &mut config,
        &cred_manager,
        &discovery,
        &agent,
        &store,
        &mut session_id,
        &mut current_provider,
        &mut current_model,
        &mut compact_provider,
        &mut compact_model,
        &mut turns,
        &mut process_expanded,
        &todo_scope,
        None,
        &[],
        &feature_toggles,
        &plan_mode,
        None,
        None,
        None,
    )
    .await
    .unwrap();
    assert!(feature_toggles.enabled(Feature::Memory));
    assert!(agent.lock().await.config().memory_enabled);
}

#[test]
fn typing_keeps_the_landing_shower_running() {
    use super::keys::AppKey;

    // Editing the prompt leaves the mark exactly where it is.
    for key in [
        AppKey::Char('a'),
        AppKey::Backspace,
        AppKey::Delete,
        AppKey::Left,
        AppKey::Right,
        AppKey::Home,
        AppKey::End,
        AppKey::Up,
        AppKey::Down,
    ] {
        assert!(
            super::key_keeps_landing(&key, false),
            "{key:?} only edits the prompt"
        );
    }

    // A slash on an empty prompt opens the command menu; one inside a prompt is just a slash.
    assert!(!super::key_keeps_landing(&AppKey::Char('/'), true));
    assert!(super::key_keeps_landing(&AppKey::Char('/'), false));

    // Enter starts a turn, Tab changes plan mode, and the control keys take over the screen.
    for key in [
        AppKey::Enter,
        AppKey::Tab,
        AppKey::Escape,
        AppKey::Ctrl('o'),
        AppKey::Ctrl('t'),
        AppKey::CtrlC,
    ] {
        assert!(
            !super::key_keeps_landing(&key, false),
            "{key:?} can move the mark"
        );
    }
}

/// The idle pass exists for sessions too short to reach the every-N-turns interval. It must fire
/// once per transcript, only after the conversation has actually gone quiet, and never while a
/// turn is still running.
#[test]
fn the_idle_memory_pass_fires_once_per_transcript_after_the_quiet_window() {
    let start = Instant::now();
    let mut pass = super::IdleMemoryPass::new(0);

    // Nothing has been said, so there is nothing to save.
    assert!(!pass.tick(0, false, 90, start + Duration::from_secs(600)));

    // A turn in flight keeps the clock at zero.
    assert!(!pass.tick(1, true, 90, start));
    assert!(!pass.tick(1, true, 90, start + Duration::from_secs(600)));

    // The turn lands; the quiet clock starts then, not at the keystroke.
    assert!(!pass.tick(1, false, 90, start + Duration::from_secs(601)));
    assert!(!pass.tick(1, false, 90, start + Duration::from_secs(690)));
    assert!(pass.tick(1, false, 90, start + Duration::from_secs(691)));

    // The same transcript never gets a second pass.
    assert!(!pass.tick(1, false, 90, start + Duration::from_secs(9_999)));

    // A new turn re-arms it, from its own landing time.
    assert!(!pass.tick(2, true, 90, start + Duration::from_secs(10_000)));
    assert!(!pass.tick(2, false, 90, start + Duration::from_secs(10_001)));
    assert!(pass.tick(2, false, 90, start + Duration::from_secs(10_091)));
}

/// Zero disables the idle pass, and a transcript that shrank (cleared, compacted) must not leave
/// the marker ahead of it forever: the next turn still re-arms the pass.
#[test]
fn the_idle_memory_pass_is_off_at_zero_and_recovers_from_a_shrunk_transcript() {
    let start = Instant::now();
    let mut pass = super::IdleMemoryPass::new(3);

    assert!(!pass.tick(3, false, 0, start + Duration::from_secs(600)));

    // The transcript shrank; the remaining turns were already covered, so no pass is owed for
    // them, but the marker must follow the transcript down.
    assert!(!pass.tick(1, false, 90, start + Duration::from_secs(700)));
    assert!(!pass.tick(1, false, 90, start + Duration::from_secs(790)));

    // A new turn after the shrink still fires, from its own landing time.
    assert!(!pass.tick(2, true, 90, start + Duration::from_secs(800)));
    assert!(!pass.tick(2, false, 90, start + Duration::from_secs(801)));
    assert!(pass.tick(2, false, 90, start + Duration::from_secs(891)));
}

/// A completion may only start a turn nobody typed when the composer is genuinely quiet. Every
/// other input is a reason to wait: stealing a draft or a queued message is worse than a job
/// notification arriving one turn later.
#[test]
fn an_unasked_turn_waits_for_a_quiet_composer() {
    assert!(can_auto_resume(false, false, 0, "", 0, false, false));
    assert!(can_auto_resume(false, false, 0, "   ", 0, false, false));

    let busy = [
        (true, false, 0, "", 0, false, false),
        (false, true, 0, "", 0, false, false),
        (false, false, 1, "", 0, false, false),
        (false, false, 0, "half a thought", 0, false, false),
        (false, false, 0, "", 1, false, false),
        (false, false, 0, "", 0, true, false),
        (false, false, 0, "", 0, false, true),
    ];
    for (turn_active, compacting, queued, draft, images, modal, plan) in busy {
        assert!(
            !can_auto_resume(turn_active, compacting, queued, draft, images, modal, plan),
            "must not resume: turn={turn_active} compacting={compacting} queued={queued} \
             draft={draft:?} images={images} modal={modal} plan={plan}"
        );
    }
}
