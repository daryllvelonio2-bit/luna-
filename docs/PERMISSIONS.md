# Permission rules

The approval gate asks a human about one call at a time. That is right for a decision and wrong for
a policy: a person who has approved `cargo test` twenty times approves the twenty-first without
reading it, and an agent that stops to ask about every read is an agent nobody runs unattended.

Permission rules are the standing answer. They are evaluated **before** the human gate:

```yaml
# ~/.luna/config.yaml
tools:
  permissions:
    - action: allow
      tool: "read_*"

    - action: allow
      tool: terminal
      arguments: { command: "cargo *" }

    - action: deny
      tool: terminal
      arguments: { command: "*git push --force*" }
      reason: force-push is never run by the agent

    - action: ask
      tool: terminal
```

## What a rule is

| field | meaning |
|---|---|
| `action` | `allow` (run without asking) · `ask` (always prompt) · `deny` (never run) |
| `tool` | glob over the tool name — `*`, `terminal`, `write_*`, `?ead_file` |
| `arguments` | optional; every entry must hold. The value is a glob over the argument's string form (a non-string argument is matched as its compact JSON) |
| `reason` | shown to the model and logged when this rule denies |

A `!` prefix inverts an argument pattern: `{"command": "!cargo *"}` means "anything that is not a
`cargo` command".

## Precedence: `deny` > `ask` > `allow`

Order in the file does not matter. If any matching rule denies, the call is denied; else if any
matching rule asks, the human is asked; else if any matching rule allows, it runs; else the gate's
default applies (mutating tools prompt, read-only tools run).

The asymmetry is deliberate. A guardrail that a later, broader `allow` can switch off is not a
guardrail, so `allow` only quiets the calls no `deny`/`ask` also matches — which is what a rule
author means by writing both.

A call matched by no rule behaves exactly as it did before rules existed, so adding an empty
`permissions:` list changes nothing.

## The other knob: the filesystem sandbox

Rules decide whether a human is asked. `paths.workspace_root` decides what a call can *reach*, and
it applies before the gate and independently of it — `--yes` approves a mutation, it does not widen
the sandbox.

```yaml
paths:
  workspace_root: ~/projects/app   # the sandbox: reads and writes stay inside
  extra_roots:                     # optional, empty by default
    - /tmp
    - /mnt/scratch
```

Every filesystem tool (`read_file`, `write_file`, `patch`, `search_files`, `list_files`) and the
`workdir` of `terminal` resolves through `tools::sandbox::Sandbox`, which:

- folds `.` and `..` lexically, so climbing out with `../../etc/passwd` is refused;
- **canonicalizes the deepest existing ancestor**, then appends the rest — so `ws/link -> /etc`
  cannot smuggle `ws/link/passwd` out (a purely lexical check would allow it);
- compares by path component, so a sibling directory named `ws-evil` is not inside `ws`;
- expands `~` *before* checking, so `~/.ssh/id_rsa` is outside the workspace rather than a hole;
- refuses an empty path instead of treating it as the workspace root.

`/tmp` is **not** special-cased: it is shared with every other process on the machine, so reaching
it is an explicit `extra_roots` entry. The resolved path is the canonicalized one, so the path a
tool writes to is the path the check looked at.

`terminal` itself is a shell: once a command runs, its reach is the process's reach. The sandbox
bounds the `workdir` it starts in and the tools that take a path, not what `sh -c` does afterwards —
that is what the approval gate and the permission rules above are for.

## What rules do not do

A rule decides **whether the human is asked**. It does not decide **what a call can reach**: an
`allow` skips the prompt, it does not grant filesystem or network reach the tool did not already
have. Two independent knobs, as elsewhere in the harness — a prompt is not a boundary, and a
boundary is not a policy.

Rules are also not a substitute for hooks: rules are declarative and evaluated by the harness,
hooks are programs you run at an event (see [`HOOKS.md`](HOOKS.md)).

## Validation

A rule with an empty `tool` pattern, or with an empty argument pattern, is rejected at startup
instead of being stored and silently matching nothing. Unknown fields are rejected too, so a typo
(`arguements:`) fails loudly rather than disabling the guardrail it was meant to add.

## Try it

```bash
luna config set tools.permissions.0.action deny     # write rules with config set, or edit the file
luna config get tools.permissions.0.tool
luna eval run --suite tests/eval --offline          # permission-rule-deny / permission-rule-allow
```
