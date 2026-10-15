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

### 3. Interactive Chat REPL
Start an unbounded, stateful agent session:
```bash
cargo run --bin luna -- chat --provider openai --model gpt-4o
```

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
