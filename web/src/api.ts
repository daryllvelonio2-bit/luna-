import type {
  AgentMode,
  AppEvent,
  BootstrapPayload,
  GoalStatus,
  GuiSessionItem,
  HeartbeatPayload,
  JobSummary,
  KnowledgeGraphResponse,
  MemoryFact,
  ModelScope,
  ModelTarget,
  NoteEntry,
  PluginBuilderSpec,
  PluginExecResponse,
  PluginSourceSnapshot,
  PluginSummary,
  ProfileItem,
  ProviderInfo,
  PinnedSkillsResponse,
  UpdateCheckResponse,
  UpdateApplyResponse,
  RotationStrategy,
  ScheduleTask,
  SessionStatePayload,
  SubagentTaskRecord,
  TodoItem,
  TranscriptMessageDto,
  TurnDiffItem,
  UsagePayload,
  WorkerProfile,
  WorkspaceBrowseResult,
} from './types';

type EventListener = (event: AppEvent) => void;

class ApiClient {
  private ws: WebSocket | null = null;
  private listeners: Set<EventListener> = new Set();
  private reconnectTimer: number | null = null;
  private isConnecting = false;

  constructor() {
    this.connectWs();
  }

  private connectWs() {
    if (this.isConnecting || (this.ws && this.ws.readyState === WebSocket.OPEN)) {
      return;
    }
    this.isConnecting = true;
    const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
    const wsUrl = `${protocol}//${window.location.host}/api/remote.mux`;

    try {
      const ws = new WebSocket(wsUrl);
      this.ws = ws;

      ws.onopen = () => {
        this.isConnecting = false;
        if (this.reconnectTimer) {
          window.clearTimeout(this.reconnectTimer);
          this.reconnectTimer = null;
        }
      };

      ws.onmessage = (event) => {
        try {
          const parsed = JSON.parse(event.data) as AppEvent;
          this.listeners.forEach((listener) => {
            try {
              listener(parsed);
            } catch (err) {
              console.error('Error in event listener', err);
            }
          });
        } catch (e) {
          console.error('Failed to parse websocket message', e);
        }
      };

      ws.onclose = () => {
        this.isConnecting = false;
        this.ws = null;
        this.scheduleReconnect();
      };

      ws.onerror = () => {
        this.isConnecting = false;
      };
    } catch {
      this.isConnecting = false;
      this.scheduleReconnect();
    }
  }

  private scheduleReconnect() {
    if (this.reconnectTimer) return;
    this.reconnectTimer = window.setTimeout(() => {
      this.reconnectTimer = null;
      this.connectWs();
    }, 1500);
  }

  public subscribeEvents(cb: EventListener): () => void {
    this.listeners.add(cb);
    return () => {
      this.listeners.delete(cb);
    };
  }

  private async request<T>(path: string, options?: RequestInit): Promise<T> {
    const res = await fetch(path, {
      ...options,
      headers: {
        'Content-Type': 'application/json',
        ...(options?.headers || {}),
      },
      credentials: 'same-origin',
    });

    const text = await res.text();
    if (!res.ok) {
      let errMsg = `Request failed: ${res.status} ${res.statusText}`;
      if (text) {
        try {
          const body = JSON.parse(text);
          errMsg = body.error || body.message || text;
        } catch {
          errMsg = text;
        }
      }
      throw new Error(errMsg);
    }

    if (!text) {
      return undefined as T;
    }
    return JSON.parse(text) as T;
  }

  public async bootstrap(sessionId?: string | null): Promise<BootstrapPayload> {
    const q = sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : '';
    return this.request<BootstrapPayload>(`/api/bootstrap${q}`);
  }

  public async listSessions(limit = 150): Promise<GuiSessionItem[]> {
    return this.request<GuiSessionItem[]>(`/api/sessions?limit=${limit}`);
  }

  public async createSession(pluginAgentId?: string | null): Promise<string> {
    const res = await this.request<{ session_id: string }>('/api/sessions', {
      method: 'POST',
      body: JSON.stringify({ plugin_agent_id: pluginAgentId ?? null }),
    });
    return res.session_id;
  }

