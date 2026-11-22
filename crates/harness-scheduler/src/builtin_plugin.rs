//! Built-in DeepSeek-style `luna-scheduler` TypeScript plugin.
//!
//! Ensures users see **Scheduler** in the app sidebar out of the box as a real plugin
//! with its own agent prompt, schedule tools (`schedule_create`, `schedule_list`,
//! `schedule_view`, `schedule_edit`), profile tools (`profile_create`, `profile_list`,
//! `profile_view`, `profile_edit`, `profile_delete`), `sleep`, and Automation Tasks panel.

use harness_plugins::{AutoAgentSpec, PluginManager, PluginManifest};

pub const SCHEDULER_PLUGIN_ID: &str = "luna-scheduler";

const SCHEDULER_TS: &str = r#"
import type { Context } from '@luna/sdk';

export const name = 'luna-scheduler';

export function apply(ctx: Context): void {
  ctx.agents.register({
    id: 'luna-scheduler',
    name: 'Scheduler',
    description: 'Design flexible automations, specialized profiles/mindsets, working-time windows, trigger logic, and continuous sleep loops.',
    icon: 'calendar-clock',
    systemPrompt: [
      'You are Luna Scheduler, an automation and profile architect.',
      'Use schedule_create, schedule_list, schedule_view, and schedule_edit to manage tasks,',
      'profile_create, profile_list, profile_view, profile_edit, and profile_delete to craft specialized personas/mindsets/behaviors,',
      'and sleep for continuous/never-stopping loops.',
      'Supported rules: daily-local:<HH:MM>, local-cron:<5-field>, daily:<HH:MM> (UTC), every:<seconds>, once:<RFC3339>, continuous[:<cooldown_secs>], or 5-field UTC cron.',
      'Always consider dedicated profiles/mindsets, targeted working hours (active_window), trigger_command/trigger_logic, and continuous vs scheduled mode when designing workflows.'
    ].join(' '),
    tools: [
      'schedule_create',
      'schedule_list',
      'schedule_view',
      'schedule_edit',
      'profile_create',
      'profile_list',
      'profile_view',
      'profile_edit',
      'profile_delete',
      'sleep'
    ],
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
        content: 'Ask **Scheduler** to build flexible automations with custom **profiles/mindsets**, **active_window** working hours, **trigger logic**, **continuous** `sleep` loops, or rules like `daily-local:09:00`, `every:3600`, `continuous`, and `0 9 * * 1-5`.'
      }
    ]
  });
}
"#;

pub fn ensure_scheduler_plugin(manager: &PluginManager) -> Result<(), String> {
    let manifest = PluginManifest {
        id: SCHEDULER_PLUGIN_ID.to_string(),
        name: "Scheduler".to_string(),
        version: "0.2.0".to_string(),
        description:
            "Built-in flexible automations, custom profiles/mindsets & continuous workflows."
                .to_string(),
        entry: "src/index.ts".to_string(),
        compiled: "dist/index.js".to_string(),
        ui_entry: None,
        enabled: true,
        builtin: true,
        auto_agent: AutoAgentSpec {
            name: "Scheduler".to_string(),
            icon: "calendar-clock".to_string(),
            system_prompt: "You are Luna Scheduler. Help users create and manage flexible automations, specialized profiles/mindsets, working-time windows, trigger logic, and continuous sleep loops."
                .to_string(),
            model: None,
            tools: vec![
                "schedule_create".to_string(),
                "schedule_list".to_string(),
                "schedule_view".to_string(),
                "schedule_edit".to_string(),
                "profile_create".to_string(),
                "profile_list".to_string(),
                "profile_view".to_string(),
                "profile_edit".to_string(),
                "profile_delete".to_string(),
                "sleep".to_string(),
            ],
        },
        config_schema: Vec::new(),
        builder_spec: None,
    };

    manager.ensure_builtin_plugin(manifest, SCHEDULER_TS)
}
