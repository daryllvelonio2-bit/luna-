# Audit pass 2 — memory, notes, MCP, plugins, evolver, voice, telemetry

Read-only audit by `luna-audit` (card `t_5d1ad34e`), audited at `70d832c`, on
`/home/janelle/Downloads/rust-agent-harness`. Pass 1 covered `harness-providers`,
`harness-core` and `harness-cli` (`t_fea37dec`); this pass covers the seven crates nobody had
audited: `harness-memory`, `harness-notes`, `harness-mcp`, `harness-plugins`, `harness-evolver`,
`harness-voice`, `harness-telemetry`. No product file was modified: everything below is a repro
command plus its observed output.

List is ordered by blast radius — **start at the top**. The first three items are filed as cards
(see §1); everything else is for the next pass to pick up, not for a new card today.

---

## 1. Cards filed from this pass

| Card | Defect | Evidence |
| --- | --- | --- |
| `t_22f8ed85` | Memory superseding is a hard failure: `UPDATE … superseded_by = <new id>` runs before the new row exists, and the bundled SQLite is built with `SQLITE_DEFAULT_FOREIGN_KEYS=1`, so **every** conflicting `save`/`update` returns `FOREIGN KEY constraint failed` and nothing is superseded. Second symptom in the same block: all note pointers share `subject='note', predicate='stored_document'`, so pointers for older notes can never stay active. | §2.1 |
| `t_26fb21a1` | `CodebaseEvolver::apply_patch` overwrites any repo file with no verification gate and no rollback, and `propose_patch` silently retargets a non-JSON payload at `crates/harness-core/src/lib.rs`. Reproduced overwriting a file with untested text. | §2.2 |

Everything else in §2 stays here on purpose: three of the items duplicate work already spec'd in
`docs/FEATURE_GAPS.md` (§12 wasm facade, §13 voice facade, §14 dead token tracker), and the MCP
items belong to card `t_16a223b8` ("Wire MCP servers from config"), which is already `ready` and
explicitly assumes the client works — the evidence was added to that card as a comment instead of
opening a second card for the same code.

---

## 2. Ranked findings

### 2.1 `harness-memory` superseding cannot work — card `t_22f8ed85`

`crates/harness-memory/src/engine.rs:184-191` (in `save`) and `:219-223` (in `update`) deactivate the
old row with `superseded_by = ?1` pointing at a record that is inserted only afterwards
(`:193-210`, `:225-242`). `memories.superseded_by` is a real FK (`crates/harness-memory/schema.sql:12`)
and this workspace builds SQLite with foreign keys on by default
(`libsqlite3-sys-*/build.rs:123`: `-DSQLITE_DEFAULT_FOREIGN_KEYS=1`), so the `UPDATE` itself fails and
the whole `save` returns `HarnessError::MemoryError`. There is no transaction: a failure between the
two statements would leave the old row deactivated with the new one missing.

