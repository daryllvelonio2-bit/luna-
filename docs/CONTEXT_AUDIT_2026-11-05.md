# Context-usage audit — 2026-11-05

Scope: how a stored session becomes the message vector sent to a provider, what the context
gauge and the fold threshold measure, and which tool outputs are allowed to grow without a
bound. Read-only measurements first; the changes listed below were then implemented and tested.

Bar used: `agents.md` and `docs/AGENT_RULES.md`. This is a context-efficiency audit, distinct
from the general core audit in [`AUDIT_CORE_2026-11-04.md`](AUDIT_CORE_2026-11-04.md).

Baseline verification:

- `cargo test -p harness-core` → all suites `ok` (lib `129 passed`).
- `cargo test -p harness-cli` → `261 passed, 2 ignored` plus both integration suites.
- `cargo clippy -p harness-core -p harness-cli --all-targets -- -D warnings` → no warnings.

---

## What was measured

Source: `~/.luna/sessions.db` (187 MB, 24,170 rows: 16,760 live, 7,412 compacted, 209 summary
rows) and `~/.luna/luna.log`. Numbers are a local snapshot, not a benchmark.

| Payload, live rows | Size |
| --- | ---: |
| Tool results | ~45.4 MB (96% of live payload) |
| Tool-call JSON | 11.1 MB |
| — thought signatures | 6.57 MB (59% of tool-call JSON) |
| — arguments | 3.86 MB |
| Reasoning | 3.32 MB across 2,141 messages |
| User content | 12.5 MB |

Tool-result shape: 8,956 live results, median 1,002 chars, p90 8,817, p99 64,167. The 9.27 MB
maximum is an October row written before the central cap existed; the largest November row is
155,251 chars.

The distribution matters more than the totals: in 20 recently updated sessions the live payload
was 14.5 MB, but the newest ten tool results were 0.39 MB — **2.7% of the payload**. The other
97% was older output re-sent on every request.

A recent 200-row replay window (`s7372de70`) was ~1.46 MB, ~365k heuristic tokens: 203k chars of
tool results and 347k chars of tool-call JSON. The provider reported ~177k prompt tokens for the
same request. That session crossed 150k prompt tokens at seq 631 and reached 238k at seq 697
while the configured fold bar was 300k and `compact_at_tokens` defaulted to 500k.

Two structural findings:

1. **The estimator ignored what dominated the request.** `message_chars()` counted content, tool
   arguments and tool results, but not `Message::reasoning` or `ToolCall::signature`. Those two
   fields were 9.9 MB of live data and are replayed on the wire. A transcript could cross the fold
   bar without the estimator seeing it.
2. **The fold bar was static.** It did not know the model's context window. The live config asked
   for 300k on a model whose published window is 256k (`nemotron-3-nano-omni-30b-a3b-reasoning`),
   so the fold could only fire after the provider had already refused the request.

Third, smaller findings: `list_files` capped entries but not bytes; subagent replies were stored
and re-pasted whole; the central tool-result cap was 200k chars and kept only the head.

---

## Changes landed

### 1. The estimator counts reasoning and signatures

`crates/harness-core/src/session/compaction.rs`

`message_chars()` now adds `message.reasoning` and every `ToolCall.signature`. The provider-reported
usage still wins when present; this only fixes the fallback and the tail after the last report.
Test: `reasoning_and_signatures_count_towards_the_estimate`.

### 2. Replay elides what the conversation has moved past

`crates/harness-core/src/session/transcript.rs`

`sendable_messages()` is the single seam used by turn replay, fold estimation, rebinding and the
context gauge, so clipping there keeps all four consistent. It now:

- keeps the newest **6** tool results verbatim up to a **100,000-char** ceiling and clips older
  ones to **4,000 chars**, head and tail with a marker (both `content` and
  `tool_result.content`, because `Message::tool` stores the text twice);
- keeps reasoning on the newest **4** assistant messages and drops it from older ones;
- keeps the newest **8** thought signatures and clears the rest. The provider layer already
  substitutes Google's documented dummy (`SKIP_THOUGHT_SIGNATURE_VALIDATOR`) when the field is
  `None`, so strict Gemini still accepts the replayed call.

Measured on the 12 most recent sessions, clipping older results to 4,000 chars removes **621 KB
of every 1,376 KB** of older tool text. The newest-six boundary is chosen because the newest ten
results were only 2.7% of the payload: the work in flight is small, the tail is not.

Non-destructive: rows are untouched in `sessions.db`; clipping happens only in the vector that
is sent (and measured).

### 3. One central tool-result cap, 100k, head and tail

`crates/harness-core/src/agent/dispatch.rs`

