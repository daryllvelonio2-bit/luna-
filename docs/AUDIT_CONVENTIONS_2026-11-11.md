# Conventions audit — 2026-11-11

Scope: the whole workspace (`crates/`), audited against `agents.md` and `docs/AGENT_RULES.md`.
This audit changed source; it is not read-only like `AUDIT_CORE_2026-11-04.md`.

Bar used, in the order the rules set it:

1. `agents.md` — hard 2,000-line file cap, no UI literals or `println!` in library crates, no magic
   numbers, fail closed and propagate errors, no provider special-casing in `harness-core`.
2. `docs/AGENT_RULES.md` — `unwrap`/`expect` only in tests or on static regexes, `tracing` in
   libraries, ~100-line functions / ~800-line files, state paths through `Config`.

Verification (commands run, current tree):

- `cargo fmt --all -- --check` → clean.
- `CARGO_BUILD_JOBS=2 cargo clippy --workspace --all-targets -- -D warnings` → `Finished`, no warnings.
- `scripts/test-changed.sh` → all 20 commands passed, including the four `harness-core` integration
  suites, memory store/engine, providers auth/client/opencode/discovery/antigravity, and voice.

---

## Fixed in this audit

### Fail-closed: errors that were dropped

| Area | Change |
| --- | --- |
| `harness-memory/src/store/hooks.rs` | Hook replies go through `json_reply`; session-end journal entry, save-marker removal, and precompact reindex failures are logged instead of discarded. |
| `harness-memory/src/store/daemon.rs` | Worker thread spawn returns `Result` and is awaited by `run_servers_on`; queue `recover_running`/`prune_terminal`/`claim_next`/`finish`/`defer` failures are logged; endpoint JSON, token permissions, socket creation, and socket permissions fail closed; connection handler errors are logged. |
| `harness-memory/src/store/mod.rs` | `write_manifest` returns `Result` (create dir, serialize, write); graph/KG index rebuilds are a single `refresh_graph_index` helper that logs derived-index failures; record deletion and stale-file removal report failures. |
| `harness-memory/src/store/journal.rs` | Malformed dates/times fall back to "today"/midnight instead of a fixed 2026-01-01 and an `unwrap`. |
| `harness-memory/src/store/kg.rs` | Removed `confidence.unwrap()` behind an `is_none` check; `is_none_or` expresses the guard. |
| `harness-memory/src/engine.rs` | Category normalization, duplicate retirement, and every `DELETE` in the forget paths now propagate SQLite errors; embeddings rows are no longer silently left behind. |
| `harness-notes/src/file_engine.rs` | Cache locks are poisoning-safe; stale part removal is logged; archiving a part and committing a note propagate errors instead of dropping them. |
| `harness-core/src/checkpoint.rs` | Checkpoint file-list lock is poisoning-safe. |
| `harness-core/src/tools/delegate.rs` | Delegated child-session persistence failure warns instead of vanishing. |
| `harness-providers/src/auth.rs` | Credential-store directory, lock-directory, and `0600` permission failures are errors, not ignored; temp-file cleanup stays best effort with the reason stated. |
| `harness-providers/src/antigravity/mod.rs` | Transport retry returns `Result<_, String>` with no `expect`; per-endpoint attempts and pauses are named constants. |
| `harness-providers/src/kiro.rs`, `discovery.rs` | Kiro/Supercode credential sync and token updates warn on failure instead of swallowing it. |
| `harness-onboarding/src/memory_step.rs`, `web_step.rs` | A missing default option returns an error instead of `expect`. |

### Magic numbers named

- `harness-core/src/agent.rs`: retry-jitter per-second, floor, and ceiling.
- `harness-core/src/tools/delegate.rs`: explicit-wait poll interval.
- `harness-providers/src/client.rs`: connect timeout, keepalive, pool idle.
- `harness-providers/src/opencode.rs`: connect timeout, probe timeout, start grace.
- `harness-providers/src/antigravity/credentials.rs`: e-mail probe timeout.
- `harness-desktop/src/deadline.rs`: no-deadline horizon.
- `harness-voice/src/pipeline.rs`: wake-word error backoff.
- `harness-memory/src/store/daemon.rs`: worker error/idle polls, shutdown poll, token retention already named.

### State paths through `PathsSettings`

`profiles_root()`, onboarding `default_notes_dir()`, memory engine/store defaults, and the provider
model-cache default no longer spell `home_dir().join(".luna")…` themselves. The canonical relative
constants in `config.rs` (`CONFIG_FILE_RELATIVE`) and `hooks.rs` (`PROJECT_HOOKS_RELATIVE`,
`USER_HOOKS_RELATIVE`) are intentionally kept: they are the one definition, not a duplicate.

