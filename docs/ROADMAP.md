# Roadmap: dependency order, file overlap, and the critical path over the open board

Snapshot — board `luna-harness`, read directly from
`~/.shiina/kanban/boards/luna-harness/kanban.db` at **2026-10-15 07:46 PST**; this repo at
**639c38e** (`main`). 24 open cards (14 ready, 5 running, 4 todo, 1 in review).

The board moves while this document is read: the audit lane filed `t_26fb21a1` and `t_8935ed8a`
landed (`e5ae82c`, merged `639c38e`) during this pass, and `t_d79b9293` was dispatched at 07:43.
Section 1 is a snapshot; sections 2–4 are structural and survive status drift. Everything here is
derived from the card bodies, the comment threads (`task_comments`) and the link table
(`task_links`) — not from `docs/FEATURE_GAPS.md`, which this pass also repaired (§5).

The `shiina kanban` CLI refuses to run inside a worker context
(`kanban: delegate_task child contexts cannot mutate Kanban tasks or boards`), so the read is raw
SQLite, read-only:

    sqlite3 ~/.shiina/kanban/boards/luna-harness/kanban.db \
      "select id,title,status,assignee,priority,workspace_kind,workspace_path,project_id \
       from tasks where status not in ('archived','done') order by priority desc;"
    sqlite3 … "select parent_id,child_id from task_links;"

---

## 0. Blockers found while building this map (read these first)

Three of these are board-shaped, not code-shaped, and none of them is a card from this pass.

### B1 — the shared `CARGO_TARGET_DIR` serves one worktree's artifacts to another; the victim sees a phantom compile error

Measured, not inferred. In the main checkout at `639c38e`:

    $ CARGO_TARGET_DIR=/home/janelle/.luna/cargo-target cargo check -p harness-cli --message-format short
    crates/harness-cli/src/commands/auth.rs:106:17: error[E0277]: `(std::string::String, CredentialStorage)`
      doesn't implement `std::fmt::Display`
    error: could not compile `harness-cli` (bin "luna") due to 1 previous error        # exit 101

`CredentialStorage` **does not exist anywhere in `main`** — `grep -rln CredentialStorage .` finds it
only under `.worktrees/t_2ccd2241/` (the credential-safety branch), and main's
`harness-providers/src/auth.rs:93` returns `Result<String, HarnessError>`. Verbose cargo shows the
foreign artifact:

    $ cargo check -p harness-cli -v | grep -o -- '--extern harness_providers=[^ ]*'
    --extern harness_providers=/home/janelle/.luna/cargo-target/debug/deps/libharness_providers-7926bf2bbcb457da.rmeta

Both trees write the **same** artifact filename, because cargo's `.d` dep-info is emitted relative to
each tree's own root (`crates/harness-providers/src/auth.rs`, not an absolute path), so the two
trees are indistinguishable in one target dir. Forcing main to rebuild the crate fixes it, and the
same filename is reused with correct content:

    $ touch crates/harness-providers/src/auth.rs && cargo check -p harness-cli
        Checking harness-providers v0.1.0 (/home/janelle/Downloads/rust-agent-harness/crates/harness-providers)
        Checking harness-cli v0.1.0 (…)
        Finished `dev` profile … in 13.56s                                   # exit 0
    $ ls -la …/libharness_providers-7926bf2bbcb457da.rmeta
    517619  Okt 15 07:00   →   384126  Okt 15 07:42     # same name, main's content, then green

Consequence for the fleet: **`main` is green, but any worker can be handed another worktree's
dependency** — a spurious error like the one above, or, worse, a *successful* build of stale code.
`AGENT_RULES.md` §2 documents the shared target dir as a wait-for-the-lock, not as a correctness
hazard, and workers will keep "fixing" a compile error that is not in their tree. This is an
environment fix (per-card target dirs, or `cargo`'s `target-dir` in `.cargo/config.toml` keyed on
the tree), not a dev card, and it should land before any dev card is trusted again.

How to tell B1 from a real error: B1's signature is an error that names **a type or symbol that does
not exist anywhere in your own tree**, usually in a file the card did not touch. Worked
counter-example from the same hour — `t_2ccd2241`'s review round 1 rejected commit `6f447b5` because
`cargo check -p harness-providers` fails E0599 at `crates/harness-providers/src/client.rs:230`; that
one is **real**, not B1: the branch has `.and_then(|c| c.first())` where `main:229` has
`.and_then(|c| c.get(0))`, and `&serde_json::Value` has no `first()`. Both kinds of failure are in
circulation today, which is the strongest argument for giving each worktree its own target dir.

### B2 — 12 of 24 open cards have no worktree: their `workspace_path` is the shared checkout

`workspace_kind = worktree`, `project_id` NULL, `workspace_path = /home/janelle/Downloads/rust-agent-harness`:
`t_d75d32db`, `t_6e419516`, `t_aed83347`, `t_fc182dc9`, `t_448d659f`, `t_75ed9224` (dev-1);
`t_d79b9293`, `t_24375a7e`, `t_29ea5d7c`, `t_22f8ed85`, `t_cfd6facc` (dev-2); `t_83845462` (qa).
A worker taking any of them would edit `main` in the shared checkout — forbidden by
`AGENT_RULES.md` §4 and colliding with every other worker and with the landing loop.

This was reported in `docs/TRENDS_2026.md` §4 for the four cards filed there; the real count is 12.
Observed mitigation, improvised by the worker rather than by the board: `t_d79b9293` was dispatched
at 07:43 with the broken row and created `.worktrees/t_d79b9293` on branch `wt/t_d79b9293` itself
(`git worktree list`, 07:43). The per-card fix is `git worktree add .worktrees/<id> -b wt/<id>`; the
board-level fix is to re-file with `project="luna-harness"`, or to give the dispatcher a fallback
that cuts `wt/<task-id>` whenever `project_id` is NULL. The 12-row list is in §3 because it is also
what makes those cards non-parallelisable.

