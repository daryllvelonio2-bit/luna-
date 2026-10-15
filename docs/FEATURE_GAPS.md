# Feature gaps: Shiina CLI capabilities → implementable Luna specs

Scope: the `luna` harness (this repo, 10 crates) measured against the Shiina CLI checkout at
`/home/janelle/Documents/GitHub/shiina-cli` (read-only reference) and public 2026 harness practice.

Method and evidence rules
- Every Shiina cell is a real `file:line` from that checkout, found with `grep -n` during this run.
- Every Luna cell is a real `file:line` in this repo, read in full before it was written down.
- `0 refs` means `grep -rn <symbol> crates/harness-cli/src` returned no hits: declared as a
  dependency of the binary, never wired into it.
- Shiina code is not reproduced here; only entry points and what they do.
- "Absent" means no such code path exists in the workspace, not "unfinished".

Reading the table
`present` = exists and is reachable from the `luna` binary. `partial` = the hard part exists but
nothing calls it, or the call site is a stub. `absent` = nothing to improve.
`verdict`: **rebuild** (write it, nothing usable to extend), **improve** (extend what is there),
**drop** (do not port this; state the reason).

---

## 1. Capability table, ordered by what unblocks the most

| # | Capability | What Shiina does (entry point) | Luna today | Verdict | Spec |
|---|-----------|-------------------------------|-----------|---------|------|
| 1 | Config file + precedence | `shiina_cli/config.py:2070` `load_config()`; documented keys in `cli-config.yaml.example:1`; `shiina config set <section.key>` (`cli-config.yaml.example:3`) | **absent** — no config file, no keys, no merged settings object. Every path/model/timeout is a literal at its use site: `crates/harness-memory/src/engine.rs:20`, `crates/harness-notes/src/engine.rs:21`, `crates/harness-providers/src/auth.rs:60`, `crates/harness-providers/src/discovery.rs:35`, `crates/harness-cli/src/main.rs:28` (`default_value = "openai"`). Format ruling for Luna: **YAML at `~/.luna/config.yaml`** — `serde_yaml` is already a workspace dependency (`Cargo.toml:28`), `toml` is not | rebuild | §2.1 |
| 2 | Filesystem + shell tools | `tools/file_tools.py:1089` `read_file`, `:1111` `write_file`, `:905` `patch_tool`, `:990` `search_tool`; `tools/terminal_tool.py:1311` `terminal` | **absent** — the registry never receives anything but memory/notes/evolver tools: `crates/harness-cli/src/commands/chat.rs:44-57`. No fs or shell crate exists; the only `Tool` impls reachable from the binary are the 10 memory/notes/evolver tools (`crates/harness-core/src/traits.rs:100` is the trait; the adapters in `harness-mcp`/`harness-plugins` are registered nowhere) | rebuild | §2.2 |
| 3 | HITL approval for mutations | `tools/write_approval.py:166` `evaluate_gate`, `:73` `stage_write`; `tools/approval_human_wait.py:108` `human_wait_window`; `acp_adapter/permissions.py:77` `await_permission` | **partial, dead** — `AgentConfig.require_hitl_for_mutation` declared at `crates/harness-core/src/agent.rs:12`, defaults `true` at `:20`, read nowhere else in the workspace (only set `false` in `crates/harness-core/tests/agent_guardrail_test.rs:165`); `HarnessError::ApprovalRejected` at `crates/harness-core/src/error.rs:66` has no producer | rebuild | §2.3 |
| 4 | Session persistence + context compaction | `shiina_state.py:441` `SessionDB`; `agent/conversation_compression.py:3681` `compress_context`; `shiina_state_compression.py:70` `SessionCompressionMixin`; `shiina_state_rewind.py:45` `SessionRewindMixin` | **absent** — `run_task` builds a fresh `Vec::new()` per call (`crates/harness-core/src/agent.rs:89`) and the REPL re-enters it per input line (`crates/harness-cli/src/commands/chat.rs:88`); nothing is written anywhere; no token estimate, no compaction, no session id | rebuild | §2.4 |
| 5 | MCP servers from config | `tools/mcp_tool.py:307` `MCPServerTask`; `tools/mcp_tool_config.py:168` `_resolve_stdio_command`; 30+ server recipes under `optional-mcps/`; `mcp_serve.py:264` `EventBridge` (Shiina as a server) | **partial, unwired** — `crates/harness-mcp/src/stdio.rs:21` `spawn` and `:100` `initialize` are implemented, `crates/harness-cli/Cargo.toml:23` depends on `harness-mcp`, and `crates/harness-cli/src` has **0 refs** to it. No config, no CLI command, no SSE transport | improve (wire it) | §2.5 |
| 6 | Declarative markdown skills | `tools/skills_tool.py:653` `skills_list`, `:668` `skill_view`; `tools/skill_manager_tool.py:854` `skill_manage`; `agent/skill_preprocessing.py:89` `preprocess_skill_content`; 14 categories under `skills/` | **partial, unwired** — `crates/harness-plugins/src/skills.rs:29` `load_skills_from_dir` (+`:81` `SkillToolAdapter`) parse frontmatter, but `harness-cli/src` has **0 refs**; no skills dir is ever resolved | improve | §3.1 |
| 7 | Semantic (vector) memory retrieval | `agent/memory_manager.py:283` `MemoryManager`; `tools/memory_tool.py:172` `memory(action=…)`; file store `~/.shiina/memories/MEMORY.md` | **partial, dead** — `memory_embeddings` table is created (`crates/harness-memory/schema.sql:15`) and never written or read; `retrieve()` is `LIKE`-only (`crates/harness-memory/src/engine.rs:58`); `ModelProvider::embed` (`crates/harness-core/src/traits.rs:34`, impl `crates/harness-providers/src/client.rs:355`) has 0 callers | improve | §3.2 |
| 8 | Streaming output in the REPL | `gateway/stream_dispatch.py:21` `GatewayEventDispatcher`; `tui_gateway/render.py:35` `make_stream_renderer` | **partial, unwired** — `ModelProvider::stream` implemented at `crates/harness-providers/src/client.rs:266` (SSE + usage), but the agent loop calls `complete` (`crates/harness-core/src/agent.rs:133`) and the REPL prints one blob after the await (`crates/harness-cli/src/commands/chat.rs:88-91`). No `reasoning_content` parsing (`crates/harness-providers/src/client.rs:240` reads `content` only) — the harness model under test hides its thinking there (`docs/AGENT_RULES.md:59`) | rebuild | §3.3 |
| 9 | Web search / fetch / browser tools | `tools/web_tools.py:268` `web_search_tool`; `tools/browser_tool.py:460-580` `browser_navigate`/`browser_snapshot`/`browser_click`/… | **absent** — no HTTP tool of any kind; `reqwest` appears only inside `harness-providers` | rebuild | §3.4 |
| 10 | Cron / scheduled jobs | `cron/scheduler_tick.py:7` `tick`; `cron/jobs.py:1749` `create_job`; `cron/scheduler.py` (delivery queue, incidents, prompt-injection block at `:332`) | **absent** | rebuild (defer) | §3.5 |
| 11 | Messaging gateway (Telegram etc.) | `gateway/` (120 modules), `shiina_state_telegram.py:98` `SessionTelegramTopicsMixin`, `shiina_cli/telegram_managed_bot.py` | **absent** — CLI-only, no gateway crate | drop for now | — |
| 12 | Sandboxed wasm plugins | `plugins/plugin_loader.py:92` `load_plugin_module` (python plugin dirs), `plugin-catalog/` recipes | **facade** — `crates/harness-plugins/src/wasm.rs:57` returns `{"status":"executed","echo":payload}`; no wasm runtime in `Cargo.toml:23-54`; `SandboxedPlugin` registered nowhere | improve | §3.6 |
| 13 | Voice: wake word / STT / TTS | `tools/wake_word.py:178` `wake_phrase` (onnx/tflite framework resolution `:85`), `tools/tts_streaming.py:99` `StreamingTTSProvider`, `tools/transcription_audio.py:116` `_prepare_audio_for_transcription` | **facade** — `DefaultWakeWordDetector` sleeps 60 s (`crates/harness-cli/src/commands/voice.rs:26-29`), `DefaultSpeechToText` returns a hardcoded string (`:37-39`), `DefaultTextToSpeech` emits zero samples (`:50-53`); `cpal`/`rodio` sit unused in `Cargo.toml:47-48` | improve | §3.7 |
| 14 | Token / cost telemetry | `shiina_state_usage.py:82` `SessionUsageMixin`; `agent/credits_tracker.py` | **dead** — `TokenTracker` (`crates/harness-telemetry/src/lib.rs:15`) has 0 instantiations; `init_telemetry` only wires `tracing` (`crates/harness-cli/src/main.rs:60`); `StreamEvent::Completed.usage` is discarded | improve | §3.8 |
| 15 | Subagent delegation | `agent/subagent_lifecycle.py:49` `SubagentLaunchRequest`; `tools/delegate_tool_dispatch.py:28` (parallel batch execution) | **absent** — no delegate tool, no child-agent registry | rebuild (defer) | §3.9 |
| 16 | Session rewind / undo | `shiina_state_rewind.py:45` `SessionRewindMixin`, `:21` `RewindOutcome` | **absent** — and nothing to rewind until §2.4 lands | defer (blocked by §2.4) | — |
| 17 | Credential hardening (OS keyring) | `shiina_cli/vault.py:213` `vault_command` (encrypted, origin-bound) | **insecure** — API keys are plaintext in `~/.luna/credentials.json` (`crates/harness-providers/src/auth.rs:33` stores `api_key: String`, `:89` writes it verbatim); `keyring` is declared in `Cargo.toml:40` and unused | improve (security) | §3.10 |
| 18 | Self-evolution restart loop | equivalent concept, different shape (`cron` + gateway restart supervision) | **partial, unreachable** — the supervisor rebuilds only on child exit code 42 (`crates/harness-cli/src/commands/supervisor.rs:34`), and nothing in the workspace can produce it (`grep -rn "exit(42)\|process::exit" crates` → 0 hits); patches are whole-file writes with no diff (`crates/harness-evolver/src/engine.rs:100`, `:142`) against a sandbox that copies the tree and runs `cargo test` (`crates/harness-evolver/src/sandbox.rs:19`) | improve | §3.11 |
| 19 | Provider resilience (429/5xx retry) | `agent/rate_limit_tracker.py` | **absent** — a 429 is converted straight into `HarnessError::RateLimited` (`crates/harness-providers/src/client.rs:206`), no retry/backoff; NIM is documented as rate-limited (`docs/AGENT_RULES.md:63`) | improve | §3.12 |
| 20 | CLI surface: sessions/memory/mcp/skills/doctor | `shiina_cli/main.py` top-level subcommands, dashboard in `shiina_cli/main_dashboard.py` | **5 subcommands only** — `chat`, `auth`, `models`, `voice`, `supervisor` (`crates/harness-cli/src/main.rs:25-55`); no way to inspect memory, notes, sessions, tools or config; `luna --help` is not verified by any test | rebuild | §3.13 |

