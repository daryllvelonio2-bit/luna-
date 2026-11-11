use harness_core::tasks::JobCompletion;

/// The first line of the prompt the harness sends itself when a background job finishes.
///
/// It is a sentinel, not prose: [`clean_user_query_for_transcript`] drops a stored prompt that
/// starts with it, so replaying a session does not show a user message nobody typed.
pub const BACKGROUND_JOB_RESUME_PREFIX: &str = "[background job finished]";

/// How much of a finished job's output the stored system message carries. The log keeps the
/// rest; this is enough to see what happened without pinning a huge line in every replay.
pub(crate) const MAX_JOB_TAIL_CHARS: usize = 4_000;

/// Format a clean single-line title from a subagent goal.
pub fn format_delegate_title(goal: &str) -> String {
    let first = goal
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or(goal)
        .trim();
    if first.chars().count() > 50 {
        format!("{}…", first.chars().take(49).collect::<String>())
    } else {
        first.to_string()
    }
}

/// Format duration into compact human-readable string (e.g. "4s", "1m 12s").
pub fn format_delegate_duration(dur: std::time::Duration) -> String {
    format_elapsed_secs(dur.as_secs())
}

/// The same compact duration, from whole seconds.
pub fn format_elapsed_secs(secs: u64) -> String {
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// One line of a shell command, bounded, for a job notice.
pub fn format_job_command(command: &str) -> String {
    let first = command
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or(command)
        .trim();
    if first.chars().count() > 60 {
        format!("{}…", first.chars().take(59).collect::<String>())
    } else {
        first.to_string()
    }
}

/// Render the line that appears above the composer when a background job stops.
pub fn format_job_notification(
    glyph: &str,
    id: &str,
    command: &str,
    status: &str,
    elapsed: &str,
) -> String {
    format!(
        "{glyph} {id}: {} — {status}, {elapsed}",
        format_job_command(command)
    )
}

/// The stored system message a finished job leaves for the model.
///
/// It carries everything the model needs to decide what to do next without another `process
/// poll`: what ran, how it ended, how long it took, where the full log is, and the end of the
/// output. The first line is the resume sentinel so replay can recognise the turn it started.
pub fn format_job_system_message(completion: &JobCompletion) -> String {
    let tail = completion.output_tail.trim_end();
    let tail = if tail.is_empty() {
        "(no output)".to_string()
    } else {
        let chars: Vec<char> = tail.chars().collect();
        if chars.len() > MAX_JOB_TAIL_CHARS {
            let kept: String = chars[chars.len() - MAX_JOB_TAIL_CHARS..].iter().collect();
            format!(
                "… ({} earlier characters trimmed; see the log)\n{}",
                chars.len() - MAX_JOB_TAIL_CHARS,
                kept
            )
        } else {
            tail.to_string()
        }
    };
    format!(
        "{BACKGROUND_JOB_RESUME_PREFIX}\n\
         Background job {} stopped: {}\n\
         Command: {}\n\
         Duration: {}\n\
         Log: {}\n\
         \n\
         Output tail:\n{}",
        completion.id,
        completion.status,
        completion.command,
        format_elapsed_secs(completion.elapsed_secs),
        completion.log_path,
        tail,
    )
}

/// The prompt that starts the harness-owned turn reacting to a completion.
///
/// The system message carries the data; this only tells the model what to do with it. It is
/// hidden from replay by the sentinel prefix.
pub fn background_job_resume_prompt() -> String {
    format!(
        "{BACKGROUND_JOB_RESUME_PREFIX}\n\
         A background shell job you started has just stopped. Read its result above, then continue \
         the task: report what happened, or take the next step the result calls for. Do not repeat \
         work that already finished."
    )
}

/// Render a delegated task notification line:
/// "delegate: title > last tool call (total running time)"
pub fn format_delegate_notification(
    glyph: &str,
    title: &str,
    last_tool: &str,
    running_time: &str,
) -> String {
    format!("{glyph} delegate: {title} > {last_tool} ({running_time})")
}
