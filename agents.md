# Luna Agent Guidelines (`agents.md`)

## 1. Core Principles

**No hardcoded UI literals.** User-visible text, labels, banners, separators, and prompts belong in `harness-cli` or configuration (`Config`), never in library crates (`harness-core`, `harness-providers`, `harness-memory`, etc.). Keep presentation decoupled from core engine logic.

**Only logic in library crates.** Core and engine crates decide *what happens* — execution loop, tool calls, state persistence, protocol handling. Presentation and formatting belong in `harness-cli`. A library crate emitting `println!` or formatting ANSI colors is a layering violation — use `tracing` for instrumentation.

**Hardcoding is allowed only for:** protocol constants, provider endpoints in catalog data, and structural constants. Everything else is resolved from configuration with documented defaults.

**No magic numbers.** A bare number in logic must either be a named constant explaining *why* that value was chosen, or a configurable field in `Config`. This applies to timeouts, retry counts, buffer capacities, and token thresholds.

## 2. Rust Code & Structure Rules

- **File size & modular decomposition:** Hard limit of 2,000 lines of code per file (target <1,000 lines). When a file grows beyond ~1,000 lines or handles multiple distinct responsibilities, decompose it into focused topical submodules under a module directory with a clean facade.
- **Idiomatic pattern matching:** Use `match` or dispatch tables over `if/else` ladders when branching across 4+ variants or commands.
- **No speculative abstraction:** No trait with only one implementation, no wrapper struct that does nothing, no unused re-exports, no flags nobody sets.
- **Fail closed & propagate errors:**
  - Never call `unwrap()` or `expect()` on I/O, network, database, or parsing in library crates; return `HarnessError`.
  - No swallowed errors: `let _ = ...` on fallible calls is forbidden unless explicitly documented why failure is harmless.
- **Explicit path resolution:** Never hardcode `$HOME/.luna` strings; resolve all runtime paths from `config.paths`.
- **Comments keep the WHY:** Document intent and invariants. Delete comments that merely restate the code.

## 3. Verification & Git

- **Reproduce before patching:** Identify the exact file and line, verify against current behavior, and write or run targeted tests.
- **Behavior contracts over snapshots:** Tests must assert invariants and contracts, not brittle text snapshots.
- **Verification gate:** Always verify with `cargo check` and `cargo test` on touched crates before completing changes.
- **Worktree isolation:** Execute edits only within the assigned workspace or worktree; never contaminate other worktrees or shared checkouts.
- **Commit authorship:** Commits must be authored and committed as `Daryll <daryllvelonio2@gmail.com>`.