Repro (source kept at `/tmp/audit-pass2/repro_memory_supersede.rs`; copy it to
`crates/harness-memory/tests/` to run — its `audit_repro_fk_ordering` test drives the same schema with
the engine's statement order vs the correct one):

    cd /home/janelle/Downloads/rust-agent-harness
    cargo test -p harness-memory --test zz_audit_repro_note_pointer -- --nocapture --test-threads=1

Observed:

    AUDIT_REPRO update_before_insert=Some("FOREIGN KEY constraint failed")
    AUDIT_REPRO update_after_insert=Ok(1)
    AUDIT_REPRO active_after_correct_order=1
    AUDIT_REPRO first_pointer=None
    AUDIT_REPRO second_pointer=Some("Memory engine error: FOREIGN KEY constraint failed")
    AUDIT_REPRO active_note_pointers=1
    AUDIT_REPRO active id=mem_ptr_note_1 raw=Note [ID: note_1]: Stripe Payment Gateway active=true superseded_by=None

Expected: the second save supersedes the first and returns `Ok`; `mem_ptr_note_1` becomes inactive and
`mem_ptr_note_2` is active. Observed: the second save fails and the first record is untouched.

Consequences: README:15 ("automatic contradiction detection and superseding mechanism") is false —
memories never supersede, every correction of a fact errors out. `update_memory` takes the same path,
so it always fails. `Agent::run_task` (`crates/harness-core/src/agent.rs:191-197`) registers a note
pointer after every successful `save_note`; from the second note on that call fails, is downgraded to
a `warn!`, and the tool still reports success to the model, so the agent believes it has registered an
awareness pointer it never wrote. Five such failures in a row also trip the 5-consecutive-failure
guardrail (`agent.rs:172-181`) and abort the turn.

Second symptom in the same block: `add_note_pointer` keys every pointer on the same
`subject='note' / predicate='stored_document'` pair (`engine.rs:260-275`), so even with the FK fixed,
each new note would deactivate the pointer for every previous note — the README promise that the agent
"retains awareness" (`README.md:19`) holds for at most one document.

### 2.2 `harness-evolver` applies unverified patches with no way back — card `t_26fb21a1`

`crates/harness-evolver/src/engine.rs:155` writes `patch.patched_content` over
`repo_root.join(target_file)` with `fs::write`, unconditionally: `apply_patch` (`:142-158`) never
inspects `patch.is_verified`, despite the trait contract it implements
(`crates/harness-core/src/traits.rs:95`: "Apply an approved and tested patch"). `StagedPatch` stores
`original_content` (`engine.rs:19`, read at `:47`, written at `:58`) and **nothing ever reads it**
(`grep -rn original_content crates/` → only those three hits), so there is no rollback path at all.
`propose_patch` (`:100-111`) treats any non-JSON payload as a whole-file body and targets the hardcoded
`crates/harness-core/src/lib.rs` (`:107`).

Repro (`/tmp/audit-pass2/repro_evolver_apply.rs`, copy to `crates/harness-evolver/tests/`):

    cargo test -p harness-evolver --test zz_audit_repro_apply -- --nocapture
    AUDIT_REPRO staged_patch_id=patch_21231c84-a272-4f6b-8993-bf8ddef7f920
    AUDIT_REPRO apply_without_verification=Ok(())
    AUDIT_REPRO file_after="THIS IS NOT RUST AND WAS NEVER TESTED\n"

Expected: `apply_patch` refuses a patch whose `is_verified` is false (or requires an approval token);
observed: it overwrites the file and reports `Ok`. The tool wrapper does verify first
(`crates/harness-evolver/src/tools.rs:155-158`), but a `cargo` that cannot be spawned is reported as
verified (`crates/harness-evolver/src/sandbox.rs:61-65`, already noted in `FEATURE_GAPS.md:54`), and
`CodebaseEvolver` is public API — any caller (including `harness-cli`) can clobber source with one
call.

### 2.3 `luna voice` is an unbounded loop with no cancellation and a canned query

`crates/harness-cli/src/commands/voice.rs:109-110` creates a fresh `CancellationToken` and passes it
straight into the loop; nothing ever cancels it, there is no signal handler, and
`crates/harness-voice/src/pipeline.rs:80` is `while !cancel.is_cancelled()`. The CLI's own adapters
make each iteration identical: wake word always succeeds after 60 s (`voice.rs:26-29`), STT returns the
hardcoded string `"Hello Luna, what are my persistent notes?"` (`voice.rs:37-39`), TTS returns 1024
zero samples (`voice.rs:50-53`). So `luna voice` re-sends the *same fabricated user query* to the paid
provider roughly once a minute, forever, until Ctrl-C — a quota/money leak with a user-visible facade
(`FEATURE_GAPS.md` §13 owns the rebuild, hence no card).

Repro is the code path itself; it is currently unreachable because the engines panic at startup
(`t_8935ed8a`): `voice.rs:77-78` call `SqliteMemoryEngine::new` / `SqliteNotesEngine::new` from an
async fn. Fix the panic first, then this loop is live.

### 2.4 Voice barge-in cannot happen

`crates/harness-voice/src/pipeline.rs:132-141` drains the TTS stream in a plain `while let` that never
polls the wake-word detector, and `interrupt_playback()` is only called at `:95`, *before* the turn
starts. The turn's own token (`:118-122`) is never cancelled by anything either. README:42
("Barge-In: instantly aborts assistant audio playback and in-flight turn when the user speaks
`"Luna"`") is therefore not implementable with this shape: the loop is single-threaded on the detector.

### 2.5 `harness-mcp` client: no id matching, no timeout, no shutdown handle

Verified working on the happy path against a real SDK server
(`from mcp.server.mcpserver import MCPServer`, mcp 2.2.0; fixture `/tmp/audit-pass2/fixture_server.py`):

    cargo test -p harness-mcp --test zz_audit_repro_stdio -- --nocapture
    AUDIT_REPRO spawn+initialize=Ok
    AUDIT_REPRO tools_list=Ok count=1
    AUDIT_REPRO tool=echo desc=Some("Echo the text back.")
    AUDIT_REPRO call_tool=Ok(echo:hello)
    AUDIT_REPRO pgrep_while_client_alive=[114389 python3 /tmp/audit-mcp-fixture/fixture_server.py]
    AUDIT_REPRO pgrep_after_client_drop=[]

So the missing `notifications/initialized` (MCP spec requires the client to send it after
`initialize`, `stdio.rs:100-115` never does) is tolerated by this SDK and is *not* a blocker. The real
weaknesses:

- `stdio.rs:78-88` reads exactly one line and assumes it is this request's reply; the JSON-RPC `id`
  (`types.rs:15`) is never compared and `send_request` never loops. Any interleaved server message
  (notifications are spec-legal) is consumed as the reply and silently degraded to
  `serde_json::Value::Null` at `stdio.rs:97`, after which `tools/list` fails with
  "Invalid tools/list result" (`:119-121`) or `call_tool` returns an empty string.
- No read timeout: `read_line` at `:82` can block a tool call forever. `call_tool` receives no
  cancellation signal to race it against (`crates/harness-mcp/src/adapter.rs:38` ignores
  `_cancel_token`).
- The child handle is dropped inside `spawn` — only stdin/stdout are taken (`stdio.rs:32-39`), the
  unused `Child` import and the unused `command` field are the compiler confirming it. Nothing can
  signal or kill the server. With this fixture the process exited on stdin EOF (pgrep empty above), so
  no orphan was observed; a server that ignores EOF would survive. `t_16a223b8` requirement 4 ("shut
  the child processes down on exit") needs a kill path added to `McpStdioClient` itself.
- `crates/harness-mcp` has zero tests (`cargo test -p harness-mcp` → 0 tests, lib and doc-tests only).

### 2.6 Timestamps are fabricated on every read path

`crates/harness-notes/src/engine.rs:125-126`, `:160-161`, `:193-194` build `Note` with
`created_at: Utc::now(), updated_at: Utc::now()` instead of parsing the stored columns, which are
selected by the same queries (`:107`, `:142`, `:178`). `crates/harness-memory/src/engine.rs:150-151`
does the same in the "recent memories" fill-up branch. Observed: `read_note` on a three-day-old note
reports `created_at = now`, while `search_notes` orders by the *stored* `updated_at` (`:145`) and
`retrieve` by stored `updated_at` (`:68`) — the value a user sees and the order they see it in come
from different sources. The `retrieve` branch is worse than cosmetic: the fill-up path is what injects
"recent memories" into the system prompt (`crates/harness-core/src/agent.rs:92-93`).

### 2.7 `forget` / `update` report success for ids that do not exist

`crates/harness-memory/src/engine.rs:250-254` and `:219-223` execute an `UPDATE … WHERE id = ?` and
return `Ok(())` regardless of `rows_affected` (never checked), then log "Deactivated memory"
(`:256`) / "Updated memory" (`:244`). The tools echo that to the model
(`crates/harness-memory/src/tools.rs:174-182`, `:224-231`): `forget_memory` with a mistyped id tells
the model the memory is gone while it is still active.

### 2.8 `search_notes` can silently answer a different question

`crates/harness-notes/src/engine.rs:104` strips only `"` from the query. FTS5 treats `*`, `(`, `)`,
`:`, `-`, `NEAR`, `AND`, `OR` as syntax: `MATCH` then errors, the error is swallowed at `:115-135`
(`if let Ok(...)`, `filter_map(Result::ok)`), and the LIKE fallback at `:138-169` answers with a
different ranking and different semantics for the *same* query. `%` and `_` are not escaped in either
engine's LIKE pattern (`:139`, `crates/harness-memory/src/engine.rs:60`), so `retrieve("%")` matches
every row. Also, results carry fabricated timestamps (§2.6) and `search_notes` never logs that the FTS
path failed.

### 2.9 Swallowed failures worth naming (code bar §5)

- `crates/harness-evolver/src/sandbox.rs:85-87` — `let _ = fs::create_dir_all` / `let _ = fs::copy`
  while building the verification tree: a partial copy silently becomes the tree that decides whether a
  patch is safe to apply.
- `crates/harness-plugins/src/skills.rs:35` — `walkdir … filter_map(|e| e.ok())`: unreadable skill
  files and permission errors disappear; the agent simply has fewer tools than the directory contains.
- `crates/harness-memory/src/engine.rs:181` and `crates/harness-notes/src/engine.rs:131`, `:166` —
  `filter_map(Result::ok)` over row mapping: a schema/type drift returns a shorter result instead of an
  error.
- `crates/harness-memory/src/engine.rs:24`, `crates/harness-notes/src/engine.rs:25` —
  `let _ = create_dir_all(parent)`; the following `Connection::open` fails with a bare
  `MemoryError`/`NotesError` string, losing the real cause.
- `crates/harness-telemetry/src/lib.rs:59`, `:65` — `.ok()` on `try_init`: a second `init_telemetry`
  call is indistinguishable from success.
- `crates/harness-evolver/src/sandbox.rs:61-65` — a `cargo` that cannot be spawned yields
  `Ok("Syntax inspection verified")`; already recorded in `FEATURE_GAPS.md:54`.

### 2.10 `verify_patch` rebuilds the whole workspace, uncached, every time

`crates/harness-evolver/src/sandbox.rs:19-44` copies the repo (minus `target/`, `.git`) into a fresh
`tempfile::tempdir()` and runs `cargo test --workspace` there. Two consequences: the sandbox build is
always cold — the copy has no `target/`, and since the fleet removed the shared `CARGO_TARGET_DIR`
nothing points it at a warm one (per-worktree `target/` is the rule; `docs/AGENT_RULES.md` §2) — so
each call is a full cold-workspace build, the tens-of-minutes case §2 budgets; and the temp dir is
deleted when `verify_patch` returns, so none of that work is ever reused. Run `apply_harness_update`
only when no other build is running (one build at a time per worker, §2).

### 2.11 README claims that do not match the code

| README | Reality |
| --- | --- |
| :13 "Hybrid relational SQLite + vector embeddings schema" | `memory_embeddings` (`crates/harness-memory/schema.sql:15-18`) is created and never read or written; `ModelProvider::embed` (`traits.rs:34`) has no callers. |
| :14 "delegated entirely to the model via native tools" | true, and the tools work — but see §2.1: the superseding they rely on fails. |
| :15 "Automatic contradiction detection and superseding (`is_active = 0`, `superseded_by`)" | broken; card `t_22f8ed85`. |
| :19 "note saves automatically register a concise 1–5 word pointer in persistent memory" | the pointer is distilled (`crates/harness-notes/src/distiller.rs:5`) but registered by `agent.rs:191-197`, outside the notes engine; every registration after the first fails (§2.1) and is only `warn!`ed. |
| :26 / :30 "Sandboxed Wasm plugins with memory and execution constraints" | `crates/harness-plugins/src/wasm.rs:49-65` executes nothing: it returns `{"status":"executed","memory_limit_mb":…,"echo":payload}`. `wasm_bytes` is never read (compiler warning). Not registered anywhere in the CLI (`FEATURE_GAPS.md` §12). |
| :39-42 voice (cpal wake word, VAD capture, rodio playback, barge-in) | stubs at `voice.rs:26-29/37-39/50-53`; `pipeline.rs:101-102` feeds a 1024-zero-byte buffer to STT; `:139` logs audio chunks instead of playing them; barge-in unreachable (§2.4). `cpal`/`rodio` are compiled in and unused. |
| :60 "token tracker" | `TokenTracker` (`crates/harness-telemetry/src/lib.rs:15-48`) has zero instantiations; `StreamEvent::Completed{usage}` is discarded in the agent loop; prices are literals (`:35`). (`FEATURE_GAPS.md` §14.) |
| :29 "MCP client implementation over Stdio and SSE" | stdio client exists and works on the happy path (§2.5); no SSE anywhere in the workspace (`grep -rn "sse" crates/` → 0 transport code), and nothing in `harness-cli` constructs the client (`FEATURE_GAPS.md` §2.5, card `t_16a223b8`). |

### 2.12 Dead weight the compiler already flagged

`harness-memory` 2 warnings (`engine.rs:1` unused `Path`, `db_path` never read), `harness-notes` 3
(`DateTime` unused, `warn` unused, `db_path` never read), `harness-mcp` 2 (`Child` unused,
`command` never read), `harness-plugins` 3 (`Arc`, `PathBuf` unused, `wasm_bytes` never read),
`harness-evolver` 1 (unused `Arc`). `harness-voice` and `harness-telemetry` build clean. Unused deps:
`similar` in `harness-evolver` (no diff exists despite the `diff` parameter at `engine.rs:100`),
`pulldown-cmark` in `harness-notes`, `opentelemetry` in `harness-telemetry`, `cpal`/`rodio` in
`harness-voice`, `reqwest`/`url`/`futures-util` in `harness-mcp` (stdio only).

---

## 3. Test status, by crate (observed today at `70d832c`)

    cargo test --no-fail-fast -p harness-memory -p harness-notes -p harness-mcp -p harness-plugins \
      -p harness-evolver -p harness-voice -p harness-telemetry -- --nocapture    # EXIT=101

| crate | result |
| --- | --- |
| `harness-memory` | lib unit tests: 0 tests. `tests/memory_superseding_test.rs`: **FAILED**, panics at `engine.rs:49` (`blocking_lock`) — known `t_8935ed8a`, not re-filed. This red test is also the one that would have caught §2.1: it never reaches the second `save`. |
| `harness-notes` | `distiller::tests::test_strict_distillation` **passes** (1 passed). `tests/note_pointer_test.rs`: **FAILED**, panics at `engine.rs:50` (`blocking_lock`) — known `t_8935ed8a`. |
| `harness-mcp` | **0 tests** (and no `tests/` dir). |
| `harness-plugins` | **0 tests**. |
| `harness-evolver` | **0 tests**. |
| `harness-voice` | **0 tests**. |
| `harness-telemetry` | **0 tests**. |

All seven crates compile (`cargo build -p harness-voice -p harness-telemetry -p harness-plugins` →
warnings only). Four of the seven have no test target at all, which is why every defect above had to be
reproduced with a scratch test rather than read off the suite.

## 4. Deliberately not re-filed

- `blocking_lock()` panic in both SQLite engines (`engine.rs:49`, `notes/engine.rs:50`) → `t_8935ed8a`.
- Credential/rotation/discovery/exit-code findings from pass 1 → `t_d79b9293`, `t_24375a7e`,
  `t_29ea5d7c`, `t_aed83347`, `t_d75d32db`.
- Wasm plugin facade, voice facade, dead token tracker, vector-memory dead end, MCP unwired, skills
  unwired → spec'd in `docs/FEATURE_GAPS.md` §12, §13, §14, §3.2, §2.5, §3.1; and the MCP wiring card
  `t_16a223b8` is already `ready` with the §2.5 evidence attached as a comment.
- MCP tools are dispatched with no approval gate → `t_c4f9c07d` ("Human-in-the-loop approval gate for
  mutating tools") covers the mutation surface.

# Verification pass — do the five landed handoffs hold on `main`? (`t_c9176d9f`)

Read-only verification by `luna-audit` at `be84886`, on `/home/janelle/Downloads/rust-agent-harness`.
No product file was modified. The binary was **rebuilt from this tree** (`CARGO_TARGET_DIR` unset;
`rm -f target/release/luna && CARGO_BUILD_JOBS=2 cargo build --release` → `Finished … in 1m 43s`,
exit 0) rather than trusting an artifact left by another worker. Every verdict below is a command and
its observed output; the claim table with the evidence lines lives in the card `t_c9176d9f` comment.

## 5. Verdicts

| Card | Claim | Verdict |
| --- | --- | --- |
| `t_0a789e15` | `cargo build --release` exit 0 | **TRUE** |
| `t_0a789e15` | `target/release/luna --help` prints the command surface | **TRUE** (artifact-path deviation disclosed in the handoff: at the time it was the shared-`CARGO_TARGET_DIR` binary; the literal path builds and prints the surface on `main` today) |
| `t_0a789e15` | `cargo test --workspace` green on 12 targets | **TRUE** |
| `t_0a789e15` | two named tests still red | **TRUE when written, superseded on `main`** — both are green since `e5ae82c`; the pre-fix source at `7188aa0` does carry `blocking_lock()` at `memory/engine.rs:49` / `notes/engine.rs:50` |
| `t_8935ed8a` | `blocking_lock()` gone from `crates/` | **TRUE** |
| `t_8935ed8a` | `cargo test -p harness-memory -p harness-notes` → 2 passed | **TRUE** (rc=0) |
| `t_8935ed8a` | live NIM round-trip through `luna chat` reached the prompt | **TRUE** (one call reproduced) |
| `t_8935ed8a` | `chat.rs`/`voice.rs` needed no edits | **TRUE** (`git show --stat e5ae82c` = the two engine files only) |
| `t_d79b9293` | 401/429 retirement works and disables the *failing* key | **TRUE** — 4 shipped tests pass, plus an independent RoundRobin/3-key witness the shipped tests do not cover |
| `t_d79b9293` | "all 4 new tests fail against the pre-fix code" | **TRUE by source inspection** (`836b5e6^` ignores `_api_key`, acts only under `FillFirst`, disables the first non-disabled entry), not re-executed — reverting product sources is outside this card's lane |
| `t_5c1eca76` | precedence defaults → file → env → flags | **TRUE** (flag layer witnessed on the `luna chat` banner, not just in unit tests) |
| `t_5c1eca76` | `config get/list/set` with source reporting | **TRUE** (41 keys; PTY prompt for a missing value) |
| `t_5c1eca76` | no-config behaviour identical to before | **TRUE** (`Provider: openai / Model: gpt-4o`, and each default equals the pre-change literal at `a36fee3^`) |
| `t_5c1eca76` | YAML, no new dependency | **TRUE** (`Cargo.lock`: `toml` 0 hits, `serde_yaml` 1) |

The author's own acceptance script (`t_5c1eca76` attachment) was re-run against the freshly built
binary: **14/14 PASS**. Nothing in the five handoffs was false. The one guess that does not survive
today is `t_0a789e15`'s "still red" pair, and it is not a false claim: it was true at `7188aa0` and the
later card `t_8935ed8a` fixed both.

## 6. Could not test, and what it would take

- **`t_d79b9293`'s "fails on pre-fix code" proof** — re-running it needs the four new tests compiled
  against the pre-fix `auth.rs`/`client.rs`. That means a source revert, which is out of this card's
  read-only lane; done by source inspection instead. A worker with a worktree can do it with
  `git stash`-free temporary edits on a branch.
- **`t_0a789e15`'s original `target/release/luna`** — the artifact the handoff's acceptance named was
  never produced at that commit (the worker used the shared target dir and said so). Retroactively
  unverifiable; substituted with a rebuild on `main`.

## 7. Filed as cards from this pass (2, the cap)

| Card | Defect | Where |
| --- | --- | --- |
| `t_e4acbdf6` | `luna chat` has no EOF branch: `read_line` → `Ok(0)` is treated as an empty line, so Ctrl-D wedges the REPL on a real tty and piped/`</dev/null` stdin busy-loops writing `User > ` forever (5.4 MB and ~99% of a core in 5 s; `rc=124`) | `crates/harness-cli/src/commands/chat.rs:74-100` |
| `t_8a84e855` | any CLI write to a closed stdout panics: `luna config list \| head -3` prints `failed printing to stdout: Broken pipe (os error 32)` and exits **101** (std sets `SIGPIPE` to `SIG_IGN`, nothing resets it) | `crates/harness-cli/src/main.rs` (one place) / every `println!` |

## 8. Verified but not filed (notes for the next pass)

- `.worktrees/t_2ccd2241/` is a stale sibling branch: it still contains both `blocking_lock()` calls
  (`crates/harness-memory/src/engine.rs:49`, `crates/harness-notes/src/engine.rs:50`) because it has not
  merged `e5ae82c`. Correct for that branch, wrong if it ever lands as-is; the `crates/` tree on `main`
  is clean.
- `crates/harness-providers/src/discovery.rs:158-165` maps *every* non-2xx from model discovery to
  `AuthenticationFailed` — a 404 (wrong `base_url`/`models_endpoint`) is reported as an auth problem.
  Self-reported in the `t_5c1eca76` handoff; read-only confirmed, message is distinguishable only by the
  embedded `HTTP 404` text. Cosmetic-ish, so it stays here rather than taking one of the two card slots.
- `t_5c1eca76` handoff's other two "flagged not fixed" items reproduce as described: `config set`
  rewrites the YAML value tree (comments and key order lost — documented in the module docs), and
  `luna config list \| head` is the SIGPIPE case above, now carded.
- `kanban_create`'s `project` parameter did not resolve in this session for either new card
  (`project_id` came back `NULL` and `workspace_path` stayed at the shared repo root, i.e. a worker
  would have been handed the shared checkout). Both rows were repaired to the sibling shape
  (`project_id = p_f47ecaea`, `.worktrees/<task-id>`, `branch_name = luna-harness/<task-id>-<slug>`)
  and re-read to confirm. Worth a tool-side fix: nothing in the worker-facing API can anchor a card
  after creation.
