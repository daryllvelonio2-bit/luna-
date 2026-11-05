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
   - Durable file-backed Markdown storage: one user-readable note per file, with front matter for metadata and the document body preserved verbatim.
   - Legacy SQLite note databases are imported once and then retired as a backup; new notes are never stored in SQLite.
   - Automated **1–5 word memory pointer distillation** protocol bridge: note saves automatically register a concise 1–5 word pointer in persistent memory so the agent retains awareness without prompt bloat.
   - Native tools: `save_note`, `search_notes`, `list_notes`, `read_note`, `update_note`, `delete_note`.

4. **Ecosystem Extensibility**:
   - Model Context Protocol (MCP) client over stdio: servers are spawned from config and their
     tools are registered as `mcp__<server>__<tool>`.
   - Sandboxed Wasm plugins with memory and execution constraints.
   - Declarative Markdown prompt skills loader.

5. **Adaptive Failure Detector & Safety Control**:
   - Unbounded execution loop that runs as long as productive progress is made.
   - **5 consecutive non-working calls threshold**: halts only when thrashing on 5 consecutive failures, resetting immediately on any successful call.
   - Cooperative cancellation via `CancellationToken`.

6. **Hands-Free Dynamic Voice Interface (Wake Word "Luna", STT & TTS)**:
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
│   ├── harness-notes/           # File-backed Markdown notes, 1-5 word pointer bridge
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

### Platform support

Luna builds and runs on Linux, macOS, and Windows. Shell commands go through `sh -c` on Unix and
`cmd /C` on Windows, paths come from `$HOME`/`%USERPROFILE%`, and state lives under `~/.luna`
(`%USERPROFILE%\.luna` on Windows).

On Windows, use the Rust MSVC toolchain together with the Visual Studio C++ build tools, because
the bundled SQLite is compiled from source. One setting is Linux-only: `tools.network: deny`
installs a seccomp filter and fails closed elsewhere, so leave it at its default on Windows.

### 1. Luna Auth: Configuring Credentials
Add your API key and configure rotation:
```bash
# Add an OpenAI key with label and priority
cargo run --bin luna -- auth add --provider openai --label my-work-key --priority

# Add a backup key
cargo run --bin luna -- auth add --provider openai --label my-backup-key

# Set rotation strategy (round_robin, fill_first, least_used, random, never)
cargo run --bin luna -- auth strategy --provider openai --strategy never

# List registered credentials
cargo run --bin luna -- auth list
```
`--priority` marks a key as the primary one: `fill_first` uses it first and falls over to others upon failure, while `never` strictly uses the priority key and disables failover rotation altogether (the other three strategies rotate across all active keys). When a provider answers 401/403/429 under rotating strategies, the offending key is retired for the session and the next call fails over — the last remaining key is never retired. Under `never`, the priority key is kept selected without rotation.

### 2. Inspecting Available Models
Dynamically query models from the provider endpoint:
```bash
cargo run --bin luna -- models list --provider openai
```

### 2.5 The Filesystem, Shell & Web Tool Surface

The registry knows ten tools; nine are enabled by default (`process` is opt-in). All of them
resolve every path
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
todo_list(action, ...)            tracked task list for the session
terminal(command, timeout_s, workdir)
                                   bounded foreground shell, non-zero exit is a normal result
process(action, ...)              background jobs sharing the terminal's task queue (opt-in)
web_search(query, max_results, allowed_domains, blocked_domains)
                                   one query → a numbered list of title / URL / snippet
web_fetch(url, prompt, allowed_domains, blocked_domains)
                                   one URL → the page as text, HTML converted to prose
```

Of the ten, `tools.enabled` defaults to all but `process`.

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

**Configuring the surface.** `tools.enabled` (default: all but `process`) is the single switch — `config`
sections and env layers resolve it like every other key, so an install can drop `terminal` and
still keep the read/write/search tools:

```yaml
tools:
  enabled: [read_file, write_file, patch, search_files, list_files]
  read_max_chars: 100_000
  terminal_timeout_secs: 300
  terminal_output_max_bytes: 50_000