  public async getTranscript(sessionId: string, limit = 150): Promise<TranscriptMessageDto[]> {
    return this.request<TranscriptMessageDto[]>(
      `/api/sessions/${encodeURIComponent(sessionId)}/transcript?limit=${limit}`
    );
  }

  public async sendMessage(
    sessionId: string,
    message: string,
    opts?: {
      images?: string[];
      mode?: 'steer' | 'queue';
      pluginAgentId?: string | null;
    }
  ): Promise<void> {
    return this.request<void>(`/api/sessions/${encodeURIComponent(sessionId)}/send`, {
      method: 'POST',
      body: JSON.stringify({
        message,
        images: opts?.images ?? [],
        mode: opts?.mode ?? null,
        plugin_agent_id: opts?.pluginAgentId ?? null,
      }),
    });
  }

  public async interrupt(sessionId: string): Promise<void> {
    return this.request<void>(`/api/sessions/${encodeURIComponent(sessionId)}/interrupt`, {
      method: 'POST',
    });
  }

  public async renameSession(sessionId: string, name: string): Promise<void> {
    return this.request<void>(`/api/sessions/${encodeURIComponent(sessionId)}/rename`, {
      method: 'POST',
      body: JSON.stringify({ name }),
    });
  }

  public async forkSession(sessionId: string): Promise<string> {
    const res = await this.request<{ session_id: string }>(
      `/api/sessions/${encodeURIComponent(sessionId)}/fork`,
      { method: 'POST' }
    );
    return res.session_id;
  }

  public async deleteSession(sessionId: string): Promise<void> {
    return this.request<void>(`/api/sessions/${encodeURIComponent(sessionId)}`, {
      method: 'DELETE',
    });
  }

  public async getSessionState(sessionId: string): Promise<SessionStatePayload> {
    return this.request<SessionStatePayload>(
      `/api/sessions/${encodeURIComponent(sessionId)}/state`
    );
  }

  public async setSessionMode(sessionId: string, mode: AgentMode): Promise<{ mode: AgentMode }> {
    return this.request<{ mode: AgentMode }>(
      `/api/sessions/${encodeURIComponent(sessionId)}/mode`,
      {
        method: 'POST',
        body: JSON.stringify({ mode }),
      }
    );
  }

  public async setSessionReasoning(
    sessionId: string,
    effort: string | null
  ): Promise<{ reasoning_effort: string | null }> {
    return this.request<{ reasoning_effort: string | null }>(
      `/api/sessions/${encodeURIComponent(sessionId)}/reasoning`,
      {
        method: 'POST',
        body: JSON.stringify({ effort }),
      }
    );
  }

  public async compactSession(
    sessionId: string
  ): Promise<{ turns: number; messages: number; summary?: string | null; context_tokens: number }> {
    return this.request(`/api/sessions/${encodeURIComponent(sessionId)}/compact`, {
      method: 'POST',
    });
  }

  public async summarizeSession(
    sessionId: string
  ): Promise<{ session_id: string; summary: string }> {
    return this.request(`/api/sessions/${encodeURIComponent(sessionId)}/summarize`, {
      method: 'POST',
    });
  }

  public async rememberSession(
    sessionId: string
  ): Promise<{ memories: number; notes: number; summary: string }> {
    return this.request(`/api/sessions/${encodeURIComponent(sessionId)}/remember`, {
      method: 'POST',
    });
  }

  public async setMemoryScope(
    sessionId: string,
    project: string | null
  ): Promise<{ memory_project_filter: string | null }> {
    return this.request(`/api/sessions/${encodeURIComponent(sessionId)}/memory-scope`, {
      method: 'POST',
      body: JSON.stringify({ project }),
    });
  }

  public async getTodos(sessionId: string): Promise<{ items: TodoItem[] }> {
    return this.request(`/api/sessions/${encodeURIComponent(sessionId)}/todos`);
  }

  public async mutateTodo(
    sessionId: string,
    payload: { action: string; id?: number; text?: string; status?: string }
  ): Promise<{ items: TodoItem[] }> {
    return this.request(`/api/sessions/${encodeURIComponent(sessionId)}/todos`, {
      method: 'POST',
      body: JSON.stringify(payload),
    });
  }

