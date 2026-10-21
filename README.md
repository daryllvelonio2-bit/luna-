# Luna Autonomous AI Agent Harness (Rust)

A production-grade, extensible **Autonomous AI Agent Harness** implemented in **Rust**.

## Architecture & Features

1. **Universal Model Interoperability & Luna Auth (BYOK)**:
   - Pre-configured catalog for major providers (OpenAI, Anthropic, Gemini, Groq, Ollama, DeepSeek, OpenRouter, Together, Mistral).
   - **Dynamic model auto-fetching** from provider endpoints upon credential addition with local JSON caching (`~/.luna/models_cache.json`).
   - Interactive `luna auth` CLI credential manager supporting multiple labeled keys, a priority
     key that `FillFirst` uses before any other, and 4 rotation strategies (`RoundRobin`,
     `FillFirst`, `Random`, `LeastUsed`). A key that a provider rejects (401/403) or rate-limits
     (429) is retired for the rest of the session, so the next call fails over to another key.

2. **Model-Driven Persistent Memory (No Harness Pre-Filtering)**:
   - Hybrid relational SQLite + vector embeddings schema.
   - The decision of what is worth remembering or correcting is **delegated entirely to the model** via native tools (`save_memory`, `update_memory`, `forget_memory`).
   - Automatic contradiction detection and superseding mechanism (`is_active = 0`, `superseded_by`).

3. **Large-Scale Note-Taking & Knowledge Store**:
   - Durable SQLite note storage with FTS5 (Full-Text Search).
   - Automated **1–5 word memory pointer distillation** protocol bridge: note saves automatically register a concise 1–5 word pointer in persistent memory so the agent retains awareness without prompt bloat.
   - Native tools: `save_note`, `search_notes`, `read_note`, `update_note`.

4. **Autonomous Self-Improvement & Evolution (`harness-evolver`)**:
   - Diagnoses friction and missing capabilities across three tiers:
     - **Tier 1**: Declarative Markdown/YAML skills under `.harness/skills/`.
     - **Tier 2**: Dynamic Wasm plugins & MCP servers.
     - **Tier 3**: Core codebase inspection (`inspect_harness_source`), patch proposal (`propose_harness_patch`), isolated build verification (`test_patch_sandbox` running `cargo test`), and supervisor recompile/restart.

5. **Ecosystem Extensibility**:
   - Model Context Protocol (MCP) client implementation over Stdio and SSE.
   - Sandboxed Wasm plugins with memory and execution constraints.
   - Declarative Markdown prompt skills loader.

6. **Adaptive Failure Detector & Safety Control**:
   - Unbounded execution loop that runs as long as productive progress is made.
   - **5 consecutive non-working calls threshold**: halts only when thrashing on 5 consecutive failures, resetting immediately on any successful call.
   - Cooperative cancellation via `CancellationToken`.

7. **Hands-Free Dynamic Voice Interface (Wake Word "Luna", STT & TTS)**:
   - Background listening for wake word `"Luna"` via `cpal` (<2% CPU loop).
   - Voice Activity Detection (VAD) audio capture.
   - Pluggable STT and streaming TTS playback via `rodio`.
   - **Barge-In**: instantly aborts assistant audio playback and in-flight turn when the user speaks `"Luna"`.

---

## Workspace Layout

```text
rust-agent-harness/
├── Cargo.toml                   # Workspace manifest
├── crates/
│   ├── harness-core/            # Core primitives, traits, agent loop, 5-failure guardrail
│   ├── harness-providers/       # LLM provider catalog, dynamic model discovery, Luna Auth & rotation
│   ├── harness-mcp/             # Model Context Protocol (MCP) client implementation
│   ├── harness-plugins/         # Sandboxed Wasm plugin runtime & skill loader
│   ├── harness-memory/          # Model-driven persistent memory engine & memory tools
│   ├── harness-notes/           # Large document storage, FTS5 indexing, 1-5 word pointer bridge
│   ├── harness-evolver/         # Self-improvement engine, codebase patcher, sandbox build runner
│   ├── harness-voice/           # Hands-free "Luna" wake word, VAD, STT & streaming TTS
│   ├── harness-telemetry/       # Structured logging, token tracker, tracing setup
│   └── harness-cli/             # CLI frontend, `luna auth`, `luna voice`, REPL & supervisor
```

---

## Getting Started

### Building the Project
```bash
cargo build --release
```

### 1. Luna Auth: Configuring Credentials
Add your API key and configure rotation:
```bash
# Add an OpenAI key with label and priority
cargo run --bin luna -- auth add --provider openai --label my-work-key --priority

# Add a backup key
cargo run --bin luna -- auth add --provider openai --label my-backup-key

# Set rotation strategy to RoundRobin or FillFirst
cargo run --bin luna -- auth strategy --provider openai --strategy round_robin

# List registered credentials
cargo run --bin luna -- auth list
```
`--priority` marks a key as the primary one, but only the `fill_first` strategy consults that flag
(the other three rotate over every active key); `luna auth priority` prints the same reminder. When
a provider answers 401/403/429 the offending key is retired for the rest of the session and the next
call uses another key — the last remaining key is never retired.

