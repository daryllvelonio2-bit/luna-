import React, { useState, useEffect, useCallback, useRef } from 'react';
import { api } from './api';
import type {
  AgentMode,
  ApprovalRequestPayload,
  AutoCompactSettings,
  ClarifyRequestPayload,
  DisplaySettings,
  FeatureFlags,
  GoalStatus,
  GuiSessionItem,
  McpServerSummary,
  ModelScope,
  ModelTarget,
  PluginAgentEntry,
  PluginBuilderSpec,
  PluginSummary,
  ProviderInfo,
  RotationStrategy,
  ScheduleTask,
  SkillSummary,
  TodoItem,
  TranscriptMessageDto,
  WorkerProfile,
} from './types';
import { Sidebar } from './components/Sidebar';
import { ChatView } from './components/ChatView';
import { AutomationsView } from './components/AutomationsView';
import { PluginsView } from './components/PluginsView';
import { PluginBuilderModal } from './components/PluginBuilderModal';
import { MemoryNotesView } from './components/MemoryNotesView';
import { SettingsModal, type SettingsTab } from './components/SettingsModal';
import { ModelPickerModal } from './components/ModelPickerModal';
import { WorkspaceModal } from './components/WorkspaceModal';
import { SessionDrawers, type ActiveDrawerTab } from './components/SessionDrawers';

