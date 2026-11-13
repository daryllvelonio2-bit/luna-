//! Autonomous multi-turn goal loop state, prompt assembly, and verdict parsing.
//!
//! Pure logic only — no terminal formatting or UI literals live here. Both the interactive chat
//! loop and the headless `luna goal` subcommand share this state machine so iteration budgets,
//! continuation prompts, and completion/blocker detection never drift apart.

use crate::error::HarnessError;
use crate::prompts::{self, GOAL_CONTINUE_PROMPT, GOAL_START_PROMPT};

/// Default iteration budget when neither `--max` nor `goal.max_iterations` is overridden.
///
/// Ten turns gives multi-step coding tasks room to plan, edit, test, and fix failures while still
/// bounding runaway token spend if a task stalls.
pub const DEFAULT_GOAL_MAX_ITERATIONS: usize = 10;

/// Hard upper bound for `goal.max_iterations` and `--max-iterations`.
///
/// Prevents typos or unbounded config values from running an unattended loop indefinitely.
pub const MAX_GOAL_ITERATIONS_CEILING: usize = 50;

/// Sentinel tag a model emits on its final line when the goal is finished and verified.
pub const GOAL_COMPLETE_TAG: &str = "<goal_complete>";

/// Prefix of the blocker sentinel (`<goal_blocked: reason>`).
pub const GOAL_BLOCKED_PREFIX: &str = "<goal_blocked:";

/// Example placeholder inserted into goal prompts so the model sees the exact syntax.
pub const GOAL_BLOCKED_EXAMPLE: &str = "<goal_blocked: reason>";

/// First line of internal continuation prompts (`iteration >= 2`).
///
/// Transcript replay checks for this prefix so automated continuation prompts do not render as
/// messages the user typed.
pub const GOAL_ITERATION_PREFIX: &str = "[goal loop iteration]";

/// What a single iteration's reply tells the goal loop to do next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalVerdict {
    /// Objective not yet marked complete or blocked; advance if budget remains.
    Continue,
    /// Model verified the objective is complete; carries the cleaned final reply.
    Complete(String),
    /// Model hit an unresolvable blocker; carries the extracted reason.
    Blocked(String),
}

/// Terminal state of a goal run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalOutcome {
    Completed { iterations: usize, summary: String },
    Blocked { iterations: usize, reason: String },
    MaxIterationsReached { iterations: usize },
    Stopped { iterations: usize },
}

/// Tracks one active autonomous goal run across iterations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalRunState {
    pub objective: String,
    pub iteration: usize,
    pub max_iterations: usize,
}

impl GoalRunState {
    /// Start a new goal run at iteration 1 after validating objective and iteration budget.
    pub fn new(objective: impl Into<String>, max_iterations: usize) -> Result<Self, HarnessError> {
        let objective = objective.into().trim().to_string();
        if objective.is_empty() {
            return Err(HarnessError::ConfigurationError(
                "goal objective must not be empty".to_string(),
            ));
        }
        let max_iterations = validate_max_iterations(max_iterations)?;
        Ok(Self {
            objective,
            iteration: 1,
            max_iterations,
        })
    }

    /// Build the model prompt for the current iteration from `prompts.md`.
    ///
    /// Iteration 1 uses `GOAL_START_PROMPT`; iterations 2..=N prepend [`GOAL_ITERATION_PREFIX`]
    /// to `GOAL_CONTINUE_PROMPT` so transcript replay can distinguish automated continuations from
    /// user-typed messages.
    pub fn prompt_for_current_iteration(&self) -> Result<String, HarnessError> {
        let section_name = if self.iteration <= 1 {
            GOAL_START_PROMPT
        } else {
            GOAL_CONTINUE_PROMPT
        };
        let template = prompts::section(section_name)?;
        let rendered = template
            .replace("{{GOAL_OBJECTIVE}}", &self.objective)
            .replace("{{ITERATION}}", &self.iteration.to_string())
            .replace("{{MAX_ITERATIONS}}", &self.max_iterations.to_string())
            .replace("{{GOAL_COMPLETE_TAG}}", GOAL_COMPLETE_TAG)
            .replace("{{GOAL_BLOCKED_EXAMPLE}}", GOAL_BLOCKED_EXAMPLE);

        if self.iteration <= 1 {
            Ok(rendered)
        } else {
            Ok(format!(
                "{} {}/{}\n\n{}",
                GOAL_ITERATION_PREFIX, self.iteration, self.max_iterations, rendered
            ))
        }
    }