### B3 — two files carry most of the queue, and neither is decomposed

`crates/harness-cli/src/commands/chat.rs` is named by **10** open cards and
`crates/harness-providers/src/auth.rs` by **6**. Two cards on one file must be serialised, so these
two files alone impose a chain of 10 and 6 respectively — with two developers, the queue's
parallelism is set by them, not by the priority column. Concrete decomposition to do before more
cards land: extract the REPL bootstrap out of `chat.rs` (`register_*` block at `:44-57`, engine
construction at `:37-38`, the turn loop at `:88-91`) into `commands/chat/` submodules, so the config,
tools, approval, MCP and session cards each own a different file. `auth.rs` needs the same split
between *store* (`CredentialManager`, `load/save`) and *selection* (`acquire_credential`, rotation,
`mark_credential_failed`), which is exactly the seam the six cards naturally fall on.

### B4 — the card bodies contradict the board on lane and on card id

Four card bodies still assert the pre-reassignment lane, so a worker reading only the body works the
wrong queue: `t_d79b9293` ("Lane: luna-dev-1 (providers)", board: `luna-dev-2`), `t_24375a7e` and
`t_29ea5d7c` ("Lane: luna-dev-1 (providers/CLI)", board: `luna-dev-2`), `t_73ce9054` ("Lane:
**luna-dev-1**", board: `luna-dev-2`; the §4 table in `FEATURE_GAPS.md` already notes the
reassignment — the body does not). The `assignee` column is authoritative and the comment threads
carry the reason, but the bodies should be corrected. Separately, `t_d79b9293`'s "Repro" line asks
for work "after t_0a789e15 lands" — that landed at `3456b9d`, and the repro command now runs.

---

## 1. Open cards, with the files each one will touch

Files are derived from each body's `file:line` anchors plus the comment threads; `(new)` means the
card creates the file. "unknown" means the body does not pin the location (a worker decision).

