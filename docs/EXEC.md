# `luna exec` — headless runs

One prompt, one turn, machine-readable output. Same agent, same tools, same gate as `luna chat`,
without the terminal UI.

```bash
luna exec "which module owns the approval gate?"      # answer on stdout, trace on stderr
luna exec --json "fix the failing test" | jq -c 'select(.type=="result")'
echo "summarise PROGRESS.md" | luna exec --json -
```

## Flags

| flag | meaning |
|---|---|
| `[PROMPT]` | the prompt; `-` or omitted reads stdin |
| `--json` | NDJSON event stream instead of the plain answer |
| `--yes` | approve every mutating call (headless default is deny) |
| `--strict` | exit non-zero when any tool call failed |
| `--session <id\|name>` | resume a stored session |
| `--fork <id\|name>` | run in a copy of a stored session |
| `--continue` | run in the most recently updated session |

Without a session flag each run starts a **fresh** session, so a pipeline does not silently append
to whatever a person was doing. Every run is still stored, so `luna sessions list` shows it and
`--session` can pick it up.

## Output

`--json` writes one object per line and flushes each, so a consumer can read while the turn runs:

```json
{"type":"start","session":"s3f2a1c9","provider":"openai","model":"gpt-4o","prompt":"…"}
{"type":"context_tokens","tokens_used":2100}
{"type":"reasoning","text":"…"}
{"type":"tool_call","name":"write_file","arguments":{"path":"report.md","content":"…"}}
{"type":"tool_result","name":"write_file","ok":true,"output":"Successfully wrote 12 bytes"}
{"type":"result","ok":true,"answer":"…","session":"s3f2a1c9","tool_calls":[…],"replayed":0,"compacted":false}
```

The last line is always `{"type":"result"}`: `ok` is false when a tool failed *or* a call was
denied. On a turn that could not complete, the result line carries `error` instead of `answer`.

Without `--json`, stdout is the answer alone and the tool trace goes to stderr — so
`answer=$(luna exec "…")` gets usable text.

## Exit codes

| code | meaning |
|---|---|
| 0 | the turn completed and nothing was denied |
| 1 | the turn failed, **or a call was denied**, or `--strict` saw a failed tool |

A denial is a failure, not a warning: the caller asked for work the policy refused, and a zero exit
status would claim it happened. That is also why headless is fail-closed.

## Headless approval is fail-closed

There is nobody to answer a prompt, so a mutating call that no `tools.permissions` rule allows is
**denied** rather than approved by default. `luna chat` treats an unattended `Ask` as "approve
everything" — correct for a chat a person launched, wrong as the default for a job nobody watches.
Use `--yes` (or `tools.approval: allow`) when the run is supposed to mutate:

```bash
luna exec --yes "apply the migration"
```

Rules are the finer instrument — `luna exec --json "run the tests"` with

```yaml
tools:
  permissions:
    - { action: allow, tool: terminal, arguments: { command: "cargo *" } }
```

runs `cargo` commands unattended and still refuses a `write_file`. See [`PERMISSIONS.md`](PERMISSIONS.md).

## What `exec` shares with the chat

Memory tools, note tools, skills, the full core filesystem/shell surface, the
permission rules and per-turn checkpoints (`/rewind` data comes from the same place). A headless run
that could do less than the chat would be a different agent wearing the same name — and one that
skipped checkpoints would be a run with no undo.
