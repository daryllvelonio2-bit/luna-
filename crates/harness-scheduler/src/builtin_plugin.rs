//! Built-in DeepSeek-style `luna-scheduler` TypeScript plugin.
//!
//! Ensures users see **Scheduler** in the app sidebar out of the box as a real plugin
//! with its own agent prompt, schedule tools (`schedule_create`, `schedule_list`,
//! `schedule_view`, `schedule_edit`), and Automation Tasks panel.

use harness_plugins::{AutoAgentSpec, PluginManager, PluginManifest};

pub const SCHEDULER_PLUGIN_ID: &str = "luna-scheduler";

const SCHEDULER_TS: &str = r#"
import type { Context } from '@luna/sdk';

export const name = 'luna-scheduler';

export function apply(ctx: Context): void {
  ctx.agents.register({
    id: 'luna-scheduler',
    name: 'Scheduler',
    description: 'Create, inspect, pause, and run scheduled reminders and recurring agent automations.',
    icon: 'calendar-clock',
    systemPrompt: [
      'You are Luna Scheduler, an automation and reminder specialist.',
      'Use schedule_create, schedule_list, schedule_view, and schedule_edit to manage tasks.',
      'Supported rules: once:<RFC3339>, every:<seconds>, daily:<HH:MM> (UTC), or 5-field cron.',
      'Always confirm the exact rule, target session mode, and next run time concisely.'
    ].join(' '),
    tools: ['schedule_create', 'schedule_list', 'schedule_view', 'schedule_edit'],
    panelId: 'scheduler-overview'
  });

  ctx.ui.registerPanel({
    id: 'scheduler-overview',
    title: 'Automation Tasks',
    icon: 'calendar-clock',
    widgets: [
      {
        kind: 'markdown',
        id: 'scheduler-guide',
        content: 'Ask **Scheduler** to create reminders like `every:3600`, `daily:09:00`, `once:2026-11-20T09:00:00Z`, or `0 9 * * 1-5`.'
      }
    ]
  });
}
"#;

pub fn ensure_scheduler_plugin(manager: &PluginManager) -> Result<(), String> {
    let manifest = PluginManifest {
        id: SCHEDULER_PLUGIN_ID.to_string(),
        name: "Scheduler".to_string(),
        version: "0.1.0".to_string(),
        description: "Built-in scheduled reminders & recurring automation tasks.".to_string(),
        entry: "src/index.ts".to_string(),
        compiled: "dist/index.js".to_string(),
        enabled: true,
        builtin: true,
        auto_agent: AutoAgentSpec {
            name: "Scheduler".to_string(),
            icon: "calendar-clock".to_string(),
            system_prompt: "You are Luna Scheduler. Help users create and manage scheduled tasks."
                .to_string(),
            model: None,
            tools: vec![
                "schedule_create".to_string(),
                "schedule_list".to_string(),
                "schedule_view".to_string(),
                "schedule_edit".to_string(),
            ],
        },
        config_schema: Vec::new(),
        builder_spec: None,
    };

    manager.ensure_builtin_plugin(manifest, SCHEDULER_TS)
}