export const App: React.FC = () => {
  const [currentTab, setCurrentTab] = useState<'chat' | 'automations' | 'plugins' | 'memories'>(
    'chat'
  );
  const [theme, setTheme] = useState<'dark' | 'light'>('dark');
  const [sidebarOpen, setSidebarOpen] = useState(() =>
    typeof window !== 'undefined' ? window.innerWidth > 768 : true
  );

  const [sessions, setSessions] = useState<GuiSessionItem[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [messages, setMessages] = useState<TranscriptMessageDto[]>([]);
  const [turnRunning, setTurnRunning] = useState(false);

  const [workspaceRoot, setWorkspaceRoot] = useState('');
  const [launchWorkspaceRoot, setLaunchWorkspaceRoot] = useState('');
  const [sidebarAgents, setSidebarAgents] = useState<PluginAgentEntry[]>([]);
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [defaultProvider, setDefaultProvider] = useState('opencode');
  const [defaultModel, setDefaultModel] = useState('');

  // Extended settings & session runtime state
  const [compactProvider, setCompactProvider] = useState<string | null>(null);
  const [compactModel, setCompactModel] = useState<string | null>(null);
  const [backgroundProvider, setBackgroundProvider] = useState<string | null>(null);
  const [backgroundModel, setBackgroundModel] = useState<string | null>(null);
  const [approvalProvider, setApprovalProvider] = useState<string | null>(null);
  const [approvalModel, setApprovalModel] = useState<string | null>(null);
  const [displaySettings, setDisplaySettings] = useState<DisplaySettings>({
    block_limit: null,
    auto_collapse: true,
    auto_hide_thinking: true,
    theme: 'dark',
  });
  const [autoCompact, setAutoCompact] = useState<AutoCompactSettings>({
    enabled: true,
    threshold: 180000,
    percent: 50,
  });
  const [features, setFeatures] = useState<FeatureFlags>({
    memory: true,
    notes: true,
    skills: true,
    mcp: true,
    memory_mode: 'session',
  });
  const [workers, setWorkers] = useState<Record<string, WorkerProfile>>({});
  const [skills, setSkills] = useState<SkillSummary[]>([]);
  const [mcpServers, setMcpServers] = useState<McpServerSummary[]>([]);

  // Active session live state
  const [sessionMode, setSessionMode] = useState<AgentMode>('build');
  const [reasoningEffort, setReasoningEffort] = useState<string | null>(null);
  const [contextTokens, setContextTokens] = useState(0);
  const [contextWindow, setContextWindow] = useState(0);
  const [queuedCount, setQueuedCount] = useState(0);
  const [todos, setTodos] = useState<TodoItem[]>([]);
  const [goal, setGoal] = useState<GoalStatus | null>(null);
  const [memoryProjectFilter, setMemoryProjectFilter] = useState<string | null>(null);

  const [pendingApproval, setPendingApproval] = useState<ApprovalRequestPayload | null>(null);
  const [pendingClarify, setPendingClarify] = useState<ClarifyRequestPayload | null>(null);

  const [schedules, setSchedules] = useState<ScheduleTask[]>([]);
  const [plugins, setPlugins] = useState<PluginSummary[]>([]);

  const [isSettingsOpen, setIsSettingsOpen] = useState(false);
  const [settingsTab, setSettingsTab] = useState<SettingsTab>('general');
  const [isModelPickerOpen, setIsModelPickerOpen] = useState(false);
  const [modelPickerTarget, setModelPickerTarget] = useState<ModelTarget>('session');
  const [modelPickerAnchor, setModelPickerAnchor] = useState<DOMRect | null>(null);
  const [isWorkspacePickerOpen, setIsWorkspacePickerOpen] = useState(false);
  const [isPluginBuilderOpen, setIsPluginBuilderOpen] = useState(false);
  const [activeDrawer, setActiveDrawer] = useState<ActiveDrawerTab>(null);

  const [notices, setNotices] = useState<Array<{ id: string; text: string; kind: 'info' | 'err' }>>(
    []
  );

  const activeSessionIdRef = useRef<string | null>(null);
  activeSessionIdRef.current = activeSessionId;

  // RAF-batched streaming delta buffer
  const pendingContentDeltaRef = useRef('');
  const pendingReasoningDeltaRef = useRef('');
  const rafIdRef = useRef<number | null>(null);
  const liveToolCallsRef = useRef<Array<{ id: string; name: string }>>([]);
  const liveToolSeqRef = useRef(0);
  const optimisticQueryRef = useRef<{ sid: string; query: string } | null>(null);

  const pushNotice = useCallback((text: string, kind: 'info' | 'err' = 'info') => {
    const id = `${Date.now()}-${Math.random().toString(36).slice(2, 7)}`;
    setNotices((prev) => [...prev.slice(-3), { id, text, kind }]);
    setTimeout(() => {
      setNotices((prev) => prev.filter((n) => n.id !== id));
    }, 6000);
  }, []);

  const showError = useCallback(
    (msg: string) => {
      pushNotice(msg, 'err');
    },
    [pushNotice]
  );

  useEffect(() => {
    document.documentElement.setAttribute('data-theme', theme);
  }, [theme]);

  const flushBufferedDeltas = useCallback(() => {
    if (rafIdRef.current !== null) {
      cancelAnimationFrame(rafIdRef.current);
      rafIdRef.current = null;
    }
    const contentChunk = pendingContentDeltaRef.current;
    const reasoningChunk = pendingReasoningDeltaRef.current;
    pendingContentDeltaRef.current = '';
    pendingReasoningDeltaRef.current = '';

    if (!contentChunk && !reasoningChunk) return;

    setMessages((prev) => {
      const last = prev[prev.length - 1];
      if (last && last.role === 'assistant') {
        return [
          ...prev.slice(0, -1),
          {
            ...last,
            content: contentChunk ? (last.content || '') + contentChunk : last.content,
            reasoning: reasoningChunk ? (last.reasoning || '') + reasoningChunk : last.reasoning,
          },
        ];
      }
      return [
        ...prev,
        {
          role: 'assistant',
          content: contentChunk || '',
          reasoning: reasoningChunk || undefined,
          tool_calls: [],
        },
      ];
    });
  }, []);

  const scheduleDeltaFlush = useCallback(() => {
    if (rafIdRef.current !== null) return;
    rafIdRef.current = requestAnimationFrame(() => {
      rafIdRef.current = null;
      flushBufferedDeltas();
    });
  }, [flushBufferedDeltas]);

  const refreshSessionState = useCallback(async (sid: string) => {
    try {
      const st = await api.getSessionState(sid);
      if (activeSessionIdRef.current !== sid) return;
      setSessionMode(st.mode);
      setReasoningEffort(st.reasoning_effort);
      setContextTokens(st.context_tokens);
      setContextWindow(st.context_window);
      setQueuedCount(st.queued_messages ? st.queued_messages.length : 0);
      setTodos(st.todos || []);
      setGoal(st.goal || null);
      setMemoryProjectFilter(st.memory_project_filter || null);
      setTurnRunning(st.turn_running);
    } catch {
      // session may not be materialized yet
    }
  }, []);

  const refreshProviders = useCallback(async () => {
    try {
      const list = await api.getProviders(activeSessionIdRef.current);
      setProviders(list);
    } catch {
      // ignore
    }
  }, []);

  const loadTranscript = useCallback(
    async (sid: string) => {
      try {
        const hist = await api.getTranscript(sid, 150);
        if (activeSessionIdRef.current === sid) {
          setMessages(hist);
        }
        refreshSessionState(sid);
      } catch (e) {
        console.error('Failed to load transcript', e);
      }
    },
    [refreshSessionState]
  );

  useEffect(() => {
    api
      .bootstrap()
      .then((data) => {
        setWorkspaceRoot(data.workspace_root || '');
        setLaunchWorkspaceRoot(data.launch_workspace_root || data.workspace_root || '');
        setSessions(data.sessions);
        setSidebarAgents(data.sidebar_agents || []);
        setProviders(data.providers || []);
        setDefaultProvider(data.default_provider || 'opencode');
        setDefaultModel(data.default_model || '');
        setCompactProvider(data.compact_provider || null);
        setCompactModel(data.compact_model || null);
        setBackgroundProvider(data.background_provider || null);
        setBackgroundModel(data.background_model || null);
        setApprovalProvider(data.approval_provider || null);
        setApprovalModel(data.approval_model || null);
        setTheme(data.theme || 'dark');
        if (data.display) setDisplaySettings(data.display);
        if (data.auto_compact) setAutoCompact(data.auto_compact);
        if (data.features) setFeatures(data.features);
        if (data.workers) setWorkers(data.workers);
        if (data.skills) setSkills(data.skills);
        if (data.mcp_servers) setMcpServers(data.mcp_servers);

        if (data.sessions.length > 0) {
          const firstId = data.sessions[0].record.id;
          setActiveSessionId(firstId);
          loadTranscript(firstId);
        }
      })
      .catch((e) => showError(`Failed to initialize: ${String(e)}`));

    api.listSchedules().then(setSchedules).catch(() => {});
    api.listPlugins().then(setPlugins).catch(() => {});
  }, [loadTranscript, showError]);

  useEffect(() => {
    const unsub = api.subscribeEvents((event) => {
      const currSid = activeSessionIdRef.current;

      switch (event.kind) {
        case 'turn_started':
          if (event.session_id === currSid) {
            setTurnRunning(true);
            liveToolCallsRef.current = [];
            const opt = optimisticQueryRef.current;
            if (opt && opt.sid === event.session_id && opt.query === event.query) {
              optimisticQueryRef.current = null;
            } else {
              setMessages((prev) => [
                ...prev,
                { role: 'user', content: event.query, tool_calls: [] },
              ]);
            }
          }
          break;

        case 'content_delta':
          if (event.session_id === currSid) {
            pendingContentDeltaRef.current += event.delta;
            scheduleDeltaFlush();
          }
          break;

        case 'reasoning_delta':
          if (event.session_id === currSid) {
            pendingReasoningDeltaRef.current += event.delta;
            scheduleDeltaFlush();
          }
          break;

        case 'tool_executing':
          if (event.session_id === currSid) {
            flushBufferedDeltas();
            liveToolSeqRef.current += 1;
            const callId = `live-${liveToolSeqRef.current}`;
            liveToolCallsRef.current.push({ id: callId, name: event.name });
            setMessages((prev) => {
              const last = prev[prev.length - 1];
              const tc = {
                id: callId,
                name: event.name,
                arguments: event.arguments,
              };
              if (last && last.role === 'assistant') {
                return [
                  ...prev.slice(0, -1),
                  { ...last, tool_calls: [...last.tool_calls, tc] },
                ];
              }
              return [...prev, { role: 'assistant', content: '', tool_calls: [tc] }];
            });
          }
          break;

        case 'tool_executed':
          if (event.session_id === currSid) {
            flushBufferedDeltas();
            const matchIdx = liveToolCallsRef.current.findIndex((t) => t.name === event.name);
            let callId: string | undefined;
            if (matchIdx >= 0) {
              callId = liveToolCallsRef.current[matchIdx].id;
              liveToolCallsRef.current.splice(matchIdx, 1);
            }
            setMessages((prev) => [
              ...prev,
              {
                role: 'tool',
                content: event.output,
                tool_call_id: callId,
                tool_calls: [],
              },
            ]);
            if (event.name === 'todo_list') {
              api.getTodos(event.session_id).then((r) => setTodos(r.items || [])).catch(() => {});
            }
          }
          break;

        case 'approval_requested':
          if (event.session_id === currSid) {
            setPendingApproval({ ...event.request, request_id: event.request_id });
          }
          break;

        case 'clarify_requested':
          if (event.session_id === currSid) {
            setPendingClarify({ ...event.request, request_id: event.request_id });
          }
          break;

        case 'model_changed':
          if (event.session_id === currSid) {
            setDefaultProvider(event.provider);
            setDefaultModel(event.model);
            refreshSessionState(event.session_id);
          }
          break;

        case 'context_tokens_updated':
          if (event.session_id === currSid) {
            setContextTokens((prev) =>
              event.reset ? event.tokens_used : Math.max(prev, event.tokens_used)
            );
          }
          break;

        case 'compaction_started':
          if (event.session_id === currSid) {
            pushNotice(event.forced ? 'Compacting conversation…' : 'Auto-compacting context…');
          }
          break;

        case 'compaction_finished':
          if (event.session_id === currSid) {
            if (event.error) {
              pushNotice(`Compaction skipped: ${event.error}`, 'err');
            } else if (event.turns) {
              pushNotice(`Compacted ${event.turns} turns (${event.messages || 0} messages).`);
              loadTranscript(event.session_id);
            }
          }
          break;

        case 'memory_saved':
          if (event.session_id === currSid) {
            pushNotice(`Saved memory: ${event.name}`);
          }
          break;

        case 'note_saved':
          if (event.session_id === currSid) {
            pushNotice(`Saved note: ${event.name}`);
          }
          break;

        case 'subagent_started':
          if (event.session_id === currSid) {
            pushNotice(`Subagent started: ${event.goal.slice(0, 60)}`);
          }
          break;

        case 'subagent_finished':
          if (event.session_id === currSid) {
            pushNotice(
              `Subagent ${event.success ? 'finished' : 'failed'}: ${event.summary.slice(0, 80)}`,
              event.success ? 'info' : 'err'
            );
          }
          break;

        case 'job_completed':
          if (event.session_id === currSid) {
            pushNotice(`Job "${event.command}" ${event.status} (${event.elapsed_secs}s)`);
          }
          break;

        case 'steered':
          if (event.session_id === currSid) {
            pushNotice(`Steered turn: "${event.text.slice(0, 60)}"`);
          }
          break;

        case 'interrupted':
          if (event.session_id === currSid) {
            flushBufferedDeltas();
            setTurnRunning(false);
            setPendingApproval(null);
            setPendingClarify(null);
            refreshSessionState(event.session_id);
          }
          break;

        case 'turn_finished':
          if (event.session_id === currSid) {
            flushBufferedDeltas();
            setTurnRunning(false);
            setPendingApproval(null);
            setPendingClarify(null);
            loadTranscript(event.session_id);
          }
          api.listSessions(150).then(setSessions).catch(() => {});
          break;

        case 'turn_failed':
          if (event.session_id === currSid) {
            flushBufferedDeltas();
            setTurnRunning(false);
            setPendingApproval(null);
            setPendingClarify(null);
            refreshSessionState(event.session_id);
            setMessages((prev) => [
              ...prev,
              {
                role: 'assistant',
                content: `**Error:** ${event.error}`,
                tool_calls: [],
              },
            ]);
          }
          break;
      }
    });

    return () => {
      unsub();
      if (rafIdRef.current !== null) {
        cancelAnimationFrame(rafIdRef.current);
      }
    };
  }, [flushBufferedDeltas, loadTranscript, scheduleDeltaFlush, refreshSessionState, pushNotice]);

  const handleSelectSession = useCallback(
    (sid: string) => {
      pendingContentDeltaRef.current = '';
      pendingReasoningDeltaRef.current = '';
      liveToolCallsRef.current = [];
      setActiveSessionId(sid);
      setPendingApproval(null);
      setPendingClarify(null);
      setTurnRunning(false);
      loadTranscript(sid);
      refreshProviders();
      if (window.innerWidth <= 768) {
        setSidebarOpen(false);
      }
    },
    [loadTranscript, refreshProviders]
  );

  const handleNewSession = useCallback(
    async (agentId?: string | null) => {
      try {
        const sid = await api.createSession(agentId);
        const updated = await api.listSessions(150);
        setSessions(updated);
        handleSelectSession(sid);
        setCurrentTab('chat');
      } catch (e) {
        showError(`Failed to create session: ${String(e)}`);
      }
    },
    [handleSelectSession, showError]
  );

  const handleForkSession = useCallback(
    async (sid: string) => {
      try {
        const newSid = await api.forkSession(sid);
        const updated = await api.listSessions(150);
        setSessions(updated);
        handleSelectSession(newSid);
        pushNotice('Conversation forked.');
      } catch (e) {
        showError(`Failed to fork session: ${String(e)}`);
      }
    },
    [handleSelectSession, showError, pushNotice]
  );

  const handleDeleteSession = useCallback(
    async (sid: string) => {
      try {
        await api.deleteSession(sid);
        const updated = await api.listSessions(150);
        setSessions(updated);
        if (activeSessionIdRef.current === sid) {
          if (updated.length > 0) {
            handleSelectSession(updated[0].record.id);
          } else {
            setActiveSessionId(null);
            setMessages([]);
          }
        }
      } catch (e) {
        showError(`Failed to delete session: ${String(e)}`);
      }
    },
    [handleSelectSession, showError]
  );

  const handleRenameSession = useCallback(
    async (sid: string, name: string) => {
      try {
        await api.renameSession(sid, name);
        const updated = await api.listSessions(150);
        setSessions(updated);
      } catch (e) {
        showError(`Failed to rename session: ${String(e)}`);
      }
    },
    [showError]
  );

  const handleSendMessage = useCallback(
    async (text: string, images?: string[], sendMode?: 'steer' | 'queue') => {
      const sid = activeSessionIdRef.current;
      if (!sid) return;

      const composedDisplay =
        images && images.length > 0
          ? `${text}${text ? '\n\n' : ''}${images.join('\n\n')}`
          : text;

      if (!turnRunning) {
        optimisticQueryRef.current = { sid, query: composedDisplay };
        setTurnRunning(true);
        setMessages((prev) => [
          ...prev,
          { role: 'user', content: composedDisplay, tool_calls: [] },
        ]);
      }

      try {
        await api.sendMessage(sid, text, { images, mode: sendMode });
        if (turnRunning && sendMode === 'queue') {
          setQueuedCount((c) => c + 1);
          pushNotice('Message queued for next turn.');
        }
      } catch (e) {
        optimisticQueryRef.current = null;
        if (!turnRunning) setTurnRunning(false);
        showError(`Failed to send message: ${String(e)}`);
      }
    },
    [showError, turnRunning, pushNotice]
  );

  const handleChangeMode = useCallback(
    async (mode: AgentMode) => {
      const sid = activeSessionIdRef.current;
      if (!sid) return;
      setSessionMode(mode);
      try {
        await api.setSessionMode(sid, mode);
        pushNotice(
          `Mode: ${mode === 'build' ? 'Build (ask on edits)' : mode === 'auto' ? 'Auto (auto-approve)' : 'Plan (read-only)'}`
        );
      } catch (e) {
        showError(`Failed to change mode: ${String(e)}`);
      }
    },
    [showError, pushNotice]
  );

  const handleChangeReasoning = useCallback(
    async (effort: string | null) => {
      const sid = activeSessionIdRef.current;
      if (!sid) return;
      setReasoningEffort(effort);
      try {
        await api.setSessionReasoning(sid, effort);
        pushNotice(`Thinking effort: ${effort || 'off'}`);
      } catch (e) {
        showError(`Failed to set reasoning effort: ${String(e)}`);
      }
    },
    [showError, pushNotice]
  );

  const handleCompactSession = useCallback(async () => {
    const sid = activeSessionIdRef.current;
    if (!sid) return;
    try {
      const res = await api.compactSession(sid);
      pushNotice(`Compacted ${res.turns} turns (${res.messages} messages).`);
      loadTranscript(sid);
    } catch (e) {
      showError(String(e));
    }
  }, [loadTranscript, pushNotice, showError]);

  const handleSummarizeSession = useCallback(async () => {
    const sid = activeSessionIdRef.current;
    if (!sid) return;
    try {
      const res = await api.summarizeSession(sid);
      const updated = await api.listSessions(150);
      setSessions(updated);
      handleSelectSession(res.session_id);
      pushNotice('Summarized and started a fresh session.');
    } catch (e) {
      showError(String(e));
    }
  }, [handleSelectSession, pushNotice, showError]);

  const handleRememberSession = useCallback(async () => {
    const sid = activeSessionIdRef.current;
    if (!sid) return;
    try {
      const res = await api.rememberSession(sid);
      pushNotice(`Memory pass saved ${res.memories} memories, ${res.notes} notes.`);
    } catch (e) {
      showError(String(e));
    }
  }, [pushNotice, showError]);

  const handleSlashCommand = useCallback(
    (raw: string, images?: string[]): boolean => {
      const trimmed = raw.trim();
      if (!trimmed.startsWith('/')) return false;
      const spaceIdx = trimmed.indexOf(' ');
      const cmd = (spaceIdx === -1 ? trimmed : trimmed.slice(0, spaceIdx)).toLowerCase();
      const arg = spaceIdx === -1 ? '' : trimmed.slice(spaceIdx + 1).trim();
      const sid = activeSessionIdRef.current;

      if (cmd === '/model') {
        const btn = document.querySelector('.composer-model-pill');
        setModelPickerAnchor(btn ? btn.getBoundingClientRect() : null);
        setModelPickerTarget('session');
        setIsModelPickerOpen(true);
        return true;
      }
      if (cmd === '/thinking' || cmd === '/reasoning') {
        if (!arg) {
          setActiveDrawer(null);
          pushNotice(`Current thinking effort: ${reasoningEffort || 'off'}`);
        } else {
          handleChangeReasoning(arg === 'off' || arg === 'none' ? null : arg);
        }
        return true;
      }
      if (cmd === '/plan') {
        handleChangeMode(sessionMode === 'plan' ? 'build' : 'plan');
        return true;
      }
      if (cmd === '/mode') {
        if (arg === 'build' || arg === 'auto' || arg === 'plan') {
          handleChangeMode(arg);
        } else {
          handleChangeMode(
            sessionMode === 'build' ? 'auto' : sessionMode === 'auto' ? 'plan' : 'build'
          );
        }
        return true;
      }
      if (cmd === '/compact') {
        handleCompactSession();
        return true;
      }
      if (cmd === '/auto-compact') {
        setSettingsTab('models');
        setIsSettingsOpen(true);
        return true;
      }
      if (cmd === '/summarize') {
        handleSummarizeSession();
        return true;
      }
      if (cmd === '/remember') {
        handleRememberSession();
        return true;
      }
      if (cmd === '/memory' || cmd === '/notes') {
        if (cmd === '/memory' && arg && sid) {
          const next = arg === 'reset' || arg === 'all' ? null : arg;
          api
            .setMemoryScope(sid, next)
            .then(() => {
              setMemoryProjectFilter(next);
              pushNotice(next ? `Memory scope: ${next}` : 'Memory scope cleared.');
            })
            .catch((e) => showError(String(e)));
        } else {
          setCurrentTab('memories');
        }
        return true;
      }
      if (cmd === '/todo') {
        setActiveDrawer('todos');
        return true;
      }
      if (cmd === '/diff' || cmd === '/rewind') {
        setActiveDrawer('checkpoints');
        return true;
      }
      if (cmd === '/subagents' || cmd === '/tasks') {
        setActiveDrawer('tasks');
        return true;
      }
      if (cmd === '/usage' || cmd === '/heartbeat') {
        setActiveDrawer('usage');
        return true;
      }
      if (cmd === '/goal') {
        if (!sid) return true;
        if (!arg) {
          setActiveDrawer('tasks');
        } else if (arg === 'stop' || arg === 'cancel') {
          api.manageGoal(sid, 'stop').then(() => {
            setGoal(null);
            pushNotice('Goal loop stop requested.');
          });
        } else {
          api
            .manageGoal(sid, 'start', arg, 10)
            .then((r) => {
              if (r.goal) setGoal(r.goal);
            })
            .catch((e) => showError(String(e)));
        }
        return true;
      }
      if (cmd === '/dir') {
        setIsWorkspacePickerOpen(true);
        return true;
      }
      if (cmd === '/connect') {
        setSettingsTab('accounts');
        setIsSettingsOpen(true);
        return true;
      }
      if (cmd === '/worker') {
        setSettingsTab('workers');
        setIsSettingsOpen(true);
        return true;
      }
      if (cmd === '/skills' || cmd === '/mcp') {
        setSettingsTab('features');
        setIsSettingsOpen(true);
        return true;
      }
      if (cmd === '/settings') {
        setSettingsTab('general');
        setIsSettingsOpen(true);
        return true;
      }
      if (cmd === '/new') {
        handleNewSession(null);
        return true;
      }
      if (cmd === '/fork' && sid) {
        handleForkSession(sid);
        return true;
      }
      if (cmd === '/delete' && sid) {
        handleDeleteSession(sid);
        return true;
      }
      if (cmd === '/name' && sid && arg) {
        handleRenameSession(sid, arg);
        return true;
      }

      // Check configured workers: /<name> <task>
      const workerKey = cmd.slice(1);
      if (workers[workerKey] && sid && arg) {
        api
          .runWorker(sid, workerKey, arg, images)
          .then((r) => {
            pushNotice(`Worker /${r.worker} started (${r.model}).`);
            setActiveDrawer('tasks');
          })
          .catch((e) => showError(String(e)));
        return true;
      }

      return false;
    },
    [
      reasoningEffort,
      sessionMode,
      workers,
      handleChangeReasoning,
      handleChangeMode,
      handleCompactSession,
      handleSummarizeSession,
      handleRememberSession,
      handleNewSession,
      handleForkSession,
      handleDeleteSession,
      handleRenameSession,
      pushNotice,
      showError,
    ]
  );

  const handleInterrupt = useCallback(async () => {
    const sid = activeSessionIdRef.current;
    if (!sid) return;
    try {
      await api.interrupt(sid);
    } catch (e) {
      console.error(e);
    }
  }, []);

  const handleResolveApproval = useCallback(
    async (
      decision: 'run' | 'deny' | 'run_with_args' | 'deny_with_feedback',
      args?: unknown,
      feedback?: string
    ) => {
      const sid = activeSessionIdRef.current;
      if (!sid) return;
      const reqId = pendingApproval?.request_id;
      try {
        await api.resolveApproval(sid, decision, args, feedback, reqId);
        setPendingApproval(null);
      } catch (e) {
        showError(`Failed to resolve approval: ${String(e)}`);
      }
    },
    [pendingApproval, showError]
  );

  const handleResolveClarify = useCallback(
    async (answers: Array<string | string[]>) => {
      const sid = activeSessionIdRef.current;
      if (!sid) return;
      const reqId = pendingClarify?.request_id;
      try {
        await api.resolveClarify(sid, answers, reqId);
        setPendingClarify(null);
      } catch (e) {
        showError(`Failed to resolve clarification: ${String(e)}`);
      }
    },
    [pendingClarify, showError]
  );

  const handleSelectModel = useCallback(
    async (provider: string, model: string, scope: ModelScope, target: ModelTarget) => {
      await api.setDefaultModel(
        provider,
        model,
        activeSessionIdRef.current,
        scope,
        target
      );
      if (target === 'compact') {
        setCompactProvider(provider);
        setCompactModel(model);
      } else if (target === 'bgsaver') {
        setBackgroundProvider(provider);
        setBackgroundModel(model);
      } else if (target === 'approval') {
        setApprovalProvider(provider);
        setApprovalModel(model);
      } else {
        setDefaultProvider(provider);
        setDefaultModel(model);
      }
      const updated = await api.listSessions(150);
      setSessions(updated);
      if (activeSessionIdRef.current) {
        refreshSessionState(activeSessionIdRef.current);
      }
      pushNotice(`Model set: ${provider} / ${model}`);
    },
    [refreshSessionState, pushNotice]
  );

  const handleRefreshProvider = useCallback(
    async (providerId: string, credentialId?: string, force = true): Promise<string[]> => {
      const models = await api.refreshProviderModels(providerId, credentialId, force);
      if (!credentialId) {
        setProviders((prev) =>
          prev.map((p) => (p.id === providerId ? { ...p, models } : p))
        );
      }
      return models;
    },
    []
  );

  const handleSaveCredential = useCallback(
    async (provider: string, key: string, label?: string) => {
      await api.saveCredential(provider, key, label, true);
      await refreshProviders();
    },
    [refreshProviders]
  );

  const handleRemoveCredential = useCallback(
    async (provider: string, credId: string) => {
      await api.removeCredential(provider, credId);
      await refreshProviders();
    },
    [refreshProviders]
  );

  const handleSetPriorityAccount = useCallback(
    async (provider: string, credId: string, scope: 'session' | 'global') => {
      await api.setPriorityCredential(
        provider,
        credId,
        scope,
        activeSessionIdRef.current
      );
      await refreshProviders();
      pushNotice(
        scope === 'session'
          ? `Session priority account updated for ${provider}.`
          : `Global priority account updated for ${provider}.`
      );
    },
    [refreshProviders, pushNotice]
  );

  const handleSetRotationStrategy = useCallback(
    async (provider: string, strategy: RotationStrategy) => {
      await api.setRotationStrategy(provider, strategy);
      await refreshProviders();
    },
    [refreshProviders]
  );

  const handleResetCooldowns = useCallback(
    async (provider?: string) => {
      await api.resetCooldowns(provider);
      await refreshProviders();
    },
    [refreshProviders]
  );

  const handleAddCustomProvider = useCallback(
    async (payload: {
      id: string;
      base_url: string;
      api_key?: string;
      context_window?: number;
    }) => {
      await api.addCustomProvider(payload);
      await refreshProviders();
    },
    [refreshProviders]
  );

  const handleSelectWorkspace = useCallback(
    async (path: string, scope: 'session' | 'global' = 'session', reset = false) => {
      const res = await api.setWorkspace(path, scope, reset);
      setWorkspaceRoot(res.workspace_root);
      pushNotice(`Workspace: ${res.workspace_root}`);
    },
    [pushNotice]
  );

  const handleUpdateSettings = useCallback(async (patch: Record<string, unknown>) => {
    await api.updateSettings(patch);
    if (patch.theme === 'dark' || patch.theme === 'light') {
      setTheme(patch.theme);
      setDisplaySettings((prev) => ({ ...prev, theme: patch.theme as 'dark' | 'light' }));
    }
    if (typeof patch.auto_collapse === 'boolean') {
      setDisplaySettings((prev) => ({ ...prev, auto_collapse: patch.auto_collapse as boolean }));
    }
    if (typeof patch.auto_hide_thinking === 'boolean') {
      setDisplaySettings((prev) => ({
        ...prev,
        auto_hide_thinking: patch.auto_hide_thinking as boolean,
      }));
    }
    if (typeof patch.auto_compact_enabled === 'boolean') {
      setAutoCompact((prev) => ({ ...prev, enabled: patch.auto_compact_enabled as boolean }));
    }
    if (typeof patch.auto_compact_threshold === 'number') {
      setAutoCompact((prev) => ({ ...prev, threshold: patch.auto_compact_threshold as number }));
    }
    if (typeof patch.auto_compact_percent === 'number') {
      setAutoCompact((prev) => ({ ...prev, percent: patch.auto_compact_percent as number }));
    }
    if (typeof patch.memory_enabled === 'boolean') {
      setFeatures((prev) => ({ ...prev, memory: patch.memory_enabled as boolean }));
    }
    if (typeof patch.notes_enabled === 'boolean') {
      setFeatures((prev) => ({ ...prev, notes: patch.notes_enabled as boolean }));
    }
    if (typeof patch.skills_enabled === 'boolean') {
      setFeatures((prev) => ({ ...prev, skills: patch.skills_enabled as boolean }));
    }
    if (typeof patch.mcp_enabled === 'boolean') {
      setFeatures((prev) => ({ ...prev, mcp: patch.mcp_enabled as boolean }));
    }
    if (patch.memory_mode === 'session' || patch.memory_mode === 'background') {
      setFeatures((prev) => ({
        ...prev,
        memory_mode: patch.memory_mode as 'session' | 'background',
      }));
    }
  }, []);

  const handleCreateSchedule = async (
    title: string,
    prompt: string,
    scheduleType: 'cron' | 'interval',
    cronExpr?: string,
    intervalSecs?: number
  ) => {
    try {
      await api.createSchedule(title, prompt, scheduleType, cronExpr, intervalSecs);
      const updated = await api.listSchedules();
      setSchedules(updated);
    } catch (e) {
      showError(`Failed to create automation: ${String(e)}`);
    }
  };

  const handleToggleSchedule = async (taskId: string, enabled: boolean) => {
    try {
      await api.toggleSchedule(taskId, enabled);
      setSchedules((prev) =>
        prev.map((s) => (s.id === taskId ? { ...s, enabled } : s))
      );
    } catch (e) {
      showError(`Failed to toggle automation: ${String(e)}`);
    }
  };

  const handleDeleteSchedule = async (taskId: string) => {
    try {
      await api.deleteSchedule(taskId);
      setSchedules((prev) => prev.filter((s) => s.id !== taskId));
    } catch (e) {
      showError(`Failed to delete automation: ${String(e)}`);
    }
  };

  const handleTogglePlugin = async (pluginId: string, enabled: boolean) => {
    try {
      await api.togglePlugin(pluginId, enabled);
      setPlugins((prev) =>
        prev.map((p) => (p.id === pluginId ? { ...p, enabled } : p))
      );
    } catch (e) {
      showError(`Failed to toggle plugin: ${String(e)}`);
    }
  };

  const handleCreatePluginFromSpec = async (spec: PluginBuilderSpec) => {
    await api.createPluginFromSpec(spec);
    const updated = await api.listPlugins();
    setPlugins(updated);
    const boot = await api.bootstrap();
    setSidebarAgents(boot.sidebar_agents || []);
  };

  const activeSessionItem = sessions.find((s) => s.record.id === activeSessionId);
  const activeTitle = activeSessionItem
    ? activeSessionItem.record.name ||
      activeSessionItem.record.title ||
      `Session ${activeSessionItem.record.id.slice(0, 7)}`
    : '';
  const activeProvider = activeSessionItem?.record.model?.[0] || defaultProvider;
  const activeModel = activeSessionItem?.record.model?.[1] || defaultModel;

  const activeProviderObj = providers.find((p) => p.id === activeProvider);
  const activeAccount =
    activeProviderObj?.accounts.find((a) => a.is_session_priority) ||
    activeProviderObj?.accounts.find((a) => a.is_priority) ||
    (activeProviderObj?.accounts.length === 1 ? activeProviderObj.accounts[0] : undefined);

  return (
    <div className="app-container">
      <Sidebar
        isOpen={sidebarOpen}
        onClose={() => setSidebarOpen(false)}
        currentTab={currentTab}
        onSelectTab={setCurrentTab}
        sessions={sessions}
        activeSessionId={activeSessionId}
        onSelectSession={handleSelectSession}
        onNewSession={handleNewSession}
        onForkSession={handleForkSession}
        onDeleteSession={handleDeleteSession}
        onRenameSession={handleRenameSession}
        sidebarAgents={sidebarAgents}
        workspaceRoot={workspaceRoot}
        onOpenWorkspacePicker={() => setIsWorkspacePickerOpen(true)}
        onOpenSettings={() => {
          setSettingsTab('general');
          setIsSettingsOpen(true);
        }}
        theme={theme}
        onToggleTheme={() => {
          const next = theme === 'dark' ? 'light' : 'dark';
          setTheme(next);
          handleUpdateSettings({ theme: next }).catch(() => {});
        }}
      />

      <main className="main-content">
        {currentTab === 'chat' && (
          <ChatView
            sessionId={activeSessionId}
            sessionTitle={activeTitle}
            workspaceRoot={workspaceRoot}
            provider={activeProvider}
            model={activeModel}
            accountLabel={activeAccount?.label || null}
            mode={sessionMode}
            reasoningEffort={reasoningEffort}
            contextTokens={contextTokens}
            contextWindow={contextWindow}
            queuedCount={queuedCount}
            todos={todos}
            goal={goal}
            workers={workers}
            skills={skills}
            notices={notices}
            onDismissNotice={(id) => setNotices((prev) => prev.filter((n) => n.id !== id))}
            messages={messages}
            turnRunning={turnRunning}
            sidebarOpen={sidebarOpen}
            onToggleSidebar={() => setSidebarOpen((v) => !v)}
            onOpenModelPicker={(rect) => {
              setModelPickerAnchor(rect ?? null);
              setModelPickerTarget('session');
              setIsModelPickerOpen(true);
            }}
            onOpenWorkspacePicker={() => setIsWorkspacePickerOpen(true)}
            onOpenDrawer={setActiveDrawer}
            onRenameSession={(name) =>
              activeSessionId && handleRenameSession(activeSessionId, name)
            }
            onCompactSession={handleCompactSession}
            onSummarizeSession={handleSummarizeSession}
            onRememberSession={handleRememberSession}
            onForkSession={() => activeSessionId && handleForkSession(activeSessionId)}
            onNewSession={() => handleNewSession(null)}
            onSendMessage={handleSendMessage}
            onSlashCommand={handleSlashCommand}
            onInterrupt={handleInterrupt}
            onChangeMode={handleChangeMode}
            onChangeReasoning={handleChangeReasoning}
            onOpenThinkingPicker={(rect) => {
              const menu = document.querySelector('.thinking-menu');
              if (menu instanceof HTMLElement) menu.focus();
              void rect;
            }}
            pendingApproval={pendingApproval}
            onResolveApproval={handleResolveApproval}
            pendingClarify={pendingClarify}
            onResolveClarify={handleResolveClarify}
            onAskApprovalQuestion={api.askApprovalQuestion.bind(api)}
          />
        )}

        {currentTab === 'automations' && (
          <AutomationsView
            tasks={schedules}
            sidebarOpen={sidebarOpen}
            onToggleSidebar={() => setSidebarOpen((v) => !v)}
            onCreateTask={handleCreateSchedule}
            onToggleTask={handleToggleSchedule}
            onDeleteTask={handleDeleteSchedule}
          />
        )}

        {currentTab === 'plugins' && (
          <PluginsView
            plugins={plugins}
            sidebarOpen={sidebarOpen}
            onToggleSidebar={() => setSidebarOpen((v) => !v)}
            onTogglePlugin={handleTogglePlugin}
            onOpenBuilder={() => setIsPluginBuilderOpen(true)}
          />
        )}

        {currentTab === 'memories' && (
          <MemoryNotesView
            sidebarOpen={sidebarOpen}
            onToggleSidebar={() => setSidebarOpen((v) => !v)}
            onSearchMemories={api.searchMemories.bind(api)}
            onSearchNotes={api.searchNotes.bind(api)}
          />
        )}
      </main>

      <SessionDrawers
        activeTab={activeDrawer}
        onClose={() => setActiveDrawer(null)}
        sessionId={activeSessionId}
        todos={todos}
        onTodosChange={setTodos}
        goal={goal}
        onGoalChange={setGoal}
        workers={workers}
        contextTokens={contextTokens}
        contextWindow={contextWindow}
        onNotify={(msg) => pushNotice(msg, 'info')}
        onError={showError}
      />

      <PluginBuilderModal
        isOpen={isPluginBuilderOpen}
        onClose={() => setIsPluginBuilderOpen(false)}
        onSubmit={handleCreatePluginFromSpec}
      />

      <ModelPickerModal
        isOpen={isModelPickerOpen}
        onClose={() => {
          setIsModelPickerOpen(false);
          setModelPickerAnchor(null);
        }}
        anchorRect={modelPickerAnchor}
        providers={providers}
        activeProvider={activeProvider}
        activeModel={activeModel}
        sessionId={activeSessionId}
        initialTarget={modelPickerTarget}
        onSelectModel={handleSelectModel}
        onRefreshProvider={handleRefreshProvider}
        onSaveCredential={handleSaveCredential}
        onRemoveCredential={handleRemoveCredential}
        onSetPriorityAccount={handleSetPriorityAccount}
        onSetRotationStrategy={handleSetRotationStrategy}
        onResetCooldowns={handleResetCooldowns}
        onAddCustomProvider={handleAddCustomProvider}
      />

      <WorkspaceModal
        isOpen={isWorkspacePickerOpen}
        onClose={() => setIsWorkspacePickerOpen(false)}
        currentWorkspace={workspaceRoot}
        launchWorkspaceRoot={launchWorkspaceRoot}
        onSelectWorkspace={handleSelectWorkspace}
      />

      <SettingsModal
        isOpen={isSettingsOpen}
        initialTab={settingsTab}
        onClose={() => setIsSettingsOpen(false)}
        sessionId={activeSessionId}
        providers={providers}
        defaultProvider={defaultProvider}
        defaultModel={defaultModel}
        compactProvider={compactProvider}
        compactModel={compactModel}
        backgroundProvider={backgroundProvider}
        backgroundModel={backgroundModel}
        approvalProvider={approvalProvider}
        approvalModel={approvalModel}
        workspaceRoot={workspaceRoot}
        launchWorkspaceRoot={launchWorkspaceRoot}
        display={displaySettings}
        autoCompact={autoCompact}
        features={features}
        memoryProjectFilter={memoryProjectFilter}
        workers={workers}
        skills={skills}
        mcpServers={mcpServers}
        onOpenModelPicker={(target) => {
          setModelPickerAnchor(null);
          setModelPickerTarget(target);
          setIsModelPickerOpen(true);
        }}
        onOpenWorkspacePicker={() => setIsWorkspacePickerOpen(true)}
        onResetWorkspace={() => handleSelectWorkspace('', 'session', true)}
        onSaveCredential={handleSaveCredential}
        onRemoveCredential={handleRemoveCredential}
        onSetPriorityAccount={handleSetPriorityAccount}
        onSetRotationStrategy={handleSetRotationStrategy}
        onResetCooldowns={handleResetCooldowns}
        onRefreshProviders={refreshProviders}
        onUpdateSettings={handleUpdateSettings}
        onRunRemember={handleRememberSession}
        onSetMemoryScope={async (project) => {
          if (!activeSessionId) return;
          await api.setMemoryScope(activeSessionId, project);
          setMemoryProjectFilter(project);
        }}
        onWorkersChange={setWorkers}
      />
    </div>
  );
};