### Code-bar defects observed while reading (not capabilities, but blocking quality)

- `crates/harness-providers/src/client.rs:46` and `crates/harness-providers/src/discovery.rs:41` — `.build().unwrap()` on a fallible client build (AGENT_RULES §5 forbids unwrap on network/IO).
- `crates/harness-providers/src/discovery.rs:53-59` — `save_cache` swallows `create_dir_all` and `write` failures with `let _ =`; a silent cache miss is misread as "provider has no models".
- `crates/harness-providers/src/client.rs:363` — embedding model `text-embedding-3-small` hardcoded, and it is wrong for any provider in the catalog except OpenAI (`crates/harness-providers/src/catalog.rs:20-92`).
- `crates/harness-memory/src/engine.rs:112-116` and `:158-164` — rows that fail to map are dropped with `if let Ok(...)`, so a schema drift silently returns partial memory instead of an error.
- `crates/harness-notes/src/distiller.rs:5` — the "1-5 word distillation" is a first-five-words truncation of the *title*; `format_as_system_prompt` (`crates/harness-memory/src/engine.rs:277`) then injects it as if it were a summary.
- `crates/harness-evolver/src/sandbox.rs:61-65` — when `cargo` cannot be spawned the sandbox returns `Ok("Syntax inspection verified")`, i.e. an unverified patch can be reported as verified.