| id | title | lane | status | prio | files it will touch |
|---|---|---|---|---|---|
| `t_5c1eca76` | Config file, precedence, `luna config` | luna-dev-1 | running | 95 | `crates/harness-core/src/config.rs` (new), `crates/harness-core/src/lib.rs`, `crates/harness-cli/src/main.rs`, `crates/harness-cli/src/commands/config.rs` (new), `crates/harness-cli/src/commands/chat.rs`, `crates/harness-memory/src/engine.rs`, `crates/harness-notes/src/engine.rs` |
| `t_d79b9293` | Credential failover is dead code | luna-dev-2 | running | 95 | `crates/harness-providers/src/auth.rs`, `crates/harness-providers/src/client.rs`, `crates/harness-providers/tests/` (new test) |
| `t_c4f9c07d` | HITL approval gate for mutating tools | luna-dev-1 | ready | 92 | `crates/harness-core/src/agent.rs`, `crates/harness-core/src/traits.rs`, `crates/harness-core/src/error.rs`, `crates/harness-core/tests/agent_guardrail_test.rs`, `crates/harness-core/tests/` (new), `crates/harness-cli/src/commands/chat.rs` |
| `t_24375a7e` | Corrupt `credentials.json` wipes every key | luna-dev-2 | ready | 90 | `crates/harness-providers/src/auth.rs`, `crates/harness-providers/tests/` |
| `t_7c2387f4` | Verify the install (build + NIM + realterm) | luna-qa | ready | 90 | none (report-only; `dir` workspace, runs the binary) |
| `t_c6cf04fa` | Filesystem and shell tool suite | luna-dev-1 | ready | 88 | `crates/harness-core/src/tools/` (new), `crates/harness-core/src/lib.rs`, `crates/harness-core/src/traits.rs`, `crates/harness-core/src/registry.rs`, `crates/harness-cli/src/commands/chat.rs`, `crates/harness-core/Cargo.toml` (tempfile dev-dep) |
| `t_d75d32db` | CLI reports failures but exits 0 | luna-dev-1 | ready | 86 | `crates/harness-cli/src/main.rs`, `crates/harness-cli/src/commands/auth.rs`, `crates/harness-cli/src/commands/models.rs`, `crates/harness-providers/src/auth.rs` |
| `t_29ea5d7c` | Rotation state is never persisted | luna-dev-2 | ready | 85 | `crates/harness-providers/src/auth.rs`, `crates/harness-cli/src/commands/auth.rs` |
| `t_2a4b4808` | Model/provider resolution + one-shot `luna ask` | luna-dev-1 | ready | 85 | `crates/harness-cli/src/main.rs`, `crates/harness-cli/src/commands/chat.rs`, `crates/harness-cli/src/commands/ask.rs` (new), `crates/harness-providers/src/catalog.rs` (read) |
| `t_6e419516` | Cancellation is unreachable from `luna chat` | luna-dev-1 | ready | 85 | `crates/harness-cli/src/commands/chat.rs`, `Cargo.toml` (**unknown** which crate carries the signal handler), `crates/harness-voice/src/pipeline.rs` (pattern only) |
| `t_73ce9054` | Session persistence and bounded context | luna-dev-2 | ready | 85 | `crates/harness-core/src/session.rs` (new), `crates/harness-core/src/agent.rs`, `crates/harness-cli/src/commands/chat.rs`, `crates/harness-cli/src/commands/sessions.rs` (new), `crates/harness-cli/src/main.rs` |
| `t_40de5ef3` | Reasoning-aware turns | luna-dev-2 | ready | 84 | `crates/harness-providers/src/client.rs`, `crates/harness-core/src/types.rs`, `crates/harness-core/src/agent.rs` |
| `t_aed83347` | Discovery cache failures swallowed + README claim | luna-dev-1 | ready | 84 | `crates/harness-providers/src/discovery.rs`, `crates/harness-providers/src/auth.rs` (only if the fetch is wired into `add_credential`), `crates/harness-cli/src/commands/models.rs`, `README.md` |
| `t_16a223b8` | Wire MCP servers from config into the registry | luna-dev-2 | ready | 80 | `crates/harness-cli/src/commands/chat.rs`, `crates/harness-cli/src/commands/mcp.rs` (new), `crates/harness-cli/Cargo.toml`, `crates/harness-mcp/src/stdio.rs`, `crates/harness-mcp/tests/fixtures/` (new) |
| `t_22f8ed85` | Memory superseding never works (FK ordering) | luna-dev-2 | ready | 80 | `crates/harness-memory/src/engine.rs`, `crates/harness-memory/src/tools.rs`, `crates/harness-memory/tests/` (new) |
| `t_fc182dc9` | Sandboxed shell execution (bwrap) | luna-dev-1 | todo | 80 | `crates/harness-core/src/sandbox.rs` (new; **unknown** vs `src/tools/sandbox.rs`), the `terminal` tool from `t_c6cf04fa`, `crates/harness-cli/src/commands/doctor.rs` (new), `crates/harness-core/src/config.rs` |
| `t_2ccd2241` | Credential safety: keyring, 0600, redaction | luna-audit | review | 78 | `crates/harness-providers/src/auth.rs`, `crates/harness-providers/src/lib.rs`, `crates/harness-providers/Cargo.toml`, `crates/harness-providers/tests/auth_keyring_test.rs` + `auth_security_test.rs`, `crates/harness-cli/src/commands/auth.rs`, `scripts/register-nvidia-key.py` |
| `t_448d659f` | Cache-stable prompt assembly + cache telemetry | luna-dev-1 | todo | 78 | `crates/harness-core/src/agent.rs`, `crates/harness-core/src/registry.rs`, `crates/harness-core/src/types.rs`, `crates/harness-providers/src/client.rs`, `crates/harness-telemetry/src/lib.rs`, `crates/harness-cli/src/commands/chat.rs` |
| `t_83845462` | Eval suite + `luna eval` gate | luna-qa | ready | 78 | `tests/eval/*` (new), `crates/harness-cli/src/commands/eval.rs` (new), `crates/harness-cli/src/main.rs`, `docs/EVALS.md` (new); a `crates/harness-evals/` crate is left as the worker's call |
| `t_26fb21a1` | Evolver applies unverified patches | luna-dev-1 | ready | 75 | `crates/harness-evolver/src/engine.rs`, `crates/harness-evolver/src/sandbox.rs`, `crates/harness-evolver/tests/` (new) |
| `t_75ed9224` | `AGENTS.md` into the stable prompt | luna-dev-1 | todo | 74 | `crates/harness-core/src/agent.rs`, `crates/harness-cli/src/commands/agents.rs` (new), `crates/harness-cli/src/main.rs`, `crates/harness-core/src/config.rs` |
| `t_cfd6facc` | Deferred tool catalog | luna-dev-2 | todo | 72 | `crates/harness-core/src/registry.rs`, `crates/harness-core/src/tools/catalog.rs` (new), `crates/harness-core/src/traits.rs`, `crates/harness-cli/src/commands/chat.rs`, `crates/harness-mcp/` (integration test) |
| `t_eb80814a` | Setup-path verification from a fresh clone | luna-qa | running | 72 | `docs/AGENT_RULES.md`, `README.md` (doc fixes on its branch); `/tmp/luna-fresh` is outside the repo |
| `t_37ec9e28` | This roadmap | luna-scout | running | 68 | `docs/ROADMAP.md` (new), `docs/FEATURE_GAPS.md`, `docs/TRENDS_2026.md` |

---

## 2. Dependency map

### 2.1 Edges that exist on the board today

From `task_links`, restricted to cards that are still open (an edge whose parent is `done` is already
satisfied and cannot hold anything up):

| parent | child | state | why it is real |
|---|---|---|---|
| `t_5c1eca76` | `t_448d659f` | **blocking** | every key the prompt card adds (`prompt.stable_prefix`, `telemetry.usage`) is defined by the config card |
| `t_5c1eca76` | `t_75ed9224` | **blocking** | same, for `prompt.agents_md` / `prompt.agents_md_max_tokens` |
| `t_448d659f` | `t_75ed9224` | **blocking** | the AGENTS.md block must be placed in the *stable* prefix the prompt card defines; landing it first would put it in the volatile head and defeat the cache |
| `t_c6cf04fa` | `t_fc182dc9` | **blocking** | the sandbox wraps the `terminal` tool that card creates; without it there is no command to wrap |
| `t_c4f9c07d` | `t_fc182dc9` | **blocking** | approval is the other knob of the same tool-execution path; both cards edit it |
| `t_16a223b8` | `t_cfd6facc` | **blocking** | the catalog layer sits on top of the flat MCP registration |
| `t_0a789e15` → `t_2a4b4808`, `t_40de5ef3`, `t_16a223b8`, `t_7c2387f4`, `t_83845462` | | satisfied | parent merged at `3456b9d`; these five are unblocked |
| `t_190a014a` → `t_5c1eca76`, `t_c4f9c07d`, `t_c6cf04fa`, `t_73ce9054` | | satisfied | spec parent, `done` |

### 2.2 Missing edges this pass can justify

Each row is a card that cannot work correctly until another lands, with the file or type that makes
it true. None of these is a preference about order — they are all "the second card edits the first
card's output".

