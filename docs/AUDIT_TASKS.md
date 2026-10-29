# Audit Task List — `unimplemented!()` / `todo!()` / `.unwrap()` / `.expect(` sweep

Scope: `crates/*/src/**` + `crates/*/tests/**` (workspace crates: harness-core, harness-providers,
harness-cli, harness-onboarding, harness-memory, harness-notes, harness-plugins, harness-mcp,
harness-voice, harness-telemetry).

Baseline verified before this list was written:
- `cargo build --workspace` → clean.
- `cargo test --workspace` → **631 passed, 2 ignored (46 suites)**.

Sweep totals (all crates):

| Pattern | Occurrences | Where |
|---|---|---|
| `unimplemented!()` | 36 | 9 files, all in `crates/harness-core/tests/`, all inside `NotesEngine` stub impls |
| `todo!()` | 0 | — |
| `.unwrap()` | 1,039 | ~1,200 of the unwrap/expect hits sit in test code (test files + `#[cfg(test)]` modules) |
| `.expect(` | 372 | production residue ≈ 145 unwraps + ~10 expects (mostly `Mutex::lock().unwrap()`) |

No `TODO`/`FIXME` comments exist in any `src/` — the earlier "TODO-style hits" were `todo::` module
paths, not markers.

---

## T1 — CRITICAL: `delegate_task` bypasses the approval gate and `ask` permission rules

**Files**
- `crates/harness-core/src/tools/delegate.rs:250` — `child_cfg.require_hitl_for_mutation = false;`
- `crates/harness-core/src/tools/delegate.rs:261` — `.with_approval(Arc::new(AllowAll), ApprovalSettings::default())`
- `crates/harness-core/src/tools/delegate.rs:288-339` — `impl Tool for DelegateTaskTool` never overrides `is_mutating()`
- `crates/harness-core/src/traits.rs:162-164` — default `is_mutating()` is `false`
- `crates/harness-core/src/agent.rs:828-851` — `needs_approval()` returns false when the flag is off
- `crates/harness-core/src/agent.rs:869-894` — `RuleVerdict::Ask` is answered by the child's approver
- `crates/harness-cli/src/commands/chat.rs:289-296` — the CLI registers the tool

**What is wrong**
`delegate_task` does not override `is_mutating()`, so the parent agent treats it as read-only and never
prompts. The child agent it spawns is built with `require_hitl_for_mutation = false` and the `AllowAll`
approver, so the child's `write_file` / `patch` / `terminal` run with no human in the loop. The child's
permission engine is still built from `config.tools.permissions` (`delegate.rs:258`), but an `ask` verdict
is answered by `AllowAll` — so **`ask` rules are bypassed too; only `deny` rules hold**.

**Reproduction (already run)**
Parent configured with `approval = ask`, `require_hitl_for_mutation = true`, and an explicit rule
`{action: ask, tool: write_file}`; a subagent was told to write a file. The file was created, no prompt
was raised, the `ask` rule was ignored.

**Fix direction**
- Override `is_mutating()` → `true` on `DelegateTaskTool` (or use `is_mutating_call()` so only `spawn`
  is gated, `list`/`poll`/`wait` stay read-only).
- Do not hand the child `AllowAll`. Either give the child the parent's approver and the parent's
  `require_hitl_for_mutation`, or add an explicit "subagents may not mutate" policy flag that the
  child config respects.
- Decide explicitly whether a subagent may mutate at all; if yes, the *parent* call must be gated and
  the child's calls must still pass the same permission rules with a human answer.

**Done when**
- [x] A regression test exists: parent `approval = ask` + rule `ask: write_file` + `delegate_task`
      → the subagent's write is either blocked or approved by a recording approver.
- [x] `cargo test -p harness-core` green.

---

## T2 — HIGH: headless `Ask` silently becomes auto-approve

**Files**
- `crates/harness-cli/src/commands/approval.rs:253-260` — `ApprovalMode::Ask` + non-TTY → `Arc::new(AllowAll)`
  (comment in the file: "Headless / non-terminal: YOLO mode, auto-approve mutating tools")
- `crates/harness-cli/src/commands/approval.rs:270-285` — a test currently *asserts* the YOLO behaviour
- `crates/harness-core/src/approval.rs:14-15, 86-103` — documented policy is "fail closed everywhere";
  `NonInteractive` exists but is never used by the CLI
- `crates/harness-cli/src/commands/chat.rs:346-355` — unattended chat falls back to `build_approver`
- `crates/harness-cli/src/commands/voice.rs:141, 188` — voice registers the **full core tool set**
  (`terminal`, `write_file`, `patch`) and uses `build_approver`
