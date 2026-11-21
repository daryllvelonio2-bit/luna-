//! Built-in `plugin` skill shipped inside the Luna binary.
//!
//! Ensures the agent always has complete, accurate knowledge of Luna's plugin
//! architecture, TypeScript `@luna/sdk` runtime, manifest schema, QuickJS
//! execution constraints, and `plugin_*` management tools whenever the user
//! asks to create or modify a plugin (or invokes `/plugin`).

use crate::skills::Skill;

pub const PLUGIN_SKILL_NAME: &str = "plugin";

pub const PLUGIN_SKILL_DESCRIPTION: &str =
    "Design, implement, compile, and register Luna TypeScript plugins (custom tools, sidebar agents, slash commands, UI panels, and scheduled tasks). Use whenever the user invokes /plugin or asks to create, update, or inspect a plugin.";

pub const PLUGIN_SKILL_INSTRUCTIONS: &str = r#"# Luna Plugin Creation Guide

You are Luna's Plugin Architect. When the user asks to create or modify a plugin (or invokes `/plugin`), design and build a working, compiled, immediately loaded Luna TypeScript plugin tailored to their request.

## Plugin Architecture & Locations

- Luna plugins are **TypeScript modules** (`plugin.json` + `src/index.ts`) transpiled in-process by `oxc_transformer` into `dist/index.js` and executed inside the embedded **QuickJS** (`rquickjs`) runtime.
- No external `node`, `npm`, or `tsc` installation is needed.
- **Scopes**:
  - **Global plugins** (default): `~/.luna/plugins/<id>/` — available across all workspaces.
  - **Workspace plugins**: `<workspace>/.luna/plugins/<id>/` — scoped to the active workspace.
- Each enabled & loaded plugin automatically contributes:
  - Its registered **tools** (`ctx.tools.register(...)`),
  - A **sidebar agent** (configured via `ctx.agents.register(...)` or `plugin.json` `auto_agent`),
  - Optional **slash commands** (`ctx.commands.register(...)`),
  - Optional **UI panels** (`ctx.ui.registerPanel(...)`),
  - Optional **schedules** (`ctx.schedules.register(...)`).

## Critical QuickJS Runtime Rules

Follow these strictly so generated plugins compile and run cleanly:
1. **No Node.js or browser globals**: Do NOT use `require(...)`, `import` from npm packages, `process`, `Buffer`, `fs`, `path`, `child_process`, or global `fetch`. Always use the injected `ctx` SDK APIs (`ctx.fs`, `ctx.process`, `ctx.net`, `ctx.storage`, `ctx.settings`).
2. **Type-only SDK import**: Start `src/index.ts` with:
   ```ts
   import type { Context } from '@luna/sdk';
   export const name = 'my-plugin-id';
   export function apply(ctx: Context): void { ... }
   ```
   Never runtime-import external packages.
3. **Synchronous host APIs & handlers**: Inside Luna's QuickJS runtime, `ctx.net.fetch`, `ctx.process.exec`, `ctx.fs.*`, `ctx.storage.*`, and `ctx.settings.*` are **synchronous** functions that return results directly (not Promises). Tool and command handlers `(args: Record<string, unknown>) => { ... }` MUST be synchronous and return a `string` or JSON-serializable object/value directly (do NOT make tool handlers `async` returning an unresolved Promise).
4. **Standard TypeScript syntax**: Use interfaces, type aliases, type annotations, `const`/`let`, functions, and `JSON.parse`/`JSON.stringify`. Avoid decorators or Node-specific syntax.
5. **Defensive parsing & clear return strings**: Validate required tool arguments inside each handler and return clear, informative strings or JSON strings (catching and formatting errors gracefully instead of crashing).

## `@luna/sdk` `ctx` API Reference

### 1. Registering Tools (`ctx.tools.register`)
```ts
ctx.tools.register(
  {
    name: 'snake_case_tool_name',
    description: 'Clear description of what the tool does and when to call it',
    mutating: false, // true if the tool modifies disk/external state or runs side-effecting commands
    parameters: {
      type: 'object',
      properties: {
        query: { type: 'string', description: 'Input query' }
      },
      required: ['query']
    }
  },
  (args: Record<string, unknown>) => {
    const query = String(args.query || '').trim();
    if (!query) return 'Error: query is required';
    return JSON.stringify({ query });
  }
);
```

