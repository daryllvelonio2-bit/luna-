# Luna Agent Guidelines (`agents.md`)

## 1. Core Principles

**No hardcoded UI literals.** User-visible text, labels, banners, separators, and prompts belong in `harness-cli` or configuration (`Config`), never in library crates (`harness-core`, `harness-providers`, `harness-memory`, etc.). Keep presentation decoupled from core engine logic.

**Only logic in library crates.** Core and engine crates decide *what happens* — execution loop, tool calls, state persistence, protocol handling. Presentation and formatting belong in `harness-cli`. A library crate emitting `println!` or formatting ANSI colors is a layering violation — use `tracing` for instrumentation.

**Hardcoding is allowed only for:** protocol constants, provider endpoints in catalog data, and structural constants. Everything else is resolved from configuration with documented defaults.

**No magic numbers.** A bare number in logic must either be a named constant explaining *why* that value was chosen, or a configurable field in `Config`. This applies to timeouts, retry counts, buffer capacities, and token thresholds.

**Global TUI theme:** Any TUI view, widget, dialog, or component must use the centralized `Theme` (`crate::tui::theme::Theme`) and respect the global `ThemeMode` (`Transparent`, `Dark`, `Light`). Never hardcode terminal colors (raw ANSI escape codes, hardcoded RGB values, or fixed background fills) directly inside views or widgets. All UI colors, card backgrounds, borders, badges, and accents must resolve through the active theme.

## 2. Rust Code & Structure Rules

- **File size:** Always stick to a hard limit of 2,000 lines of code per file. Decompose any file that reaches 2,000 lines into focused topical submodules under a module directory with a clean facade.
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
- **Build and install:** Always build and install (`cargo install --path crates/harness-cli`) upon completing tasks so that the environment runs the updated `luna` binary.
- **Keep only main & clean worktrees:** Delete all other worktrees and keep only `main`. Never leave implemented features stranded or unused in abandoned worktrees; any temporary worktrees must be merged and pruned immediately.
- **Commit authorship:** Commits must be authored and committed as `Daryll <daryllvelonio2@gmail.com>`.
- **Push verified work:** After the verification gate passes, commit the completed change and push it to the remote tracking branch (`git push origin <branch>`) unless the user explicitly asks not to. Never push unverified or half-finished work.
