# Luna Rust Harness — working rules for agents in this repo

Read this before touching anything. It overrides habit.

## 1. What this is

`luna` is a Rust autonomous-agent harness: a 12-crate workspace under `crates/`. It is a real
binary, not a mock — `cargo run --bin luna -- --help`. It talks to live model providers, keeps
SQLite memory records and markdown-file notes under `paths.notes_dir`, speaks MCP, and runs
sandboxed wasm plugins.

Board: `luna-harness` (`shiina kanban --board luna-harness …`). Owner: Daryll. Every card is
assigned by the dispatcher; you never pick your own lane.

## 2. Environment (already set up — do not re-invent)

- Toolchain: `cargo` 1.98 / `rustc` 1.98 on PATH. No rustup needed.
- **Every worktree builds into its own `target/` — never share one between trees.** Cargo's dep-info
  is written relative to each tree's own root, so two checkouts produce the *same* artifact filename
  and one tree can be handed the other's crate: the signature is a compile error naming a type or
  symbol that does not exist anywhere in your tree, usually in a file your card never touched. That
  is contamination, not your bug — `cargo clean -p <crate>` (or `touch` the file) and rebuild,
  and say so in the handoff instead of "fixing" it. The same mechanism can silently build stale code.
  Measured on this fleet (2026-10-15): a shared target dir handed one worktree's artifacts to another.
- The first build in a fresh worktree is **cold, and it is the slowest thing you will do**: budget
  tens of minutes, not minutes. Measured on this box while other workers compile: 16+ minutes for a
  release build alone, and a fresh-clone full test build was still compiling at 55 minutes. There is
  no per-tree cache to reuse — `~/.cargo` (dependency *sources* and the registry cache) is shared, but
  `target/` is not. Start the build early in your card, and prefer `cargo build -p <crate>` for the
  inner loop over a workspace-wide build.
  Measured on this fleet (2026-10-15): a shared target dir handed one worktree's artifacts to another.
- **Builds are limited to two parallel jobs each** (`CARGO_BUILD_JOBS=2`). This machine is 7.6 GB with
  an 8-core CPU and up to five workers; unbounded rustc parallelism (`zbus`, `rustls`, `reqwest` at
  ~500 MB each) pushed the box into swap and made the dispatcher refuse to spawn anyone. Do not raise
  it, and do not run a build in the background while another one is running in your own card — one
  build at a time per worker.
- **Inner loop is targeted.** `cargo check -p <crate> --all-targets` is the fast feedback loop
  (measured ~1.2 s warm on this box). Then `scripts/test-changed.sh` (or `cargo test -p <crate>
  --bin luna` / `--test <name>`), and `cargo clippy -p <crate> --all-targets -- -D warnings`. A
  workspace-wide `cargo test` / `cargo clippy` is for changes that really span the workspace, not
  the default.
- **Release build:** `cargo build --release -p harness-cli --bin luna` → `target/release/luna`
  (per-tree; measured ~1.2 s warm). If your shell still exports `CARGO_TARGET_DIR` (an old worker
  session, for instance), the binary lands at `$CARGO_TARGET_DIR/release/luna` instead — a path other
  trees write to as well, so it is never proof of your branch: rebuild before testing it. Measured
  cold-build cost (card t_eb80814a): one uninterrupted release build of this workspace (~800
  dependency crates, bundled sqlite/audio/tls) was still compiling when `timeout 900` killed it at
  55 minutes, and the tree finished at 65 minutes of wall clock including two restarts — the first
  build in a fresh worktree is not a quick step.
- **Rebuild scope.** Cargo reuses fingerprints: editing `harness-cli` rebuilds only the CLI; editing
  `harness-core` rebuilds nearly every crate that depends on it; a new crate rebuilds itself plus its
  dependents. Changing `[profile.*]`, `RUSTFLAGS`, the linker, or the edition forces a full rebuild —
  do not touch those for a feature change.
- **Install reuses release artifacts.** `cargo install --path crates/harness-cli --force` and the
  same command with `--root ~/.local` share `target/release`; build once, then install. Do not run
  `cargo clean` before installing.
