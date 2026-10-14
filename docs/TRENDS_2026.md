# Trends 2026: what the agent-harness field changed, and what this harness should do about it

Scope: the `luna` harness in this repo (10 crates) measured against the 2026 state of the field —
provider APIs, agent protocols, sandboxing, context/memory architecture, eval practice, and what
shipping harnesses actually do. Research pass for the card `t_f2592dd8`, 2026-10-15.

Evidence rules used here
- **Every claim carries a URL and, where the source states one, a date.** The kind of source is
  labelled: `spec` (normative), `provider docs` (first-party API documentation), `paper`
  (preprint or proceedings), `vendor blog` / `blog` (secondary prose), `measurement` (run on this
  machine during this card, with the command and observed output quoted in §6).
- **Nothing in this document is spec'd that needs a service this machine does not have.** The live
  provider is NVIDIA NIM (`deepseek-ai/deepseek-v4.1-flash`); items that would need a new credential
  are named as such and skipped, not spec'd around.
- Where secondary sources disagree on a date, the doc says so instead of picking one.
- Precedents: this doc is the trends companion to `docs/FEATURE_GAPS.md` (Shiina-CLI capability
  inventory → specs). That doc owns *feature parity*; this one owns *direction*. Where they
  overlap, this doc references the existing spec instead of restating it.

What this document is not: a list of everything that exists. The kill list (§5) is deliberately
long, because a second provider client, a second agent protocol, or a vector retrieval stack are
all things this harness would pay for and not use.

---

## 1. The one-screen version

Between the last time this repo was planned and now, four things moved that matter to a
10-crate, single-user Rust harness on one machine:

1. **Prompt caching became automatic on every major API**, and the provider we actually call now
   reports cache hits in `usage.prompt_tokens_details.cached_tokens` (measurement, §6). A harness
   that rebuilds its system prompt every turn (`crates/harness-core/src/agent.rs:92-117`) and
   returns tool definitions in `HashMap` order (`crates/harness-core/src/registry.rs:32-35`) is
   paying full input price for a prefix it could be reusing.
2. **Reasoning became a controllable, persistable channel, not an internal detail.** OpenAI
   persists reasoning items across calls; Anthropic ships an effort dial and a thinking-block
   retention policy; NIM returns `reasoning_content` and accepts a reasoning-budget hint
   (measurement, §6) but 500s on the documented `nvext.max_thinking_tokens` field when hosted.
3. **Safety moved from "ask the human" to "the kernel decides."** Both shipping CLI harnesses now
   run shell commands inside an OS boundary (Seatbelt / bubblewrap + seccomp) and treat approval
   as a *separate* knob. `bwrap` is installed on this machine (measurement, §6).
4. **Agent protocols consolidated and then broke their own compatibility.** MCP's 2026-07-28
   revision removed the handshake this repo's client is written around
   (`crates/harness-mcp/src/stdio.rs:102` sends `"protocolVersion": "2024-11-05"`), and the
   ecosystem standardised on `AGENTS.md`, `SKILL.md` and a neutral foundation (AAIF, Linux
   Foundation).

The three changes worth building first are §2.1 prompt-prefix discipline (S), §2.2 a sandboxed
shell (M), and §2.3 a deferred tool catalog (M). Everything else is either already specified in
`docs/FEATURE_GAPS.md` or killed in §5.

---

## 2. Trends table

`cost` is engineering size for this repo (S ≤ a day, M = days, L = a week+). `crate` is where the
work lands — no new crate is proposed anywhere in this table.

