---
name: terminal-pty
description: "Run any program on a real pseudo-terminal and verify what a user actually sees: drive it with keystrokes, read the rendered screen, wait for output, assert on it, and capture PNG screenshots of the frame. Use for any terminal work — a TUI/REPL/CLI check, a UI change, a slash command, a panel, an onboarding flow, or any claim about user-visible behaviour that a unit test cannot see. Also exposed as MCP tools (`pty_*`) when registered in `mcp.servers`."
command: "python3 {skill_dir}/scripts/terminal_pty.py"
timeout_secs: 180
parameters:
  type: object
  properties:
    action:
      type: string
      description: "What to do: 'start' (launch a program on a pty), 'send' (type text + Enter), 'keys' (named keys: Ctrl+C, Up, Escape), 'wait' (block until a pattern is on screen), 'idle' (wait for the screen to settle), 'assert' (gate on a pattern being present/absent), 'screen' (read the rendered viewport), 'screenshot' (render the screen to a PNG), 'run' (drive a whole multi-step workflow in one call), 'list', 'close'."
      enum: ["start", "send", "keys", "wait", "idle", "assert", "screen", "screenshot", "run", "list", "close"]
    session_id:
      type: string
      description: "Target session from a previous 'start'/'run'. Omit when exactly one session is live; sessions survive across calls."
    text:
      type: string
      description: "Text to type (action 'send')."
    keys:
      type: string
      description: "Key spec for action 'keys', e.g. 'Ctrl+C', 'Down,Enter', 'char:/help,Enter'."
    pattern:
      type: string
      description: "Regex to look for on the rendered screen (actions 'wait', 'assert')."
    present:
      type: boolean
      description: "For 'assert': require the pattern present (default true) or absent (false)."
    steps:
      type: array
      description: "For 'run': ordered steps, each {\"action\": \"send|keys|wait|idle|assert|screen|screenshot|sleep\", ...}. Execution stops at the first failure."
      items:
        type: object
    argv:
      type: array
      description: "Command to launch (action 'start'/'run'), e.g. [\"luna\",\"--help\"], [\"htop\"], or [\"python3\",\"-i\"]. Defaults to $SHELL."
      items:
        type: string
    command:
      type: string
      description: "Shell-string alternative to argv, run via `sh -lc`."
    cwd:
      type: string
      description: "Working directory for the launched process."
    env:
      type: object
      description: "Extra environment variables for the child process."
    cols:
      type: integer
      description: "Terminal width (default 120). Narrow it to check wrapping."
    rows:
      type: integer
      description: "Terminal height (default 34)."
    label:
      type: string
      description: "Human label for the session; also becomes its id when free."
    path:
      type: string
      description: "Output .png path for 'screenshot' (default: a numbered file under ~/.terminal-pty/shots/)."
    scrollback:
      type: boolean
      description: "Include lines scrolled off the top (default false for 'screen')."
    ansi:
      type: boolean
      description: "Keep colour escape sequences in the screen text (default false)."
    timeout:
      type: number
      description: "Seconds to wait for 'wait'/'assert'/'start' (default 15-20)."
    quiet_ms:
      type: integer
      description: "Milliseconds of silence that counts as settled (default 250-400)."
    font_size:
      type: integer
      description: "Screenshot font size in pixels (default 15)."
  required: ["action"]
---

# terminal-pty

Drive any program on a live pseudo-terminal and verify what a user would see.
Nothing here is simulated: the program runs for real, a terminal emulator model
reconstructs the frame, and the screenshot is drawn from that frame. Use it for
a TUI, a REPL, a plain interactive CLI, a shell, a prompt-driven installer — any
surface whose truth is the screen rather than a unit test.