- **Keep disk headroom.** `target/` exceeds 30 GB. Keep at least 5 GB free (`df -h .`). When it is
  full, delete `target/debug/incremental` and `target/debug/examples` first — they are regenerable
  and much cheaper than a cold `cargo clean`. A full disk makes builds and installs fail or crawl.
- Credentials: the harness reads `~/.luna/credentials.json`. The NVIDIA NIM key comes from the
  Shiina CLI config, never from a file in this repo:

      python3 scripts/register-nvidia-key.py     # prints key=***<last4> only (see the note below)

  Never print, log, `echo`, commit or paste the key. A card that needs another provider extends
  that script — nobody hand-writes JSON. It is **not idempotent today**: it replaces
  `providers.<provider>` wholesale with a fresh entry (new id, `usage_count`/`last_used` reset) and
  therefore drops credentials added for that provider with `luna auth add`. Card t_968e1d04 makes it
  merge; until then, re-running it after `luna auth add` costs you those entries.
- Live model for testing: provider `nvidia`, model `deepseek-ai/deepseek-v4.1-flash`
  (endpoint `https://integrate.api.nvidia.com/v1`). Verified on this machine: `/models` returns 80
  ids including the target; a chat round-trip answers in 1–4s.
  Two things about this model that will bite you:
    - it is a **reasoning** model: the answer text is in `content` and the thinking is in a separate
      `reasoning_content` field (streaming: `delta.reasoning_content`). A small `max_tokens`
      (≤32) is swallowed entirely by reasoning tokens and yields `content: null` — budget at least
      1024, and a turn whose `content` is empty is not necessarily a failure.
    - NIM is rate-limited: retry once on 429/503 before calling it a bug.

## 3. Testing is real, not narrated

- Interactive flows (chat REPL, `luna auth` prompts, `luna voice`) run under the **realterm MCP**:
  `term_start`, `term_run`, `term_read`, `term_wait`, `term_write`, `term_list`, `term_kill`. It is
  a real PTY with a rendered screen, so it is the only way to see a prompt, spinner or full-screen
  TUI. Do not pipe stdin into these commands as a substitute — that is not what a user does.
- Non-interactive paths: plain shell, capture stdout+stderr, quote the exact command and its
  observed output in the card handoff.
- "Should work" is not verification. Verification is a command and its output.
- Anything that spends NIM quota (chat round-trips, model discovery) runs once per card, not in a
  loop. One real round-trip beats ten speculative ones.

### Targeted test runner

`scripts/test-changed.sh` is the default test command. It diffs the working tree against `HEAD`
(staged, unstaged, and untracked files) and runs only what those files affect:

| Changed path | Runs |
|---|---|
| `crates/<crate>/tests/<stem>.rs` | `cargo test -p <crate> --test <stem>` |
| `crates/<crate>/src/<module>.rs` | `cargo test -p <crate> --lib <module::path>` (`--bins` for `harness-cli`) |
| `crates/<crate>/src/lib.rs`, `src/main.rs` | `cargo test -p <crate>` |
| `crates/<crate>/src/bin/<name>.rs` | `cargo test -p <crate> --bin <name>` |
| `crates/<crate>/Cargo.toml`, `build.rs`, `tests/fixtures/` | `cargo test -p <crate>` |
| root `Cargo.toml` / `Cargo.lock` / `.cargo/` | `cargo test --workspace` |
| `docs/`, `scripts/`, `*.md` | nothing |

A source module also pulls in an integration test whose filename contains that module name as a
word (`tools/shell.rs` → `tests/fs_shell_tools_test.rs`). When any library crate changes, the runner
first does `cargo check -p harness-cli --bin luna`; the CLI depends on nearly every crate, so this is
the cheapest way to catch a dependent that no longer compiles.

Options: `--dry-run` prints the plan; `--full` (or `--workspace`) runs the old
`cargo test --workspace`; `--no-check` skips the compile gate; `--base <rev>` diffs against another
revision; `--keep-going` runs every command after a failure; and explicit file/directory arguments
plan from those paths instead of the diff.

