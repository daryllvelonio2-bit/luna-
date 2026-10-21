# Usage and cost

Every turn writes one row to `~/.luna/usage.db` (`paths.usage_db`): provider, model, tokens in and
out, duration, tool calls, denials, and what it cost — when that is knowable.

```bash
luna usage                    # totals + per-model breakdown
luna usage --since 7d         # last week (30m, 12h, 7d, 2w, or RFC 3339)
luna usage --by session       # or model, source, day
luna usage --recent --limit 20   # the newest individual turns
luna usage --json             # one object, for a dashboard
```

## Cost is only claimed when it is known

A price comes from `usage.prices` layered over a small built-in table for the models the harness
defaults to. A model with no price records `cost_usd = NULL` and the report counts it separately:

```
  cost             $0.0001 (+1 unpriced)
  note             1 turn(s) used a model with no price; set usage.prices to cost them
```

That is deliberate. A "blended average per token" turns an unknown price into a confident dollar
figure, and a spending report that quietly guesses is worse than one that admits what it does not
know.

```yaml
usage:
  enabled: true                # default; false records nothing at all
  prices:
    openai/gpt-4o: { input_per_million: 2.50, output_per_million: 10.00 }
    local/*:       { input_per_million: 0.0,  output_per_million: 0.0 }   # a model you host
    */gpt-4o-mini: { input_per_million: 0.15, output_per_million: 0.60 }
```

Keys are `provider/model`, with `provider/*` and `*/model` wildcards; the exact key wins, then the
provider wildcard, then the model wildcard.

## What a row means

| field | meaning |
|---|---|
| `prompt_tokens` / `completion_tokens` | summed over **every model call in the turn** (tool loops, retries, compaction is its own turn) |
| `total_tokens` | sum of the two |
| `duration_ms` | wall clock for the turn, including tool execution and retry backoff |
| `tool_calls` / `denied_calls` | how many ran, how many the gate or a rule refused |
| `ok` | false when a tool failed or a call was denied — the tokens were spent either way |
| `source` | `chat`, `exec`, `voice`, `delegate` |

A turn the provider did not report usage for writes **no row**. `None` (unknown) is not zero, and a
zero row would quietly deflate every average built on top of it.

## Second source of truth

The provider's own figure (its `usage` object) is what gets recorded — not a character estimate.
The estimation heuristic exists only to decide *when to compact*, and it says so.

## Surfacing

- `luna exec --json` carries `usage` and `duration_ms` on its `result` event.
- `luna usage --json` prints `{ totals, by_model }` or `{ grouped_by, totals, buckets }`; with
  `--recent` it prints one JSON object per line.