    /// Advance to the next iteration if budget remains. Returns `false` when the budget is spent.
    pub fn advance(&mut self) -> bool {
        if self.iteration < self.max_iterations {
            self.iteration += 1;
            true
        } else {
            false
        }
    }

    /// Parse a turn reply for `<goal_complete>` or `<goal_blocked: ...>` directives.
    pub fn evaluate_reply(reply: &str) -> GoalVerdict {
        evaluate_reply(reply)
    }
}

/// Validate a goal iteration limit against `1..=MAX_GOAL_ITERATIONS_CEILING`.
pub fn validate_max_iterations(max_iterations: usize) -> Result<usize, HarnessError> {
    if max_iterations == 0 || max_iterations > MAX_GOAL_ITERATIONS_CEILING {
        return Err(HarnessError::ConfigurationError(format!(
            "goal.max_iterations must be between 1 and {MAX_GOAL_ITERATIONS_CEILING}, got {max_iterations}"
        )));
    }
    Ok(max_iterations)
}

/// Parse optional `--max <N>` or `--max-iterations <N>` flags at the front of `/goal` arguments.
pub fn parse_goal_args(raw: &str, default_max: usize) -> Result<(usize, String), HarnessError> {
    let mut rest = raw.trim();
    let mut max_iterations = validate_max_iterations(default_max)?;

    while let Some(after) = strip_flag(rest, "--max-iterations")
        .or_else(|| strip_flag(rest, "--max"))
        .or_else(|| strip_flag(rest, "-n"))
    {
        let after = after.trim_start();
        let (num_str, tail) = if let Some(eq_rest) = after.strip_prefix('=') {
            split_first_token(eq_rest.trim_start())
        } else {
            split_first_token(after)
        };
        if num_str.is_empty() {
            return Err(HarnessError::ConfigurationError(
                "expected a number after --max / --max-iterations".to_string(),
            ));
        }
        let parsed = num_str.parse::<usize>().map_err(|_| {
            HarnessError::ConfigurationError(format!("invalid max iterations value '{num_str}'"))
        })?;
        max_iterations = validate_max_iterations(parsed)?;
        rest = tail.trim_start();
    }

    let objective = rest.trim().to_string();
    if objective.is_empty() {
        return Err(HarnessError::ConfigurationError(
            "goal objective must not be empty".to_string(),
        ));
    }
    Ok((max_iterations, objective))
}

fn strip_flag<'a>(input: &'a str, flag: &str) -> Option<&'a str> {
    let tail = input.strip_prefix(flag)?;
    if tail.is_empty() || tail.starts_with('=') || tail.starts_with(char::is_whitespace) {
        Some(tail)
    } else {
        None
    }
}

fn split_first_token(input: &str) -> (&str, &str) {
    match input.find(char::is_whitespace) {
        Some(idx) => (&input[..idx], &input[idx..]),
        None => (input, ""),
    }
}

/// Inspect a model reply outside fenced code blocks (bottom-up) for completion or blocker tags.
pub fn evaluate_reply(reply: &str) -> GoalVerdict {
    let lines: Vec<&str> = reply.lines().collect();
    let mut in_fence = vec![false; lines.len()];
    let mut fenced = false;
    for (idx, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence[idx] = true;
            fenced = !fenced;
        } else {
            in_fence[idx] = fenced;
        }
    }

    for idx in (0..lines.len()).rev() {
        if in_fence[idx] {
            continue;
        }
        let line = lines[idx].trim();
        if line.is_empty() {
            continue;
        }
        if let Some(reason) = extract_blocked_reason(line) {
            return GoalVerdict::Blocked(reason);
        }
        if line == GOAL_COMPLETE_TAG || line.ends_with(GOAL_COMPLETE_TAG) {
            let cleaned = strip_complete_tag(&lines, idx);
            return GoalVerdict::Complete(cleaned);
        }
    }

    GoalVerdict::Continue
}