### Concurrent work

The tree also contains uncommitted setup-command and onboarding work that this audit did not author
(`harness-cli/src/main.rs`, `harness-onboarding/src/{flow,lib,store}.rs`, `tests/flow.rs`, and the new
`crates/harness-cli/tests/setup_test.rs`). That last file had an unused `PathBuf` import that failed
`-D warnings`; the import alone was removed so the workspace gate could run. Nothing was committed
or reverted.

---

## Remaining structural debt (not changed here)

### Files over the hard 2,000-line cap

| Lines | File |
| ---: | --- |
| 4270 | `crates/harness-memory/src/saver.rs` |
| 3509 | `crates/harness-cli/src/commands/chat/commands/mod.rs` |
| 2989 | `crates/harness-cli/src/commands/chat.rs` |
| 2851 | `crates/harness-providers/src/opencode.rs` |
| 2796 | `crates/harness-cli/src/commands/chat/render/mod.rs` |
| 2343 | `crates/harness-cli/src/commands/chat/ui/mod.rs` |

Two over-cap files are test suites (`harness-core/tests/session_test.rs` 2725,
`harness-core/tests/fs_shell_tools_test.rs` 2121); the rule is written for code files, but the same
split applies if they keep growing.

These splits are large mechanical refactors with real regression risk. They need their own change
per file, with the matching test command, not a tail-end edit in a broad audit.

### Functions over ~100 lines (production, excluding test modules and static tables)

| Lines | Location |
| ---: | --- |
| 498 | `harness-memory/src/store/mcp.rs:26` `all_mcp_tools` (static tool table) |
| 464 | `harness-notes/src/docs.rs:318` `render_system` |
| 297 | `harness-memory/src/saver.rs:2451` `json_object` |
| 146 | `harness-note-graph/src/extract.rs:56` `extract_note` |
| 138 | `harness-core/src/agent/prompt.rs:29` `system_prompt` |
| 132 | `harness-providers/src/opencode.rs:2417` `fetch_opencode_models` |
| 130 | `harness-core/src/tasks.rs:109` `spawn` |
| 122 | `harness-memory/src/store/daemon.rs:998` `run_servers_on` |
| 121 | `harness-memory/src/engine.rs:461` `retrieve` |
| 117 | `harness-core/src/config.rs:1030` `resolve` |
| 112 | `harness-core/src/tools/netguard.rs:139` `install_no_network` |
| 112 | `harness-notes/src/docs.rs:1044` `scan_source` |
| 112 | `harness-providers/src/auth.rs:654` `acquire_credential` |
| 111 | `harness-notes/src/file_engine.rs:1107` `parse_note_file` |
| 108 | `harness-providers/src/catalog.rs:741` `get_context_window` |
| 107 | `harness-note-graph/src/export.rs:431` `svg_layout` |
| 104 | `harness-providers/src/client.rs:76` `delta_events` |

The 564-line `catalog.rs::get_provider_catalog` is a data literal, not control flow; splitting it
buys nothing unless the catalog is moved to a data file.

### Remaining `let _ =` (94 production occurrences)

The audit only changed ones that could lose data. The rest are deliberate and should stay:

- progress-channel sends (`tx.send(...)`) where a closed receiver means the UI is gone;
- best-effort cleanup after a real error is already being returned (temp files, sockets);
- `Drop` impls and shutdown paths;
- probes whose failure is the answer (for example the sudo/RTK probes in `tools/shell.rs`).

### Remaining `unwrap`/`expect`

All remaining hits are test modules or `OnceLock<Regex>` initializers, which `AGENT_RULES.md`
explicitly allows. No production I/O, network, parse, or DB `unwrap` remains in library crates.

### Presentation in library crates

No production `println!`/`eprintln!` outside `harness-cli` (examples only). No provider-id
special-casing in `harness-core` and no UI literals in library crates. CLI raw ANSI sequences in
`chat/keys.rs` are keyboard-protocol control codes, not presentation.

---

## Suggested follow-ups, in order

1. Split `saver.rs` (largest file, most `unwrap`s removed from it already): facade plus
   operations/validation/transcript submodules.
2. Split `chat/commands/mod.rs` and `chat.rs` by command family; the command table is the natural
   facade.
3. Split `opencode.rs` into service lifecycle, protocol, and model catalog.
4. Split `render/mod.rs` and `ui/mod.rs` by widget group.
5. Extract `docs.rs::render_system` and `saver.rs::json_object` into focused helpers first — they are
   the highest-line functions with runtime logic.
