export interface SessionRecord {
  id: string;
  name: string;
  title: string;
  created_at: string;
  updated_at: string;
  message_count: number;
  model: [string, string] | null;
}

export interface GuiSessionItem {
  record: SessionRecord;
  plugin_agent_id: string | null;
}

export interface PluginAgentEntry {
  id: string;
  plugin_id: string;
  name: string;
  description: string;
  icon: string | null;
  system_prompt: string;
  model: string | null;
  registered_tools: string[];
  auto_generated: boolean;
}

export interface PluginUiWidget {
  type: "markdown" | "status_card" | "button" | "kv_table" | "schedule_list";
  title?: string;
  content?: string;
  value?: string;
  hint?: string;
  label?: string;
  command?: string;
  payload?: unknown;
}

export interface PluginUiPanel {
  plugin_id: string;
  id: string;
  title: string;
  icon: string | null;
  description: string | null;
  widgets: PluginUiWidget[];
}

export interface PluginSummary {
  id: string;
  name: string;
  version: string;
  description: string;
  icon: string | null;
  enabled: boolean;
  builtin: boolean;
  source_kind: string;
  root_dir: string;
  error: string | null;
  agent: PluginAgentEntry | null;
  panels: PluginUiPanel[];
  settings: Record<string, unknown>;
}

export interface DeliveryReceipt {
  delivered_at: string;
  session_id: string;
  status: string;
  summary: string;
}

export interface ScheduleTask {
  id: string;
  plugin_id: string | null;
  name: string;
  instruction: string;
  rule: string;
  paused: boolean;
  next_run_at: string | null;
  last_run_at: string | null;
  last_status: string | null;
  history: DeliveryReceipt[];
}

export interface GuiBootstrapPayload {
  workspace_root: string;
  default_provider: string;
  default_model: string;
  sessions: GuiSessionItem[];
  sidebar_agents: PluginAgentEntry[];
  plugins: PluginSummary[];
  schedules: ScheduleTask[];
}

export interface TranscriptMessageDto {
  role: string;
  content: string | null;
  reasoning: string | null;
  tool_calls: Array<{ id: string; name: string; arguments: unknown }>;
  tool_call_id: string | null;
}

export interface GuiProviderInfo {
  id: string;
  display_name: string;
  configured: boolean;
  models: string[];
}

export interface CredentialItem {
  id: string;
  label: string;
  is_priority: boolean;
}

export type AppEventPayload =
  | { type: "turn_started"; session_id: string; query: string }
  | { type: "thinking_delta"; session_id: string; text: string }
  | { type: "content_delta"; session_id: string; text: string }
  | {
      type: "tool_call_started";
      session_id: string;
      name: string;
      arguments: unknown;
      diff: string | null;
    }
  | {
      type: "tool_call_finished";
      session_id: string;
      name: string;
      output: string;
      is_error: boolean;
      duration_ms: number;
    }
  | {
      type: "turn_finished";
      session_id: string;
      reply: string;
      duration_ms: number;
      prompt_tokens: number;
      completion_tokens: number;
    }
  | { type: "turn_failed"; session_id: string; error: string }
  | {
      type: "approval_requested";
      request_id: string;
      session_id: string;
      request: {
        tool: string;
        call_id: string;
        target: string | null;
        diff: string | null;
        arguments: unknown;
      };
    }
  | {
      type: "clarify_requested";
      request_id: string;
      session_id: string;
      request: {
        questions: Array<{ question: string; choices: string[] }>;
      };
    }
  | {
      type: "sudo_requested";
      request_id: string;
      session_id: string;
      command: string;
    }
  | {
      type: "subagent_started";
      session_id: string;
      task_id: string;
      goal: string;
    }
  | {
      type: "subagent_progress";
      session_id: string;
      task_id: string;
      activity: string;
    }
  | {
      type: "subagent_finished";
      session_id: string;
      task_id: string;
      success: boolean;
      summary: string;
    }
  | {
      type: "model_changed";
      session_id: string;
      provider: string;
      model: string;
    };

export interface BuilderToolParamDef {
  name: string;
  param_type: "string" | "number" | "boolean";
  description: string;
  required: boolean;
}

export type BuilderToolAction =
  | { kind: "shell"; command_template: string }
  | { kind: "http"; method: string; url_template: string; body_template?: string }
  | { kind: "read_file"; path_template: string }
  | { kind: "write_file"; path_template: string; content_template: string }
  | { kind: "kv_get"; key_template: string }
  | { kind: "kv_set"; key_template: string; value_template: string };

export interface BuilderToolDef {
  name: string;
  description: string;
  params: BuilderToolParamDef[];
  action: BuilderToolAction;
}

export interface BuilderScheduleDef {
  name: string;
  rule: string;
  instruction: string;
}

export interface BuilderPanelWidgetDef {
  kind: "markdown" | "status_card" | "button";
  title: string;
  body: string;
  command: string | null;
}

export interface PluginBuilderSpec {
  id: string;
  name: string;
  description: string;
  icon: string | null;
  system_prompt: string;
  model: string | null;
  tools: BuilderToolDef[];
  schedules: BuilderScheduleDef[];
  widgets: BuilderPanelWidgetDef[];
}
