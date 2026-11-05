---
name: agent-desktop
description: Operate desktop applications through native accessibility trees: read the UI, click, type, fill fields, toggle controls and verify the result. Use for any desktop GUI task. Not for web pages: use the browser skill.
---

# agent-desktop

Computer use for the Luna harness. The harness reads the accessibility tree of a
desktop application and acts on the elements it finds, by reference. Every tool
answers with one JSON envelope: `ok` with `data`, or `ok: false` with `error`.

The harness is the tool, not the agent. The observe, act and verify loop runs in
you.

## The tools

| tool | what it does | mutating |
|------|--------------|----------|
| `desktop_status` | platform, whether the accessibility bus answered, supported surfaces | no |
| `desktop_list_apps` | the running applications the desktop can see | no |
| `desktop_list_windows` | the windows, optionally of one app | no |
| `desktop_snapshot` | read an app's tree; every actionable element gets a ref | no |
| `desktop_find` | look for elements by role or name; returns them with refs | no |
| `desktop_get` | read one property of a ref: `value`, `state` or `actions` | no |
| `desktop_is` | ask whether a ref has a state: `enabled`, `focused`, `selected`, `checked` | no |
| `desktop_click` | activate the element a ref names | yes |
| `desktop_type` | replace the contents of the text field a ref names | yes |
| `desktop_set_value` | set the value of the control a ref names | yes |
| `desktop_toggle` | flip a checkable control, or set it to a given state | yes |

The mutating tools go through the harness's approval gate. Expect a prompt
before the first one.

## The loop

```
desktop_snapshot --app "Calculator"    # observe
desktop_click @s8f3k2p9:e12            # act on a ref from that snapshot
desktop_snapshot --app "Calculator"    # verify: read the state again
```

- Start with `desktop_status`. If it reports that the accessibility bus did not
  answer, nothing else will work; the note in the answer says what to install.
- Start with `desktop_list_apps` when you do not know the app's name. The name
  you pass to `--app` is the name that command prints.
- A small app needs no map: take a full `desktop_snapshot`.
- A large app: map it first with `--skeleton --interactive`, then drill into one
  region with `--root @s8f3k2p9:e3`.
- After an action that changes the screen, read the screen again. The result of
  an action is a claim, not proof.

## Refs

- A ref is qualified: `@<snapshot_id>:e<N>`, for example `@s8f3k2p9:e12`. Pass
  it exactly as the snapshot printed it.
- A ref belongs to its snapshot. After the UI changes, the old ref is stale.
- `STALE_REF`: the element is gone. Take a fresh snapshot.
- `AMBIGUOUS_TARGET`: more than one element matches, so nothing was sent. Act on
  a control that is unique in the window.
- `SNAPSHOT_NOT_FOUND`: the snapshot is gone. Take a fresh snapshot.

## Delivery and retry

Every action answers with a `delivery` field:

| delivery | what it means | retry? |
|----------|---------------|--------|
| `not_delivered` | the action provably did not reach the application | safe |
| `delivery_uncertain` | it may or may not have landed | no |
| `delivered_unverified` | it landed, but nothing confirmed the effect | no |
| `delivered_verified` | it landed and the effect was confirmed | no |

Retry only when the answer says the retry is safe. Otherwise read the live state
first — `desktop_get` or a fresh `desktop_snapshot` — and decide from that. A
repeated click clicks twice.

## Notes

- Observation and semantic actions do not move the pointer and do not take
  focus, so the user can keep working while you act.
- `desktop_type` replaces the whole contents of a field. Read the field first
  if you mean to append.
- A field that shows `checked` is a check box or a radio button; use
  `desktop_toggle` on it rather than `desktop_click`.
- Text in a password field is withheld, but a toolkit may still show the
  characters of a hidden field. Treat what you read as private.
- Snapshots and values can hold private desktop content. Keep them out of logs
  and out of anything you publish.