---

## 2. Specs — the five that unblock the most

Each spec is startable as written: behaviour, config keys, acceptance criteria that can be asserted,
a test plan with the real command that exercises it, and the owning lane.

### §2.1 Config file (`~/.luna/config.yaml`), precedence, and a `luna config` command
Lane: **luna-dev-1** (providers / CLI / agent core). Priority: highest — every other spec below adds
config keys, and there is nowhere to put them today.

User-visible behaviour
- `luna` reads `~/.luna/config.yaml` — **YAML**, because `serde_yaml` is already a workspace dependency
  (`Cargo.toml:28`) and `toml` is not; the path is overridable by `LUNA_CONFIG`, and a missing file is not
  an error.
- Precedence, lowest to highest: built-in defaults → config file → `LUNA_*` environment variables →
  CLI flags. The resolved value and which layer produced it are both printable.
- `luna config get <dotted.key> [--show-source]` prints the value; `luna config list` prints every
  effective key; `luna config set <dotted.key> <value>` writes the user file; unknown key on `get`/`set`
  is a clear error listing the nearest known key (no silent typo acceptance).
- Behaviour with no config file is byte-identical to today: the defaults resolve to the same paths,
  provider and model that are currently hardcoded.

Config keys (v1)
- `model.provider` (default `openai`), `model.default` (default `gpt-4o`)
- `paths.memory_db` (`~/.luna/memory.db`), `paths.notes_db` (`~/.luna/notes.db`),
  `paths.credentials` (`~/.luna/credentials.json`), `paths.models_cache` (`~/.luna/models_cache.json`)
- `providers.<id>.base_url`, `.models_endpoint`, `.timeout_secs` (override the catalog at
  `crates/harness-providers/src/catalog.rs:20`)
- `agent.max_consecutive_failures` (default 5, today the literal at `crates/harness-core/src/agent.rs:19`),
  `agent.require_hitl_for_mutation` (default true)
- `telemetry.json` (default false), `telemetry.level`
- `paths.workspace_root` (default process cwd — the scope every filesystem tool in §2.2 is confined to)

Implementation shape (deliberately small)
One module, `config` in `harness-core`, deserialising that YAML with `serde_yaml` into a `Config` struct
with `serde` defaults, and
one `resolve` function implementing the precedence order. Engines keep their `new(Option<PathBuf>)`
signatures; the CLI passes resolved values in instead of letting each engine guess `~/.luna/…`. No new
crate, no trait, no plugin layer.

Acceptance criteria (testable)
1. With no `~/.luna/config.yaml` present, `luna config get paths.memory_db` prints `$HOME/.luna/memory.db`.
2. With `LUNA_MODEL_DEFAULT=x` in the environment and `model.default = "y"` in the file,
   `luna config get model.default --show-source` prints `x` and source `env`.
3. `luna config get model.nope` exits non-zero and its stderr names `model.default`.
4. `luna chat --help` shows the default provider from config, not a hardcoded literal (the
   `default_value` at `crates/harness-cli/src/main.rs:28` is gone).
5. A malformed YAML file fails fast with the parse error and line number; it does not silently fall back.

Test plan
- Unit (`harness-core`, module tests): precedence matrix — file beats default, env beats file, flag beats
  env; missing file; malformed file; unknown key; round-trip of `config set` → `load`.
- Unit: defaults equal today's literals (assert each path in one test) so the refactor is provably
  behaviour-preserving.
- E2E, non-interactive:
  `HOME=/tmp/lunahome cargo run --bin luna -- config get paths.memory_db --show-source`
  `HOME=/tmp/lunahome LUNA_MODEL_DEFAULT=nvidia/x cargo run --bin luna -- config get model.default --show-source`
  `HOME=/tmp/lunahome cargo run --bin luna -- config set agent.max_consecutive_failures 3 && HOME=/tmp/lunahome cargo run --bin luna -- config get agent.max_consecutive_failures`
- E2E, interactive (realterm MCP PTY): `luna config set` with no value must prompt rather than write a
  broken file — assert the rendered prompt with `term_read`.

### §2.2 Filesystem and shell tool suite
Lane: **luna-dev-1**. Priority: high — the agent currently cannot touch the machine, so every
harness-level capability (evolve, note, patch) is unreachable from a conversation.