  public async listCheckpoints(sessionId: string): Promise<{ turns: TurnDiffItem[] }> {
    return this.request(`/api/sessions/${encodeURIComponent(sessionId)}/checkpoints`);
  }

  public async rewindCheckpoint(
    sessionId: string,
    seq: number
  ): Promise<{ restored: string[]; deleted: string[]; skipped: string[] }> {
    return this.request(`/api/sessions/${encodeURIComponent(sessionId)}/rewind`, {
      method: 'POST',
      body: JSON.stringify({ seq }),
    });
  }

  public async manageGoal(
    sessionId: string,
    action: 'start' | 'stop',
    objective?: string,
    maxIterations?: number
  ): Promise<{ goal?: GoalStatus; stopped?: boolean }> {
    return this.request(`/api/sessions/${encodeURIComponent(sessionId)}/goal`, {
      method: 'POST',
      body: JSON.stringify({
        action,
        objective,
        max_iterations: maxIterations,
      }),
    });
  }

  public async listTasks(
    sessionId: string
  ): Promise<{ subagents: SubagentTaskRecord[]; jobs: JobSummary[]; goal: GoalStatus | null }> {
    return this.request(`/api/sessions/${encodeURIComponent(sessionId)}/tasks`);
  }

  public async killTask(
    sessionId: string,
    taskId: string,
    kind: 'subagent' | 'job'
  ): Promise<{ killed: boolean }> {
    return this.request(
      `/api/sessions/${encodeURIComponent(sessionId)}/tasks/${encodeURIComponent(taskId)}/kill`,
      {
        method: 'POST',
        body: JSON.stringify({ kind }),
      }
    );
  }

  public async runWorker(
    sessionId: string,
    workerName: string,
    task: string,
    images?: string[]
  ): Promise<{ task_id: string; child_session_id: string; worker: string; provider: string; model: string }> {
    return this.request(`/api/sessions/${encodeURIComponent(sessionId)}/worker`, {
      method: 'POST',
      body: JSON.stringify({ worker_name: workerName, task, images }),
    });
  }

  public async resolveApproval(
    sessionId: string,
    decision: 'run' | 'deny' | 'run_with_args' | 'deny_with_feedback',
    argumentsJson?: unknown,
    feedback?: string,
    requestId?: string
  ): Promise<void> {
    return this.request<void>(`/api/sessions/${encodeURIComponent(sessionId)}/approval`, {
      method: 'POST',
      body: JSON.stringify({
        request_id: requestId,
        decision,
        arguments: argumentsJson,
        feedback,
      }),
    });
  }

  public async resolveClarify(
    sessionId: string,
    answers: Array<string | string[]>,
    requestId?: string
  ): Promise<void> {
    return this.request<void>(`/api/sessions/${encodeURIComponent(sessionId)}/clarify`, {
      method: 'POST',
      body: JSON.stringify({ request_id: requestId, answers }),
    });
  }

  public async askApprovalQuestion(
    toolName: string | null,
    target: string | null,
    question: string
  ): Promise<string> {
    const res = await this.request<{ reply: string }>('/api/approval/question', {
      method: 'POST',
      body: JSON.stringify({ tool_name: toolName, target, question }),
    });
    return res.reply;
  }

  public async getProviders(sessionId?: string | null): Promise<ProviderInfo[]> {
    const q = sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : '';
    return this.request<ProviderInfo[]>(`/api/providers${q}`);
  }

  public async refreshProviderModels(
    providerId: string,
    credentialId?: string,
    force = true
  ): Promise<string[]> {
    const params = new URLSearchParams();
    if (credentialId) params.set('credential_id', credentialId);
    if (!force) params.set('force', 'false');
    const qs = params.toString();
    return this.request<string[]>(
      `/api/providers/${encodeURIComponent(providerId)}/refresh${qs ? `?${qs}` : ''}`,
      {
        method: 'POST',
      }
    );
  }

  public async setDefaultModel(
    provider: string,
    model: string,
    sessionId?: string | null,
    scope: ModelScope = 'session',
    target: ModelTarget = 'session'
  ): Promise<{ provider: string; model: string; scope: string; target: string }> {
    return this.request('/api/models/default', {
      method: 'POST',
      body: JSON.stringify({
        provider,
        model,
        session_id: sessionId ?? null,
        scope,
        target,
      }),
    });
  }

