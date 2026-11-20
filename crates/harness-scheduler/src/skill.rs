//! Built-in `automation` skill shipped inside the Luna binary.
//!
//! Ensures the agent can always inspect `skill_view("automation")` when handling
//! `/automation` requests, even outside a workspace containing `.harness/skills`.

use harness_plugins::Skill;

pub const AUTOMATION_SKILL_NAME: &str = "automation";

pub const AUTOMATION_SKILL_DESCRIPTION: &str =
    "Plan, clarify, and create persistent recurring agent automations and scheduled workflows for any domain. Use whenever the user invokes /automation or asks to automate recurring tasks.";

pub const AUTOMATION_SKILL_INSTRUCTIONS: &str = r#"# Automation & Scheduled Workflows Guide

You are Luna's Automation Architect. When the user invokes `/automation` or asks to build a recurring workflow, convert their natural-language request into reliable, self-contained, persistent scheduled automation task(s).

## Workflow

### 1. Decompose the Request
Identify the core components of any requested automation:
- **Stages & Task Boundaries**: Decide whether the workflow is a single recurring task or multiple sequential tasks on separate schedules.
  - When stage B depends on stage A's output (for example, a research or collection step followed later by a build, report, or publishing step), create separate tasks and define a shared handoff location (such as a workspace file path or persistent note via `save_note` / `read_note`).
- **Cadence & Schedule Rules**: Translate natural language timing into valid `schedule_create` rules:
  - `daily-local:HH:MM` — daily at a 24-hour time using host device local time (e.g. `daily-local:20:00` for 8pm). Default for all user-stated wall-clock times. Do NOT ask the user to convert to UTC.
  - `daily:HH:MM` — daily at a 24-hour UTC time (legacy or when the user explicitly requests UTC).
  - `every:<seconds>` — fixed recurring interval, minimum 10 seconds (e.g. `every:3600`).
  - `once:<RFC3339>` — one-shot scheduled timestamp.
  - `local-cron:<min> <hour> <dom> <mon> <dow>` — 5-field cron in host device local time.
  - 5-field UTC cron (`min hour dom mon dow`, e.g. `0 9 * * 1-5`).
- **Dependencies, Tools & Feasibility**: Identify what tools, external accounts, credentials, files, or environment setup the unattended agent will need on each run.

### 2. Clarify Missing Details Before Creating
Do not guess when critical execution parameters are missing or ambiguous. Wall-clock times directly use `daily-local:HH:MM` on device local time without timezone clarification. Use the `clarify` tool first to ask concise, focused questions (2–4 questions with clickable options plus free-text) when other details are missing:
- **Execution Pipeline & Tools**: Ask which tools, scripts, services, or output formats should be used if multiple approaches exist.
- **Side Effects & Safety**: When an automation publishes, posts, modifies external state, or deploys, ask whether to execute automatically or save outputs/drafts locally for review first.
- **Inputs & Outputs**: Confirm target directories, repositories, channels, or data sources if unspecified.

### 3. Create or Manage Persistent Tasks
Once requirements are confirmed, use the scheduling tools:
- `schedule_create`: Create each automation task with:
  - `name`: Short, clear task title.
  - `rule`: Validated recurrence rule.
  - `instruction`: A complete, self-contained prompt telling the unattended agent:
    1. What inputs or prior-stage outputs to read.
    2. Exact steps, tools, and quality criteria to follow.
    3. Where to write outputs, artifacts, or handoff notes.
    4. How to handle failures gracefully (e.g. log error details and preserve partial outputs).
  - `target`:
    - `new_session_each_run` (default — isolated fresh session per run).
    - `single_new_session` (reuses one dedicated automation thread across runs).
    - `existing_session` (delivers into an existing session ID).
- `schedule_list`, `schedule_view`, `schedule_edit`: Inspect, update, pause, or resume existing automations.

### 4. Summarize for the User
After creating or modifying tasks, report:
- Each task's **Name**, **ID**, **Rule**, and **Next Run Time**.
- Any shared file paths or notes connecting multi-stage tasks.
- That tasks can be monitored, edited, toggled, or manually triggered via **Run Now** in the dedicated **Automations** view (`luna web`) or inspected via `schedule_list`.
"#;

/// Build the built-in `automation` skill.
pub fn automation_skill() -> Skill {
    Skill::builtin(
        AUTOMATION_SKILL_NAME,
        AUTOMATION_SKILL_DESCRIPTION,
        AUTOMATION_SKILL_INSTRUCTIONS,
    )
}

/// Default pre-fill string when the user selects `/automation` in CLI/TUI/Web composers.
pub const AUTOMATION_PREFILL: &str = "/automation guide me with creating an  automation ";

/// Format a user `/automation <arg>` request into the model prompt that tells
/// the agent to load the `automation` skill, clarify if needed, and create tasks.
pub fn build_automation_turn_prompt(user_request: &str) -> String {
    let trimmed = user_request.trim();
    format!(
        "Automation Workflow Request:\n\
         1. First call `skill_view` with `name: \"automation\"` to load the global automation workflow instructions.\n\
         2. Follow those instructions carefully for the user's request below.\n\
         3. If critical details (tools/pipeline, side-effect boundaries, inputs/outputs, etc.) are missing or ambiguous, call `clarify` first before creating any schedules (wall-clock times map directly to daily-local without timezone clarification).\n\
         4. Once requirements are clear, create or update the scheduled automation task(s) using `schedule_create` / `schedule_edit`.\n\n\
         User request:\n{trimmed}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_automation_skill_is_general_purpose() {
        let s = automation_skill();
        assert_eq!(s.name, "automation");
        assert!(s.instructions.contains("schedule_create"));
        assert!(s.instructions.contains("clarify"));
    }

    #[test]
    fn turn_prompt_instructs_skill_view_and_clarify() {
        let prompt = build_automation_turn_prompt("guide me with automation daily report at 7pm");
        assert!(prompt.contains("skill_view"));
        assert!(prompt.contains("automation"));
        assert!(prompt.contains("clarify"));
        assert!(prompt.contains("daily report at 7pm"));
    }
}