User-visible behaviour
- Five new tools registered into the same `DefaultToolRegistry` the REPL builds today:
  `read_file(path, offset, limit)`, `write_file(path, content)`, `patch(path, old_string, new_string,
  replace_all)`, `search_files(pattern, path, file_glob, output_mode)`, `terminal(command, timeout_s,
  workdir)`.
- Every path is resolved against `paths.workspace_root` from §2.1; escapes (`..`, absolute paths outside
  the root, symlinks out) are refused with an explanatory `ToolResult::error`, never silently clamped.
- `read_file` truncates at a configurable character budget (`tools.read_max_chars`, default 100_000) and
  says so in its result, rather than dumping a whole file into the transcript.
- `write_file`/`patch` refuse to overwrite a file whose content changed since the last `read_file` in the
  same session (read-before-write), because a stale overwrite is data loss.
- `terminal` runs foreground with a hard timeout (`tools.terminal_timeout_secs`, default 120) and a
  bounded output capture; a non-zero exit is a normal `ToolResult` with `is_error = true` carrying the
  exit code plus stderr, not a harness abort.
- All five go through the approval gate in §2.3 when `agent.require_hitl_for_mutation` is true.

Config keys
`paths.workspace_root`, `tools.read_max_chars`, `tools.terminal_timeout_secs`,
`tools.terminal_output_max_bytes`, `tools.enabled` (list of tool names; default all five).

Acceptance criteria (testable)
1. After startup, `list_tools()` contains exactly the pre-existing memory/notes/evolver tools plus these
   five (assert by name in a test, not by eyeballing a chat).
2. In a real session, "list the files in crates/" produces a `search_files` or `terminal` tool call and a
   listing that matches what the shell shows.
3. `write_file` to `../../etc/hosts` returns an error result and the file is untouched.
4. `read_file` on a synthetic 5 MB file returns a truncated body with the truncation notice.
5. `terminal("false")` yields `is_error = true`, the turn continues, and one failure does not halt the
   agent (the 5-in-a-row guardrail at `crates/harness-core/src/agent.rs:172` still governs).
6. `write_file` after the file changed underneath returns a refusal, not a clobber (test: write, mutate
   out-of-band, retry).

Test plan
- Unit, new module `tools` in `harness-core` (or a sibling `harness-tools` crate only if `harness-core`
  would cross 800 lines — AGENT_RULES §5): one test per tool against `tempfile::tempdir()`, including
  escape attempts, truncation, stale-write refusal, timeout on `terminal("sleep 300")`, and a
  command that writes stderr and exits 3.
- Integration: register all five on `DefaultToolRegistry`, assert `list_tools()` names and that
  `execute()` returns `ToolResult` (never `Err`) for a failing command.
- E2E (realterm MCP PTY, real turn, one NIM call — AGENT_RULES §3 “one real round-trip per card”):
  `cargo build --release && target/release/luna chat --provider nvidia --model deepseek-ai/deepseek-v4.1-flash`
  then prompt `create scratch/hello.txt with the text hi, then read it back`; verify with
  `ls -l scratch/hello.txt && cat scratch/hello.txt` in a plain shell.

### §2.3 Human-in-the-loop approval gate for mutating tools
Lane: **luna-dev-1**. Priority: high, and it must land *with or before* §2.2 — shipping shell and write
tools without a gate is the one irreversible mistake available in this repo. It is also the config flag
that is already declared and lying: `crates/harness-core/src/agent.rs:12`.

User-visible behaviour
- Each tool declares whether it mutates (`Tool::is_mutating()`, default false; `write_file`, `patch`,
  `terminal`, `apply_harness_update` return true).
- With `agent.require_hitl_for_mutation = true` (default), a mutating call is shown to the human before it
  runs: tool name, target path or exact command, and for `patch` a unified diff rendered with the
  `similar` crate already in the manifest (`Cargo.toml:51`).
- The prompt is a single keypress-style choice: run / edit-and-run / deny (dialoguer is already a
  dependency, `Cargo.toml:44`). No tty (stdin not a terminal) means deny, unless
  `tools.approval = "allow"` or `--yes` was passed.
- Denial is a normal tool result the model can react to: `ToolResult::error(call_id, "approval denied")`,
  and `HarnessError::ApprovalRejected` (`crates/harness-core/src/error.rs:66`) finally has a producer for
  the non-interactive path.
- No answer within `tools.approval_timeout_secs` (default 120) counts as deny.
- The decision and the tool call are logged (tracing) with the tool name and target, never with file
  contents or secrets.

Config keys
`agent.require_hitl_for_mutation` (default true), `tools.approval` (`ask` | `allow` | `deny`, default
`ask`), `tools.approval_timeout_secs` (default 120), `tools.mutating_overrides` (list of tool names
forced mutating).

Acceptance criteria (testable)
1. With approval denied at the prompt, the target file does not exist afterwards (`ls` confirms).
2. With approval accepted, the same call writes the file and the tool result is a success.
3. With `agent.require_hitl_for_mutation = false` in config, no prompt appears and the write lands —
   proving the flag is actually read (today it is read nowhere, `crates/harness-core/src/agent.rs:12`).
4. Non-interactive run (`echo "write x to y" | luna chat`, no tty) refuses the mutation and reports
   `approval denied` in the turn output; it does not hang.
5. Timeout test: a fake approver that never answers produces a deny at the configured timeout.
6. Read-only tools never prompt (assert the prompt count is 0 across a turn that only reads).

