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
  request_id?: string;
  tool?: string;
  tool_name?: string;
  call_id?: string;
  target?: string | null;
  diff?: string | null;
  arguments: unknown;
}

export interface ClarifyItem {
  question: string;
  choices?: string[];
}

export interface ClarifyRequestPayload {
  request_id?: string;
  question?: string;
  choices?: string[];
  questions?: ClarifyItem[];
}

export interface WorkspaceBrowseEntry {
  name: string;
  path: string;
}

export interface WorkspaceBrowseResult {
  current: string;
  parent: string | null;
  home: string | null;
  entries: WorkspaceBrowseEntry[];
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
  | { kind: 'turn_started'; session_id: string; query: string }
  | { kind: 'turn_finished'; session_id: string; reply: string; duration_ms: number; prompt_tokens: number; completion_tokens: number }
  | { kind: 'turn_failed'; session_id: string; error: string }
  | { kind: 'reasoning_delta'; session_id: string; delta: string }
  | { kind: 'content_delta'; session_id: string; delta: string }
  | { kind: 'tool_executing'; session_id: string; name: string; arguments: unknown }
  | { kind: 'tool_output'; session_id: string; name: string; chunk: string }
  | { kind: 'tool_executed'; session_id: string; name: string; is_error: boolean; output: string }
  | { kind: 'interrupted'; session_id: string; text: string; reasoning: string; partial: string }
  | { kind: 'model_changed'; session_id: string; provider: string; model: string }
  | { kind: 'approval_requested'; request_id: string; session_id: string; request: ApprovalRequestPayload }
  | { kind: 'clarify_requested'; request_id: string; session_id: string; request: ClarifyRequestPayload };

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
