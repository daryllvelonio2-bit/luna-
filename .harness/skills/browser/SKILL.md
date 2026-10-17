---
name: browser
description: "Browse websites, search the web, fetch clean page markdown, evaluate JavaScript, or take screenshots."
command: "python3 {skill_dir}/scripts/browse.py"
timeout_secs: 60
parameters:
  type: object
  properties:
    action:
      type: string
      description: "Browsing action: 'search' (web query), 'fetch' (fast HTTP read), 'open' (headless Chromium browser with JS), 'eval' (run JS in page), or 'screenshot' (capture image)."
      enum: ["search", "fetch", "open", "eval", "screenshot"]
    query:
      type: string
      description: "Search query or target URL"
    url:
      type: string
      description: "Target website URL"
    js_code:
      type: string
      description: "JavaScript expression to evaluate in the page (for 'eval')"
    screenshot_path:
      type: string
      description: "File path to save the screenshot image (for 'screenshot')"
    limit:
      type: integer
      description: "Maximum number of search results to return (default: 5)"
  required: ["action"]
---

# Browser Skill

A portable, modular web browser skill for Luna harness.

## Capabilities

1. **`search`**: Fast web search (DuckDuckGo) with zero external dependencies. Returns numbered results with titles, URLs, and snippet summaries.
2. **`fetch`**: Fast HTTP webpage text and markdown extraction. Strips boilerplate (scripts, ads, navigation) and extracts readable article text. Built-in SSRF protection blocks requests to internal network ranges or localhost.
3. **`open`**: Full headless Chromium browser via Playwright. Renders dynamic client-side JavaScript (SPAs, React/Vue apps), waits for DOM ready, and extracts rendered text.
4. **`eval`**: Evaluates custom JavaScript in the headless browser page context and returns the result.
5. **`screenshot`**: Captures a page screenshot to a designated local file path.

## CLI / Direct Usage

You can also run the underlying script directly via the `terminal` tool:
```bash
python3 {skill_dir}/scripts/browse.py search "rust lang 2026"
python3 {skill_dir}/scripts/browse.py fetch "https://news.ycombinator.com"
python3 {skill_dir}/scripts/browse.py open "https://example.com"
python3 {skill_dir}/scripts/browse.py screenshot "https://example.com" /tmp/example.png
```

## Changing Configuration

To customize user agent, timeouts, or search engine backend, edit `{skill_dir}/scripts/browse.py`.