| # | missing edge | why (specific) |
|---|---|---|
| M1 | `t_2ccd2241` → `t_24375a7e` | `crates/harness-providers/src/auth.rs`: the credential-safety branch already replaces the `load_from_disk(...).unwrap_or_default()` path this card is about — dev-2's own comment on `t_2ccd2241` asks `t_24375a7e` to build on `CredentialStorage`/`credentials.json.corrupt` instead of adding a second mechanism. Landing them in parallel produces two handlers for one failure. |
| M2 | `t_2ccd2241` → `t_29ea5d7c` | same file, `save_to_disk` (`auth.rs:82-91`): persisting `usage_count` means writing through the new storage layer; a persistence built on the file-only path is thrown away by the merge. |
| M3 | `t_2ccd2241` → `t_d79b9293` | `auth.rs` **and** `client.rs`. The failover path picks a key via `build_auth_headers` (`client.rs:51-87`), which the keyring/env resolution rewrites; and `t_d79b9293` is *running now* while `t_2ccd2241` sits in `review` — this collision is live, not theoretical. |
| M4 | `t_40de5ef3` → `t_d79b9293` | `client.rs`: `complete()`'s response parsing (`:222-264`) and the 429 path (`:206`) are in the same function; the lead already asserted this order in a comment on `t_d79b9293` — it is simply not an edge in the DB. |
| M5 | `t_40de5ef3` → `t_448d659f` | `client.rs` (usage/`cached_tokens` parsing lands in the same `complete`/`stream` blocks) and `crates/harness-core/src/types.rs` (`TokenUsage`, `:109-114`): the reasoning field and the cache field are two edits to one struct. |
| M6 | `t_448d659f` → `t_cfd6facc` | `crates/harness-core/src/registry.rs`: `list_tools()`'s contract (`:32-35`) is redefined twice — deterministically sorted by the prompt card, restricted to the eager set by the catalog card. Two definitions of one signature. |
| M7 | `t_c6cf04fa` → `t_448d659f` | `registry.rs` again, and more sharply: the prompt card's acceptance test asserts `list_tools()` order stability **with the new tools registered**. Landing it before the tool suite means re-testing it after. |
| M8 | `t_5c1eca76` → `t_c4f9c07d`, `t_c6cf04fa`, `t_fc182dc9`, `t_73ce9054`, `t_cfd6facc` | each of these five adds config keys (`tools.approval*`, `tools.read_max_chars`, `tools.sandbox.*`, `paths.sessions_db` + `agent.compact_at_tokens`, `tools.catalog.*`). Without the config module they each need to invent a loader, which is the one duplication the config spec explicitly forbids. The two cards that already cite the dependency (`t_448d659f`, `t_75ed9224`) do not cover these. |
| M9 | `t_6e419516` → `t_73ce9054` | `crates/harness-cli/src/commands/chat.rs`: the cancel handler wraps the same per-turn call the session card is rewriting (`:88-91`); the cancellation card itself says "coordinate with the session-persistence card if both touch the chat loop". |
| M10 | `t_2a4b4808` → `t_83845462` | the eval suite is scripted against a non-interactive entry point; `luna eval run` driving the binary needs `luna ask` / `chat --once` to exist. Today `main.rs` has no such path (`:25-55`), which is exactly why `t_7c2387f4`'s step 5 says "if it does not, say so". |
| M11 | `t_2a4b4808` → `t_7c2387f4` | same reason, weaker: install verification asks for a non-interactive round-trip in step 5. |
| M12 | `t_c6cf04fa` and `t_c4f9c07d` → `t_83845462` | the eval suite's cases are "two-step write-then-read" and "denied-approval turn" — they have nothing to assert until the tools and the gate exist. Offline-fixture cost, not a hard block. |
| M13 | `t_26fb21a1` → `t_fc182dc9` | both change how an unverified/hostile command is allowed to run (`harness-evolver/src/sandbox.rs` vs the shared `tools.sandbox.*` policy). If the sandbox card lands first, the evolver should reuse it instead of keeping its own `cargo test` copy. Direction is a judgement call; the conflict (two sandboxes) is not. |

Satisfied-by-landing, no edge needed: `t_8935ed8a` → `t_22f8ed85`. The `blocking_lock()` panic that
was blocking the memory tests is fixed on `main` (`e5ae82c`, merged `639c38e`), and
`memory/engine.rs`'s constructors now build the connection before sharing it behind the mutex, so
`t_22f8ed85`'s acceptance (`cargo test -p harness-memory` green) is reachable without waiting.

---

## 3. File-overlap map

Grouped by file; a group is a serialisation chain, not a set that can run in parallel. Counts are of
open cards only.