Test plan
- Unit, `harness-core`: the gate is a `dyn Approver` (closure) injected into `Agent`, mirroring the fake
  provider already used in `crates/harness-core/tests/agent_guardrail_test.rs`; a `RecordingTool` with
  `is_mutating = true` asserts `execute` ran exactly once on allow, zero times on deny, zero on timeout;
  assert `require_hitl_for_mutation = false` short-circuits.
- Unit: `is_mutating` defaults — a read-only tool prompts zero times, the 5-failure guardrail test still
  passes unchanged.
- E2E (realterm MCP PTY): `target/release/luna chat --provider nvidia --model deepseek-ai/deepseek-v4.1-flash`,
  prompt a write, answer `n` with `term_write`, then `term_read` the rendered prompt and confirm with
  `ls` that nothing was written; repeat with `y` and confirm the file exists. Quote both screens in the
  handoff.

### §2.4 Session persistence and bounded context (compaction)
Lane: **luna-dev-2** (filed to dev-1 by this spec, reassigned by the lead — session storage is not dev-1's
lane). Priority: high — without it the harness is a stateless one-shot: `run_task` throws
its own transcript away every call (`crates/harness-core/src/agent.rs:89`) and the REPL sends only the
current line (`crates/harness-cli/src/commands/chat.rs:88`). Multi-turn work, and everything downstream
that assumes a transcript (rewind, titles, search), is impossible until this lands.

User-visible behaviour
- Every turn of `luna chat` is appended to `~/.luna/sessions.db` (sqlite, WAL) under a session id printed
  at startup. `luna sessions list` shows id, created/updated, message count and the first user line;
  `luna chat --session <id>` resumes one; `luna chat --continue` resumes the most recent.
- A resumed session replays its recent transcript to the provider, so "what did I just ask you?" works
  across process restarts.
- History sent to the provider is bounded. When the estimated transcript size exceeds
  `agent.compact_at_tokens` (default 60_000), the oldest turns are summarised in one model call into a
  single synthetic `system`-role summary turn, the last `agent.keep_recent_turns` (default 8) turns stay
  verbatim, and the console prints one line: `compacted 42 turns into 1 summary (session s1)`.
- Compaction failure is non-fatal: the transcript is unchanged, a warning is logged, the turn proceeds on
  the full history, and the attempt is not retried in the same turn (no loop).
- Token estimate uses provider-reported usage when available (`StreamEvent::Completed.usage` /
  response usage) and a documented character heuristic otherwise; the same estimate is what the
  compaction threshold compares against. No silent divergence between the two.

Config keys
`paths.sessions_db` (default `~/.luna/sessions.db`), `session.resume_max_messages` (bound on replay,
default 200), `agent.compact_at_tokens` (default 60_000), `agent.keep_recent_turns` (default 8),
`agent.compaction_model` (default = `model.default`).

Storage shape (minimum that satisfies the acceptance criteria)
One table `sessions(id, created_at, updated_at, title)` and one `messages(id, session_id, seq, role,
content, tool_calls_json, tool_result_json, is_summary, created_at)`. Compaction writes a summary row and
leaves the original rows in place with a `compacted_at` column set, so nothing is destroyed and a later
rewind spec has raw material.

Acceptance criteria (testable)
1. Run two turns with `--session s1`, `exit`, then `luna sessions list` → one row, message count 4.
2. `luna chat --session s1` then "what did I just say?" answers about the previous turn (real NIM
   round-trip), proving replay.
3. `sqlite3 ~/.luna/sessions.db "select count(*) from messages"` equals the number of turns executed plus
   tool messages — persisted, not reconstructed.
4. With `agent.compact_at_tokens = 2000` and a 10-turn session, exactly one compaction occurs, the console
   shows the compaction line, and the next answer is still coherent (no lost recent turn).
5. With the provider forced to fail during compaction (`agent.compaction_model` pointed at a provider with
   no credentials), the session continues, the DB still holds every original message, and no second
   compaction is attempted in that turn.
6. A resumed session with more than `session.resume_max_messages` messages replays at most that many, and
   says so.

Test plan
- Unit, `harness-core`: `SessionStore` trait with a sqlite impl; tests for append/read/seq ordering,
  resume bound, and summary insertion. `compact()` tested against a fake provider that returns a canned
  summary — assert the message vector passed to the next `complete()` call is bounded and starts with the
  summary, and that a failing provider leaves the vector untouched.
- Unit: token estimator — usage-reported path and heuristic path agree within a documented tolerance on a
  fixed sample.
- E2E: two realterm PTY runs (`luna chat --session s1`, then `luna chat --session s1` again) with the NIM
  round-trip, plus `sqlite3` row counts and `luna sessions list` output quoted in the handoff.
- E2E compaction: one PTY run with `agent.compact_at_tokens = 2000`, ten short turns, capture the render.

### §2.5 MCP servers from config, wired into the registry
Lane: **luna-dev-2** (MCP). Priority: high value per line of code — the client already speaks the
protocol (`crates/harness-mcp/src/stdio.rs:21`, `:100`, `:117`, `:132`); it is simply not reachable from
the binary (`crates/harness-cli/src` has 0 refs) and has no configuration surface.

User-visible behaviour
- `mcp.servers.<name>` in config (from §2.1) with `command`, `args`, `env`, `enabled` (default true),
  `startup_timeout_secs`. On REPL start every enabled server is spawned, `initialize`d, and its
  `tools/list` result is registered into the same registry as the native tools, each under
  `<server>__<tool>` so two servers cannot collide.
- `luna mcp list` prints each configured server, its status (`ready` / `failed: <reason>`), and its tool
  count; `luna mcp tools <server>` prints names and descriptions. Neither command requires the model.