```

Mutating tools (`write_file`, `patch`, `terminal`) go through the approval gate when
`agent.require_hitl_for_mutation` is set; `read_file`, `search_files`, `list_files`, `web_search`
and `web_fetch` are read-only and never prompt.

**Reading the web.** `web_search` turns one query into a numbered list of results; `web_fetch`
turns one URL into readable text. Both are on by default, because an agent that has to be told
where to look is an agent that reaches for `curl` instead.

A fresh install searches through DuckDuckGo's HTML endpoint — no account, no key, nothing to sign
up for, and no contract either, which is why the parser is the most defensive code in the module.
`tools.web.search_backend: brave | tavily` points search at a keyed JSON API instead; the key is
read from the environment variable named by `tools.web.search_api_key_env` and is never written to
the config file. `search_backend: none` registers `web_fetch` alone, and `tools.web.enabled: false`
registers neither.

**What a URL is allowed to reach** is decided in one place (`tools::web::policy`), because a URL
the model hands over is attacker-influenced text when the page that suggested it was not
trustworthy:

- `http`/`https` only, and credentials in a URL are refused;
- an optional allow/deny domain policy (`allowed_domains`, `blocked_domains`, glob semantics, deny
  wins), which the per-call `allowed_domains` argument can narrow but never widen;
- every address a host resolves to must be publicly routable — so `127.0.0.1`, `169.254.169.254`
  (cloud instance credentials), RFC1918 and CGNAT addresses are refused *before a socket is
  opened*, unless `allow_private_hosts` says otherwise;
- the validated address is pinned into the client for that host, so the lookup that was checked is
  the address that is dialled rather than a second one;
- redirects are followed by hand and re-checked on every hop, capped at `max_redirects`;
- the body is capped while streaming (`fetch_max_bytes`), a content-type allowlist keeps binaries
  out, `robots.txt` is honoured, and one request gets `timeout_secs`.

The result is bounded in characters too (`fetch_max_chars`), and every cut is stated in the result
rather than left to look complete. A page fetched once is reused for `cache_ttl_secs`, so a loop
that re-reads its own sources costs nothing.

`web_fetch` accepts a `prompt` argument and **ignores** it: the page text comes back and the reading
is left to the model that asked for it, because a hidden second model summarising a page is a call
the user cannot see, cache, price, or check against the source. For pages that need JavaScript,
screenshots or clicking, the [`browser` skill](.harness/skills/browser/SKILL.md) still drives
headless Chromium; these two tools are the fast path for text.

```yaml
tools:
  web:
    enabled: true
    search_backend: duckduckgo    # duckduckgo | brave | tavily | none
    search_api_key_env: ""        # e.g. BRAVE_SEARCH_API_KEY; empty uses the backend's default
    search_max_results: 8
    fetch_max_bytes: 262144
    fetch_max_chars: 40000
    timeout_secs: 30
    max_redirects: 3
    cache_ttl_secs: 900
    allowed_domains: []           # file-only keys: comma lists are not accepted from the env layer
    blocked_domains: []
    allow_private_hosts: false
    respect_robots: true
