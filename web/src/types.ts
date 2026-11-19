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

export interface AccountInfo {
  id: string;
  label: string;
  is_priority: boolean;
  is_session_priority: boolean;
  usage_count: number;
  last_used?: string | null;
  is_temporarily_disabled: boolean;
}

export interface ProviderInfo {
  id: string;
  display_name: string;
  configured: boolean;
  models: string[];
  accounts: AccountInfo[];
  rotation_strategy: RotationStrategy;
  is_custom?: boolean;
  base_url?: string | null;
}

export type RotationStrategy =
  | 'round_robin'
  | 'fill_first'
  | 'least_used'
  | 'random'
  | 'never';

export type AgentMode = 'build' | 'auto' | 'plan';

export type ReasoningEffort =
  | 'off'
  | 'minimal'
  | 'low'
  | 'medium'
  | 'high'
  | 'xhigh'
  | 'max';

export type ModelTarget = 'session' | 'default' | 'compact' | 'bgsaver' | 'approval';
export type ModelScope = 'session' | 'global';

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

export type DeliveryTarget =
  | { kind: 'new_session_each_run' }
  | { kind: 'single_new_session'; session_id?: string | null }
  | { kind: 'existing_session'; session_id: string };

export interface DeliveryReceipt {
  delivered_at: string;
  session_id: string;
  status: string;
  summary: string;
}