  public async browseWorkspace(path?: string): Promise<WorkspaceBrowseResult> {
    const q = path ? `?path=${encodeURIComponent(path)}` : '';
    return this.request<WorkspaceBrowseResult>(`/api/workspace/browse${q}`);
  }

  public async setWorkspace(
    path: string,
    scope: 'session' | 'global' = 'session',
    reset = false
  ): Promise<{ workspace_root: string }> {
    return this.request<{ workspace_root: string }>('/api/workspace', {
      method: 'POST',
      body: JSON.stringify({ path, scope, reset }),
    });
  }

  public async saveCredential(
    provider: string,
    apiKey: string,
    label?: string,
    isPriority = true
  ): Promise<void> {
    return this.request<void>('/api/credentials', {
      method: 'POST',
      body: JSON.stringify({
        provider,
        api_key: apiKey,
        label: label || undefined,
        is_priority: isPriority,
      }),
    });
  }

  public async removeCredential(provider: string, credentialId: string): Promise<void> {
    return this.request<void>(
      `/api/credentials/${encodeURIComponent(provider)}/${encodeURIComponent(credentialId)}`,
      { method: 'DELETE' }
    );
  }

  public async setPriorityCredential(
    provider: string,
    credentialId: string,
    scope: 'session' | 'global',
    sessionId?: string | null
  ): Promise<void> {
    return this.request<void>(`/api/credentials/${encodeURIComponent(provider)}/priority`, {
      method: 'POST',
      body: JSON.stringify({
        credential_id: credentialId,
        scope,
        session_id: sessionId ?? null,
      }),
    });
  }

  public async clearPriorityCredential(
    provider: string,
    credentialId: string | null,
    scope: 'session' | 'global',
    sessionId?: string | null
  ): Promise<void> {
    return this.request<void>(`/api/credentials/${encodeURIComponent(provider)}/priority`, {
      method: 'DELETE',
      body: JSON.stringify({
        credential_id: credentialId,
        scope,
        session_id: sessionId ?? null,
      }),
    });
  }

  public async setRotationStrategy(provider: string, strategy: RotationStrategy): Promise<void> {
    return this.request<void>(`/api/credentials/${encodeURIComponent(provider)}/strategy`, {
      method: 'POST',
      body: JSON.stringify({ strategy }),
    });
  }

  public async resetCooldowns(provider?: string): Promise<{ reset_count: number }> {
    return this.request('/api/credentials/reset-cooldowns', {
      method: 'POST',
      body: JSON.stringify({ provider: provider ?? null }),
    });
  }

  public async importCredentials(
    kind: 'antigravity' | 'opencode' | 'codex'
  ): Promise<{ imported: string[] }> {
    return this.request('/api/credentials/import', {
      method: 'POST',
      body: JSON.stringify({ kind }),
    });
  }

  public async addCustomProvider(payload: {
    id: string;
    base_url: string;
    api_key?: string;
    label?: string;
    context_window?: number;
  }): Promise<{ id: string; base_url: string }> {
    return this.request('/api/providers/custom', {
      method: 'POST',
      body: JSON.stringify(payload),
    });
  }

  public async getUsage(sessionId?: string | null): Promise<UsagePayload> {
    const q = sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : '';
    return this.request<UsagePayload>(`/api/usage${q}`);
  }

  public async updateSettings(payload: {
    theme?: 'dark' | 'light';
    auto_collapse?: boolean;
    auto_hide_thinking?: boolean;
    block_limit?: number | null;
    auto_compact_enabled?: boolean;
    auto_compact_threshold?: number;
    auto_compact_percent?: number;
    memory_enabled?: boolean;
    notes_enabled?: boolean;
    skills_enabled?: boolean;
    mcp_enabled?: boolean;
    memory_mode?: 'session' | 'background';
    session_id?: string | null;
  }): Promise<void> {
    return this.request<void>('/api/settings', {
      method: 'POST',
      body: JSON.stringify(payload),
    });
  }

