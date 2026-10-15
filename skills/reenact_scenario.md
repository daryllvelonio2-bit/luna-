---
name: reenact_scenario
description: Re-enact a reproducer scenario in a terminal PTY against an improved worktree binary, check for regressions, and stage the update.
---

# Scenario Re-enactment & Regression Verification

When a background improvement job reaches `ReadyForReenactment` in the job queue:
1. Call `list_improvement_jobs` or `check_improvement_job` with the `job_id` to inspect the job details and compiled worktree binary path.
2. Review the original problem scene or bottleneck context that triggered `delegate_improvement`.
3. Construct the exact reproducer sequence or input commands that triggered the failure.
4. Call `verify_and_stage_update` with:
   - `job_id`: The ID of the completed job.
   - `scenario_input`: The precise input string / commands to execute in the PTY.
   - `absent_pattern`: The exact error message, warning, or bottleneck pattern that must NOT be present in output.
5. If `verify_and_stage_update` reports clean exit code 0 and absent pattern verified:
   - The job is marked as `ScheduledUpdate` and `pending_update.json` is created.
   - Inform the user that the improvement has been verified against the reproducer without regressions and will be applied on the next Luna launch.
6. If verification fails:
   - Analyze the failure reason or regression output.
   - Re-delegate or notify the user of what went wrong.