- A server that fails to start is logged as a warning and skipped: startup never blocks, the REPL still
  comes up, and the failure text is visible in `luna mcp list`.
- Server stdout is the protocol channel, so server stderr goes to `~/.luna/mcp/<name>.stderr.log` (never
  to the REPL screen); the log path is printed by `luna mcp list`.
- An MCP tool call error becomes `ToolResult::error` with the server's message, so the model sees it and
  the turn continues (matching `crates/harness-mcp/src/adapter.rs:42`).
- Requests have a configurable timeout (`mcp.request_timeout_secs`, default 60) and a timed-out call
  returns an error result rather than hanging the turn forever.

Config keys
`mcp.servers.<name>.command`, `.args[]`, `.env{}`, `.enabled`, `.startup_timeout_secs`,
`mcp.request_timeout_secs`, `mcp.enabled` (master switch, default true), `mcp.log_dir`.
Out of scope for this card (declare it in the handoff): SSE/HTTP transport — stdio only. The current
`harness-mcp` has no streaming-reader, so a long-running tool call with progress notifications will time
out rather than stream; file a follow-up card if that is needed.

Acceptance criteria (testable)
1. With one configured server (a fixture script committed under `crates/harness-mcp/tests/fixtures/`),
   `luna mcp list` prints the server name, `ready`, and a tool count ≥ 1.
2. `list_tools()` from a test contains `fixture__echo` alongside the native tool names.
3. A real turn in `chat` calls the fixture tool and the model's answer contains the echoed marker string,
   proving the round-trip through the registry.
4. A server configured with a nonexistent command logs a warning, `luna mcp list` shows
   `failed: <reason>`, and the REPL still starts and answers a turn.
5. A tool that returns an MCP error yields `is_error = true` and the turn continues to a final answer.
6. With `mcp.enabled = false`, no process is spawned (assert via `pgrep`/`term_list` and `luna mcp list`
   output).

Test plan
- Unit (`harness-mcp`): spawn the fixture server → `initialize` → `tools/list` → `tools/call` for a happy
  path and an error path; assert `call_tool` surfaces the error text; assert a timeout returns an error.
  Add the fixture as a tiny stdio JSON-RPC echo server script (committed, no network).
- Unit (`harness-cli` or wherever registration lives): config → registry mapping, name prefixing, disabled
  server skipped, duplicate tool names rejected.
- E2E: `target/release/luna mcp list` and a realterm PTY chat turn invoking `fixture__echo` with the NIM
  model; quote the rendered tool call and the final answer.

---

## 3. Queued specs (next tier — table rows only, no card filed)

Short-form specs so the next scout/dev can pick them up without re-mining. Full specs should be written
when the row is taken.

- **§3.1 Skills loaded into the tool surface** (luna-dev-2). Behaviour: resolve `<workspace>/.harness/skills`
  and `~/.luna/skills`, parse each `*.md` with frontmatter (`crates/harness-plugins/src/skills.rs:29`
  already does this), and register each as a tool plus expose two meta-tools — list and view — so the model
  sees the catalog without every skill body being in the system prompt. Shiina reference:
  `tools/skills_tool.py:653`, `:668`, `tools/skill_manager_tool.py:854`. Acceptance: `list_tools()`
  contains one entry per skill file; a chat turn loads a skill and follows it; a malformed frontmatter file
  is reported by `luna skills list` and does not abort startup. Test: unit against a fixture skills dir;
  e2e `luna skills list` + one PTY turn.
- **§3.2 Vector memory retrieval** (luna-dev-2). Behaviour: on `save_memory` compute an embedding via
  `ModelProvider::embed` (`crates/harness-core/src/traits.rs:34`) and store it in the existing, unused
  `memory_embeddings` table (`crates/harness-memory/schema.sql:15`); `retrieve()` ranks by cosine
  similarity with the `LIKE` path (`crates/harness-memory/src/engine.rs:58`) kept as fallback when no
  embedding is available. Shiina reference: `agent/memory_manager.py:283`. Acceptance: a memorable fact
  saved in one session is retrieved by a paraphrase in another (assert ordering, not just presence); with
  the embedding provider unconfigured, retrieval still returns the `LIKE` results and logs why. Also fix
  the hardcoded embedding model (`crates/harness-providers/src/client.rs:363`) into config.
- **§3.3 Streaming + reasoning-content output** (luna-dev-1). Behaviour: the loop consumes `stream()`
  (`crates/harness-providers/src/client.rs:266`) and prints deltas as they arrive; `reasoning_content`
  deltas are shown as a dimmed thinking line and never mixed into the answer body; usage from the stream
  updates the token tracker. Shiina reference: `gateway/stream_dispatch.py:21`,
  `tui_gateway/render.py:35`. Acceptance: a PTY chat turn shows output before the response completes
  (capture two `term_read` snapshots with a growing line); a reasoning-model turn whose `content` is null
  but `reasoning_content` is present still renders something and reports the empty-content case
  (`docs/AGENT_RULES.md:59`); no ANSI garbage in redirected stdout.
- **§3.4 Web search / fetch tool** (luna-dev-1). Behaviour: `web_search(query, limit)` and
  `web_fetch(url)` with a configurable backend and a bounded response size, timeouts, and private-address
  refusal (a personal harness must not be an SSRF proxy). Shiina reference: `tools/web_tools.py:268`.
  Acceptance: a real query returns N results with URLs; fetching one URL returns extracted text; a
  `http://127.0.0.1:…` URL and a `file://` URL are refused with an error result.
