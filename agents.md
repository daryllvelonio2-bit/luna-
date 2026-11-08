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
- **Verification gate:** Run `scripts/test-changed.sh` (only the tests affected by the diff) plus `cargo check`/`cargo clippy` on touched crates before completing changes. Use `scripts/test-changed.sh --full` before pushing a change that spans crates.
- **Build and install:** Always build and install (`cargo install --path crates/harness-cli`) upon completing tasks so that the environment runs the updated `luna` binary.
- **Keep only main & clean worktrees:** Delete all other worktrees and keep only `main`. Never leave implemented features stranded or unused in abandoned worktrees; any temporary worktrees must be merged and pruned immediately.
- **Commit authorship:** Commits must be authored and committed as `Daryll <daryllvelonio2@gmail.com>`.
- **Push verified work:** After the verification gate passes, commit the completed change and push it to the remote tracking branch (`git push origin <branch>`) unless the user explicitly asks not to. Never push unverified or half-finished work.

- **Version bumping:** Bump `version.workspace` in `Cargo.toml` on every release. Increment the patch by +1 each update (`0.1.1 → 0.1.2 → … → 0.1.100`), then bump the minor (`0.1.100 → 0.2.1`) once patch exceeds 100. Update both `version.workspace` and the `version` string in `crates/harness-cli/src/main.rs` (`name = "luna"`) to match.

- **Always commit and push to main:** After any completed task (verification gate passed), commit all changes and push to `main` (`git push origin main`). Do not leave changes uncommitted or stranded in the working tree. Never push unverified or half-finished work.

## 4. Fast Builds (this workspace)

- **Warm is seconds, cold is tens of minutes.** Measured warm: `cargo check -p harness-cli --bin luna` ~1.2 s; `cargo build --release -p harness-cli --bin luna` ~1.2 s.
- **Stay targeted.** Root `cargo build`/`cargo test` builds all 12 members. Prefer `cargo check -p <crate>`, `scripts/test-changed.sh` (or `cargo test -p <crate> --bin luna` / `--test <name>`), and `cargo clippy -p <crate> --all-targets -- -D warnings`.
- **Targeted tests.** `scripts/test-changed.sh` maps the working-tree diff to affected crates and test targets, then runs only those. `--dry-run` prints the plan, `--full` runs `cargo test --workspace`, `--no-check` skips the dependent-compile gate, and `--base <rev>` diffs against another revision. It is the default; do not start with `cargo test --workspace`.
- **Rebuild scope.** Editing `harness-cli` rebuilds only the CLI. Editing `harness-core` rebuilds nearly every crate that depends on it. A new crate rebuilds itself plus its dependents. `[profile.*]`, `RUSTFLAGS`, linker, or edition changes force a full rebuild — avoid them for feature work.
- **Parallelism stays at 2.** `CARGO_BUILD_JOBS=2`; the box has 8 cores but ~7 GB RAM, and more jobs push rustc into swap.
- **Install reuses the release build.** Build `cargo build --release -p harness-cli --bin luna` once, then `cargo install --path crates/harness-cli --force` (and `--root ~/.local`). Do not `cargo clean` first.
- **Disk.** `target/` can exceed 30 GB. Keep at least 5 GB free; when full, delete `target/debug/incremental` and `target/debug/examples` before considering `cargo clean`.

See `docs/AGENT_RULES.md` §2 for the cold-build and shared-`target/` caveats.

## 5. Remote Build Automation (off-device builds)

- **Why:** `target/` can exceed 30 GB. On a full disk, building locally fills `/` and fails. Build on GitHub Actions instead — no local disk used.
- **Workflow file:** `.github/workflows/release-build.yml` (`workflow_dispatch` trigger).
  - Runs on `ubuntu-latest`.
  - Checks out the repo (ref from input, default `main`).
  - Installs stable Rust toolchain (`dtolnay/rust-toolchain@stable`).
  - Installs ALSA system dependency (`sudo apt-get install -y libasound2-dev`, required by `harness-voice`/`cpal`/`alsa-sys`).
  - Runs `cargo build --release --bin luna`.
  - Uploads the binary as artifact `luna-binary` via `actions/upload-artifact@v4` (path: `target/release/luna`).
- **Trigger + install script:** `scripts/auto_build_install.py`.
  1. Resolves the workflow ID from the GitHub Actions API (`GET /repos/{owner}/{repo}/actions/workflows`).
  2. Triggers `workflow_dispatch` via `POST /repos/{owner}/{repo}/actions/workflows/{workflow_id}/dispatches` with `ref: main`.
  3. Polls `GET /repos/{owner}/{repo}/actions/runs?per_page=1` until status is `completed`.
  4. Lists artifacts for the run (`GET /repos/{owner}/{repo}/actions/runs/{run_id}/artifacts`) and finds `luna-binary`.
  5. Downloads the ZIP (`GET /repos/{owner}/{repo}/actions/artifacts/{artifact_id}/zip`).
  6. Extracts and installs `luna` to `~/.local/bin/luna` (chmod +x).
- **Run:** `GITHUB_TOKEN=<token> python3 scripts/auto_build_install.py`
- **Token scope:** `workflow` + `repo` on `daryllvelonio2-bit/luna-`. Token is already stored in `~/.git-credentials`.
- **Verification:** After install, run `~/.local/bin/luna --version` (expects `luna 0.1.0`).
- **Installed:** `luna 0.1.0` at `~/.local/bin/luna` (on PATH).