### 2. Inspecting Available Models
Dynamically query models from the provider endpoint:
```bash
cargo run --bin luna -- models list --provider openai
```

### 2.5 The Filesystem & Shell Tool Surface

The model drives the machine through six registered tools. All of them resolve every path
against `paths.workspace_root`, and an attempt to escape it (`..`, an absolute path outside the
root, a symlink pointing out) is refused with an explanatory error rather than silently clamped.

```text
read_file(path, offset, limit)     read with line numbers, whole lines, bounded output
write_file(path, content)         create or overwrite (requires read-before-write)
patch(path, old_string, new_string, replace_all)
                                   ambiguous edits are refused unless replace_all is set
search_files(pattern, path, file_glob, output_mode)
                                   literal substring search: content | files_only | count
list_files(path, glob)            recursive listing, find files by name
terminal(command, timeout_s, workdir)
                                   bounded foreground shell, non-zero exit is a normal result
```

`read_file` emits whole lines and stops before the line that would overflow its budget, then
appends a notice naming the resume offset — a cut line cannot be recovered by an offset, so it
stops at the line boundary instead of truncating mid-line.

`write_file` and `patch` refuse to touch a file whose content changed since the last `read_file`
in the same session. A stale overwrite is data loss, so it is refused, not clobbered.

**Glob semantics** (`search_files`'s `file_glob` and `list_files`'s `glob`) are real glob
matching, shared by both tools: `*` matches any run of characters, `?` exactly one. A bare
pattern (`*.rs`, `*_test.rs`) matches the file name; a pattern containing a slash (`src/*.rs`)
matches the workspace-relative path. A pattern with no wildcard must equal the name or path
exactly, so `rs` does not match `cars`.

**Announced limits.** Neither tool can return an exhaustive-looking result that was silently
cut. `search_files` skips files above 2 MB, stops after 20,000 visited files, caps results per
output mode (200 matching lines for `content`, 500 files for `files_only` and `count`), and
appends a notice naming whatever ceiling it hit and how to narrow it. `list_files` skips `.git`,
`target` and `node_modules`, and caps at 1,000 entries with the same style of notice.

**Configuring the surface.** `tools.enabled` (default: all six) is the single switch — `config`
sections and env layers resolve it like every other key, so an install can drop `terminal` and
still keep the read/write/search tools:

```yaml
tools:
  enabled: [read_file, write_file, patch, search_files, list_files]
  read_max_chars: 100_000
  terminal_timeout_secs: 120
  terminal_output_max_bytes: 50_000
```

Mutating tools (`write_file`, `patch`, `terminal`) go through the approval gate when
`agent.require_hitl_for_mutation` is set; `read_file`, `search_files` and `list_files` are
read-only and never prompt.

### 2.6 Lifecycle Hooks

Shell commands bound to harness events — `pre_tool_use` (which can refuse a call or rewrite its
arguments), `post_tool_use`, `user_prompt_submit`, `stop`, `session_start`/`session_end`,
`pre_compact`, `subagent_*`. Off by default; repository-supplied hooks need explicit trust.

```yaml
hooks:
  enabled: true            # default false
  trust_workspace: false    # default false: <workspace>/.luna/hooks.json stays inert
  timeout_secs: 30
```

```json
{ "hooks": { "pre_tool_use": [ { "matcher": "terminal", "command": ".luna/hooks/guard.sh" } ] } }
```

A hook receives the event as JSON on stdin and may answer with `{"decision":"deny","reason":"…"}`
or `{"updatedInput":{…}}`; exit code `2` also refuses. Hooks can narrow what happens but never
approve what the human gate would refuse. Full contract: [`docs/HOOKS.md`](docs/HOOKS.md).

### 2.7 Permission rules

Standing policy evaluated before the human is asked, with **`deny` > `ask` > `allow`** precedence
regardless of order, tool-name globs and argument globs (`!` inverts):

```yaml
tools:
  permissions:
    - { action: allow, tool: terminal, arguments: { command: "cargo *" } }
    - { action: deny,  tool: terminal, arguments: { command: "*git push --force*" }, reason: never }
    - { action: ask,   tool: "write_*" }
```

A rule decides *whether the human is asked*, never *what a tool can reach*; calls matched by no
rule behave exactly as before. Details: [`docs/PERMISSIONS.md`](docs/PERMISSIONS.md).

### 2.8 Checkpoints and `/rewind`