- Contrast (already correct): `crates/harness-cli/src/commands/exec.rs:383-396` and
  `crates/harness-cli/src/commands/gateway/mod.rs:48-54` have their own fail-closed `headless_approver`

**What is wrong**
`luna chat` driven from a pipe/script/CI (non-TTY) with the default `tools.approval = ask` auto-approves
every mutating tool. `luna voice` is *always* non-TTY, so the hands-free loop can run shell commands and
write files with zero approval — audio in the room (or a crafted audio file) becomes an unauthenticated
mutation channel.

**Fix direction**
- Use `NonInteractive` (fail closed) for an unattended `Ask`, or require an explicit `--yes` /
  `tools.approval = allow`.
- If voice must stay hands-free, make its mutating capability explicit and opt-in (e.g. voice runs with
  the terminal/write tools disabled unless the operator enables them).
- Align `exec`/`gateway`/`chat`/`voice` on one helper instead of two.

**Done when**
- [ ] `build_approver` no longer returns `AllowAll` for an unattended `Ask` (or the voice path stops
      using it) — and the test at `approval.rs:274-285` is rewritten to the new contract.
- [ ] A test asserts that a non-TTY chat/voice run cannot mutate without `--yes`.

---

## T3 — HIGH: the sudo password leaks into the model's tool output

**Files**
- `crates/harness-core/src/tools/shell.rs:486` — `let stdin_data = (pwd + "\n").repeat(sudo_count);`
- `crates/harness-core/src/tools/shell.rs:519-523, 555-560` — that data is written to the **shell's** stdin
- `crates/harness-core/src/tools/shell.rs:474` — the password is cached for later calls
- `crates/harness-core/src/tools/shell.rs:48` — `SUDO_PASSWORD` is read from the environment
- `crates/harness-core/src/tools/shell.rs:505-527` — the child inherits the full parent env (no `env_clear`)
- `crates/harness-core/src/tools/shell.rs:388-395` — the approval preview shows the pre-sudo-rewrite command

**What is wrong**
The password is piped into the shared stdin of an arbitrary shell command. Any part of the command that
reads stdin before `sudo` consumes it prints the password, and the tool output goes straight back to the
model and into the transcript. Because the password is cached, a later innocuous-looking command leaks it
again. Separately, `SUDO_PASSWORD` is inherited by every child process, so `echo $SUDO_PASSWORD` prints it.

**Reproduction (already run)**
With a fake prompter returning `S3CRET-SUDO-PASSWORD`:
- `cat; sudo true` → output contains `S3CRET-SUDO-PASSWORD`
- `head -c 40; sudo -v` (cached password, no prompt) → same leak

**Fix direction**
- Feed the password to `sudo` on a dedicated fd/pipe, never on the shared stdin of the user's command
  (e.g. run `sudo -S` with its own stdin, or `exec 3<&0` style plumbing with the user's stdin kept at
  `/dev/null`).
- Do not cache the password across calls (or scope the cache to a single command).
- Strip `SUDO_PASSWORD` from the child environment.
- Make the approval preview show the sudo-rewritten command, so what runs is what was approved.

**Done when**
- [ ] A test drives `terminal` with a fake `SudoPrompter` and a command that reads stdin
      (`cat; sudo true`) and asserts the password does **not** appear in the tool output.
- [ ] The child environment no longer contains `SUDO_PASSWORD`.

---

## T4 — MEDIUM: production panic surface — ~145 `Mutex/RwLock::lock().unwrap()`

**Hot files**
- `crates/harness-core/src/tasks.rs` — 12 (`:103,108,119,128,132,161,166,173,185,193,215,219`)
- `crates/harness-core/src/agent.rs` — 8 (`:318,526,929,1095,1118,1433,1820,1895`)
- `crates/harness-core/src/checkpoint.rs` — 12 (`:266,279,298,309-311,328,333,334,409,426,433`)
- `crates/harness-core/src/tools/shell.rs` — 3 (`:467,474,588`, sudo cache)
- `crates/harness-core/src/tools/delegate.rs` — 3 (`:197,202` `try_write().unwrap()`, `:771` steer lock)
- `crates/harness-cli/src/commands/chat/ui/mod.rs` — 48
- `crates/harness-cli/src/commands/chat.rs` — 40
- `crates/harness-cli/src/commands/chat/commands/picker.rs` — 8
- `crates/harness-cli/src/commands/chat/turn.rs` — 5

**What is wrong**
A poisoned mutex (any earlier panic while holding the lock) turns into a panic at the next `lock().unwrap()`,
which kills the turn or the TUI instead of degrading. `delegate.rs:197` and `:202` are worse:
`try_write()` fails whenever the lock is *held*, not only when poisoned, so `with_notifier()` /
`with_steer_handle()` can panic under contention.