Use the targeted runner while iterating. Run `scripts/test-changed.sh --full` before pushing when the
change spans crates or touches a public API; the targeted run intentionally does not compile every
other crate's test binaries.

## 4. Boundaries

- **`/home/janelle/Documents/GitHub/shiina-cli` is read-only reference.** Read, grep and run it to
  see what a feature does. Never write, format, move or commit in it.
- Work only inside the workspace the dispatcher gave you (your card's worktree). Never edit a
  shared checkout, never `git push`, never touch another card's branch.
- Commits you make: authored and committed as `Daryll <daryllvelonio2@gmail.com>`.
- Runtime state lives under `$HOME/.luna/` — never inside this repo. `target/` and `*.db` stay
  gitignored.
- No API key, token or credential ever lands in a tracked file, a test fixture, a log line or a
  card body.

## 5. Code bar

- No `unwrap()`/`expect()` on IO, network, parse or DB results in library crates: return
  `HarnessError`. `unwrap()` in tests, or on a `static` regex, is fine.
- No swallowed errors: `let _ = …` on a fallible call is a defect unless the failure really is
  inconsequential and a comment says why.
- No `println!` in library crates — `tracing` only. `println!` belongs in `harness-cli`.
- No magic numbers: name it or put it in config. Model ids, base URLs, timeouts, thresholds and
  paths are config, not literals.
- No provider/model special-casing in the agent loop — providers are data (`catalog.rs`).
- One error type: `HarnessError`. No `Box<dyn Error>` in public signatures.
- Cancellation must actually propagate (`CancellationToken`); every spawned task has a bounded
  lifetime. A loop with no exit condition needs a comment explaining why.
- Keep functions under ~100 lines and files under ~800; extract a sibling module rather than
  growing the one you are in.

## 6. Card etiquette

- Read your card body fully — including parent handoffs and comments — before doing anything.
- `luna-dev-1` / `luna-dev-2` implement. `luna-audit` audits (read-only on code, files findings),
  `luna-scout` specs features, `luna-qa` tests real user flows and picks up unowned supporting
  work. Never do another lane's job: if the right fix belongs to another lane, file a card for it.
- **File cards with a real workspace.** A card for code work needs `--workspace worktree --project
  luna-harness` (kanban tool: `workspace_kind="worktree"`, `project="luna-harness"`). `scratch`
  hands the worker a tmp dir that is deleted at completion, so nothing it writes can ever be
  merged — unrecoverable, not untidy. Read-only lanes (audit) may use `dir:<repo>`.
- **Before filing a card, list the board** (`shiina kanban --board luna-harness list`) and fold your
  findings into an existing card for the same work instead of filing a second one. Duplicates cost a
  worker a whole run. A `kanban_create` call can take 30–90s here; if it looks like it timed out,
  list the board before retrying rather than double-filing.
- **Route by lane, and keep each developer's queue at five open cards.** Specifications past that
  go into the docs (FEATURE_GAPS.md / TRENDS_2026.md) and get filed when a slot frees. A backlog of
  twenty specs filed before the binary compiles is a wish list, not a plan: each developer drains one
  card at a time. The cap is about *speculative specs* — an evidence-backed defect (audit finding with
  `file:line` and a repro) is filed when it is found, because that is what makes the queue honest.
- **Config is YAML:** `~/.luna/config.yaml`, loaded with `serde_yaml` (already a workspace
  dependency; `toml` is not). Any new tunable goes there with a documented default — not a literal,
  not a new env var.
- **Priority policy,** highest first: blockers (the binary does not build) → evidence-backed defects
  in shipped code → false README claims → prerequisites of already-queued work → new features. If a
  spec is a capability that does not exist yet, it ranks below anything that is currently wrong.
- Every finding needs `file:line`, the exact reproducing command, and observed-vs-expected.
- Before completing a card: `cargo test` for the crates you touched, plus the real command you ran
  to verify the feature. Put both in the handoff.
- If a card is wrong or under-specified, or blocked by a missing decision, say so in the handoff
  instead of guessing and block it with the `kind` you are actually waiting on.
