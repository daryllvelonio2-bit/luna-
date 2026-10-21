# Lifecycle hooks

Hooks run a shell command when the harness reaches a defined moment — before a tool call, after a
turn, at session start. They are how a policy that is not the approval prompt gets enforced:
"never touch `main`", "log every terminal command to the audit file", "reformat the file this
patch just wrote", "notify me when a turn ends".

Hooks are **off by default**. Enabling them is a deliberate act, because a hook is code execution
with the same privileges as the harness.

## Enable

```yaml
# ~/.luna/config.yaml
hooks:
  enabled: true          # run hooks at all (default false)
  trust_workspace: false # run <workspace>/.luna/hooks.json too (default false)
  timeout_secs: 30       # a hook that outlives this window is killed
  disabled: []           # e.g. ["post_tool_use"]
```

`hooks.enabled` / `hooks.trust_workspace` / `hooks.timeout_secs` / `hooks.disabled` are normal
config keys: `luna config get hooks.enabled`, `luna config set hooks.enabled true`, or
`LUNA_HOOKS_ENABLED=true`.

Two files are read, in this order:

| file | when it runs |
|---|---|
| `~/.luna/hooks.json` | whenever `hooks.enabled` is true |
| `<workspace>/.luna/hooks.json` | only when `hooks.trust_workspace` is also true |

The split exists so a cloned repository cannot configure code execution for itself. A project hook
file is ignored (with a `debug` line) until the operator trusts the workspace.

## Write a hook

```json
{
  "hooks": {
    "pre_tool_use": [
      { "matcher": "terminal", "command": ".luna/hooks/block-force-push.sh", "timeout_secs": 5 }
    ],
    "post_tool_use": [
      { "matcher": "write_*", "command": "prettier --write \"$LUNA_HOOK_FILE\" || true" }
    ]
  }
}
```

The flat form (`{"pre_tool_use": [...]}` with no `hooks` wrapper) is also accepted. `matcher` is a
glob over the tool name (`*`, `terminal`, `write_*`, `?earch`); an event without a tool ignores
matchers. Unknown event names are a configuration error — a typo must not silently disable a
guardrail.

## Events

| event | when | can refuse? |
|---|---|---|
| `session_start` | the session is created | yes (the run aborts) |
| `user_prompt_submit` | after the prompt is stored, before the model sees it | yes |
| `pre_tool_use` | before the approval gate and the tool | yes, and can rewrite arguments |
| `post_tool_use` | the tool returned successfully | no, but can replace the result the model reads |
| `post_tool_use_failure` | the tool returned an error | no, but can replace the error text |
| `permission_request` | the human gate is about to ask | yes |
| `pre_compact` / `post_compact` | around a context compaction | `pre_` only |
| `subagent_start` / `subagent_stop` | around a delegated agent | no |
| `stop` | the turn produced its final reply | yes |
| `session_end` | the session is closing | no |
| `notification` | operator-visible notice | no |

## Input

One JSON document on stdin. Only the fields that make sense for the event are present.

```json
{
  "event": "pre_tool_use",
  "cwd": "/home/me/project",
  "project_dir": "/home/me/project",
  "session_id": "e4f1…",
  "tool": "terminal",
  "arguments": { "command": "git push --force" }
}
```

Also exported as environment: `LUNA_HOOK_EVENT`, `LUNA_HOOK_TOOL`, `LUNA_PROJECT_DIR`. Hooks run
with the workspace as their working directory.

## Output

| outcome | effect |
|---|---|
| exit `0`, no output | the event proceeds |
| exit `0`, `{"decision":"deny","reason":"…"}` | refused; `reason` reaches the model as a tool error |
| exit `0`, `{"updatedInput":{…}}` | the tool runs with replacement arguments (`pre_tool_use` only) |
| exit `0`, `{"content":"…"}` | replaces what the model reads, without changing what the tool did (`post_tool_use` / `post_tool_use_failure` only) |
| exit `2` | refused; stderr (or stdout) is the reason |
| other exit code | recorded as a hook error; the event proceeds |
| timeout | a *gating* event (`pre_tool_use`, `permission_request`) is refused; anything else is only logged |

`{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"…"}}` is accepted
too, since that is the shape other harnesses document.

Non-empty output that is not a decision (an unescaped newline in a `printf`, a stray log line) is
reported as a warning and the event proceeds — a hook that believes it decided something should not
be silent about it.

### Redacting a tool result

`post_tool_use` can replace the text the model reads while leaving the tool's real effect alone —
which is what a redaction or truncation hook needs:

```json
{ "hooks": { "post_tool_use": [
  { "matcher": "terminal", "command": ".luna/hooks/redact-secrets.sh" }
] } }
```

```sh
# .luna/hooks/redact-secrets.sh — reads the event JSON on stdin
python3 -c 'import json,sys,re; e=json.load(sys.stdin); print(json.dumps({"content": re.sub(r"AKIA[0-9A-Z]{16}", "[redacted]", e["message"])}))'
```

The transcript, and therefore the next model call, carries the hook's text. A payload with no
`content` key is ignored rather than blanking the result.

## Which refusals actually stop something

Exit `2` is honoured everywhere, but only three turn-level events can act on it:
`user_prompt_submit` (the prompt is refused), `stop` (the turn does not end) and `pre_compact`
(compaction is refused). The rest are observations — a `notification` or `session_end` hook that
exits `2` is logged and the turn continues, because an observer must not be able to fail what it was
only watching. `pre_tool_use` and `permission_request` gate the call they are attached to.

## Every surface runs them

`luna chat` and `luna exec` load the same hooks from the same files, and both bracket a turn with
`session_start` / `session_end`. A guardrail that only arms in the interactive REPL is off exactly
where nobody is watching, so `luna exec` is not a separate policy.

## Inspect what is loaded

```bash
luna hooks          # files read, whether they are trusted, every bound hook
luna hooks --json   # the same, machine-readable
```

This reads the *runner*, not the files: a hook in `~/.luna/hooks.json` is not armed if
`hooks.enabled` is false, if its event name is a typo, if the event is in `hooks.disabled`, or if it
lives in a project file that is not trusted. The listing shows what will actually run.

## What hooks cannot do

A hook can refuse a call and can rewrite its arguments. It **cannot approve** a mutating call that
the human gate would otherwise refuse: the gate runs after the hooks, always. Hooks narrow, the
gate decides.