  public async upsertWorker(payload: {
    name: string;
    provider: string;
    model: string;
    reasoning_effort?: string | null;
  }): Promise<{ workers: Record<string, WorkerProfile> }> {
    return this.request('/api/workers', {
      method: 'POST',
      body: JSON.stringify(payload),
    });
  }

  public async deleteWorker(name: string): Promise<{ workers: Record<string, WorkerProfile> }> {
    return this.request(`/api/workers/${encodeURIComponent(name)}`, {
      method: 'DELETE',
    });
  }

  public async listProfiles(): Promise<{ profiles: ProfileItem[] }> {
    return this.request('/api/profiles');
  }

  public async createProfile(payload: {
    name: string;
    summary?: string;
    persona?: string;
    behavior?: string;
  }): Promise<void> {
    return this.request('/api/profiles', {
      method: 'POST',
      body: JSON.stringify(payload),
    });
  }

  public async deleteProfile(name: string): Promise<void> {
    return this.request(`/api/profiles/${encodeURIComponent(name)}`, {
      method: 'DELETE',
    });
  }

  public async getHeartbeat(sessionId?: string | null): Promise<HeartbeatPayload> {
    const q = sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : '';
    return this.request<HeartbeatPayload>(`/api/heartbeat${q}`);
  }

  public async listPlugins(): Promise<PluginSummary[]> {
    return this.request<PluginSummary[]>('/api/plugins');
  }

  public async getPlugin(pluginId: string): Promise<PluginSummary> {
    return this.request<PluginSummary>(`/api/plugins/${encodeURIComponent(pluginId)}`);
  }

  public async getPluginSource(pluginId: string): Promise<PluginSourceSnapshot> {
    return this.request<PluginSourceSnapshot>(
      `/api/plugins/${encodeURIComponent(pluginId)}/source`
    );
  }

  public async reloadPlugin(pluginId: string): Promise<PluginSummary> {
    return this.request<PluginSummary>(`/api/plugins/${encodeURIComponent(pluginId)}/reload`, {
      method: 'POST',
    });
  }

  public async runPluginCommand(
    pluginId: string,
    commandName: string,
    args?: Record<string, unknown>
  ): Promise<PluginExecResponse> {
    return this.request<PluginExecResponse>(
      `/api/plugins/${encodeURIComponent(pluginId)}/commands/${encodeURIComponent(commandName)}`,
      {
        method: 'POST',
        body: JSON.stringify(args || {}),
      }
    );
  }

  public async runPluginTool(
    pluginId: string,
    toolName: string,
    args?: Record<string, unknown>
  ): Promise<PluginExecResponse> {
    return this.request<PluginExecResponse>(
      `/api/plugins/${encodeURIComponent(pluginId)}/tools/${encodeURIComponent(toolName)}`,
      {
        method: 'POST',
        body: JSON.stringify(args || {}),
      }
    );
  }

  public async getPluginSettings(pluginId: string): Promise<Record<string, unknown>> {
    return this.request<Record<string, unknown>>(
      `/api/plugins/${encodeURIComponent(pluginId)}/settings`
    );
  }

  public async updatePluginSettings(
    pluginId: string,
    patch: Record<string, unknown>
  ): Promise<PluginSummary> {
    return this.request<PluginSummary>(
      `/api/plugins/${encodeURIComponent(pluginId)}/settings`,
      {
        method: 'POST',
        body: JSON.stringify(patch),
      }
    );
  }

  public async createPluginFromSpec(spec: PluginBuilderSpec): Promise<void> {
    return this.request<void>('/api/plugins/build', {
      method: 'POST',
      body: JSON.stringify(spec),
    });
  }

  public async togglePlugin(pluginId: string, enabled: boolean): Promise<void> {
    return this.request<void>(`/api/plugins/${encodeURIComponent(pluginId)}/toggle`, {
      method: 'POST',
      body: JSON.stringify({ enabled }),
    });
  }

  public async deletePlugin(pluginId: string): Promise<void> {
    return this.request<void>(`/api/plugins/${encodeURIComponent(pluginId)}`, {
      method: 'DELETE',
    });
  }

  public async listSchedules(): Promise<ScheduleTask[]> {
    return this.request<ScheduleTask[]>('/api/schedules');
  }