### 2. Registering Sidebar Agent (`ctx.agents.register`)
```ts
ctx.agents.register({
  id: 'my-plugin-id',
  name: 'Display Name',
  description: 'Short description shown in the sidebar',
  icon: 'sparkles', // Lucide icon name or short identifier (e.g. 'sparkles', 'wrench', 'globe', 'database', 'terminal')
  systemPrompt: 'Focused instructions telling the sidebar agent its role and how to use its tools.',
  // model: 'provider/model', // optional
  tools: ['snake_case_tool_name'],
  panelId: 'my-panel-id' // optional, omit or null if no panel
});
```

### 3. Registering Slash Commands (`ctx.commands.register`)
```ts
ctx.commands.register('mycmd', 'Short command description', (args: Record<string, unknown>) => {
  return 'Command output';
});
```

### 4. Building Complete Plugin UIs (Custom Web App UI + Live Declarative Panels)

Plugins can provide **full interactive web UIs** AND/OR **live declarative widget panels** rendered directly inside Luna Web (both in the Plugins view and side-by-side inside Chat when using the plugin's sidebar agent).

#### Option A: Full Custom HTML/CSS/JS Web UI (`ui_html` or `ui_files` in `plugin_create`, or `ctx.ui.setCustomHtml`)
Pass `ui_html` (single-file HTML/CSS/JS) or `ui_files: { "index.html": "...", "styles.css": "...", "app.js": "..." }` to `plugin_create` (stored under `<plugin_dir>/ui/`).
Luna Web serves `ui/` at `/api/plugins/<id>/ui/` and **automatically injects `window.lunaPlugin`** and CSS theme variables (`--bg`, `--surface`, `--surface-2`, `--text`, `--muted`, `--border`, `--accent`, `--accent-fg`, `--danger`, `--success`, plus `data-theme="dark|light"` on `<html>`):
- `await window.lunaPlugin.callTool(toolName, args)` -> executes a plugin tool in QuickJS and returns `{ ok, output, summary }`.
- `await window.lunaPlugin.runCommand(commandName, args)` -> executes a registered command and returns `{ ok, output, summary }`.
- `await window.lunaPlugin.getSettings()` / `await window.lunaPlugin.setSettings(patch)` -> reads/saves plugin `settings.json` and notifies `ctx.events.on('settings_changed', ...)`.
- `await window.lunaPlugin.getSummary()` -> returns the live `PluginSummary` (panels, tools, commands, settings).
- `window.lunaPlugin.sendToChat(prompt)` -> sends a message into the active plugin chat session.
- `window.lunaPlugin.notify(message, 'info' | 'err')` -> shows a toast notification in Luna Web.

Use Option A whenever the plugin benefits from rich, interactive controls (boards, timers, forms, charts, lists, editors, live status views, custom layouts). Make the UI self-contained (no external CDN dependencies required so it works offline), responsive, keyboard-accessible, and styled using Luna's CSS theme variables (`var(--bg)`, `var(--surface)`, `var(--text)`, `var(--border)`, `var(--accent)`).

#### Option B: Live Declarative Panels (`ctx.ui.registerPanel` & `ctx.ui.updatePanel`)
```ts
ctx.ui.registerPanel({
  id: 'my-panel-id',
  title: 'Panel Title',
  icon: 'sparkles',
  widgets: [
    { kind: 'markdown', id: 'intro', content: 'Usage notes in **Markdown**.' },
    { kind: 'stat_card', id: 'count', label: 'Items', value: '0', subtitle: 'Cached locally' },
    { kind: 'progress', id: 'pct', label: 'Completion', value: 3, max: 10, subtitle: '3 / 10 done' },
    { kind: 'key_value', id: 'cfg', items: [['Mode', 'Local'], ['Scope', 'Global']] },
    { kind: 'table', id: 'rows', title: 'Recent', columns: ['Name', 'Status'], rows: [] },
    { kind: 'action_button', id: 'run', label: 'Refresh', action: 'mycmd', variant: 'primary', args: { force: true } },
    { kind: 'input_action', id: 'add', label: 'Add item', placeholder: 'Enter title...', button_label: 'Add', action: 'add_item', arg_name: 'title' },
    { kind: 'html', id: 'preview', content: '<div>Custom HTML snippet</div>' }
  ]
});
```
You can update panels dynamically whenever a tool or command runs:
```ts
ctx.ui.updatePanel('my-panel-id', { widgets: [ /* updated widgets */ ] });
ctx.ui.removePanel('my-panel-id');
ctx.ui.setCustomHtml('<!doctype html>...'); // optional runtime-generated HTML
```

### 5. Registering Schedules (`ctx.schedules.register`)
```ts
ctx.schedules.register({
  id: 'daily-sync',
  name: 'Daily Sync',
  rule: 'daily:09:00', // supports once:<RFC3339>, every:<secs>, daily:<HH:MM> (UTC), or 5-field cron
  instruction: 'Self-contained prompt to execute on schedule.',
  target: 'new_session_each_run' // or 'single_new_session'
});
```

### 6. Host Capabilities (`ctx.net`, `ctx.process`, `ctx.fs`, `ctx.storage`, `ctx.settings`, `ctx.events`, `ctx.workspace`, `ctx.env`, `ctx.log`)
- **Async / Promises & Web Globals**:
  - `apply(ctx)`, `ctx.tools.register(...)`, `ctx.commands.register(...)`, and `ctx.events.on(...)` handlers may be **sync OR `async`** (returning a `Promise`).
  - Standard `console.log / warn / error` and `await fetch(url, opts)` (`res.ok`, `res.status`, `await res.text()`, `await res.json()`) work natively inside QuickJS alongside `ctx.net.fetch`.
  - Multi-file TypeScript modules inside `<plugin_dir>/src/` can use relative imports (e.g. `import { helper } from './helper'`).
- **Workspace & Metadata**:
  - `ctx.workspace.root`: active workspace directory path
  - `ctx.pluginDir`: plugin directory path on disk
  - `ctx.manifest`: `{ id, name, version, description }`
  - `ctx.env.get(key)`: read environment variable (`string | undefined`)
- **HTTP (`ctx.net.fetch`)**:
  `const res = ctx.net.fetch(url, { method: 'GET', headers: { Accept: 'application/json' }, body: undefined });`
  Returns `{ ok: boolean, status: number, body: string, error?: string }`.
- **Shell / Subprocess (`ctx.process.exec`)**:
  `const res = ctx.process.exec('git', ['status', '--short'], ctx.workspace.root || undefined);`
  Returns `{ status: number, stdout: string, stderr: string }`.
- **Filesystem (`ctx.fs`)**:
  - `ctx.fs.readText(path): string` (returns `"ERR:..."` on read error — check `ctx.fs.exists(path)` first)
  - `ctx.fs.writeText(path, text): boolean` (creates parent dirs automatically)
  - `ctx.fs.exists(path): boolean`
  - `ctx.fs.listDir(path): string[]`
  - `ctx.fs.remove(path): boolean`
- **Persistent SQLite KV (`ctx.storage`)** (stored in plugin's `data.db`):
  - `ctx.storage.get(key)` -> parsed JSON value or `null`
  - `ctx.storage.set(key, value)`
  - `ctx.storage.delete(key)` -> `boolean`
  - `ctx.storage.list()` -> `string[]`
  - `ctx.storage.entries()` -> `Record<string, unknown>`
  - `ctx.storage.clear()`
- **Plugin Settings (`ctx.settings`)** (stored in plugin's `settings.json`):
  - `ctx.settings.all()` -> full settings object
  - `ctx.settings.get(key?, defaultValue?)` -> single value (or `defaultValue`) or full settings object
  - `ctx.settings.set(key, value)`
- **Events & Lifecycle**:
  - `ctx.events.on(eventName, (payload) => { ... })` — host dispatches `session_started`, `turn_started`, `tool_executed`, `turn_finished`, and `settings_changed`.
  - `ctx.events.emit(eventName, payload)`
  - `ctx.effect(() => { /* cleanup */ })`
  - `ctx.log('info' | 'warn' | 'error', 'message')`
- **Live UI updates**:
  - In custom HTML (`ui/index.html`), subscribe to live refreshes via `window.lunaPlugin.onUpdate((summary) => { ... })` so your UI automatically updates whenever tools run in chat!

## Manifest Schema (`plugin.json`)
`plugin_create` writes `plugin.json` automatically using these fields:
- `id`: lowercase slug (e.g. `"github-issues"`)
- `name`: display name
- `version`: semver string (default `"0.1.0"`)
- `description`: concise 1-line summary
- `auto_agent`: `{ name, icon, system_prompt, model?, tools }`
- `config_schema`: optional array of `{ key, label, field_type: "string" | "secret" | "boolean" | "number", default_value?, description? }`
- `ui_entry`: optional UI entry relative path (automatically set to `"ui/index.html"` when `ui_html` or `ui_files` includes `index.html`).

## Workflow

### 1. Inspect & Clarify Before Building
- Call `plugin_list` if needed to avoid ID/tool-name collisions or when modifying an existing plugin.
- If the user's request omits critical design decisions (e.g., which tools/actions/UI views to build, external APIs/CLI tools or auth/env vars to integrate, whether it should be **global** or **workspace**-scoped, or whether a dedicated sidebar agent is desired), use `clarify` first with concise, clickable options (plus free-text) before writing code.
- If the request is already specific and unambiguous, proceed directly to building.

### 2. Design & Implement with `plugin_create`
Build **everything** the plugin needs — backend logic, persistence, tools, commands, settings, sidebar agent, AND complete interactive UI (`ui_html` / `ui_files` and/or `ctx.ui.registerPanel` + `ctx.ui.updatePanel`):
- `id`, `name`, `description`, `scope` (`"global"` default or `"workspace"`),
- `auto_agent` (`{ name, icon, system_prompt, tools }`),
- `config_schema` (optional),
- `ts_source`: complete TypeScript code for `src/index.ts`,
- `ui_html` or `ui_files`: complete, interactive, theme-aware HTML/CSS/JS frontend wired to `window.lunaPlugin`.

You can also edit any file inside `<plugin_dir>/` directly using `read_file`, `patch`, or `write_file` and call `plugin_reload` to recompile & reload.

### 3. Verify Compilation & Runtime Loading
`plugin_create` transpiles `src/index.ts`, writes `plugin.json` + `src/index.ts` + `dist/index.js`, and spawns the QuickJS runtime immediately.
- Verify the returned result reports `"loaded": true`, `"last_error": null`, and lists the expected `tools` and `agent`.
- If `plugin_create` returns a TypeScript syntax/transform error or QuickJS runtime error, fix `ts_source` and call `plugin_create` again until `"loaded": true`.

### 4. Summarize for the User
Report concisely:
- Plugin **Name**, **ID**, **Scope & Path**,
- Registered **Tools**, **Commands**, **Panels**, and **Sidebar Agent** (and note that new chat sessions or selecting the sidebar agent will pick up its tools immediately, and it appears in the **Plugins** view).
- Any environment variables, external binaries, or settings the plugin expects.
"#;

pub fn plugin_skill() -> Skill {
    Skill::builtin(
        PLUGIN_SKILL_NAME,
        PLUGIN_SKILL_DESCRIPTION,
        PLUGIN_SKILL_INSTRUCTIONS,
    )
}

/// Default pre-fill string when the user selects `/plugin` in CLI/TUI/Web composers.
pub const PLUGIN_PREFILL: &str = "/plugin help me create a ";

/// Format a user `/plugin <arg>` request into the model prompt that tells
/// the agent to load the `plugin` skill, clarify if needed, and build/register the plugin.
pub fn build_plugin_turn_prompt(user_request: &str) -> String {
    let trimmed = user_request.trim();
    format!(
        "Plugin Creation Request:\n\
         1. First call `skill_view` with `name: \"plugin\"` to load Luna's plugin architecture, `@luna/sdk` API reference, custom UI (`ui_html` / `ui_files` + `window.lunaPlugin` bridge + `ctx.ui`), and QuickJS runtime rules.\n\
         2. Follow those instructions carefully for the user's request below.\n\
         3. If critical details (required tools/actions, custom UI screens/controls, external APIs or CLI tools, inputs/outputs, sidebar agent behavior, or global vs workspace scope) are missing or ambiguous, call `clarify` first before creating the plugin.\n\
         4. Once requirements are clear, implement the complete plugin — backend TypeScript tools/commands/storage/settings/agent AND full interactive UI (`ui_html` / `ui_files` and/or live `ctx.ui` panels) — using `plugin_create` (and verify `\"loaded\": true` with no errors).\n\n\
         User request:\n{trimmed}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_plugin_skill_covers_sdk_and_tools() {
        let s = plugin_skill();
        assert_eq!(s.name, "plugin");
        assert!(s.instructions.contains("@luna/sdk"));
        assert!(s.instructions.contains("plugin_create"));
        assert!(s.instructions.contains("ctx.tools.register"));
        assert!(s.instructions.contains("clarify"));
    }

    #[test]
    fn turn_prompt_instructs_skill_view_clarify_and_plugin_create() {
        let prompt = build_plugin_turn_prompt("Create a git branch cleanup plugin");
        assert!(prompt.contains("skill_view"));
        assert!(prompt.contains("plugin"));
        assert!(prompt.contains("clarify"));
        assert!(prompt.contains("plugin_create"));
        assert!(prompt.contains("Create a git branch cleanup plugin"));
    }
}