**Fix direction**
- Replace `lock().unwrap()` with `lock().unwrap_or_else(|e| e.into_inner())` (keep the data) or
  `match lock() { Ok(g) => …, Err(e) => … }` at the few places where losing the lock must be an error.
- `delegate.rs:197,202`: use `if let Ok(mut w) = self.notifier.try_write()` (as `set_notifier` already
  does at `:185-188`) or `blocking_write()` in an async context.

**Done when**
- [ ] Zero `lock().unwrap()` left in the crates listed above, or an explicit allow-list with a comment
      saying why the panic is acceptable.
- [ ] `cargo test --workspace` green.

---

## T5 — LOW: `process` tool does not declare itself mutating

**Files**
- `crates/harness-core/src/tools/process.rs` — no `is_mutating()` override, although the tool has a `kill`
  action that terminates a background job.

**What is wrong**
`is_mutating()` defaults to `false`, so `process(action="kill")` never reaches the human gate. It can only
kill jobs this session started, so the blast radius is small — but the tool *does* change machine state and
the trait's own rule says a mutating tool must say so.

**Fix direction**
- Override `is_mutating()` → `true` (or `is_mutating_call()` → `true` only for the `kill` action).

**Done when**
- [ ] A test asserts `process` is mutating for `action = "kill"`.

---

## T6 — LOW: 36 `unimplemented!()` stubs in the test suite

**Files** (all in `crates/harness-core/tests/`)
`permission_rules_test.rs:397,406,415,418` · `usage_ledger_test.rs:420,429,438,441` ·
`agent_guardrail_test.rs:118,127,136,139` · `approval_gate_test.rs:202,211,220,223` ·
`hooks_agent_test.rs:141,150,159,162` · `background_saver_test.rs:75,84,93,96` ·
`hooks_test.rs:438,447,456,459` · `checkpoint_test.rs:347,356,365,368` ·
`fs_shell_tools_test.rs:1388,1408,1417,1420,1542,1562,1571,1574`

**What is wrong**
Every one of them is a `NotesEngine` stub (`save_note`, `read_note`, `update_note`, `delete_note`) that the
test never calls. If a test path ever starts calling one, it panics with an opaque message instead of
failing cleanly.

**Fix direction**
- Replace with `unreachable!("not used by this test")` or a real stub returning `Ok(...)`.

**Done when**
- [ ] `unimplemented!()` count in `crates/` is 0.

---

## T7 — INFO: clean results from the same sweep (do not "fix" these)

Verified by live probe, not by reading alone:

- **Filesystem sandbox holds.** Absolute paths, `..` traversal, the `crates/` fallback path, `patch` and
  `search_files` all refuse to escape `paths.workspace_root`
  (`crates/harness-core/src/tools/sandbox.rs:61-108` canonicalizes the deepest existing ancestor, so
  symlink escapes are closed too).
- **Background jobs inherit the network boundary.** `crates/harness-core/src/tasks.rs:107` applies the same
  seccomp policy to queued jobs, so `&` is not an escape hatch for `tools.network = deny`.
- **`rtk` rewrite is skipped under `network = deny`** (`crates/harness-core/src/tools/shell.rs:111-117`), so
  the rewriting daemon cannot run outside the filter.
- `todo_list` is intentionally ungated (`crates/harness-core/tests/fs_shell_tools_test.rs:1755-1759`) even
  though it writes files — that is a deliberate decision, not an oversight.

---

## Verification commands

```bash
cd ~/Downloads/rust-agent-harness
cargo build --workspace
cargo test --workspace                     # baseline: 631 passed, 2 ignored
cargo test -p harness-core                 # for T1, T3, T4, T5
cargo test -p harness-cli                  # for T2
```

Re-run the sweep after each task:

```bash
# via the harness's own search tools (native search_files), not shell grep
search_files(path="crates", file_glob="*.rs", pattern="unimplemented!()")
search_files(path="crates", file_glob="*.rs", pattern="todo!()")
search_files(path="crates", file_glob="*.rs", pattern=".unwrap()")
search_files(path="crates", file_glob="*.rs", pattern=".expect(")
```

---

## Order of work

1. **T1** — approval bypass (critical, isolated change in `delegate.rs`).
2. **T2** — headless auto-approve (touches `approval.rs`, `chat.rs`, `voice.rs`).
3. **T3** — sudo credential leak (touches `shell.rs`; verify against the fake-prompter test).
4. **T4** — mutex unwrap cleanup (mechanical, best done after T1–T3 so the diffs stay separable).
5. **T5**, **T6** — small cleanups.