| file | cards | note |
|---|---|---|
| `crates/harness-cli/src/commands/chat.rs` | **10**: `t_5c1eca76`, `t_c4f9c07d`, `t_c6cf04fa`, `t_2a4b4808`, `t_6e419516`, `t_73ce9054`, `t_16a223b8`, `t_448d659f`, `t_cfd6facc`, `t_83845462` | the queue's bottleneck; see B3. Registration block `:44-57`, loop `:88-91` |
| `crates/harness-providers/src/auth.rs` | **6**: `t_2ccd2241`, `t_d79b9293`, `t_24375a7e`, `t_29ea5d7c`, `t_d75d32db` (unknown-provider rejection at `:148`), `t_aed83347` (only if it wires the fetch into `add_credential`) | four of these are in flight or ready at once |
| `crates/harness-core/src/agent.rs` | **5**: `t_c4f9c07d`, `t_40de5ef3`, `t_448d659f`, `t_73ce9054`, `t_75ed9224` | gate hooks, reasoning handling, prompt assembly, compaction, AGENTS.md all land in the loop |
| `crates/harness-providers/src/client.rs` | **3**: `t_40de5ef3`, `t_d79b9293`, `t_448d659f` | `complete()` `:222-264`, 429 at `:206`, usage at `:329-343` |
| `crates/harness-core/src/registry.rs` | **3**: `t_c6cf04fa`, `t_448d659f`, `t_cfd6facc` | `list_tools()` semantics; `:13`, `:32-35` |
| `crates/harness-cli/src/main.rs` | **4**: `t_5c1eca76`, `t_2a4b4808`, `t_6e419516`, `t_73ce9054` (+`t_83845462` adds `eval`) | subcommand enum `:25-55`, dispatch `:60-87` |
| `crates/harness-cli/src/commands/auth.rs` | **3**: `t_2ccd2241`, `t_29ea5d7c`, `t_d75d32db` | the `Used:` column and the exit codes |
| `crates/harness-core/src/traits.rs` | **3**: `t_c4f9c07d` (`is_mutating`), `t_c6cf04fa`, `t_cfd6facc` | `Tool` trait `:100` |
| `crates/harness-cli/src/commands/models.rs` | **2**: `t_d75d32db`, `t_aed83347` | exit codes vs cache warnings |
| `crates/harness-core/src/types.rs` | **2**: `t_40de5ef3`, `t_448d659f` | `TokenUsage` `:109-114` |
| `crates/harness-memory/src/engine.rs` | **1 open**: `t_22f8ed85` (`t_8935ed8a` landed) | `save`/`update` `:184-223`, pointers `:260-275` |
| `crates/harness-core/src/config.rs` | **4 consumers, 1 author**: authored by `t_5c1eca76`; `t_73ce9054`, `t_75ed9224`, `t_fc182dc9`, `t_c4f9c07d` add keys | read-only for the other three, but all four edits are to one file |
| `crates/harness-core/src/sandbox.rs` (new) | 1: `t_fc182dc9` | |
| `crates/harness-providers/src/discovery.rs` | 1: `t_aed83347` | |
| `crates/harness-mcp/src/stdio.rs` | 1: `t_16a223b8` | `:102` hardcoded `2024-11-05` |
| `crates/harness-telemetry/src/lib.rs` | 1: `t_448d659f` | `TokenTracker` `:15` |
| `crates/harness-evolver/src/{engine,sandbox}.rs` | 1: `t_26fb21a1` | |
| `crates/harness-memory/src/tools.rs` | 1: `t_22f8ed85` | |
| `docs/AGENT_RULES.md` | 1 author: `t_eb80814a` (doc fixes) | `t_37ec9e28` repairs the two *citations* of it, not the file |

### 3.1 Pairs that are safe to run in parallel (disjoint file sets)

Cross-lane pairs, which is what matters with two developers:

| pair | why it is disjoint |
|---|---|
| `t_26fb21a1` (dev-1) ∥ `t_22f8ed85` (dev-2) | `harness-evolver` vs `harness-memory`; no shared file, no shared type |
| `t_aed83347` (dev-1) ∥ `t_40de5ef3` (dev-2) | `discovery.rs` + `commands/models.rs` vs `client.rs` + `types.rs` + `agent.rs`. **Conditional**: only if `t_aed83347` fixes the README instead of wiring the auto-fetch into `add_credential` — that branch touches `auth.rs` and collides with the whole M1–M3 chain |
| `t_c4f9c07d` (dev-1) ∥ `t_22f8ed85` (dev-2) | `harness-core` vs `harness-memory` |
| `t_d75d32db` (dev-1) ∥ `t_22f8ed85` (dev-2) | CLI exit codes vs memory engine. **Not** parallel with `t_29ea5d7c`/`t_2ccd2241` (all three edit `auth.rs`) |
| `t_c6cf04fa` (dev-1) ∥ `t_22f8ed85` (dev-2) | fs/shell tools vs memory |
| `t_83845462` (qa) ∥ `t_26fb21a1` (dev-1), ∥ `t_22f8ed85` (dev-2) | the eval suite is new files under `tests/eval/` + a new command; it reads the binary, it does not edit it |
| `t_7c2387f4` (qa) ∥ *everything* | verification report, `dir` workspace, no source edits |
| `t_eb80814a` (qa) ∥ `t_5d1ad34e`-style doc lanes | docs-only edits on its own branch |

Within a lane the only clean parallel pair is `t_26fb21a1` ∥ `t_aed83347` (evolver vs
providers/discovery) — dev-1's queue is otherwise a chain through `chat.rs` and `auth.rs`, which is
why the recommendation in B3 matters more than any reordering.

### 3.2 Pairs that must be serialised (same file)

| file | order | reason |
|---|---|---|
| `auth.rs` | `t_2ccd2241` → {`t_24375a7e`, `t_d79b9293`} → `t_29ea5d7c` → `t_d75d32db` (→ `t_aed83347` if it wires the fetch) | M1–M3; the audit's own hotspot comment proposes this exact order |
| `client.rs` | `t_40de5ef3` → `t_d79b9293` → `t_448d659f` | reasoning parsing, then the 429/retry path, then usage/`cached_tokens` |
| `registry.rs` / `traits.rs` | `t_c4f9c07d` → `t_c6cf04fa` → `t_448d659f` → `t_cfd6facc` | the `Tool` trait gains `is_mutating`, then five impls, then `list_tools()` ordering, then visibility |
| `agent.rs` | `t_c4f9c07d` → `t_40de5ef3` → `t_73ce9054` → `t_448d659f` → `t_75ed9224` | the loop is edited five times; gate first (it changes the per-call path), prompt assembly last (it depends on the AGENTS.md input and the compaction estimator) |
| `commands/chat.rs` | any two of the 10 | no safe pair; see B3 |
| `commands/main.rs` | `t_5c1eca76` → `t_2a4b4808` → {`t_73ce9054`, `t_6e419516`} → `t_83845462` | one subcommand enum |

