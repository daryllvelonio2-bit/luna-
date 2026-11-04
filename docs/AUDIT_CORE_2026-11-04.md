# Core audit — 2026-11-04

Scope: `crates/harness-core` (agent loop, session/compaction, checkpoint, tools) plus the
`harness-cli` code that wires the core config. Read-only audit: no source was changed.

Bar used: `agents.md` + `docs/AGENT_RULES.md`.

Baseline verification (commands run, output observed):

- `cargo clippy -p harness-core --all-targets -- -D warnings` → `Finished dev profile` with no warnings.
- `cargo test -p harness-core` → every suite `ok`; lib suite `120 passed; 0 failed`.
- `wc -l` over `crates/**.rs` for the size findings.

Findings are ordered by priority policy: defect in shipped code → false README claim → rule
deviation → cleanup.

---

## P1 — Retry guard is turn-scoped, so transient errors are not retried after the first tool call

`crates/harness-core/src/agent/turn.rs:260,282-284`

```rust
let tools_before_call = executed_tools.len();      // :260
...
Err(e) if e.is_retryable()
    && attempt < max_retries
    && executed_tools.is_empty() =>               // :284  <-- whole turn, not this call
```

`executed_tools` is the turn ledger (declared at :177, appended by every dispatch). Once round 1
has run a tool, it is non-empty for the rest of the turn, so a 429/503/timeout on the round-2
model call fails the turn instead of retrying — exactly the failure mode retries exist for.
`persist_provider_tools` and OpenCode's provider-executed tools have the same effect.

Expected: `executed_tools[tools_before_call..].is_empty()` — the check that `tools_before_call`
was introduced for. *Observed*: turn aborts on the first retryable error after any tool ran.

Repro, executed (a temporary mock test was added, run, and reverted — the tree is unchanged):

```text
mock: call 1 -> assistant_with_tools([failing_tool]); calls 2,3 -> ProviderError("Model is overloaded (HTTP 503)"); call 4 -> assistant
$ cargo test -p harness-core --test agent_guardrail_test test_retry_after_a_tool_round_succeeds -- --nocapture
AUDIT-REPRO result=Err(ProviderError { provider: "tool-then-overload", message: "Model is overloaded (HTTP 503)" }) provider_calls=2
test result: FAILED. 0 passed; 1 failed
```

Expected (what the same mock does on a no-tool turn, `test_agent_retries_transient_overloaded_error_and_succeeds`,
tests/agent_guardrail_test.rs:253): three round-2 attempts, then success, `provider_calls=4`.
Observed: the turn aborts on the first retryable error after the tool round, `provider_calls=2`.
The existing retry test only covers a no-tool turn, which is why this is untested.

## P2 — The documented `auto_compact_*` config keys are dead; `/auto-compact` does not survive restart

The settings exist twice:

- top level: `crates/harness-core/src/config.rs:81-92` (declared), `:149-152` (defaults),
  `:163-183` (validated);
- under `agent`: `crates/harness-core/src/agent.rs:63-77`, `:139-142`.

The agent reads only the `agent.*` copy (`AgentConfig::fold_policy`, agent.rs:462-480;
`auto_compact_settings`, :447-455), and the CLI builds the agent from `config.agent.clone()`
(`crates/harness-cli/src/commands/chat.rs:486`). Nothing reads the top-level fields:

```text
$ grep -rn "\.auto_compact_" crates --include='*.rs' | grep -v "agent.rs" | grep -v tests
crates/harness-core/src/session/mod.rs:9://! threshold — `agent.auto_compact_threshold` when auto-compaction is on, else
```

Consequences, all user-visible:

1. `~/.luna/config.yaml` with `auto_compact_enabled: false` (the key README §3 documents) has no
   effect; the agent stays at `agent.auto_compact_enabled: true`.
2. `/auto-compact off` and the setup box persist the *top-level* key
   (`commands/chat/commands/mod.rs:2090,2110,2206-2214`) and change the in-memory `AgentConfig`.
   It works until the next start, then the saved value is ignored.
3. Defaults already disagree: top level is `Some(50)` (config.rs:151), `AgentConfig` is `None`
   (agent.rs:141), so `/compact` without `/auto-compact` folds by turns, not the 50% README
   promises.
4. Validation rejects a top-level value the agent never reads (config.rs:163-170), so a dead key
   can still refuse to start.
5. First-run onboarding writes the same dead keys (`crates/harness-onboarding/src/store.rs:37-43`,
   `:140-170`), so a fresh install's compaction answers are discarded at the next start too.

Fix: one source of truth — drop the top-level fields and write `agent.auto_compact_*`, or merge
the top-level fields into `AgentConfig` at load (with a documented precedence rule).

## P3 — README claims a 120 s subagent idle limit; the code uses 180 s

`crates/harness-core/src/tools/delegate.rs:32` — `SUBAGENT_IDLE_TIMEOUT_SECS: u64 = 180;`
`README.md:335` — “A child fails only after **120 seconds with no model or tool activity**”.

Pick one and make the other follow. The 5-minute `wait` window (delegate.rs:36) matches.

## P4 — Presentation literals in the core crate

- `crates/harness-core/src/agent/turn.rs:510`:
  `"(The model completed reasoning but produced no text output. Press Ctrl+O to inspect reasoning.)"`
  This is both returned as the turn's answer and appended to the transcript (:519-521), so a UI
  hint becomes conversation history sent to the provider on every later turn. `Ctrl+O` is a
  `harness-cli` binding the core cannot know about.
