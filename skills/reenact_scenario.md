---
name: reenact_scenario
description: Re-enact a reproducer scenario in a terminal PTY against an improved worktree binary, check 2D screen captures for visual regressions, and stage the update.
---

# Scenario Re-enactment & Regression Verification

When a background improvement job reaches `ReadyForReenactment` in the job queue:

## 1. Discover Ready Jobs
- Call `list_improvement_jobs` to see all jobs currently in the queue.
- Call `check_improvement_job(job_id)` to review the implementation logs, modified branch, and compiled worktree binary path.

## 2. Interactive PTY Screen Inspection
You have access to `pty_screen_capture` to interact with any binary in a real virtual terminal emulator (VT100) and receive a full 2D screen capture snapshot:
- Parameters:
  - `binary`: Path to the executable (e.g. the worktree binary `worktree_dir/target/release/luna`).
  - `args`: Command-line flags (e.g. `["--session", "test"]`).
  - `inputs`: Ordered array of timed keystrokes: `[{"delay_ms": 400, "input": "/model\r"}]`. Use `\r` for Enter, `\x03` for Ctrl-C, `\x1b[B` for Down arrow.
  - `rows` & `cols`: Virtual terminal dimensions (e.g. 24x80 or 40x100).
- The returned `screen_display` shows row-by-row text with cursor position (`▶`), exactly as a human sees it in their terminal.

## 3. Visual & Functional Invariant Checks
Before approving an update, verify the screen capture:
- **No Line Wrap Spam**: Menus, status bars, and boxes must not duplicate or shift downwards across redraws.
- **Layout Cleanliness**: Dividers `───`, status bars, and prompts `☾` must stay on their expected lines.
- **Absence of Regressions**: Confirm that raw ANSI codes, stray error messages, or previous symptoms are completely absent from the rendered display.

## 4. Re-enacting and Staging
- Call `verify_and_stage_update`:
  - `job_id`: The ID of the completed job.
  - `scenario_input`: The precise reproducer keystroke sequence that triggered the original bottleneck.
  - `absent_pattern`: The error substring or regression pattern that must NOT appear in output or on the screen.
- If verified:
  - The tool returns the final 2D screen capture and marks the job `ScheduledUpdate`, staging `pending_update.json`.
  - Inform the user with a concise summary of the verification and confirm the update will apply automatically on the next launch.
- If verification fails:
  - The worktree is kept or safely torn down with the failure reason logged.
  - Report the regression or failed invariant to the user.