The safety cap is now `MAX_TOOL_RESULT_CHARS = 100_000` (was 200k) and uses `clip_middle`
instead of a head cut, so a long build log keeps its closing lines. Every tool, MCP server and
plugin goes through this path. The guardrail test was updated to assert the new marker and bound.

### 4. `list_files` has a byte budget

`crates/harness-core/src/tools/fs.rs`

`MAX_LISTED_ENTRIES` (1,000) now has a companion `MAX_LISTED_OUTPUT_CHARS = 40_000`. One real
call produced 9.27 MB of listing text; the entry cap alone did not bound the bytes.

### 5. Subagent replies are bounded, the log is not

`crates/harness-core/src/tools/delegate.rs` and `tools/delegate/tests.rs`

A completed subagent reply is capped at **24,000 chars** before it is stored in the task list or
pasted into the parent. The full reply is written to the job log first, and the marker names that
log path. A long delegation now costs the parent a conclusion, not another copy of the
investigation.

### 6. The fold bar follows the model window

`crates/harness-core/src/agent.rs` and the CLI call sites

`AgentConfig` gained `context_window: Option<usize>` and `fold_threshold()` clamps the configured
bar to **70%** of it. The remaining 30% is headroom for the summary request, the reply and the
next tool round. Wired in:

- `chat.rs` — resolved once from the session's model and kept on the shared config, so the
  delegate tool's child agents inherit it;
- `chat/commands/mod.rs` — `/model` updates the bound with `Agent::set_context_window()`;
- `exec.rs`, `voice.rs`, `gateway/mod.rs` — each looks up its own provider/model.

Example: 300k configured on a 256k window now folds at 179,200 tokens instead of never firing
before the provider's limit.

---

## Remaining work, in priority order

1. **A synchronous preflight fold before an over-window request.** Folds are announced in the
   background, so the turn that crosses the bar still sends the full transcript and adopts the
   fold on the next round. The new 70% clamp makes this much less likely, but a single
   tool-heavy turn can still overshoot. Fold synchronously when the estimate is above the
   ceiling and there is no fold already in flight; keep the background path for the ordinary
   case.

2. **Bound replay by tokens, not only by rows.** `session.resume_max_messages` is 200 rows with
   no token bound; a 200-row agentic window measured ~365k heuristic tokens. Drop oldest complete
   tool rounds at replay time until the sendable vector fits a ceiling. This is the structural
   fix behind the 238k-prompt observation.

3. **Clip old tool-call arguments.** Arguments are 3.86 MB live, and old `write_file`/`patch`
   calls can be tens of kilobytes each. Their results are the useful part once the round is
   behind the conversation; a marker in place of the argument body would save most of that.
   Riskier than results because the model sometimes refers back to what it wrote — keep the
   newest rounds whole, as with results.

4. **Cap each retained reasoning block.** The newest four blocks are kept whole, and one live
   block was 137,791 chars (~34k tokens). A per-block ceiling (head + tail) would bound the worst
   case without dropping the current reasoning entirely. Verify strict Code Assist accepts a
   rewritten thought part for the current turn before shipping.

5. **Bound the summariser's own transcript.** `render_transcript(folded)` elides images but not
   large tool results. At a 70% fold bar the summary call re-reads most of what it is replacing;
   a head/tail clip in the summariser prompt would make the compaction call itself cheaper and
   keep it under the model window.

6. **Make `list_files` cheaper at the source.** The 40k cap is a backstop; a non-recursive default
   or a depth limit would avoid producing the listing at all. Watch whether models actually need
   deep recursive listings or are using them as a substitute for `search_files`.

---

## Re-measuring

```bash
# live tool-result distribution
sqlite3 ~/.luna/sessions.db "
  SELECT count(*), sum(length(tool_result_json))
  FROM messages WHERE compacted_at IS NULL AND tool_result_json IS NOT NULL;"

# signatures as a share of tool-call JSON
python3 - <<'PY'
import json, os, sqlite3
con = sqlite3.connect(f"file:{os.path.expanduser('~/.luna/sessions.db')}?mode=ro", uri=True)
total = args = sig = 0
for (raw,) in con.execute(
    "SELECT tool_calls_json FROM messages WHERE tool_calls_json IS NOT NULL AND compacted_at IS NULL"
):
    for call in json.loads(raw):
        total += len(json.dumps(call))
        args += len(json.dumps(call.get("arguments") or {}))
        sig += len(call.get("signature") or "")
print(f"calls={total} args={args} signatures={sig} ({100*sig/max(total,1):.1f}%)")
PY
```

To see the effect of replay clipping in a live session, compare
`agent.estimate_session_tokens()` (which goes through `sendable_messages()`) with the raw row
sizes for the same session; the difference is the elided payload.
