import type {
  AppEvent,
  BootstrapPayload,
  GuiSessionItem,
  MemoryFact,
  NoteEntry,
  PluginBuilderSpec,
  PluginSummary,
  ProviderInfo,
  ScheduleTask,
  TranscriptMessageDto,
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

  public async bootstrap(): Promise<BootstrapPayload> {
    return this.request<BootstrapPayload>('/api/bootstrap');
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

  public async sendMessage(sessionId: string, message: string, pluginAgentId?: string | null): Promise<void> {
    return this.request<void>(`/api/sessions/${encodeURIComponent(sessionId)}/send`, {
      method: 'POST',
      body: JSON.stringify({ message, plugin_agent_id: pluginAgentId ?? null }),
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
    const res = await this.request<{ session_id: string }>(`/api/sessions/${encodeURIComponent(sessionId)}/fork`, {
      method: 'POST',
    });
    return res.session_id;
  }

  public async deleteSession(sessionId: string): Promise<void> {
    return this.request<void>(`/api/sessions/${encodeURIComponent(sessionId)}`, {
      method: 'DELETE',
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
    toolName: string,
    target: string | null,
    question: string
  ): Promise<string> {
    const res = await this.request<{ reply: string }>('/api/approval/question', {
      method: 'POST',
      body: JSON.stringify({ tool_name: toolName, target, question }),
    });
    return res.reply;
  }

  public async getProviders(): Promise<ProviderInfo[]> {
    return this.request<ProviderInfo[]>('/api/providers');
  }

  public async refreshProviderModels(providerId: string): Promise<string[]> {
    return this.request<string[]>(`/api/providers/${encodeURIComponent(providerId)}/refresh`, {
      method: 'POST',
    });
  }

  public async setDefaultModel(
    provider: string,
    model: string,
    sessionId?: string | null
  ): Promise<{ provider: string; model: string }> {
    return this.request<{ provider: string; model: string }>('/api/models/default', {
      method: 'POST',
      body: JSON.stringify({ provider, model, session_id: sessionId ?? null }),
    });
  }

  public async browseWorkspace(path?: string): Promise<WorkspaceBrowseResult> {
    const q = path ? `?path=${encodeURIComponent(path)}` : '';
    return this.request<WorkspaceBrowseResult>(`/api/workspace/browse${q}`);
  }

  public async setWorkspace(path: string): Promise<{ workspace_root: string }> {
    return this.request<{ workspace_root: string }>('/api/workspace', {
      method: 'POST',
      body: JSON.stringify({ path }),
    });
  }

  public async saveCredential(provider: string, apiKey: string): Promise<void> {
    return this.request<void>('/api/credentials', {
      method: 'POST',
      body: JSON.stringify({ provider, api_key: apiKey }),
    });
  }

  public async listPlugins(): Promise<PluginSummary[]> {
    return this.request<PluginSummary[]>('/api/plugins');
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

  public async listSchedules(): Promise<ScheduleTask[]> {
    return this.request<ScheduleTask[]>('/api/schedules');
  }

  public async createSchedule(
    title: string,
    prompt: string,
    scheduleType: 'cron' | 'interval',
    cronExpr?: string,
    intervalSecs?: number
  ): Promise<{ id: string }> {
    return this.request<{ id: string }>('/api/schedules', {
      method: 'POST',
      body: JSON.stringify({
        title,
        prompt,
        schedule_type: scheduleType,
        cron_expr: cronExpr,
        interval_secs: intervalSecs,
      }),
    });
  }

  public async toggleSchedule(taskId: string, enabled: boolean): Promise<void> {
    return this.request<void>(`/api/schedules/${encodeURIComponent(taskId)}/toggle`, {
      method: 'POST',
      body: JSON.stringify({ enabled }),
    });
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
}

export const api = new ApiClient();