Every turn snapshots the files its `write_file`/`patch` calls are about to change, into
`~/.luna/checkpoints/<session>/turn_000N/` (plain files, written during the turn). `/rewind` lists
them with a per-turn stat line; `/rewind 3` restores turn 3's files and deletes what that turn
created. `/diff [turn]` (or `luna diff`) reviews what a turn changed first — a read-only diff of the
snapshot against the workspace, `created`/`modified`/`deleted`/`unchanged` derived from the bytes.
Files changed by a shell command are named as *unrecorded* — visible, and explicitly not undoable. `terminal` is not
intercepted — a shell command's targets are not knowable from its arguments. On by default
(`checkpoints.enabled`, `checkpoints.keep_turns`). Details: [`docs/CHECKPOINTS.md`](docs/CHECKPOINTS.md).

### 2.9 Headless runs: `luna exec`

One prompt, one turn, scriptable output — the same agent, tools, rules and checkpoints as the chat,
without the UI:

```bash
luna exec "which module owns the approval gate?"          # answer on stdout, trace on stderr
luna exec --json "fix the failing test" | jq -c .         # NDJSON: start → tool_call → tool_result → result
echo "summarise PROGRESS.md" | luna exec --json -         # prompt from stdin
luna exec --json --session s3f2a1c9 "and then?"           # resume a stored session
```

Headless is **fail-closed**: a mutating call no `allow` rule covers is denied, and a denial exits
non-zero (`--yes` approves all). `--strict` also fails on tool errors. Details:
[`docs/EXEC.md`](docs/EXEC.md).

### 2.10 Usage and cost: `luna usage`

Every turn lands in `~/.luna/usage.db`: tokens in/out, provider, model, duration, tool calls and
cost. `luna usage`, `--since 7d`, `--by model|session|source|day`, `--recent`, `--json`.

Cost is only reported when it is known: a model with no price is counted as **unpriced**, never
guessed at. Prices live in `usage.prices` (`provider/model`, with `provider/*` and `*/model`
wildcards) over a small built-in table. Details: [`docs/USAGE.md`](docs/USAGE.md).

### 2.11 Hooks on every surface: `luna hooks`

`luna chat` and `luna exec` load the same lifecycle hooks and bracket a turn with
`session_start`/`session_end`. `pre_tool_use` can refuse or rewrite a call (the gate still decides),
`post_tool_use` can replace the result the model reads — a redaction hook that actually redacts.
`luna hooks` prints what is *loaded*, not what the files say. Details:
[`docs/HOOKS.md`](docs/HOOKS.md).

### 2.12 The workspace is a boundary: `paths.extra_roots`

Every filesystem tool and the `terminal` workdir resolve through one sandbox: `..` folding, the
deepest existing ancestor canonicalized (so `ws/link -> /etc` cannot smuggle a path out), component
comparison (so `ws-evil` is not inside `ws`), `~` expanded *before* the check. `/tmp` is not special;
opt in with `paths.extra_roots`. `--yes` approves a mutation, it does not widen the sandbox.
Details: [`docs/PERMISSIONS.md`](docs/PERMISSIONS.md).

### 2.13 `tools.network`: a socket boundary the child inherits

`tools.network: allow | deny`. Under `deny`, every `terminal` child and background job gets a
seccomp filter installed between `fork` and `exec`: `socket(2)` works for `AF_UNIX` only, everything
else returns `EPERM` — `curl`, DNS and dependency fetches fail before a packet leaves the machine,
while local IPC keeps working. Kernel enforcement, not a command-string match; fails closed where it
cannot be enforced. Details: [`docs/PERMISSIONS.md`](docs/PERMISSIONS.md).

### 3. Interactive Chat REPL
Start an unbounded, stateful agent session (running `luna` opens the CLI automatically):
```bash
# Launch interactive chat directly:
luna

# Or specify provider and model:
luna --provider openai --model gpt-4o
```

**The todo list belongs to the session.** `todo_list` keeps one file per conversation in
`paths.todos_dir` (`~/.luna/todos/<session>.json`), so a new session starts with an empty
list and never shows another session's tasks. A `/fork` carries the list into the fork,
because the fork continues the same work. The chat draws the list as a checklist under
the transcript — an empty box for what is open, a half box for what is in flight, a tick
for what is done — and folds it to a single line with **Ctrl+T** (or `/todo`).

### 4. Hands-Free Voice Mode
Launch background listening for wake word `"Luna"`:
```bash
cargo run --bin luna -- voice --provider openai
```

### 5. Running Supervised Agent
Run agent under the supervisor to enable live self-patching and automatic rebuild restarts:
```bash
cargo run --bin luna -- supervisor chat
```

### 6. Eval Suite (regression gate)
Scripted end-to-end scenarios that drive the real agent loop; offline by default (no credential, no
quota), `--live` for the real provider. One line per case, non-zero exit on failure,
machine-readable `eval-report.json`. Format and case-authoring guide: [`docs/EVALS.md`](docs/EVALS.md).
```bash
cargo run --bin luna -- eval run --suite tests/eval --offline
cargo run --bin luna -- eval run --suite tests/eval --live --filter write-then-read
```
