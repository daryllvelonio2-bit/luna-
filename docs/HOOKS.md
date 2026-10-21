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
| `session_start` | the chat session is created | yes (the run aborts) |
| `user_prompt_submit` | after the prompt is stored, before the model sees it | yes |
| `pre_tool_use` | before the approval gate and the tool | yes, and can rewrite arguments |
| `post_tool_use` | the tool returned successfully | no |
| `post_tool_use_failure` | the tool returned an error | no |
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
| exit `2` | refused; stderr (or stdout) is the reason |
| other exit code | recorded as a hook error; the event proceeds |
| timeout | a *gating* event (`pre_tool_use`, `permission_request`) is refused; anything else is only logged |

`{"hookSpecificOutput":{"permissionDecision":"deny","permissionDecisionReason":"…"}}` is accepted
too, since that is the shape other harnesses document.

## What hooks cannot do

A hook can refuse a call and can rewrite its arguments. It **cannot approve** a mutating call that
the human gate would otherwise refuse: the gate runs after the hooks, always. Hooks narrow, the
gate decides.
