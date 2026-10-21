---
name: agent-browser
description: "Browser automation CLI (vercel-labs/agent-browser, native Rust). Use to interact with websites: navigate, fill forms, click, screenshot, extract data, test web apps, or run JS in a page. Complements the built-in 'browser' skill with full interaction (accessibility-tree snapshots + @eN refs) and compact text output."
command: "agent-browser"
timeout_secs: 120
---

# agent-browser

Fast native-Rust browser automation CLI for AI agents. Chrome/Chromium via CDP with
accessibility-tree snapshots and compact `@eN` element refs.

Installed globally: `npm i -g agent-browser` (binary at `~/.npm-global/bin/agent-browser`).
Chrome for Testing resolved automatically.

## Load the real guide first

This file is a stub. Before running commands, pull the canonical workflow content:

```bash
agent-browser skills get core          # workflows, patterns, troubleshooting
agent-browser skills get core --full   # full command reference + templates
```

## Quick start

```bash
agent-browser open example.com          # launch + navigate
agent-browser snapshot                  # accessibility tree with @eN refs
agent-browser click @e2                 # click by ref
agent-browser fill @e3 "user@x.com"     # fill by ref
agent-browser get text @e1              # read text
agent-browser screenshot page.png       # capture
agent-browser read [url]                # agent-readable text/markdown
agent-browser eval "document.title"     # run JS in page
agent-browser close
```

Selectors also supported: `agent-browser click "#submit"`, `agent-browser find role button click --name "Submit"`.

## Notes

- `screenshot --if-changed` skips unchanged images to save tokens.
- `--hide-scrollbars false` keeps native scrollbars visible in headless shots.
- Clicks fail early if another element (consent banner/modal) covers the target; dismiss it, re-snapshot, retry.
- On Linux containers pass `--args "--no-sandbox"` if the browser fails to launch.