```

The setup flow asks this question too, so a first run ends with a working search rather than a
config to edit by hand: `luna setup`, or `luna config set tools.web.search_backend brave` later.

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
wildcards) over a small built-in table. A provider's reported cache reads are stored per turn and
priced at `cached_input_per_million` when it is set — at the full input rate when it is not, so the
figure stays an upper bound. Details: [`docs/USAGE.md`](docs/USAGE.md).

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
cannot be enforced.

That filter confines a child, not the harness, so `deny` also withholds `web_search` and
`web_fetch`: they are not registered, and the reason — naming `tools.network` and
`tools.web.enabled` — is logged at start-up. A machine that has declared itself offline should not
be able to reach the internet through the agent's own sockets. Details:
[`docs/PERMISSIONS.md`](docs/PERMISSIONS.md).

### 2.14 Delegated subagents stay observable

`delegate_task` can run isolated child agents in the background. The chat composer keeps a live
row for every running child, showing its task id, goal, and current model/tool activity; the row
is removed when the child finishes. Progress repaints that one row in place — a child's tool
calls never append a line each to the transcript — so a long delegation leaves the conversation
readable, and one verdict line is printed when the child ends. `/tasks` (or `Ctrl+Shift+B`) opens
the viewer: a delegated child or a worker is painted with the **same transcript renderer as the
main CLI** — user line, process block, tool calls, reasoning and reply — while a shell background
job keeps its raw log. A worker is a conversation fork, so its viewer shows the worker's latest
turn rather than pasting the parent history underneath the task. A child fails only after **180
seconds with no model or tool activity**. Every streamed response and every tool event resets
that inactivity timer, so a long
task is not killed merely because it ran for a long time. `delegate_task(action: "wait")` waits
for up to **5 minutes** by default and leaves the child running in the background if that wait
window expires.

### 2.15 MCP servers

```yaml
mcp:
  enabled: true            # default true: servers only run because you listed them
  startup_timeout_secs: 20
  call_timeout_secs: 120
  servers:
    - name: filesystem     # the tool namespace: mcp__filesystem__<tool>
      command: npx
      args: ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
      tools: []           # empty = every tool the server advertises
```

Each server is spawned once per session over stdio, and its tools are registered as
`mcp__<server>__<tool>` so two servers may expose the same tool name without colliding. A
server that fails to start or list its tools is reported and skipped — it never fails the turn.
`luna mcp list` prints the configured servers from the config alone; `luna mcp tools` spawns
each enabled server and prints the namespaced tools the agent actually sees.

### 3. Interactive Chat REPL
Start an unbounded, stateful agent session (running `luna` opens the CLI automatically):
```bash
# Launch interactive chat directly:
luna

# Or specify provider and model:
luna --provider openai --model gpt-4o
```

**Context Auto-Compaction & Percentage Trimming:**
Luna automatically bounds session context tokens before making model requests and dynamically updates in-flight turns:
- **Configuration** in `~/.luna/config.yaml`:
  - `auto_compact_enabled: true` (default `true`)
  - `auto_compact_threshold: 150000` (valid range `100_000` to `700_000`, default `150_000`)
  - `auto_compact_percent: 50` (percentage of context to fold away, range `1` to `90`, default `50`)
  - `auto_compact_turns: 20` (backwards-compatible turn count fallback)
- **First-run Onboarding**: Prompted during setup to enable/disable auto-compaction, select token threshold between 100k and 700k, and specify the context percentage to compact (1%-90%).
- **Interactive Slash Command**:
  - `/auto-compact` — opens the interactive setup box with live feedback (warning if ≤10%, healthy status for balanced ranges, aggressive indicator above 60%).
  - `/auto-compact on` | `/auto-compact off` — toggle auto-compaction.
  Both the runtime in-memory state and the configuration file are updated immediately.
  - `/compact-model` — choose a dedicated provider and model for transcript compaction, with interactive scope selection: **This session only** (saved to session SQLite row across restarts) or **Globally** (persisted into `~/.luna/config.yaml`). Reset with `/compact-model reset`.
- **Mid-Turn Context Re-binding**: When background compaction completes during multi-round tool iterations, active turn messages are immediately rebound to the compacted transcript and the UI context gauge drops dynamically, avoiding repeated re-threshold thrashing.
- **Structured handoff checkpoints**: every fold — and `/summarize` — writes a durable checkpoint instead of a 3–5 line snapshot: `Objective`, `Requirements`, `Decisions`, `Work State` (`Completed` / `Active` / `Blocked`), `Next Move`, `Relevant Files`, and `Important Context`. The summariser merges any earlier checkpoint rather than summarising a summary, preserves exact paths, symbols, commands, flags, versions, exit codes and error strings, and marks unverified claims as unverified. The checkpoint is size-bounded so one summary cannot cost the turns it replaced.

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
