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

### 4. Registering UI Panels (`ctx.ui.registerPanel`)
```ts
ctx.ui.registerPanel({
  id: 'my-panel-id',
  title: 'Panel Title',
  icon: 'sparkles',
  widgets: [
    { kind: 'markdown', id: 'intro', content: 'Usage notes in **Markdown**.' },
    { kind: 'stat_card', id: 'count', label: 'Items', value: '0', subtitle: 'Cached locally' },
    { kind: 'key_value', id: 'cfg', entries: [['Mode', 'Local'], ['Scope', 'Global']] },
    { kind: 'table', id: 'rows', title: 'Recent', columns: ['Name', 'Status'], rows: [] },
    { kind: 'action_button', id: 'run', label: 'Refresh', action: 'mycmd', variant: 'primary' }
  ]
});
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

### 6. Host Capabilities (`ctx.net`, `ctx.process`, `ctx.fs`, `ctx.storage`, `ctx.settings`, `ctx.events`, `ctx.log`)
- **HTTP (`ctx.net.fetch`)**:
  `const res = ctx.net.fetch(url, { method: 'GET', headers: { Accept: 'application/json' }, body: undefined });`
  Returns `{ ok: boolean, status: number, body: string, error?: string }`.
- **Shell / Subprocess (`ctx.process.exec`)**:
  `const res = ctx.process.exec('git', ['status', '--short'], '/path/to/cwd');`
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
- **Plugin Settings (`ctx.settings`)** (stored in plugin's `settings.json`):
  - `ctx.settings.get(key?)` -> single value or full settings object
  - `ctx.settings.set(key, value)`
- **Events & Lifecycle**:
  - `ctx.events.on(eventName, (payload) => { ... })` / `ctx.events.emit(eventName, payload)`
  - `ctx.effect(() => { /* cleanup */ })`
  - `ctx.log('info' | 'warn' | 'error', 'message')`

## Manifest Schema (`plugin.json`)
`plugin_create` writes `plugin.json` automatically using these fields:
- `id`: lowercase slug (e.g. `"github-issues"`)
- `name`: display name
- `version`: semver string (default `"0.1.0"`)
- `description`: concise 1-line summary
- `auto_agent`: `{ name, icon, system_prompt, model?, tools }`
- `config_schema`: optional array of `{ key, label, field_type: "text" | "secret" | "boolean" | "number", required, default_value?, description? }`

## Workflow

### 1. Inspect & Clarify Before Building
- Call `plugin_list` if needed to avoid ID/tool-name collisions or when modifying an existing plugin.
- If the user's request omits critical design decisions (e.g., which tools/actions to expose, external APIs/CLI tools or auth/env vars to integrate, whether it should be **global** or **workspace**-scoped, or whether a dedicated sidebar agent/panel is desired), use `clarify` first with concise, clickable options (plus free-text) before writing code.
- If the request is already specific and unambiguous, proceed directly to building.

### 2. Design & Implement with `plugin_create`
Call `plugin_create` with:
- `id`, `name`, `description`, `scope` (`"global"` default or `"workspace"`),
- `auto_agent` (`{ name, icon, system_prompt, tools }`),
- `config_schema` (optional),
- `ts_source`: complete TypeScript code for `src/index.ts`.

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
pub const PLUGIN_PREFILL: &str = "/plugin guide me with creating a plugin ";

/// Format a user `/plugin <arg>` request into the model prompt that tells
/// the agent to load the `plugin` skill, clarify if needed, and build/register the plugin.
pub fn build_plugin_turn_prompt(user_request: &str) -> String {
    let trimmed = user_request.trim();
    format!(
        "Plugin Creation Request:\n\
         1. First call `skill_view` with `name: \"plugin\"` to load Luna's plugin architecture, `@luna/sdk` API reference, and QuickJS runtime rules.\n\
         2. Follow those instructions carefully for the user's request below.\n\
         3. If critical details (required tools/actions, external APIs or CLI tools, inputs/outputs, sidebar agent behavior, or global vs workspace scope) are missing or ambiguous, call `clarify` first before creating the plugin.\n\
         4. Once requirements are clear, implement, compile, and register the plugin using `plugin_create` (and verify `\"loaded\": true` with no errors).\n\n\
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