| # | Trend | What changed in 2026 | What it implies for this harness | Cost | Crate |
|---|-------|----------------------|----------------------------------|------|-------|
| 1 | Prefix caching is the default, not an option | Anthropic: "automatic caching", 5-minute default TTL or 1-hour at 2× write price, cached reads at 0.1× input (`docs.claude.com/en/docs/build-with-claude/prompt-caching`, provider docs). OpenAI: "Prompt caching is enabled by default for supported OpenAI models", "discounted up to 90%", plus a Prompt Cache Diagnostics tool (`platform.openai.com/docs/guides/prompt-caching`, provider docs). MCP's 2026-07-28 changelog now asks *servers* for the same discipline: they "SHOULD return tools from `tools/list` in a deterministic order to enable client-side caching and improve LLM prompt cache hit rates" (`modelcontextprotocol.io/specification/2026-07-28/changelog`, spec). NIM reports `cached_tokens` (measurement §6) | Make the request prefix byte-stable and *record* the cache hit: sort tool definitions, stop rebuilding the system message per turn with retrieved memories, parse `prompt_tokens_details` (never read in the non-streaming path, `crates/harness-providers/src/client.rs:222-264`; the streaming path reads only three fields, `:329-340`; absent from `TokenUsage` at `crates/harness-core/src/types.rs:109-114`) | **S** | harness-core, harness-providers, harness-telemetry |
| 2 | Reasoning is a first-class, tunable channel | OpenAI: `reasoning.effort` none→max, `reasoning.mode` standard/pro, persisted reasoning via `previous_response_id` + `reasoning.context: all_turns`, items opaque (`platform.openai.com/docs/guides/reasoning`). Anthropic: `output_config.effort` with five levels low/medium/high/xhigh/max — top-level effort needs no beta header, per-message effort is in beta; the docs call it "a behavioural signal, not a strict token budget" (that phrase and the claim that per-message effort "preserves the prompt cache" are quoted verbatim by several secondary write-ups; this pass extracted only the page head, so treat both phrasings as vendor-doc wording not re-verified character-for-character here) (`platform.claude.com/docs/en/build-with-claude/effort`, provider docs). Task budgets beta `task-budgets-2026-03-13`: `task_budget` takes `type`/`total`/optional `remaining`, and "API responses do not include a remaining-budget field… To track spend client-side, sum token usage across the requests in your loop" (`.../task-budgets`). OpenAI: `reasoning.effort` `none|minimal|low|medium|high|xhigh|max`, `reasoning.mode` `standard|pro`, `reasoning.context` `auto|current_turn|all_turns` (GPT-5.6 defaults to `all_turns`), persisted reasoning via `previous_response_id` or replayed `encrypted_content`, and reasoning items "remain opaque, and the API does not return their reasoning text" (`platform.openai.com/docs/guides/reasoning` + the `openai-python` `Reasoning` typed dict). NIM: `reasoning_content` returned, `reasoning_effort: "low"` accepted (HTTP 200), the documented thinking-budget field `nvext.max_thinking_tokens` returns HTTP 500 when hosted (measurement §6) | Keep reasoning inside the turn (interleaved thinking needs prior thinking blocks handed back), show it without mixing it into the answer, and expose effort/budget as config — with provider differences as data, never `if provider == …` in the loop | **M** | harness-providers, harness-core |
| 3 | Context management moved server-side | Anthropic context editing (beta) clears tool results (`clear_tool_uses_20250919`) and thinking blocks (`clear_thinking_20251015`) with `trigger`/`keep`/`clear_at_least`, and warns that clearing invalidates the cached prefix — "clear enough tokens to make the cache invalidation worthwhile" (`.../context-editing`). Server-side compaction is described as "generally preferred" over client-side summarising in the same doc | Session storage must keep raw tool results addressable and replaceable by placeholders, and compaction must preserve the stable prefix; a summary that rewrites the head of the transcript costs more than it saves | **M** | harness-core (+ sessions spec §2.4 in FEATURE_GAPS) |
| 4 | Tool definitions are a context cost with an accuracy cliff | Anthropic tool search: a five-server setup "can consume ~55k tokens in definitions before Claude does any work"; on-demand loading "typically reduces this by over 85 percent"; selection accuracy "degrades once you exceed 30–50 available tools" (`.../tool-search-tool`). OpenAI `tool_search`, gpt-5.4+ only (`platform.openai.com/docs/guides/function-calling`) | MCP wiring (card `t_16a223b8`) is about to register every server's tools flatly. Keep definitions out of the request until asked for, and ship the meta-tools in the harness so it works on NIM too | **M** | harness-core, harness-mcp |
| 5 | Shell execution runs under an OS boundary | Claude Code: `sandbox.enabled`, `sandbox.filesystem.allow*/deny*`, `sandbox.network.allowedDomains` via a local proxy; auto-approval is justified *because* the kernel enforces the boundary; sandbox covers shell commands only — file tools, MCP servers and hooks run outside it (`code.claude.com/docs/en/sandboxing`, provider docs). Codex CLI: `sandbox_mode` (`read-only` / `workspace-write` / `danger-full-access`) orthogonal to `approval_policy`, `--full-auto` being the `workspace-write` + `on-request` alias; Apple Seatbelt on macOS, while the first-party docs say Linux "combines Landlock and seccomp APIs" and the sandbox crate's own README documents the current path — bubblewrap is "the default filesystem sandbox", the filesystem is read-only via `--ro-bind / /`, writable roots are layered with `--bind <root> <root>`, `PR_SET_NO_NEW_PRIVS` and a seccomp network filter are applied in-process, and `.git` (including a resolved `gitdir:` pointer), `.codex` and `.agents` are re-applied read-only inside a writable root; a missing `bwrap` falls back to a bundled binary and raises a startup warning instead of failing quietly (`developers.openai.com/codex/sandbox` and `github.com/openai/codex/blob/main/codex-rs/linux-sandbox/README.md`, first-party docs; the Landlock-vs-bubblewrap wording differs between the two pages, which is itself worth knowing before copying either) | `terminal` (spec §2.2 in FEATURE_GAPS) must not be a bare `Command::new("sh")`. Two knobs, fail closed when `bwrap` is missing, and prove the boundary with a write probe rather than reading the config | **M** | harness-core, harness-cli |
| 6 | Deterministic hooks beat instructions | Claude Code fires user-defined commands at `PreToolUse` / `PostToolUse` / `Stop` / `SessionStart`; a non-zero exit blocks the tool call before it runs, "the model cannot talk its way past"; hooks configured in skills/subagents too (`docs.claude.com/en/docs/claude-code/hooks`, provider docs) | The approval gate (`t_c4f9c07d`) should offer a *command* hook, not only a keypress — an unattended turn cannot answer a prompt | **S/M** | harness-core, harness-cli |
| 7 | Repo instruction files won, with a caveat | `AGENTS.md` is a founding AAIF project alongside MCP and goose (Linux Foundation press release, "SAN FRANCISCO, Dec. 9, 2025", `linuxfoundation.org/press/linux-foundation-announces-the-formation-of-the-agentic-ai-foundation`, mirrored at `aaif.io/news/…`; the same release credits AGENTS.md with "more than 60,000 open source projects and agent frameworks"). Adoption, measured: Context Files appeared in 2,586 of 2,853 studied repositories (90.6%, 4,768 files) and AGENTS.md is "emerging as an interoperable standard across tools" (`arxiv.org/abs/2602.14690`, preprint v5, submitted 2026-02-16, revised 2026-06-30). Effect, measured — and negative: "context files result in no improvement in task success rates, while also increasing inference cost by over 20%" (`arxiv.org/abs/2602.11988`, preprint, MemAgents @ ICLR 2026), and a third study reports "the real AGENTS.md never converts a near-miss to a pass on either agent" (`arxiv.org/abs/2607.27250`, preprint). The secondary source that aggregated all three states the finer split — human-written files helped ~4%, machine-generated ones reduced success, with 14–22% more reasoning tokens either way (`marmelab.com/blog/2026/09/24/the-state-of-ai-harness-engineering-2026.html`, blog) | Load a workspace/user instruction file, token-capped and line-truncated, in the *stable* part of the prompt; never auto-generate or auto-append to it | **S** | harness-core, harness-cli |
| 8 | MCP dropped the handshake this client speaks | 2026-07-28 revision: stateless core — the `initialize`/`initialized` exchange is gone (SEP-2575) and so is the `Mcp-Session-Id` header (SEP-2567), client identity/version/capabilities travel in `_meta` on every request, `server/discover` exists for capability discovery but is optional, HTTP POSTs must carry `Mcp-Method`/`Mcp-Name` that match the body (SEP-2243), MRTR replaces server-initiated `roots/list`/`sampling`/`elicitation` (SEP-2322), every result carries a required `resultType`, list/read results carry `ttlMs` + `cacheScope` via the new `CacheableResult` interface (SEP-2549), `extensions` is a field on client/server capabilities with Tasks (`io.modelcontextprotocol/tasks`) and MCP Apps as the shipping extensions, and Roots/Sampling/Logging are deprecated as one proposal (SEP-2577; `ping` and client→server progress are removed outright). The deprecations sit under a stated lifecycle policy, but no fixed window (e.g. "12 months") could be verified this pass, and legacy HTTP+SSE is documented on a backward-compatibility path rather than an explicit deprecation (`modelcontextprotocol.io/specification/2026-07-28/changelog`, spec + the `changelog.mdx` in `modelcontextprotocol/modelcontextprotocol`) | Version negotiation instead of a hardcoded `2024-11-05`; treat tool annotations as untrusted hints; long-running tool calls need the Tasks extension or an explicit timeout story | **M** (version-aware) / **L** (stateless transport) | harness-mcp |
| 9 | Harnesses are evaluated on containerised task suites | Terminal-Bench 2.0 + Harbor: 89 verified tasks, six agents × 16 models, 32,155 trials, frontier < 65% resolved; failure taxonomy Execution / Coherence / Verification (`tbench.ai/news/announcement-2-0`, project announcement; `proceedings.iclr.cc/…2026…`, paper) | A scenario suite that runs offline (fake provider) and optionally live, with a non-zero exit on regression, is the smallest honest version of this — and this fleet keeps re-learning the same failures by hand | **M** | harness-qa (new `tests/eval` or `crates/harness-evals`) |
| 10 | Harness self-testing is rare (and that is the opportunity) | Audit of 246 open-source harness repos + 57 publications + 145 large non-AI controls: 63% of those controls ship an agent instruction file; "out of the 97 harnesses big enough to test, 60% have neither a test nor an eval", 21% test themselves, 27% version evals, only 7 do both; 4.4% of security rules in public `.claude/` files are backed by an executable guard (`marmelab.com/blog/2026/09/24/the-state-of-ai-harness-engineering-2026.html`, blog, published 2026-09-24) | Same as row 9: the eval suite is differentiating, not box-ticking. Also: version the harness files (`docs/AGENT_RULES.md`) with the code, which this repo already does | **S** | repo hygiene |
| 11 | Deterministic retrieval beat embeddings in shipping runtimes | Source-code study of eleven harnesses plus one meta-harness, pinned to July 2026 releases (13 cross-cutting observations, 29 recurring patterns): across ~4M LOC "no agent runtime imports a general-purpose agentic framework … and none retrieves code with vector embeddings"; retrieval is ripgrep/tree-sitter/glob/auto-discovered Markdown; `SKILL.md` skills lead MCP in adoption (9/11 vs 8/11); ACP ships in six systems and gained a third role, hosting rival harnesses (`arxiv.org/abs/2609.00006`, preprint) | Cheap to act on: no vector store for code. Memory retrieval for *facts* is different (FEATURE_GAPS §3.2) but should degrade to `LIKE`/keyword rather than requiring embeddings | — | (a constraint on other cards, not work) |
| 12 | A private harness should still speak the neutral protocols | ACP standardises editor↔agent sessions and publishes a Rust library (`agentclientprotocol.com`, protocol docs; ACP's adoption is independently counted at six systems in row 11). A2A and its v1.0 release date (2026-03-12, multi-bindings, signed AgentCards, version negotiation) could **not** be re-verified in this pass — the search backend returned errors for those queries, so treat the date as unconfirmed rather than sourced. | **Skip both for now** (§5) — but keep `luna`'s own CLI non-interactive paths clean, because that is the shape any future host (editor, scheduler, another agent) would call | — | — |
| 13 | Provider surface is consolidating around OpenAI-shaped chat + Responses | OpenAI tells users to reach for the Responses API for newer models' tool calling, offers an "Agents API" for hosted persistent agents, and gates `tool_search` to "gpt-5.4 and later models" (`platform.openai.com/docs/guides/function-calling`). NIM serves `/v1/chat/completions`, `/v1/responses`, `/v1/messages` (Anthropic-compatible) and `/v1/models` from the same vLLM backend (`docs.nvidia.com/nim/large-language-models/latest/reference/api-reference.html`, vendor docs; the copies retrieved carried last-updated stamps of Aug 2026 — the "2026-09-30" date this row previously asserted is not supported and has been dropped) | Do **not** add a Responses client now. Keep the chat-completions path and make the provider layer honest about which transport each catalog entry supports (FEATURE_GAPS §3.14) | — | (kill, §5) |
| 14 | Cost/usage is now part of the API contract | Anthropic task budgets deliberately expose *no* remaining-budget field — "To track spend client-side, sum token usage across the requests in your loop" (`.../task-budgets`); OpenAI adds a "Prompt Cache Diagnostics" surface and session usage fields (`.../prompt-caching`) | Telemetry must be recorded, not narrated: persisted per-session prompt/completion/**cached** tokens (FEATURE_GAPS §3.8 + row 1 here) | **S** | harness-telemetry |

Verdict on the table: rows 1, 4, 5 and 7 are cheap-to-medium and now carry cards of their own
(`t_448d659f`, `t_cfd6facc`, `t_fc182dc9`, `t_75ed9224`); row 2 folds into an existing card
(`t_40de5ef3`); row 8 folds into a card that has not started (`t_16a223b8`); rows 3, 6, 9 and 14 are
already specified elsewhere or are one config key each; rows 11–13 are constraints, not work, and K1
above states why the provider-surface row is a kill rather than a plan.

---

## 3. Specs for the three highest-value items

Each spec is startable as written. Cards exist for all four argued-for items (§4); the three specs
below are the highest-value ones, and the acceptance criteria here are the same ones those cards
carry, so a worker reading either gets the same contract.
Priorities below are the board's current values after the operator's 2026-10-15 reorder, which put
the audit's evidence-backed defects ahead of every new capability in this document (sandbox 80,
prompt assembly 78, AGENTS.md 74, deferred catalog 72), and made each of the three specs that extend
in-flight work depend on the card it extends.

### §2.1 Cache-stable prompt assembly and prompt-cache telemetry
Lane: **luna-dev-1**. Card: `t_448d659f` (gated on the config card `t_5c1eca76`). Cost: **S**.
Priority: 78 (was 84 before the operator's defect-first reorder).

Behaviour
- One function assembles the request in a fixed order — static instructions, workspace instruction
  file (row 7), tool definitions sorted by name, then conversation — and the volatile memory block
  moves to the *end* of the system content instead of the head. Today the system message is
  rebuilt from scratch every turn with freshly retrieved memories
  (`crates/harness-core/src/agent.rs:92-117`), which guarantees a cache miss on the first two
  sections of the prefix that providers document as the cached part.
- Tool definitions are returned in a deterministic order. `DefaultToolRegistry` holds a `HashMap`
  and `list_tools()` iterates it (`crates/harness-core/src/registry.rs:13`, `:32-35`), so the
  serialized tool array can differ between runs — a cache miss, and unreproducible runs for a
  reason nobody will guess from the symptom.
- The provider layer parses `usage.prompt_tokens_details.cached_tokens` when present and it flows
  into `TokenUsage` and telemetry; the REPL prints prompt / completion / cached tokens and the
  session cache-hit ratio on exit.

Config keys (all in `~/.luna/config.yaml`, per `docs/AGENT_RULES.md` §6)
`prompt.stable_prefix` (default `true`; `false` restores today's layout byte-for-byte),
`prompt.agents_md_max_tokens` (used by row 7), `telemetry.usage` (default `true`).

Acceptance criteria (testable)
1. `list_tools()` returns the same order for two registries populated in opposite orders (test).
2. Two identical live turns against NIM show a non-zero cached-token count on the second, quoted
   with the exact command. Prompt length is part of the test: this pass saw
   `prompt_tokens_details.cached_tokens = 0` on both calls of a 58-token prompt, and 0 → 2,432 on a
   2,448-token prompt (§6), so use a prompt above the endpoint's minimum cacheable length. If the
   endpoint's shared cache still evicts between calls, fall back to asserting request bytes: dump
   both bodies (redacted, tracing) and show the prefix is byte-identical apart from the appended
   memory block.
3. A turn that retrieves *different* memories still reports cached tokens > 0 for the shared prefix.
4. `prompt.stable_prefix = false` produces a system message byte-identical to a fixture captured
   from `agent.rs:95-115` at `b8032aa`.
5. `TokenUsage` round-trips `cached_tokens: None` for a provider that omits the field (no
   fabricated zero presented as a measurement).

Test plan
- Unit, `harness-core`: ordering test above; a fixture test for the two prompt layouts; a
  serialization test asserting the tool array is sorted and stable.
- Unit, `harness-providers`: response fixtures with and without `prompt_tokens_details`.
- E2E, non-interactive: the two-turn NIM run in criterion 2 (one real round-trip budget per
  AGENT_RULES §3 — do not loop the provider).
- E2E, PTY (`realterm`): one session, three turns, `term_read` captures the cache telemetry line.

### §2.2 Sandboxed shell execution: two orthogonal knobs, one kernel boundary
Lane: **luna-dev-1**. Card: `t_fc182dc9` (gated on the filesystem/shell tools card `t_c6cf04fa`
and the approval gate `t_c4f9c07d`). Cost: **M**. Priority: 80 (was 86; the operator moved the audit's defects above it).

Behaviour
- `terminal` runs under `bwrap` when available: whole filesystem read-only (`--ro-bind / /`),
  writable roots layered back with `--bind <root> <root>` (workspace root + TMPDIR),
  `--unshare-user --unshare-pid`, `PR_SET_NO_NEW_PRIVS`, and no network unless
  `tools.sandbox.network = true`. Inside a writable root, `.git` and `~/.luna` stay read-only, so
  the agent cannot rewrite history or its own configuration without a prompt.
- **Two knobs, documented as orthogonal:** `agent.require_hitl_for_mutation` / `tools.approval`
  decide *whether* a mutating call runs; `tools.sandbox.mode` decides *what it can reach once it
  runs*. `read-only` + no approval fails closed; `workspace-write` + no approval is the mode that
  fails open, and the docs must say so in those words.
- Missing `bwrap` is not a quietly-degraded state: `read-only` and `workspace-write` fail closed
  with a clear error unless `tools.sandbox.fail_open = true`, which logs a warning. Never run a
  "sandboxed" command unsandboxed while claiming otherwise.
- The child environment is rebuilt from an allow-list (`PATH`, `HOME`, `TERM`, `LANG`, plus
  `tools.sandbox.env_allow`); the provider key in the parent environment must not reach a
  model-authored command.
- Scope honesty: this covers shell commands only. File tools, MCP servers and (future) hooks run
  outside it — the same caveat Claude Code documents for its own sandbox, and the reason the file
  tools keep their own path-confinement checks.

Config keys
`tools.sandbox.mode` (`read-only` | `workspace-write` | `danger-full-access`, default
`workspace-write`), `tools.sandbox.network` (default `false`),
`tools.sandbox.writable_roots` (default: workspace root),
`tools.sandbox.env_allow` (default empty), `tools.sandbox.fail_open` (default `false`),
`tools.sandbox.bwrap_path` (default: `bwrap` from PATH — `/usr/bin/bwrap` here).

Acceptance criteria (testable)
1. Argv builder tests for all three modes; missing-bwrap fails closed; env allow-list drops an
   `NVIDIA_API_KEY`-shaped variable.
2. `terminal("touch /etc/luna-probe")` under `workspace-write`: non-zero exit, no file created
   (`ls -l /etc/luna-probe` quoted), error surfaced as a normal `ToolResult` the model can react to.
3. Boundary proof quoted once: outbound `curl` fails by default and succeeds with
   `tools.sandbox.network = true`. Treat "exit 0 with nothing created" as a silent-failure
   signature, not a pass — this is the documented trap when the sandbox cannot start inside a container.
4. `git commit --allow-empty` inside the workspace succeeds; a command writing `~/.gitconfig` fails.
5. `danger-full-access` runs unsandboxed and prints a startup warning (assert the string).

Test plan
- Unit (fixture-based, no damage to the real machine): argv construction, mode matrix, missing-bwrap
  and env-scrub paths.
- Integration: run the probes above from a Rust integration test with a tempdir workspace, and
  assert both exit status and filesystem state.
- E2E (PTY): one real turn that writes inside the workspace and one that tries to write outside,
  with both `term_read` captures in the handoff.

### §2.3 Deferred tool catalog: keep tool definitions out of the request until asked for
Lane: **luna-dev-2**. Card: `t_cfd6facc` (gated on the MCP wiring card `t_16a223b8`). Cost: **M**.
Priority: 72 (was 76).

Behaviour
- A tool declares whether it is eager or deferred. Native tools default eager; MCP-provided tools
  default deferred. `list_tools()` returns eager tools plus two meta-tools, never the whole catalog.
- `list_tools(query, limit)` searches deferred definitions with keyword matching (no embeddings —
  see row 11) and returns names with one-line descriptions; `load_tools(names)` promotes named
  tools into the request for the rest of the session. Both are registered native tools, so the
  mechanism works on NIM and on any future provider that lacks a `tool_search` equivalent.
- Executing a deferred tool by name still works even if it was never loaded: resolution happens
  against the full registry, because a correctly-named tool call should not fail for bookkeeping
  reasons.
- The visible count is capped (`tools.catalog.max_visible`, default 24) below the documented
  30–50 tool accuracy cliff; a server that advertises an unreasonable number of tools is truncated
  with a warning.

Config keys
`tools.catalog.enabled` (default `true`), `tools.catalog.eager` (list, default: native tool names),
`tools.catalog.max_visible` (default `24`), `tools.catalog.defer_patterns` (default `<server>__*`),
`mcp.servers.<name>.max_tools` (default `50`, warning + truncation above it).

Acceptance criteria (testable)
1. 40 registered tools (2 eager, 38 deferred) → `list_tools()` returns 2 + 2 meta-tools;
   `list_tools("git")` finds matches; `load_tools(["x"])` makes `x` visible next call; a direct call
   to an unloaded tool still executes.
2. `tools.catalog.max_visible = 5` caps the visible set and the rest stay reachable.
3. Request-body assertion with a recording provider: no deferred definition appears before
   `list_tools` is called; after `load_tools`, exactly the loaded definitions appear.
4. One measured number in the handoff: prompt tokens for the same MCP fixture with the catalog on
   vs off.
5. `cargo test --workspace` passes and `cargo clippy --workspace --all-targets -- -D warnings` is clean.

Test plan
- Unit, `harness-core`: registry visibility rules, meta-tool matching, load/promote semantics,
  direct-call path, cap enforcement.
- Integration, `harness-mcp`: fixture stdio server (from `t_16a223b8`) with a recording provider —
  assert the serialized request never carries deferred definitions until asked.
- E2E (PTY, one live turn): the model discovers and calls a deferred MCP tool; quote the rendered
  tool call and final answer.

---

## 4. Cards filed, folds, and what was deliberately not filed

Four cards came out of this document. Priorities are the board's values after the operator's
2026-10-15 reorder (defect fixes first); every gate is a real edge in the board DB, not prose.

| Card | Item | Rows | Lane | Priority | Waits on |
|------|------|------|------|----------|----------|
| `t_fc182dc9` | Sandboxed shell execution (bwrap, approval vs sandbox mode) | 5, 6 | luna-dev-1 | 80 | `t_c6cf04fa` (fs/shell suite), `t_c4f9c07d` (approval gate) |
| `t_448d659f` | Cache-stable prompt assembly + prompt-cache telemetry | 1, 3 | luna-dev-1 | 78 | `t_5c1eca76` (config file) |
| `t_75ed9224` | AGENTS.md / workspace instruction loading into the stable prefix | 7 | luna-dev-1 | 74 | `t_5c1eca76`, `t_448d659f` |
| `t_cfd6facc` | Deferred tool catalog (`list_tools`/`load_tools`, no embeddings) | 4, 5 | luna-dev-2 | 72 | `t_16a223b8` (MCP wiring) |

Folded into cards that already existed, rather than filed twice (AGENT_RULES §6):

- **Row 2 (reasoning channel)** → `t_40de5ef3` already owns reasoning-aware turns
  (`reasoning_content`, empty `content` is not a failure). This document adds only the config/effort
  angle to that card's scope; nothing here justifies a second card in the same files.
- **Row 6 (hooks)** → `t_c4f9c07d` (approval gate) is the only place a pre-tool veto can live. A
  separate hooks subsystem would be a second policy engine.
- **Row 8 (MCP 2026-07-28)** → `t_16a223b8` (MCP wiring) is writing the client that hardcodes
  `2024-11-05`. Version negotiation belongs in that card's scope, not in a follow-up.
- **Rows 9/10 (eval suite)** → `t_83845462` (luna-qa, `luna eval`) already exists; this document
  only supplies the argument and the numbers.

Not filed on purpose, with the reason:

- **Row 14 (usage telemetry)** is one field pair (`cached_tokens`) on the same `TokenUsage` the
  prompt-assembly card already edits — filing it separately would put two cards in one struct.
- **Row 3 (server-side context editing / compaction)** transfers to this repo as "keep raw tool
  results addressable", which is the sessions spec already in `docs/FEATURE_GAPS.md` §2.4.
- **Rows 11–13** are constraints and kills (§5), not work.
- Everything else in this document that survives review stays *here* until a queue slot frees: the
  operator's lead call on 2026-10-15 was to stop filing and finish the doc, with dev-1 and dev-2 each
  holding ten open cards and draining one at a time. A backlog filed ahead of the binary compiling is
  a wish list, not a plan (AGENT_RULES §6).

**Workspace defect found while filing (worth fixing before a dev picks these up).** All four cards
were created with `workspace_kind = worktree` but no project linkage, so the dispatcher resolved
`workspace_path` to the shared checkout `/home/janelle/Downloads/rust-agent-harness` instead of a
fresh `<repo>/.worktrees/<task-id>`. Cards created with project linkage look different — compare
`t_2a4b4808`, `t_40de5ef3`, `t_16a223b8`, whose paths are `.worktrees/<task-id>`. Consequence: a dev
who takes one of these four would be editing `main` in the shared checkout, which AGENT_RULES §4
forbids and which collides with every other worker. Observed with a read-only query of the board DB
(`select id, workspace_kind, workspace_path from tasks`), output quoted in §6. Either the dispatcher
re-files them with project linkage, or the worker runs
`git worktree add .worktrees/<task-id> -b wt/<task-id>` before touching a file.

---

## 5. The kill list

Stated as decisions, because the useful output of a research pass is usually what it refuses.

| # | Killed idea | Why |
|---|-------------|-----|
| K1 | A second provider client (OpenAI Responses API, Anthropic Messages) | Our live path is `chat/completions` against the hosted NIM endpoint, and NIM's own Responses/Messages endpoints are *container* features (`POST /v1/responses`, `/v1/messages` in the NIM API reference). Nothing in this repo needs a second transport until a second model actually requires it — and any OpenAI/Anthropic client needs a credential this machine does not have, which makes it a flagged dependency, not a spec. Keep the client honest about transports (FEATURE_GAPS §3.14) and revisit when a credential exists. |
| K2 | Server-side context editing (`clear_tool_uses_*`) and task budgets as features | Both are Anthropic-API-only. The transferable parts are already carded: keep raw tool results replaceable with placeholders (sessions spec) and record usage (`cached_tokens`, row 1). Building a config surface for a beta that our provider cannot serve is negative value. |
| K3 | A thinking/reasoning *budget* knob on NIM | Measured: `nvext.max_thinking_tokens` returns HTTP 500 from the hosted endpoint (§6), while `reasoning_effort` is accepted. Ship effort, not a budget field that 500s. |
| K4 | Vector embeddings for code retrieval | Eleven shipping harnesses, ~4M LOC, none does it (row 11); the measured practice is rg/tree-sitter/glob. Memory-fact retrieval may want recall later, but it should degrade to `LIKE`/keyword before it needs a vector store, an index and an embedding provider. |
| K5 | A2A (agent-to-agent) support | Release details could not even be re-verified this pass (row 12), and a single-user CLI has no counterpart to talk to. ACP matters only in the editor-hosting direction, which is a host's job, not ours. |
| K6 | Auto-generating `AGENTS.md`/instruction files | The measured effect is negative: machine-generated context files reduced task success while costing >20% more inference (row 7). Load files; never write them. |
| K7 | A new `crates/harness-evals` | Row 9's smallest honest version is a scripted scenario suite under `tests/eval` driving the existing binary. A new crate is a build-time and review-time cost with no payoff until the suite outgrows one directory. |
| K8 | Hooks as a subsystem (separate config tree, separate executor) | Fold into the approval gate: what an unattended turn needs is *a command that can veto a tool call*, which is one field on the existing gate (row 6), not a second policy engine. |
| K9 | Structured-output / strict-schema enforcement as a first-class harness feature | Not killed forever — killed *now*: it is `t_40de5ef3`-adjacent provider work, and the two harness crates with the strongest evidence behind them (rows 1, 5) do not touch it. Revisit after the eval suite exists to prove a schema change helps. |
| K10 | Any item requiring a paid credential this machine lacks (OpenAI, Anthropic, Gemini keys) | Stated for the record so nobody specs one in later: they are *flagged*, not spec'd. The only live credential here is NVIDIA NIM (`deepseek-ai/deepseek-v4.1-flash`), registered by `scripts/register-nvidia-key.py`. |

---

## 6. Verification: commands run in this pass, and what they returned

Run on this machine (Linux, kernel `7.2.4-arch1-2`) on 2026-10-15, against checkout `319bf38` of
this repo. Anything not verified is listed at the end rather than phrased as though it had been.

Code facts (read with the exact lines, not inferred):

- `crates/harness-core/src/registry.rs:13` holds `RwLock<HashMap<String, Arc<dyn Tool>>>`, and
  `list_tools()` (`:32-35`) is `map.values().map(|t| t.definition()).collect()` — so tool-definition
  order is HashMap iteration order, which Rust does not guarantee and randomises per process. That is
  a code reading; the cost consequence is what the card measures.
- `crates/harness-providers/src/client.rs`: `complete()` (`:222-264`) parses `choices[0].message` and
  never touches `usage`; `stream()` requests usage (`"stream_options": { "include_usage": true }`,
  `:284`) and reads exactly `prompt_tokens`, `completion_tokens`, `total_tokens` (`:329-340`).
  `grep -rn cached_tokens crates` → no matches.
- `crates/harness-core/src/types.rs:109-114`: `TokenUsage { prompt_tokens, completion_tokens,
  total_tokens }` — no cache field.
- `crates/harness-core/src/agent.rs:92-117`: the system text is `format!`-built every turn with
  `memory.format_as_system_prompt(...)` interpolated into the middle, so the prefix head changes
  whenever retrieval changes.
- `crates/harness-mcp/src/stdio.rs:102`: `"protocolVersion": "2024-11-05"`.
- `grep -rn TokenTracker crates` → only its definition (`harness-telemetry/src/lib.rs:15,20`); nothing
  constructs it.

Sandbox (row 5) — `bwrap` on this box:

    $ which bwrap && bwrap --version && uname -r
    /usr/bin/bwrap
    bubblewrap 0.12.0
    7.2.4-arch1-2

    $ bwrap --ro-bind / / --dev /dev --proc /proc --unshare-user --unshare-pid --unshare-net \
        sh -c 'touch /etc/luna-probe'
    touch: cannot touch '/etc/luna-probe': Read-only file system
    exit=1
    $ ls -l /etc/luna-probe
    ls: cannot access '/etc/luna-probe': No such file or directory

    $ mkdir -p /tmp/luna-sbx && bwrap --ro-bind / / --bind /tmp/luna-sbx /tmp/luna-sbx \
        --dev /dev --proc /proc --unshare-user --unshare-pid --unshare-net \
        sh -c 'touch /tmp/luna-sbx/ok.txt && echo wrote-inside'
    wrote-inside
    exit=0
    $ ls -l /tmp/luna-sbx/
    -rw-r--r-- 1 janelle janelle 0 … ok.txt

    $ bwrap --ro-bind / / --dev /dev --proc /proc --unshare-user --unshare-pid --unshare-net \
        sh -c 'curl -s -m 3 https://example.com'
    exit=6                      # name resolution fails: no network inside the namespace

So the read-only-whole-filesystem boundary, the writable-root carve-out and the network-off default
are all demonstrated here; the missing-`bwrap` fail-closed path stays a unit-test obligation in the
card, because I cannot uninstall the binary to stage it.

Prompt cache on the live endpoint (`POST integrate.api.nvidia.com/v1/chat/completions`,
`deepseek-ai/deepseek-v4.1-flash`; the key is read from `~/.luna/credentials.json` inside the probe
process and never printed or logged). Two byte-identical bodies, the second sent ~2 s after the
first:

    small prompt (58 prompt tokens), twice:
      cold: prompt_tokens=58  prompt_tokens_details={"cached_tokens": 0}
      warm: prompt_tokens=58  prompt_tokens_details={"cached_tokens": 0}
    large prompt (2,448 prompt tokens), twice:
      cold: prompt_tokens=2448  prompt_tokens_details={"cached_tokens": 0}
      warm: prompt_tokens=2448  prompt_tokens_details={"cached_tokens": 2432}

The cache signal is real on the exact endpoint this harness calls, and it only moves once the prompt
is above the endpoint's minimum cacheable length — below it the field is present and always 0, which
a naive test would misread as "caching is broken". This also corrects the evidence quoted in card
`t_448d659f` (a 648-token probe reporting `cached_tokens: 640`): that number did not reproduce, the
finding is the same in shape, and the card's criterion 2 now says to use a prompt above the minimum
(comment added on the card). Two further probes, one request each:

    nvext.max_thinking_tokens=2048 → HTTP 500 {"message":"Internal server error", "type":"Internal Server Error", "code":500, "details":{"backend_status":500}}
    reasoning_effort="low"         → HTTP 200, content="ready"

which is the measurement behind row 2 and kill K3 (`reasoning_content` also observed, 1,446 chars of
it on the small-prompt call).

Board state — read-only query against `~/.shiina/kanban/boards/luna-harness/kanban.db` (the
`shiina kanban` CLI refuses to run inside a worker context, so the query is raw SQLite, no writes):

    $ python3 -c "…select id, assignee, status, priority, workspace_kind, workspace_path from tasks…"
    t_fc182dc9 luna-dev-1 todo          p=80 worktree:/home/janelle/Downloads/rust-agent-harness
    t_448d659f luna-dev-1 todo          p=78 worktree:/home/janelle/Downloads/rust-agent-harness
    t_75ed9224 luna-dev-1 todo          p=74 worktree:/home/janelle/Downloads/rust-agent-harness
    t_cfd6facc luna-dev-2 todo          p=72 worktree:/home/janelle/Downloads/rust-agent-harness
    t_2a4b4808 luna-dev-1 todo          p=85 worktree:…/.worktrees/t_2a4b4808
    t_40de5ef3 luna-dev-2 todo          p=84 worktree:…/.worktrees/t_40de5ef3

    edges: t_5c1eca76→t_448d659f, t_5c1eca76→t_75ed9224, t_448d659f→t_75ed9224,
           t_16a223b8→t_cfd6facc, t_c6cf04fa→t_fc182dc9, t_c4f9c07d→t_fc182dc9

That is the §4 workspace defect and the §4 gate list, observed rather than asserted. Open cards at
the time of writing: 10 for luna-dev-1, 9 for luna-dev-2 (the reason §4 stops filing).

Not verified in this pass, and labelled as such wherever it appears:

- **A2A v1.0's release date** (row 12): both search attempts returned backend errors.
- **Full-text retrieval for `arxiv.org`, `modelcontextprotocol.io`, `tbench.ai`, `docs.nvidia.com`**:
  this box's extractor refuses them ("URL targets a private or internal network address" — those
  hosts resolve through a NAT64 prefix the extractor misreads, not because the pages are private).
  Their claims were checked against the same pages' titles, abstracts and snippets returned by
  search — weaker than a full read, and the quotes used are sentences those snippets themselves
  contained.
- **The MCP "12-month" deprecation window** and legacy-HTTP+SSE deprecation: neither appears in the
  changelog text I could read, so row 8 says so instead of repeating them.
- **Claude Code hooks wording** (row 6) was not re-fetched this pass; no card depends on its exact
  phrasing, and the row is cited as provider docs, not as a spec.

---

## 7. Sources

One row per source actually cited above. `kind` distinguishes a normative spec from first-party API
documentation, a preprint, project/vendor prose, and measurement on this machine.

| Source | kind | URL | date used |
|--------|------|-----|-----------|
| Anthropic prompt caching | provider docs | `platform.claude.com/docs/en/build-with-claude/prompt-caching` (canonical; `docs.claude.com/...` 301s here) | retrieved 2026-10-15; "5-minute lifetime", 1-hour at 2× write, cache reads 0.1× |
| Anthropic context editing | provider docs | `…/build-with-claude/context-editing` | retrieved 2026-10-15; `clear_tool_uses_20250919`, `clear_thinking_20251015`, cache-invalidation wording |
| Anthropic effort | provider docs | `…/build-with-claude/effort` | retrieved 2026-10-15; five levels, top-level no beta, per-message in beta |
| Anthropic task budgets | provider docs | `…/build-with-claude/task-budgets` | beta id `task-budgets-2026-03-13`; no remaining-budget field in responses |
| Anthropic tool search | provider docs | `platform.claude.com/docs/en/agents-and-tools/tool-use/tool-search-tool` | retrieved 2026-10-15; ~55k tokens, >85%, 30–50 tools |
| OpenAI prompt caching | provider docs | `platform.openai.com/docs/guides/prompt-caching` | retrieved 2026-10-15; on by default, up to 90%, Diagnostics tool |
| OpenAI function calling / `tool_search` | provider docs | `platform.openai.com/docs/guides/function-calling` | retrieved 2026-10-15; "Only `gpt-5.4` and later models support `tool_search`" |
| OpenAI reasoning | provider docs | `platform.openai.com/docs/guides/reasoning` + `openai-python` `types/shared_params/reasoning.py` | retrieved 2026-10-15; effort values, `reasoning.context`, opacity |
| Claude Code sandboxing | provider docs | `code.claude.com/docs/en/sandboxing` | retrieved 2026-10-15; `sandbox.network.allowedDomains`, "shell commands only" |
| Claude Code hooks | provider docs | `code.claude.com/docs/en/claude-code/hooks` | cited from an earlier pass; not re-fetched 2026-10-15 |
| Codex sandbox (docs + crate README) | first-party docs | `developers.openai.com/codex/sandbox`; `github.com/openai/codex/blob/main/codex-rs/linux-sandbox/README.md` | retrieved 2026-10-15; modes, Seatbelt, bubblewrap argv, protected paths |
| MCP 2026-07-28 changelog | spec | `modelcontextprotocol.io/specification/2026-07-28/changelog` + `changelog.mdx` in `modelcontextprotocol/modelcontextprotocol` | revision dated 2026-07-28; SEP-2575/2567/2243/2322/2549/2577, Tasks + MCP Apps |
| MCP blog for that release | vendor blog | `blog.modelcontextprotocol.io/posts/2026-07-28/` | posted 2026-07-28 |
| Terminal-Bench 2.0 paper | paper | `proceedings.iclr.cc/paper_files/paper/2026/file/444a3737…-Paper-Conference.pdf` | 89 tasks, 6 agents × 16 models, 32,155 trials, <65%, failure taxonomy |
| Terminal-Bench 2.0 / Harbor announcement | project announcement | `tbench.ai/news/announcement-2-0` | retrieved 2026-10-15 |
| "Harness Engineering … Eleven Systems" | preprint | `arxiv.org/abs/2609.00006` | pinned to July 2026 releases; 11 + 1 systems, ~4M LOC, SKILL.md 9/11, ACP 6 systems |
| "Harness Engineering for Agentic AI Coding Tools" | preprint | `arxiv.org/abs/2602.14690` (v5) | submitted 2026-02-16, revised 2026-06-30; 2,853 repos, Context Files 90.6% |
| "Evaluating AGENTS.md" | preprint | `arxiv.org/abs/2602.11988` (via the source below) | MemAgents @ ICLR 2026; no success improvement, >20% cost increase |
| "Do Context Files Help Coding Agents?" | preprint | `arxiv.org/abs/2607.27250` | ablation + manipulation probe; AGENTS.md never converts a near-miss |
| "The State Of AI Harness Engineering 2026" | blog | `marmelab.com/blog/2026/09/24/the-state-of-ai-harness-engineering-2026.html` | published 2026-09-24; 246 repos, 57 publications, 97 testable harnesses, 4.4% guards |
| AAIF formation | press release | `linuxfoundation.org/press/linux-foundation-announces-the-formation-of-the-agentic-ai-foundation` (+ `aaif.io/news/…`, `openai.com/index/agentic-ai-foundation/`) | dated 2025-12-09; AGENTS.md/MCP/goose founding projects, 60,000+ repos |
| NVIDIA NIM API reference | vendor docs | `docs.nvidia.com/nim/large-language-models/latest/reference/api-reference.html` | retrieved 2026-10-15 via search snippets; endpoint list, Aug 2026 last-updated stamps |
| ACP | protocol docs | `agentclientprotocol.com` | availability confirmed; Rust library, editor↔agent sessions |
| A2A | — | not verified this pass (search backend errors) | — |

Measurement sources: the three sandbox probes, the four cache probes and the two handheld-parameter
probes in §6, plus the read-only board query. Everything else in this document is a citation, and
where a citation rests on a search snippet rather than a full page read, the document says so.

