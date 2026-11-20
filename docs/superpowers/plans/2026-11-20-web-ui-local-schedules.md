# Implementation Plan: Web UI/UX Optimization and Device-Local Automations

Date: 2026-11-20
Spec: `docs/superpowers/specs/2026-11-20-web-ui-local-schedules-design.md`
Target: Modernize Luna web UI/UX for clarity and responsiveness, and implement native device-local wall-clock scheduling.

## Overview & Architecture

This plan executes across three focused tracks:
1. **Scheduler Engine Core**: Extend rule syntax with `daily-local:HH:MM` and `local-cron:<expr>` evaluated against host device time (`chrono::Local`), handling DST transitions and preserving legacy UTC rules.
2. **Web Scheduling Architecture**: Extract a dedicated `scheduleUtils.ts` layer with unit tests for rule parsing, human-readable previews, and timezone indicators.
3. **Web UI/UX Refinement**: Elevate `AutomationsView`, `Sidebar`, `ChatView`, and responsive CSS styles for accessible, clear, and resilient operation across mobile and desktop viewports.

---

## Tasks

### Task 1: Isolated Worktree Setup
- **Files**: `.git/` worktree management
- **Objective**: Create an isolated git worktree branch `feat/web-ui-local-schedules` tracking `origin/main` to ensure clean separation from unrelated modified files.
- **Verification**: `git branch --show-current` outputs `feat/web-ui-local-schedules`.

### Task 2: Scheduler Local Rules (TDD - Tests First)
- **Files**:
  - `crates/harness-scheduler/src/model.rs`
  - `crates/harness-scheduler/src/lib.rs`
- **Steps**:
  1. Write failing tests in `crates/harness-scheduler/src/model.rs`:
     - Test `compute_next_run("daily-local:20:00", after)` verifies the returned UTC timestamp matches 20:00:00 in host device local time.
     - Test `compute_next_run("local-cron:0 20 * * *", after)` verifies 20:00 in host local time.
     - Test legacy `compute_next_run("daily:20:00", after)` still computes strictly against UTC.
     - Test edge cases: invalid local rule syntax, nonexistent local time in spring-forward DST (skips forward to valid minute), ambiguous time in fall-back DST (picks earliest).
  2. Implement local rule evaluation in `model.rs`:
     - Implement `compute_next_run_daily_local(hour, minute, after: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, String>`.
     - Implement `compute_next_run_cron_local(expr, after: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, String>`.
     - Update `validate_rule` to accept `daily-local:HH:MM` and `local-cron:...`.
  3. Verify all tests pass cleanly.

### Task 3: Automation Skill & Tool Documentation Updates
- **Files**:
  - `crates/harness-scheduler/src/tools.rs`
  - `crates/harness-scheduler/src/skill.rs`
  - `.harness/skills/automation/SKILL.md`
- **Steps**:
  1. Update `schedule_create` description and schema in `tools.rs` documenting `daily-local:HH:MM` and `local-cron:...` as default local rules.
  2. Update embedded skill instructions and `.harness/skills/automation/SKILL.md`:
     - Instruct agent that wall-clock times (e.g. "8pm") must be scheduled with `daily-local:20:00` without asking the user for timezone conversion.
     - Document that `daily:` and bare cron remain available for explicit UTC requirements.

### Task 4: Web Schedule Utilities & Unit Tests (TDD)
- **Files**:
  - `web/src/scheduleUtils.ts`
  - `web/src/scheduleUtils.test.ts` or standalone validation script
- **Steps**:
  1. Implement and test helper functions:
     - `parseScheduleRule(rule: string)` returning cadence type (`daily_local`, `daily_utc`, `interval`, `cron_local`, `cron_utc`, `once`) and parameters.
     - `formatScheduleRule(...)` producing normalized rule strings.
     - `describeScheduleRule(rule: string)` returning clear user-facing description (e.g., "Daily at 8:00 PM (Device local time)", "Every 15 minutes", "Daily at 20:00 UTC (Legacy)").
     - `getLocalDeviceTimezone()` returning browser timezone and offset note.
  2. Execute standalone tests using Node to verify all rule formats parse and describe accurately.

### Task 5: Web UI Redesign - Automations View
- **Files**:
  - `web/src/components/AutomationsView.tsx`
  - `web/src/styles.css`
- **Steps**:
  1. Add direct "New Automation" modal directly within `AutomationsView.tsx` (mirroring the editor modal with empty state and default `daily-local` cadence).
  2. Introduce prominent cadence selector:
     - "Daily (Device local)" (default for time-of-day)
     - "Interval" (every N minutes/hours/seconds)
     - "One-time"
     - "Advanced (Cron)" with local/UTC toggle
  3. Display clear local time guidance banner: "Runs according to Luna device time".
  4. Redesign schedule cards:
     - Clear title hierarchy, friendly schedule badges, concise instruction excerpt.
     - Compact metadata footer with next run time, last run status badge, and history drawer toggle.
     - Refined action bar: "Run now", "Pause/Resume", "Edit", "Delete".
  5. Refine responsive layout for mobile screens (`< 560px` and `< 768px`).

### Task 6: Web UI Redesign - Workspace Layout, Sidebar, and Chat
- **Files**:
  - `web/src/components/Sidebar.tsx`
  - `web/src/components/ChatView.tsx`
  - `web/src/components/Composer.tsx`
  - `web/src/styles.css`
- **Steps**:
  1. **Sidebar**:
     - Add accessible backdrop for mobile overlay drawer.
     - Add `aria-current="page"` to active tab nav item.
     - Smooth sidebar collapse/expand transitions and Esc key dismissal on mobile.
  2. **ChatView & Composer**:
     - Clean up header badges and eliminate visual clutter.
     - Strengthen composer visual anchoring and clarify action states.
     - Standardize focus rings (`:focus-visible`) and contrast across dark and light themes.
  3. **Accessibility**:
     - Ensure all interactive buttons have `aria-label` or visible text.

### Task 7: Build & Mechanical Design Verification
- **Files**: `web/`
- **Steps**:
  1. Run `npm run typecheck` in `web/`.
  2. Run `npm run build` in `web/`.
  3. Run `/home/janelle/.luna/skills/impeccable/scripts/impeccable context` and mechanical detector to verify no craft floor violations.
  4. Inspect UI in browser via `desktop_screenshot` and snapshot.

### Task 8: GitHub Actions CI Verification & Final Report
- **Files**: Repository git branch
- **Steps**:
  1. Push feature branch to origin remote (`daryllvelonio2-bit/luna-`).
  2. Trigger GitHub Actions CI workflow using `gh workflow run ci.yml --ref <branch>` or API dispatch.
  3. Monitor workflow completion to verify `cargo clippy`, `cargo test`, `npm run typecheck`, and `npm run build` pass in clean CI environment.
  4. Produce concise completion report.