- `crates/harness-core/src/tools/delegate.rs:1008,1044,1121,1278` and `:735-742` build
  `**Status:** Failed`, `### Error:` and `──────────────────` blocks — UI text in a library crate.

Both violate `agents.md` §1 (“No hardcoded UI literals”). Move the placeholder to the CLI; have
the core return a structured `TurnOutcome` reason instead.

## P5 — Magic numbers that `agents.md` §1 forbids

| location | value | what it controls |
|---|---|---|
| `agent/turn.rs:259` | `max_retries = 3` | provider retry budget |
| `tools/shell.rs:179` | `1500 ms` | external command rewriter timeout |
| `tools/shell.rs:300` | `500 ms` | `sudo -n` probe timeout |
| `tools/delegate.rs:412` | `10 ms` floor | idle-check polling floor |

Each needs a named constant with a why, or a `Config` field. `delegate.rs:32,36,39` already show
the pattern to follow.

## P6 — Hardcoded state path outside `config.paths`

`crates/harness-core/src/session/mod.rs:187-190`

```rust
pub fn default_sessions_db() -> PathBuf {
    crate::config::home_dir().join(".luna").join("sessions.db")
}
```

This is a second definition of `PathsSettings::sessions_db` (config.rs:639-655). `AGENT_RULES.md`
§5: “No hardcoded `$HOME/.luna` strings; resolve all runtime paths from `config.paths`.” Move the
default onto `PathsSettings` (or take it from the caller) so a relocated `paths.sessions_db`
cannot be silently bypassed by `SqliteSessionStore::new(None)`.

## P7 — A poisoned mutex panics a library API

`crates/harness-core/src/agent.rs:505-510`

```rust
self.steer_queue.lock().unwrap().push_back(trimmed.to_string());
```

Every other lock in the crate recovers (`unwrap_or_else(|e| e.into_inner())`, e.g. :471,
checkpoint.rs:366). One panicking task poisons this mutex and every later `steer()` panics the
whole process — including the TUI thread. Align it with the rest.

## P8 — Swallowed errors without a justifying comment

`AGENT_RULES.md` §5: `let _ = …` on a fallible call is a defect unless the failure is
inconsequential and a comment says why.

- `tools/delegate.rs:81,93,104` — `create_dir_all`, `writeln!`, `write!` for the subagent log are
  discarded. The TUI shows the child as fine while its log silently stops recording; at minimum
  log a `warn!` once.
- `traits.rs:112-115` — `remove_note_pointer` discards both `forget` failures and returns `Ok(())`.
  A dangling memory pointer is exactly what the method exists to prevent.
- `agent/fold.rs:317,337` — `self.system_prompt("").await.unwrap_or_default()` turns a
  memory-engine failure into an empty system prompt; the token estimate built from it understates
  the request, so a fold that should fire is skipped this turn.

## P9 — Checkpoint review is O(files × recorded) and blocks the async thread

- `crates/harness-core/src/checkpoint.rs:362,935` — `TreeScan::of` uses blocking `std::fs` walks
  from `finish()`, which runs on the async turn path (including the `Drop` guard), twice per turn
  with external tracking on.
- `checkpoint.rs:1001-1006` — `changes_against` tests every after-file against
  `recorded.iter().any(...)`. With the 20k-file cap and a 50-file turn that is ~1M string compares,
  on the runtime worker.

Fix: `HashSet<&str>` for `recorded`, and put the scan behind `spawn_blocking` (or cache the
baseline walk between turns and only re-stamp changed directories).

## P10 — Dead duplicate of the `/auto-compact` handler

`crates/harness-cli/src/commands/chat/commands/context.rs` (344 lines) is a byte-identical copy of
the handler at `commands/mod.rs:2075-2224` (verified with `diff`), but no `mod context;` declares
it, so it has never been compiled since the move. It is a second copy that can only drift (it
already duplicates the top-level key bug in P2). Delete it.

## P11 — File-size limits

`agents.md` §2 sets a **hard 2,000-line** cap; `docs/AGENT_RULES.md` §5 asks for **~800**.

Over the hard cap:

- `crates/harness-cli/src/commands/chat/commands/mod.rs` — 2,708
- `crates/harness-cli/src/commands/chat.rs` — 2,427
- `crates/harness-providers/src/opencode.rs` — 2,404

Over the ~800 guidance inside core: `config.rs` 1,609 · `tools/delegate.rs` 1,418 ·
`tools/fs.rs` 1,351 · `checkpoint.rs` 1,073 · `agent/turn.rs` 1,036 · `session/sqlite.rs` 1,004 ·
`tools/web/policy.rs` 892 · `tools/web/search.rs` 740 · `tools/todo.rs` 774.

`config.rs` is the highest-value split: `PathsSettings`, `HooksSettings`, `CheckpointSettings`,
`ToolsSettings` and the flat-schema/`set_in_file` code are independent units already.

---

## Suggested order of work

1. P1 (retry) — one-line condition plus the missing test; highest user impact.
2. P2 (dead auto-compact keys) — delete or merge; it is a broken documented feature.
3. P3 (README 120/180) — doc fix.
4. P7 (panic) and P8 (swallowed errors) — small, local.
5. P4/P5/P6 — rule compliance, mechanical.
6. P9, P10, P11 — performance, dead code, and the size debt.