- **§3.5 Cron / scheduled jobs** (luna-dev-2, defer until §2.4). Behaviour: `luna cron add --cron "…"
  --prompt "…"`, jobs persisted in sqlite, a scheduler tick that runs a job as a normal agent turn and
  records the result, `luna cron list|logs|rm`. Shiina reference: `cron/scheduler_tick.py:7`,
  `cron/jobs.py:1749`. Acceptance: a `* * * * *` job fires within 70 s and its output is retrievable;
  a failing job records the error and does not stop the scheduler.
- **§3.6 Real sandboxed plugin runtime** (luna-dev-2). Behaviour: either drop the wasm claim or make it
  true — the current `SandboxedPlugin::execute` (`crates/harness-plugins/src/wasm.rs:57`) echoes its input
  and would mislead any caller. Minimum honest version: a plugin manifest + a wasmtime host with a fuel
  and memory cap and a documented host-function ABI. Shiina reference: `plugins/plugin_loader.py:92`.
  Acceptance: a committed fixture `.wasm` returns a computed value; an infinite-loop fixture is killed by
  fuel exhaustion; the tool result reports the trap instead of hanging.
- **§3.7 Real voice pipeline** (luna-dev-2). Behaviour: replace the three stubs
  (`crates/harness-cli/src/commands/voice.rs:26-53`) with `cpal` capture, an energy/VAD gate, a
  configurable STT and TTS provider, and keep the existing barge-in cancellation
  (`crates/harness-voice/src/pipeline.rs:57`). Shiina reference: `tools/wake_word.py:178`,
  `tools/tts_streaming.py:99`. Acceptance: speaking after the wake phrase produces a transcribed turn;
  `Ctrl-C` mid-playback aborts audio and the loop returns to listening; the pipeline is exercisable
  headless with a fixture WAV and a stub provider in tests.
- **§3.8 Token/cost telemetry that is actually recorded** (luna-dev-1). Behaviour: instantiate
  `TokenTracker` (`crates/harness-telemetry/src/lib.rs:15`), feed it usage from every completion, persist
  per-session totals to the session DB from §2.4, and show them on exit and in `luna usage`.
  Shiina reference: `shiina_state_usage.py:82`. Acceptance: after a NIM turn, `luna usage` prints the
  reported prompt/completion tokens matching the provider's own numbers; an estimate is never presented as
  a measurement.
- **§3.9 Subagent delegation** (luna-dev-1, defer). Behaviour: a `delegate_task` tool that spawns child
  agents with an isolated transcript and returns a summary, bounded by a configurable concurrency and
  timeout. Shiina reference: `agent/subagent_lifecycle.py:49`. Blocked by §2.4 (children need their own
  session) and §2.2 (children need tools worth delegating).
- **§3.10 Credential hardening** (luna-dev-1, security). Behaviour: store provider credentials in the OS
  keyring via the already-declared `keyring` dependency (`Cargo.toml:40`), keeping the file backend only
  as an explicit opt-in with a 0600 check and a startup warning; never log the value
  (`crates/harness-providers/src/auth.rs:33` currently persists it in cleartext). Acceptance: after
  `luna auth add`, `grep -c '<key>' ~/.luna/credentials.json` is 0 and the key round-trips from the
  keyring; the `luna auth list` output still prints only labels, ids and counters.
- **§3.11 Self-evolution loop made reachable** (luna-dev-1). Behaviour: `apply_harness_update` signals the
  supervisor that a rebuild is needed — via a real exit code or a sentinel file — and the supervisor
  (`crates/harness-cli/src/commands/supervisor.rs:34`) rebuilds and respawns; the sandbox stops claiming
  verification when `cargo` could not run (`crates/harness-evolver/src/sandbox.rs:61-65`) and patches
  become diffs, not whole-file content (`crates/harness-evolver/src/engine.rs:100`). Acceptance: a guided
  self-patch lands, the supervisor rebuilds, the new binary runs, and a patch whose sandbox run failed is
  never applied.
- **§3.12 Provider retry/backoff** (luna-dev-1). Behaviour: retry 429/5xx with exponential backoff and
  jitter up to `providers.<id>.max_retries` (default 2), honouring `Retry-After`, then return
  `HarnessError::RateLimited` (`crates/harness-providers/src/client.rs:206` is the current straight
  failure). Acceptance: a fake 429-then-200 server (wiremock or a local axum fixture) answers on the second
  attempt; a permanent 429 fails after the configured attempts with the provider named; no retry on 4xx
  other than 429.
- **§3.13 CLI surface for inspection** (luna-dev-1). Behaviour: `luna sessions`, `luna memory list|forget`,
  `luna notes list|read`, `luna tools list`, `luna doctor` (config paths, credential presence by last-4,
  DB reachability, binary version). Today `luna --help` lists only `chat`, `auth`, `models`, `voice`,
  `supervisor` (`crates/harness-cli/src/main.rs:25-55`), so none of the state the harness writes is
  inspectable without a sqlite client. Acceptance: each command has a failing-path test and prints a
  stable, greppable format; `luna doctor` exits non-zero when a configured path is unwritable.
- **§3.14 Provider matrix honesty** (luna-dev-1). The catalog (`crates/harness-providers/src/catalog.rs:20`)
  claims 10 providers, but only OpenAI-compatible chat is implemented: Anthropic needs its own message
  shape and versioned header (`crates/harness-providers/src/client.rs:72-79` sets the header and then
  sends OpenAI-shaped JSON), Gemini needs `generateContent` and a different tool-call schema, and Ollama
  is unverified. Acceptance: either implement per-shape adapters or mark each catalog entry with the
  transport it actually supports and refuse a mismatch with a clear message instead of a 400 from the
  provider.

