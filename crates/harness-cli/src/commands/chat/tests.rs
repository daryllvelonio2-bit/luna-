use super::turn::{composer_command, submission, ComposerCommand};

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

/// The three states a submit can meet: idle takes the message, active steers it mid-run,
/// and while the transcript is being folded, everything waits in the queue.
#[test]
fn a_free_agent_takes_the_message_and_a_busy_one_steers_the_turn() {
    assert_eq!(
        submission("run the tests", false, false),
        super::Submission::Turn
    );
    assert_eq!(
        submission("run the tests", true, false),
        super::Submission::Steer
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