---

## 4. Merge order and critical path

Five cards are already in flight and are not re-ordered here: `t_5c1eca76` (dev-1, config),
`t_d79b9293` (dev-2, failover — dispatched 07:43), `t_2ccd2241` (luna-audit, credential safety, in
`review`), `t_eb80814a` (qa, setup path), and this card. The table below is the order a single
developer can drain the remaining queue in; "inversion" means the slot is not the raw-priority slot,
with the reason.

| # | card | prio | why this slot | inversion |
|---|---|---|---|---|
| 0 | `t_5c1eca76` | 95 | in flight; the config module is the input to 7 other cards | — |
| 1 | `t_2ccd2241` | 78 | **first thing to land in `auth.rs`**: M1–M3 hang off it, it is already written (`6f447b5`), and every other auth card landing first is rework. Churn after the snapshot: review round 1 requested changes at 07:53 (`client.rs:230` does not compile — B1's counter-example in §0) and the card is back to `ready` on `luna-dev-2`, so it needs a fix cycle before it can take this slot; the slot itself does not move. | yes — 13 slots up |
| 2 | `t_c4f9c07d` | 92 | the gate must exist before the tools that must go through it (`t_c6cf04fa`), and it is what makes `agent.require_hitl_for_mutation` true (`agent.rs:12`) | — |
| 3 | `t_24375a7e` | 90 | head of the auth chain behind `t_2ccd2241` (M1); the data-loss half of the same function | — |
| 4 | `t_29ea5d7c` | 85 | `save_to_disk` through the new storage (M2), before the CLI card that asserts on the persisted column | yes — before `t_d75d32db` (86) because both edit `commands/auth.rs`/`auth.rs` and this one writes the state the other asserts on |
| 5 | `t_d75d32db` | 86 | last `auth.rs`/`commands/auth.rs` writer; its acceptance runs `auth strategy`/`auth remove`, i.e. the code the four cards above rewrote | yes — one slot down |
| 6 | `t_c6cf04fa` | 88 | the five tools; also the registry change `t_448d659f`'s test needs present (M7) | — |
| 7 | `t_fc182dc9` | 80 | third and last piece of the tool-execution path (gate → tools → sandbox), so the three land as one change to that path | yes — 5 slots up; both parents are done by then |
| 8 | `t_22f8ed85` | 80 | independent of everything; safe to land at any point, and its blocking `t_8935ed8a` has already landed | yes — can go much earlier if dev-2 is free |
| 9 | `t_40de5ef3` | 84 | **it was supposed to land before `t_d79b9293`** (lead's comment) and did not — the dispatcher started the second card of that pair at 07:43. Landing it now is a race; whoever merges second must re-run `cargo test -p harness-providers` and re-read `complete()` | yes — should have been #0b |
| 10 | `t_aed83347` | 84 | `discovery.rs` + `models.rs`; after the auth chain because the "auto-fetch on credential add" branch touches `auth.rs` | yes — 2 slots down, to keep one writer on `auth.rs` |
| 11 | `t_6e419516` | 85 | before the session card (M9): the SIGINT handler wraps the same `run_task` call the session card rewrites | — |
| 12 | `t_73ce9054` | 85 | sessions + compaction; also the estimator `t_75ed9224` reuses | — |
| 13 | `t_2a4b4808` | 85 | the non-interactive entry point; both QA cards and the eval suite depend on it (M10, M11) | yes — the two QA cards are held behind it rather than run first |
| 14 | `t_7c2387f4` | 90 | the install verification is only complete once step 5 (non-interactive round-trip) has something to run | yes — 12 slots down; run early only as a partial report |
| 15 | `t_16a223b8` | 80 | MCP wiring, on the flat registry | yes — before `t_448d659f`, because both change `list_tools()` and this one is additive |
| 16 | `t_448d659f` | 78 | prompt assembly: needs the tools registered (M7), the reasoning field (M5), and the config keys; it is on the critical path | yes — 2 slots down |
| 17 | `t_75ed9224` | 74 | AGENTS.md block goes into the stable prefix `t_448d659f` defines | — |
| 18 | `t_cfd6facc` | 72 | last: M6 requires the sorted `list_tools()` and the flat MCP registry | — |
| 19 | `t_83845462` | 78 | the eval gate asserts on tools, gate and the one-shot path, and is the fleet's regression net for everything above | yes — near the end on purpose (it is how the rest is kept honest) |
| 20 | `t_26fb21a1` | 75 | evolver; independent, and best landed after the sandbox policy exists (M13) | — |
| 21 | `t_eb80814a`, `t_37ec9e28` | 72/68 | in flight; documentation lanes | — |

Other inversions worth stating plainly, with the reason:

- **config before everything that adds a key** (`t_5c1eca76` at #0 although several cards outrank it
  by number): five cards add keys and none of them can name a module to add them to. Without it each
  invents a loader, which the config spec exists to prevent.
- **`t_c4f9c07d` before `t_c6cf04fa`** even though it is lower priority: shipping `write_file` /
  `terminal` without the gate is the one irreversible mistake available in this repo (`FEATURE_GAPS.md`
  §2.3 says the same).
- **`t_d75d32db` after `t_29ea5d7c`**: the exit-code card's acceptance asserts on a `Used:` column
  that is only meaningful once rotation state persists.
- **`t_40de5ef3`/`t_d79b9293` order is already broken**: the sequencing note on `t_d79b9293` says
  "t_40de5ef3 first, because it is what lets QA verify a live NIM round-trip at all, then this one" —
  but the board dispatched `t_d79b9293` at 07:43 while `t_40de5ef3` is still `ready`. Both edit
  `client.rs`. The landing loop should serialise the merges and the second card should `git merge main`
  and re-read `complete()`.

### 4.1 The critical path

Longest chain in the (existing + justified) edge graph:

    t_5c1eca76 (config) → t_c6cf04fa (fs/shell tools) ─┬→ t_448d659f (prompt assembly) → t_cfd6facc (deferred catalog)
                                                       │
    t_5c1eca76 (config) → t_40de5ef3 (reasoning turns) ┘

Four hops, two equal branches through `t_448d659f`. Nothing on the critical path is a feature that
can be skipped: config is a prerequisite, tools and reasoning are the two things the other cards
consume, and the catalog is the leaf.

**The card whose landing unblocks the most others is `t_5c1eca76` (config)** — 2 real edges plus 5
justified ones, 7 in total. Second is `t_40de5ef3` (3: `t_d79b9293`, `t_448d659f`, and QA's ability
to verify a live NIM round-trip at all), then `t_c6cf04fa` and `t_2a4b4808` (2 each). If only one
card from this roadmap is protected from preemption, it is the config card.

---

## 5. Staleness pass over `FEATURE_GAPS.md` and `TRENDS_2026.md`

Re-read against `main 639c38e`. `LIVE` = the spec's anchors and its "absent/partial" verdict still
match the code. `STALE` = an anchor moved, an API changed, or a landed card changed the verdict.
`DONE` = the work exists on `main`. Anchors that moved were repaired in place in those two files
(this pass's edits are listed in §6).

### `docs/FEATURE_GAPS.md`

| spec | verdict | evidence at `639c38e` |
|---|---|---|
| §2.1 config file (row 1) | LIVE | `crates/harness-core/src/` holds only `agent.rs error.rs lib.rs registry.rs traits.rs types.rs` — no config module; `crates/harness-cli/src/main.rs:28` still `default_value = "openai"`; `t_5c1eca76` running |
| §2.2 fs/shell tools (row 2) | LIVE | `grep -rn harness_mcp\|harness_plugins crates/harness-cli/src` → 0; `commands/chat.rs:44-57` still registers only memory/notes/evolver |
| §2.3 HITL gate (row 3) | LIVE | `require_hitl_for_mutation` appears exactly three times: `agent.rs:12` (declaration), `:20` (default), `tests/agent_guardrail_test.rs:165` — still read by no production path; `HarnessError::ApprovalRejected` has no producer |
| §2.4 sessions + compaction (row 4) | LIVE | `agent.rs:89` `let mut messages = Vec::new();`, `agent.rs:135` `.complete(...)` on the fresh vector, `commands/chat.rs:88` one `run_task` per input line |
| §2.5 MCP from config (row 5) | LIVE | `harness-mcp/src/stdio.rs:21 spawn`, `:100 initialize`, `:117 list_tools`, `:132 call_tool` all present; `harness-cli/src` has 0 refs |
| §3.1 skills | LIVE | `harness-plugins/src/skills.rs:29 load_skills_from_dir`, `:81 SkillToolAdapter`; 0 refs from `harness-cli` |
| §3.2 vector memory | LIVE | `grep -rn memory_embeddings crates --include=*.rs` → 0; `memory/engine.rs:58` is still LIKE-only; `traits.rs:34 embed` has 0 callers (`grep -rn "\.embed(" crates` → nothing) |
| §3.3 streaming + reasoning | LIVE | `harness-core/src/agent.rs` calls `.complete(` once (`:135`) and never `.stream(`; `client.rs:240` reads `content` only |
| §3.4–§3.7, §3.9 (web, cron, wasm, voice, subagents) | LIVE | `harness-plugins/src/wasm.rs:57` still echoes its payload; `commands/voice.rs:26-53` still the three stubs; no HTTP tool, no cron, no delegate |
| §3.8 telemetry | LIVE | `TokenTracker` appears only at `harness-telemetry/src/lib.rs:15,20` — declared, never constructed |
| §3.10 credential hardening | **STALE (superseded by an in-flight branch)** | the *main-side* claim is still literally true (`auth.rs:33 pub api_key: String`, `Cargo.toml:40 keyring` unused, `auth.rs:82-91` writes it verbatim), but the work exists on branch `luna-harness/t_2ccd2241-…` @ `6f447b5` (`CredentialStorage`, keyring test, `auth_security_test.rs`) and the card is in `review`. Read this spec as "land `t_2ccd2241`", not "write §3.10" |
| §3.11 self-evolution | LIVE | `commands/supervisor.rs:34` is still `if code == 42 {`; `evolver/src/sandbox.rs:19` + `engine.rs:100` unchanged. Note `t_26fb21a1` (filed by audit pass 2) now owns the unverified-patch half |
| §3.12 retry/backoff | LIVE, but now overlapping | `client.rs:206` still maps 429 straight to `RateLimited`; `t_d79b9293` is editing that same function region for key retirement — two cards, one `if` |
| §3.13 CLI surface | LIVE | `main.rs:25-55` still five subcommands (`chat`, `auth`, `models`, `voice`, `supervisor`) |
| §3.14 provider matrix honesty | LIVE | `client.rs:72-79` sets the Anthropic header and then serialises OpenAI-shaped JSON; `catalog.rs:20-92` unchanged |
| §4 "Cards filed from this inventory" table | LIVE | all five ids, lanes and priorities match the board; the seven archived originals are still `archived` |
| §4 build-state paragraph (`:456-465`) | **STALE (DONE)** | the 24 `harness-cli` errors are fixed: `t_0a789e15` committed `7188aa0`, merged at `3456b9d`; `cargo check -p harness-cli` → `Finished`, exit 0 (measured this pass). The diagnosis is kept as history; the sentence "every test plan above assumes that card's green build" is now satisfied |
| §4 "pre-existing cards corroborated" (`:452-454`) | LIVE | `t_40de5ef3`, `t_2ccd2241`, `t_2a4b4808` still the right homes; `t_0a789e15` and `t_7c2387f4` still on the board |

Anchors repaired in `FEATURE_GAPS.md`: `docs/AGENT_RULES.md:30` → `:41` (twice; the reasoning-model
note moved when the four rules commits landed) and `docs/AGENT_RULES.md:35` → `:46` (the
rate-limit note). The `error.rs:65` citations were an off-by-one at write time — the variant is
`ApprovalRejected` at `:66` — corrected in both places.

### `docs/TRENDS_2026.md`

| spec | verdict | evidence at `639c38e` |
|---|---|---|
| §2.1 prompt assembly + cache telemetry | LIVE | `registry.rs:13` `RwLock<HashMap<…>>` and `:32-35` unsorted `list_tools()`; `agent.rs:92-117` rebuilds the system text each turn; `types.rs:109-114` has no cache field; `client.rs:222-264` ignores `usage`; `client.rs:284` asks for usage only in `stream()`; `grep -rn cached_tokens crates` → nothing |
| §2.2 sandboxed shell | LIVE | re-measured today: `bwrap --version` → `bubblewrap 0.12.0`, `/usr/bin/bwrap`; kernel `7.2.4-arch1-2`. `t_fc182dc9` still `todo`, parents still open |
| §2.3 deferred catalog | LIVE | `t_16a223b8` still `ready`, `t_cfd6facc` still `todo`; no `visibility`/`defer_loading` anywhere in `harness-core` |
| table row 2 (reasoning) fold → `t_40de5ef3` | LIVE | card still open with the fold comment attached |
| table row 8 (MCP 2026-07-28) fold → `t_16a223b8` | LIVE | `harness-mcp/src/stdio.rs:102` still sends `"protocolVersion": "2024-11-05"` |
| rows 6, 9, 10, 14 folds (`t_c4f9c07d`, `t_83845462`, `t_73ce9054`) | LIVE | all four cards still open |
| §4 workspace-defect paragraph | **STALE (understated)** | it reports four cards; the real count is **12 open cards** whose `workspace_path` is the shared checkout (see B2) |
| §6 board-state quote | **STALE (statuses + counts drifted)** | `t_2a4b4808` and `t_40de5ef3` were `todo`, both are `ready` now; the edge list omitted `t_448d659f → t_75ed9224`; open counts are now dev-1 11, dev-2 8, qa 3, audit 1, scout 1 = 24 |
| §6 "against checkout `319bf38`" | LIVE, now dated | the code facts were re-read at `639c38e` this pass and still hold; the revision line in the doc now names the current one |
| §7 sources | not re-verified | citations are unchanged; this pass re-tested only the two machine-local measurements (bwrap, toolchain), not the web sources |

Anchors repaired in `TRENDS_2026.md`: the usage-parsing block cited as
`client.rs:329-340` is `:329-343` (the `usage` branch through the `StreamEvent::Completed`
construction); §4's four-card workspace list and §6's board-state block were rewritten with the
current counts.

---

## 6. What this pass changed, and what it deliberately did not

Edits to the two documents (anchors and stale claims only — no spec was rewritten, nothing deleted):

- `docs/FEATURE_GAPS.md`: three moved anchor citations repaired (`AGENT_RULES.md:30 → :41` twice,
  `:35 → :46`); the two `error.rs:65` citations corrected to `:66`; the §4 build-state paragraph
  marked as fixed, naming `7188aa0`/`3456b9d` and the command that proves it.
- `docs/TRENDS_2026.md`: the `client.rs:329-340` anchor corrected to `:329-343`; §4's
  four-card workspace-defect list replaced with the current count and the query that produces it;
  §6's board-state block re-quoted from the 07:46 read (statuses, edges, per-lane counts).

Not done, on purpose: **no new card was filed from this pass.** The board carries 11 open cards for
dev-1 and 8 for dev-2 (one more arrived during this run), and adding a 25th work item would delay the
first one landing. B1–B4 are reported here and in the card's comment thread instead, because they are
environment/board repairs rather than development work — except B3, which is a refactor of two files
and belongs to whoever owns `commands/chat.rs` next.

Also not done: no product code was touched, `main` was not rebased or merged into, and the only
uncommitted artefact this pass started with (`docs/AUDIT_NOTES.md`) has since been committed by the
audit lane itself at `cd63c36`.

Reproduce the two headline numbers:

    # B1 — main is green, but the shared target dir can serve another worktree's rmeta
    cd /home/janelle/Downloads/rust-agent-harness
    CARGO_TARGET_DIR=/home/janelle/.luna/cargo-target cargo check -p harness-cli   # foreign type or green, depending on who built last
    touch crates/harness-providers/src/auth.rs && \
      CARGO_TARGET_DIR=/home/janelle/.luna/cargo-target cargo check -p harness-cli   # Finished, exit 0

    # B2 — open cards with no worktree
    sqlite3 ~/.shiina/kanban/boards/luna-harness/kanban.db \
      "select id,assignee from tasks where status not in ('archived','done') \
        and workspace_kind='worktree' and project_id is null;"
    # 12 rows at 2026-10-15 07:46

Verified while writing this document: `crates/harness-memory/src/engine.rs` now builds its connection
before sharing it (`e5ae82c`), so §2.4-of-`FEATURE_GAPS`'s blocking item and the "also in scope"
`create_dir_all` swallows at `memory/engine.rs:24` / `notes/engine.rs:25` are both gone from `main` —
the remaining memory work is `t_22f8ed85` alone.



