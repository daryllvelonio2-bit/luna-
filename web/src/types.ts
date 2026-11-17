export interface SessionRecord {
  id: string;
  created_at: string;
  updated_at: string;
  title: string;
  name: string;
  message_count: number;
  model: [string, string] | null;
  compact_model: [string, string] | null;
  credential_priorities?: Record<string, string>;
}

export interface GuiSessionItem {
  record: SessionRecord;
  plugin_agent_id?: string | null;
}

export interface ToolCallItem {
  id: string;
  name: string;
  arguments: unknown;
}

export interface TranscriptMessageDto {
  role: string;
  content?: string | null;
  reasoning?: string | null;
  tool_calls: ToolCallItem[];
  tool_call_id?: string | null;
  is_summary?: boolean;
}

export interface ProviderInfo {
  id: string;
  display_name: string;
  configured: boolean;
  models: string[];
}

export interface PluginAgentEntry {
  id: string;
  name: string;
  description: string;
  icon?: string;
  system_prompt?: string;
  tools?: string[];
  default_provider?: string;
  default_model?: string;
}

export interface PluginSummary {
  id: string;
  name: string;
  version: string;
  description: string;
  author?: string;
  enabled: boolean;
  tools_count: number;
  commands_count: number;
  agents_count: number;
  schedules_count: number;
  source_kind: 'workspace' | 'global' | 'builtin';
}

export interface ScheduleTask {
  id: string;
  title: string;
  prompt: string;
  schedule_type: 'cron' | 'interval';
  cron_expr?: string;
  interval_secs?: number;
  provider?: string;
  model?: string;
  enabled: boolean;
  last_run_at?: string;
  next_run_at?: string;
  created_at: string;
  updated_at: string;
}

export interface MemoryFact {
  id?: string;
  category?: string;
  predicate?: string;
  text: string;
  updated_at?: string;
}

export interface NoteEntry {
  id?: string;
  title: string;
  body?: string;
  path?: string;
  updated_at?: string;
}

export interface ApprovalRequestPayload {
  session_id: string;
  tool_name: string;
  target?: string | null;
  diff?: string | null;
  preview?: string | null;
  arguments: unknown;
  activity_token?: string;
}

export interface ClarifyItem {
  question: string;
  choices?: string[];
}

export interface ClarifyRequestPayload {
  session_id: string;
  question: string;
  choices: string[];
  questions?: ClarifyItem[];
}

export interface BootstrapPayload {
  workspace_root: string;
  default_provider: string;
  default_model: string;
  sessions: GuiSessionItem[];
  sidebar_agents: PluginAgentEntry[];
  providers: ProviderInfo[];
  theme: 'dark' | 'light';
  approval_mode: string;
}

export type AppEvent =
  | { type: 'TurnStarted'; session_id: string; query: string }
  | { type: 'StreamDelta'; session_id: string; delta: string }
  | { type: 'StreamReasoningDelta'; session_id: string; delta: string }
  | { type: 'ToolCallStarted'; session_id: string; tool_name: string; call_id: string; arguments: unknown }
  | { type: 'ToolCallFinished'; session_id: string; call_id: string; result: string; is_error: boolean }
  | { type: 'ApprovalRequested'; session_id: string; request: ApprovalRequestPayload }
  | { type: 'ApprovalResolved'; session_id: string; approved: boolean }
  | { type: 'ClarifyRequested'; session_id: string; request: ClarifyRequestPayload }
  | { type: 'ClarifyResolved'; session_id: string }
  | { type: 'TurnCompleted'; session_id: string; response: string; prompt_tokens?: number; completion_tokens?: number; total_tokens?: number; duration_ms?: number }
  | { type: 'TurnError'; session_id: string; error: string }
  | { type: 'SessionSwitched'; session_id: string };

export interface PluginBuilderSpec {
  name: string;
  description: string;
  author: string;
  version: string;
  tools: Array<{
    name: string;
    description: string;
    parameters_schema: unknown;
    handler_code: string;
  }>;
  agents: Array<{
    name: string;
    description: string;
    system_prompt: string;
    tools: string[];
    default_provider?: string;
    default_model?: string;
  }>;
  schedules: Array<{
    id: string;
    title: string;
    prompt: string;
    cron_expr?: string;
    interval_secs?: number;
  }>;
}