  public async createSchedule(payload: {
    name: string;
    instruction: string;
    rule: string;
    target?: 'new_session_each_run' | 'single_new_session' | 'existing_session';
    session_id?: string;
    require_approval?: boolean;
    profile?: string;
    execution_mode?: 'scheduled' | 'continuous';
    active_window?: string;
    trigger_command?: string;
    trigger_logic?: string;
  }): Promise<ScheduleTask> {
    return this.request<ScheduleTask>('/api/schedules', {
      method: 'POST',
      body: JSON.stringify(payload),
    });
  }

  public async updateSchedule(
    taskId: string,
    payload: {
      name?: string;
      instruction?: string;
      rule?: string;
      paused?: boolean;
      target?: 'new_session_each_run' | 'single_new_session' | 'existing_session';
      session_id?: string;
      require_approval?: boolean;
      profile?: string;
      execution_mode?: 'scheduled' | 'continuous';
      active_window?: string;
      trigger_command?: string;
      trigger_logic?: string;
    }
  ): Promise<ScheduleTask> {
    return this.request<ScheduleTask>(`/api/schedules/${encodeURIComponent(taskId)}`, {
      method: 'PUT',
      body: JSON.stringify(payload),
    });
  }

  public async toggleSchedule(taskId: string, paused: boolean): Promise<ScheduleTask> {
    return this.request<ScheduleTask>(`/api/schedules/${encodeURIComponent(taskId)}/toggle`, {
      method: 'POST',
      body: JSON.stringify({ paused }),
    });
  }

  public async runSchedule(
    taskId: string
  ): Promise<{ task_id: string; session_id: string; status: string }> {
    return this.request<{ task_id: string; session_id: string; status: string }>(
      `/api/schedules/${encodeURIComponent(taskId)}/run`,
      {
        method: 'POST',
      }
    );
  }

  public async deleteSchedule(taskId: string): Promise<void> {
    return this.request<void>(`/api/schedules/${encodeURIComponent(taskId)}`, {
      method: 'DELETE',
    });
  }

  public async searchMemories(q: string): Promise<MemoryFact[]> {
    return this.request<MemoryFact[]>(`/api/memories?q=${encodeURIComponent(q)}`);
  }

  public async searchNotes(q: string): Promise<NoteEntry[]> {
    return this.request<NoteEntry[]>(`/api/notes?q=${encodeURIComponent(q)}`);
  }

  public async getNote(noteId: string): Promise<NoteEntry> {
    return this.request<NoteEntry>(`/api/notes/${encodeURIComponent(noteId)}`);
  }

  public async getKnowledgeGraph(q = ''): Promise<KnowledgeGraphResponse> {
    const qs = q.trim() ? `?q=${encodeURIComponent(q.trim())}` : '';
    return this.request<KnowledgeGraphResponse>(`/api/knowledge-graph${qs}`);
  }

  public async getPinnedSkills(sessionId?: string): Promise<PinnedSkillsResponse> {
    const q = sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : '';
    return this.request<PinnedSkillsResponse>(`/api/skills/pinned${q}`);
  }

  public async pinSkills(
    skills: string[],
    scope: 'session' | 'global',
    sessionId?: string
  ): Promise<PinnedSkillsResponse> {
    return this.request<PinnedSkillsResponse>('/api/skills/pin', {
      method: 'POST',
      body: JSON.stringify({ skills, scope, session_id: sessionId }),
    });
  }

  public async unpinSkill(
    skill: string,
    scope?: 'session' | 'global' | 'both',
    sessionId?: string
  ): Promise<PinnedSkillsResponse> {
    return this.request<PinnedSkillsResponse>('/api/skills/unpin', {
      method: 'POST',
      body: JSON.stringify({ skill, scope, session_id: sessionId }),
    });
  }

  public async checkUpdate(): Promise<UpdateCheckResponse> {
    return this.request<UpdateCheckResponse>('/api/update');
  }

  public async applyUpdate(): Promise<UpdateApplyResponse> {
    return this.request<UpdateApplyResponse>('/api/update', {
      method: 'POST',
    });
  }
}

export const api = new ApiClient();
