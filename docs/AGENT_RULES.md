# Luna Rust Harness — working rules for agents in this repo

Read this before touching anything. It overrides habit.

## 1. What this is

`luna` is a Rust autonomous-agent harness: a 10-crate workspace under `crates/`. It is a real
binary, not a mock — `cargo run --bin luna -- --help`. It talks to live model providers, keeps
SQLite memory and notes, speaks MCP, runs sandboxed wasm plugins, and can patch its own source.

Board: `luna-harness` (`shiina kanban --board luna-harness …`). Owner: Daryll. Every card is
assigned by the dispatcher; you never pick your own lane.

## 2. Environment (already set up — do not re-invent)

- Toolchain: `cargo` 1.98 / `rustc` 1.98 on PATH. No rustup needed.
- **All five worker profiles share one target dir** (`CARGO_TARGET_DIR=/home/janelle/.luna/cargo-target`),
  so a build that prints `Blocking waiting for file lock on build directory` is waiting on another
  worker, not hung — let it wait, and do not override `CARGO_TARGET_DIR` per card. Release
  dependencies are warm in that dir; the **first debug/test build after a checkout is cold and can
  take ~10 minutes** (bundled sqlite + audio + tls). Prefer `cargo build -p <crate>` for the inner
  loop over `cargo build --workspace`, and run the workspace-wide test only at the end of a card.
- **Builds are limited to two parallel jobs each** (`CARGO_BUILD_JOBS=2`). This machine is 7.6 GB with
  an 8-core CPU and up to five workers; unbounded rustc parallelism (`zbus`, `rustls`, `reqwest` at
  ~500 MB each) pushed the box into swap and made the dispatcher refuse to spawn anyone. Do not raise
  it, and do not run a build in the background while another one is running in your own card — one
  build at a time per worker.
- Build: `cargo build --release` from the repo root → binary at `target/release/luna`.
- Inner loop: `cargo build -p harness-providers` (or whichever crate the card touches).
- Tests: `cargo test --workspace`. Lint: `cargo clippy --workspace --all-targets -- -D warnings`.
- Credentials: the harness reads `~/.luna/credentials.json`. The NVIDIA NIM key comes from the
  Shiina CLI config, never from a file in this repo:

      python3 scripts/register-nvidia-key.py     # idempotent, prints key=***<last4> only

  Never print, log, `echo`, commit or paste the key. A card that needs another provider extends
  that script — nobody hand-writes JSON.
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