---

## 4. Cards filed from this inventory

All five top capabilities now have a card on the `luna-harness` board. Two of them already had cards when
this inventory was written, and those pre-existing cards win: the spec below was folded into them by
comment instead of creating a second card that would decide the same question twice.

| Capability | Card | Lane | Prio | Note |
|---|---|---|---|---|
| §2.1 Config file + precedence | **t_5c1eca76** | luna-dev-1 | 95 | Filed from this spec; the tool-created original (t_f3507eb9) had no project worktree anchor and was archived by the lead, as was the older t_3240b75d. Format ruling: YAML at `~/.luna/config.yaml`. |
| §2.3 HITL approval gate | **t_c4f9c07d** | luna-dev-1 | 92 | Filed from this spec; the tool-created original (t_5cea6d69) was archived for the same workspace defect. |
| §2.2 Filesystem + shell tools | **t_c6cf04fa** | luna-dev-1 | 88 | Filed from this spec. First two attempts (t_cb3c0d83, t_ef20415a) were archived by the lead: `kanban_create` accepts `workspace_kind="worktree"` but leaves `project_id` null, so the worktree has no anchor. |
| §2.4 Session persistence + compaction | **t_73ce9054** | luna-dev-2 | 85 | Filed from this spec, and deliberately reassigned by the lead to luna-dev-2 (session storage is not dev-1's lane and dev-1's queue was eight deep). Only the lane in the §2.4 header below is stale; the spec is unchanged. |
| §2.5 MCP servers from config | **t_16a223b8** (pre-existing) | luna-dev-2 | 80 | Already on the board with the same scope; this spec's acceptance criteria and config keys were added as a comment instead of a duplicate card. |

Two filing lessons for the next scout, now also in `docs/AGENT_RULES.md` §6: `kanban_create` can take
30-90 s on this board, so an apparent timeout must be followed by a board listing rather than a retry
(four of my cards were double-filed that way and had to be archived); and `workspace_kind="worktree"`
must be paired with `project="luna-harness"`, or the worker gets an unanchored worktree.

Pace rules adopted after the lead's review of this batch (they apply to every remaining spec in §3):
queue cap of five open cards per developer, and route by lane — **luna-dev-1** is the busy lane
(agent loop, CLI, providers) and already had six open cards including the compile fix, so nothing else
from §3 goes there until something lands; **luna-dev-2** owns memory, notes, MCP, plugins, evolver, voice,
telemetry **and storage**, which is where §2.4 was moved. Specs whose work straddles the two lanes must be
split, or name the lane they must not be filed to. §3 therefore stays in the doc as the backlog: the next
card is filed when a card of mine lands, not when a spec is finished.

Archived by the lead, listed here so the trail is complete: **t_f3507eb9**, **t_5cea6d69**, **t_cb3c0d83**,
**t_ef20415a**, **t_bd2527dc**, **t_cb21a910**, **t_3240b75d**.

Pre-existing board cards this inventory corroborates rather than duplicates (no card filed for them; the
queued specs above are the detail): **t_40de5ef3** (reasoning-aware turns — §3.3), **t_2ccd2241**
(credential safety — §3.10), **t_2a4b4808** (model/provider resolution from config — the CLI half of §2.1),
**t_0a789e15** (make the workspace compile again), **t_7c2387f4** (luna-qa: build + live NIM + realterm).

Build state — **STALE, fixed since** (`t_0a789e15`, commit `7188aa0`, merged at `3456b9d`; re-verified at
`639c38e` with `cargo check -p harness-cli` → `Finished`, exit 0). The paragraph below is the state this
inventory was written against, kept because its diagnosis was handed to that card; the 24 errors it
describes are gone from `main`. Recorded as written: `cargo check --workspace --message-format short` at
`1fc995d` exits `101` with `error: could not compile \`harness-cli\` (bin "luna") due to 24 previous errors`
(plus 14 warnings). Every library crate type-checks; only the binary fails. The 24 split as
**20 × E0599** (`no method named \`register\` found for struct \`Arc<DefaultToolRegistry>\`` — the
`ToolRegistry` trait is not in scope at `crates/harness-cli/src/commands/chat.rs:44-57` and
`voice.rs:82-91`), **3 × E0195** (`voice.rs:24`, `:37`, `:46` — the `#[async_trait]` impls compiled without
the macro), and **1 × E0432** (`voice.rs:2` — `async_trait` is a workspace dependency at `Cargo.toml:29`
but is not declared in `crates/harness-cli/Cargo.toml`). The full diagnosis was handed to **t_0a789e15**,
the card in flight for this. Every test plan above assumes that card's green build, and no test plan here
claims a passing run that this card did not execute. **Update (roadmap pass, `639c38e`):** that card has
landed, so the assumption is satisfied — the build is green.


Verification used for this doc
- Shiina side: `grep -n` over the read-only checkout for every symbol cited; the reads are current as of
  this run (Shiina `main`).
- Luna side: every file cited under `crates/` was read in full in this run, plus
  `grep -rn <symbol> crates/harness-cli/src` for the "0 refs" claims (`harness_plugins`: 0,
  `harness_mcp`: 0, `harness_voice`: 1, `harness_telemetry`: 1) and
  `grep -rn "exit(42)\|process::exit" crates` → 0 hits for §3.11.
- Board state: read directly from `~/.shiina/kanban/boards/luna-harness/kanban.db` (task ids, statuses,
  workspace kind/path, project id, priorities) rather than from the cards' prose.