export interface ScheduleTask {
  id: string;
  name: string;
  instruction: string;
  rule: string;
  delivery_target: DeliveryTarget;
  plugin_id?: string | null;
  paused: boolean;
  completed: boolean;
  last_run_at?: string | null;
  next_run_at?: string | null;
  created_at: string;
  updated_at: string;
  history: DeliveryReceipt[];
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

export interface DisplaySettings {
  theme: 'dark' | 'light';
  auto_collapse: boolean;
  auto_hide_thinking: boolean;
  block_limit?: number | null;
}

export interface AutoCompactSettings {
  enabled: boolean;
  threshold: number;
  percent: number;
}

export interface FeatureFlags {
  memory: boolean;
  notes: boolean;
  skills: boolean;
  mcp: boolean;
  memory_mode: 'session' | 'background';
}

export interface WorkerProfile {
  provider: string;
  model: string;
  reasoning_effort?: string | null;
}

export interface SkillSummary {
  name: string;
  description: string;
  source: string;
}

export interface McpServerSummary {
  name: string;
  command: string;
  enabled: boolean;
}

export interface TodoItem {
  id: number;
  text: string;
  status: 'pending' | 'in_progress' | 'done';
}

export interface GoalStatus {
  objective: string;
  iteration: number;
  max_iterations: number;
  running: boolean;
}

export interface SessionStatePayload {
  mode: AgentMode;
  reasoning_effort: string | null;
  context_tokens: number;
  context_window: number;
  memory_project_filter: string | null;
  goal: GoalStatus | null;
  queued_messages: string[];
  todos: TodoItem[];
  turn_running: boolean;
}

export interface FileDiffItem {
  path: string;
  kind: 'created' | 'modified' | 'deleted' | 'unchanged';
  additions: number;
  deletions: number;
  diff: string;
  note?: string | null;
}

export interface TurnDiffItem {
  seq: number;
  session_id: string;
  created_at: string;
  prompt?: string | null;
  files: FileDiffItem[];
  unrecorded?: Array<{ path: string; kind: string }>;
}

export interface SubagentTaskRecord {
  id: string;
  goal: string;
  context?: string | null;
  status:
    | { status: 'running' }
    | { status: 'completed'; reply: string; duration_ms: number }
    | { status: 'failed'; error: string; duration_ms: number }
    | string;
  started_at: string;
  log_path?: string;
}

export interface JobSummary {
  id: string;
  command: string;
  workdir: string;
  status: string;
  elapsed_secs: number;
  log_path: string;
}

export interface UsageTotals {
  turns: number;
  ok_turns: number;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  cached_tokens: number;
  cost_usd: number;
  unpriced_turns: number;
  duration_ms: number;
  tool_calls: number;
  denied_calls: number;
}

export interface UsagePayload {
  enabled: boolean;
  session: UsageTotals | null;
  global: UsageTotals;
  by_model: Array<{ key: string; totals: UsageTotals }>;
}

export interface ProfileItem {
  name: string;
  summary: string;
  builtin: boolean;
  persona?: string | null;
  behavior?: string | null;
}

export interface HeartbeatPayload {
  pid: number;
  rss_mb?: number | null;
  workspace_root: string;
  git?: { branch: string; dirty: number } | null;
  active_sessions: number;
  session_busy: boolean;
  tools_count: number;
  tools: string[];
  skills_count: number;
  mcp_servers_count: number;
  context_tokens: number;
}

export interface BootstrapPayload {
  workspace_root: string;
  launch_workspace_root?: string;
  default_provider: string;
  default_model: string;
  compact_provider?: string | null;
  compact_model?: string | null;
  background_provider?: string | null;
  background_model?: string | null;
  approval_provider?: string | null;
  approval_model?: string | null;
  sessions: GuiSessionItem[];
  sidebar_agents: PluginAgentEntry[];
  providers: ProviderInfo[];
  theme: 'dark' | 'light';
  approval_mode: string;
  display?: DisplaySettings;
  auto_compact?: AutoCompactSettings;
  features?: FeatureFlags;
  workers?: Record<string, WorkerProfile>;
  skills?: SkillSummary[];
  mcp_servers?: McpServerSummary[];
}

export type AppEvent =
  | { kind: 'turn_started'; session_id: string; query: string }
  | {
      kind: 'turn_finished';
      session_id: string;
      reply: string;
      duration_ms: number;
      prompt_tokens: number;
      completion_tokens: number;
    }
  | { kind: 'turn_failed'; session_id: string; error: string }
  | { kind: 'reasoning_delta'; session_id: string; delta: string }
  | { kind: 'content_delta'; session_id: string; delta: string }
  | { kind: 'tool_executing'; session_id: string; name: string; arguments: unknown }
  | { kind: 'tool_output'; session_id: string; name: string; chunk: string }
  | { kind: 'tool_executed'; session_id: string; name: string; is_error: boolean; output: string }
  | { kind: 'context_tokens_updated'; session_id: string; tokens_used: number; reset: boolean }
  | { kind: 'compaction_started'; session_id: string; forced: boolean }
  | {
      kind: 'compaction_finished';
      session_id: string;
      turns?: number | null;
      messages?: number | null;
      summary?: string | null;
      error?: string | null;
    }
  | { kind: 'memory_saved'; session_id: string; name: string; detail?: string | null; status: string }
  | { kind: 'note_saved'; session_id: string; name: string; detail?: string | null; status: string }
  | { kind: 'memory_pass_started'; session_id: string; model: string }
  | { kind: 'memory_pass_finished'; session_id: string; memories: number; notes: number }
  | { kind: 'interrupted'; session_id: string; text: string; reasoning: string; partial: string }
  | { kind: 'steered'; session_id: string; text: string }
  | { kind: 'subagent_started'; session_id: string; id: string; goal: string }
  | { kind: 'subagent_progress'; session_id: string; id: string; activity: string }
  | { kind: 'subagent_finished'; session_id: string; id: string; success: boolean; summary: string }
  | {
      kind: 'job_completed';
      session_id: string;
      job_id: string;
      command: string;
      status: string;
      elapsed_secs: number;
    }
  | { kind: 'model_changed'; session_id: string; provider: string; model: string }
  | {
      kind: 'approval_requested';
      request_id: string;
      session_id: string;
      request: ApprovalRequestPayload;
    }
  | {
      kind: 'clarify_requested';
      request_id: string;
      session_id: string;
      request: ClarifyRequestPayload;
    };

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