`luna` (this repo's TUI) is the running example below, but every action is
program-agnostic: pass the `argv` or `command` you want and drive it the same
way.

## Two ways in

**MCP** (preferred — full structured arguments). Register it in the harness
config (`~/.luna/config.yaml` for Luna):

```yaml
mcp:
  enabled: true
  servers:
    - name: terminal-pty
      command: python3
      args:
        - /path/to/repo/.harness/skills/terminal-pty/scripts/terminal_pty.py
        - mcp
      enabled: true
```

The tools register as `mcp__terminal-pty__pty_start`,
`..._pty_send`, `..._pty_screenshot`, `..._pty_assert`, `..._pty_run`, and so
on. Confirm the harness can see them with `luna mcp tools`.

**CLI** (same implementation, no MCP needed) — pass a JSON object on stdin or
`SKILL_ARGS`, or use flags:

```bash
python3 .harness/skills/terminal-pty/scripts/terminal_pty.py start --argv '["luna"]' --label chat
python3 .harness/skills/terminal-pty/scripts/terminal_pty.py screenshot --session chat
python3 .harness/skills/terminal-pty/scripts/terminal_pty.py close --session chat
```

Both talk to a background daemon over a unix socket
(`~/.terminal-pty/live.sock`), which is what lets a session outlive the call
that started it, and lets two agents watch the same terminal.
`terminal_pty.py shutdown` stops it.

Starting without `argv`/`command` launches `$SHELL`, so a bare `start` is a
generic interactive terminal; pass an explicit command for a specific target.

## The workflow that matters

One call, one verdict, one screenshot. Example: verify the Luna chat TUI:

```json
{
  "action": "run",
  "label": "luna-verify",
  "argv": ["luna"],
  "cols": 100, "rows": 28,
  "steps": [
    {"action": "assert", "pattern": "Luna is inactive", "note": "composer painted"},
    {"action": "assert", "pattern": "ctx:", "note": "status bar painted"},
    {"action": "send", "text": "/", "enter": false, "quiet_ms": 900, "note": "open slash picker"},
    {"action": "assert", "pattern": "/model", "note": "picker lists commands"},
    {"action": "screenshot", "note": "picker evidence"},
    {"action": "keys", "keys": "Escape", "note": "dismiss picker"},
    {"action": "send", "text": "exit", "note": "quit"},
    {"action": "wait", "pattern": "Resume: luna", "timeout": 20},
    {"action": "assert", "pattern": "Resume: luna --session", "note": "resume hint printed"}
  ]
}
```

The same shape works for any target — swap `argv` for `["htop"]`, a Python
REPL, or a `sh -lc` installer command, and change the patterns to that
program's text.

That is a verified run: 10/10 steps, ending in
`lunaverify-002-<timestamp>.png`. Any `assert` or `wait` failure stops the run,
sets `passed: false`, and returns the screen that failed it.

## Reading the result

- `screen` — the rendered viewport, exactly the characters a terminal would
  paint (no escape codes). `scrollback: true` prepends what scrolled off.
- `screenshot.path` — absolute PNG path; the image is drawn with box-drawing and
  Nerd Font glyphs, real 256-colour/true-colour backgrounds, and the cursor
  block. Also returned as `width`/`height`/`bytes`/`sha256`; compare the hash to
  prove two frames are identical.
- `assert.passed` / `run.passed` — the verdict. `found` and `elapsed_ms` say why.
- On the CLI, a failed `assert`, a timed-out `wait`, or a failed `run` exits **1**
  and prints the report (with the failing screen) to **stderr**. A verification
  that reports success when the screen disagrees is worse than no verification.

## Waiting, correctly

`idle` is the primitive to reach for: it returns once the child has produced no
output for `quiet_ms`, so a redraw finishes before you judge or photograph it.
Prefer `{"action": "idle"}` over `sleep` — a fixed delay either wastes time or
photographs a half-drawn frame. Use `wait` when a specific string is the signal.

## Notes and gotchas

- **Screenshots are drawn, not captured.** There is no window manager to ask, so
  the PNG is rendered from the terminal model. Glyphs the primary mono font
  lacks (`⏵ ⏸ ⏩ ✔ ✖` from Adwaita Mono, `📷` from Noto Symbols) fall back to a
  font that has them, because a real terminal does the same and tofu would hide
  the thing being verified.
- **Kitty keyboard sequences are filtered.** Programs using the kitty keyboard
  protocol (Luna writes `ESC[>1u` / `ESC[<u`) would make the emulator paint a
  literal `u` for the pop sequence — a character no user sees — so those
  sequences are stripped before they reach the model.
- **Dialogs use the alternate screen**, so approval/sudo/clarify popups are
  modelled as such: the dialog shows while it is up, and the transcript returns
  when it closes.
- **Luna-specific:** onboarding is skipped for driven sessions
  (`LUNA_FIRST_RUN=false`). A real onboarding verification should pass
  `env: {"LUNA_FIRST_RUN": ""}` to leave it on — and back up
  `~/.luna/credentials.json` first, because onboarding rewrites it.
- **Luna-specific:** the interactive chat needs a provider; `luna --help`,
  `luna mcp tools`, `luna config`, and `luna sessions` do not, which makes them
  cheap smoke tests.
- Sessions are cheap but not free: `close` when done. `close --all` reaps every
  session if one is left behind.
- Artifacts live under `~/.terminal-pty/` (`shots/`, `daemon.log`); override the
  location with `TERMINAL_PTY_DIR` (or the socket with
  `TERMINAL_PTY_SOCKET`).

## Verifying the tool itself

```bash
python3 .harness/skills/terminal-pty/scripts/terminal_pty.py selftest
```

16 checks over the whole path: daemon, pty spawn, UTF-8 box drawing, keystrokes,
`wait`, PNG validity and content, ANSI reconstruction, assert pass/fail/absent,
process reaping, and an MCP handshake (`initialize` → `tools/list` → `tools/call`,
including the namespaced `mcp__terminal-pty__pty_list` spelling the harness
sends).