fn extract_blocked_reason(line: &str) -> Option<String> {
    let start = line.find(GOAL_BLOCKED_PREFIX)?;
    let after_prefix = &line[start + GOAL_BLOCKED_PREFIX.len()..];
    let end = after_prefix.find('>')?;
    let reason = after_prefix[..end].trim();
    if reason.is_empty() {
        Some("goal reported blocked".to_string())
    } else {
        Some(reason.to_string())
    }
}

fn strip_complete_tag(lines: &[&str], tag_line_idx: usize) -> String {
    let mut out = Vec::with_capacity(lines.len());
    for (idx, line) in lines.iter().enumerate() {
        if idx == tag_line_idx {
            let stripped = line
                .trim_end()
                .strip_suffix(GOAL_COMPLETE_TAG)
                .unwrap_or(line);
            let trimmed = stripped.trim_end();
            if !trimmed.is_empty() {
                out.push(trimmed);
            }
        } else {
            out.push(*line);
        }
    }
    out.join("\n").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_state_starts_at_iteration_one_and_advances_to_limit() {
        let mut state = GoalRunState::new("fix failing tests", 3).expect("valid state");
        assert_eq!(state.iteration, 1);
        assert_eq!(state.max_iterations, 3);
        assert!(state.advance());
        assert_eq!(state.iteration, 2);
        assert!(state.advance());
        assert_eq!(state.iteration, 3);
        assert!(!state.advance());
        assert_eq!(state.iteration, 3);
    }

    #[test]
    fn empty_objective_and_invalid_limits_are_rejected() {
        assert!(GoalRunState::new("   ", 5).is_err());
        assert!(GoalRunState::new("ok", 0).is_err());
        assert!(GoalRunState::new("ok", MAX_GOAL_ITERATIONS_CEILING + 1).is_err());
    }

    #[test]
    fn parse_goal_args_extracts_optional_max_flag() {
        let (max, obj) = parse_goal_args("--max 12 run cargo test", 10).unwrap();
        assert_eq!(max, 12);
        assert_eq!(obj, "run cargo test");

        let (max, obj) = parse_goal_args("--max-iterations=4 fix clippy", 10).unwrap();
        assert_eq!(max, 4);
        assert_eq!(obj, "fix clippy");

        let (max, obj) = parse_goal_args("build and verify", 10).unwrap();
        assert_eq!(max, 10);
        assert_eq!(obj, "build and verify");

        assert!(parse_goal_args("--max 0 task", 10).is_err());
        assert!(parse_goal_args("--max 5", 10).is_err());
    }

    #[test]
    fn prompts_load_and_prefix_continuation_turns() {
        let mut state = GoalRunState::new("ship feature", 5).unwrap();
        let first = state.prompt_for_current_iteration().unwrap();
        assert!(first.contains("iteration 1/5"));
        assert!(first.contains("ship feature"));
        assert!(!first.starts_with(GOAL_ITERATION_PREFIX));

        assert!(state.advance());
        let second = state.prompt_for_current_iteration().unwrap();
        assert!(second.starts_with(GOAL_ITERATION_PREFIX));
        assert!(second.contains("iteration 2/5"));
        assert!(second.contains("ship feature"));
    }

    #[test]
    fn evaluate_reply_detects_complete_blocked_and_ignores_code_fences() {
        assert_eq!(
            evaluate_reply("Still working on step 2."),
            GoalVerdict::Continue
        );

        assert_eq!(
            evaluate_reply("All tests passed.\n<goal_complete>"),
            GoalVerdict::Complete("All tests passed.".to_string())
        );

        assert_eq!(
            evaluate_reply("Need API token.\n<goal_blocked: missing credentials>"),
            GoalVerdict::Blocked("missing credentials".to_string())
        );

        let quoted =
            "I will check how `<goal_complete>` works:\n```\n<goal_complete>\n```\nContinuing now.";
        assert_eq!(evaluate_reply(quoted), GoalVerdict::Continue);
    }
}
