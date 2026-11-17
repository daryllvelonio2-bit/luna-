import React, { useState, useEffect, useCallback, useRef } from 'react';
import { api } from './api';
import type {
  ApprovalRequestPayload,
  ClarifyRequestPayload,
  GuiSessionItem,
  PluginAgentEntry,
  PluginBuilderSpec,
  PluginSummary,
  ProviderInfo,
  ScheduleTask,
  TranscriptMessageDto,
} from './types';
import { Sidebar } from './components/Sidebar';
import { ChatView } from './components/ChatView';
import { AutomationsView } from './components/AutomationsView';
import { PluginsView } from './components/PluginsView';
import { PluginBuilderModal } from './components/PluginBuilderModal';
import { MemoryNotesView } from './components/MemoryNotesView';
import { SettingsModal } from './components/SettingsModal';
import { ModelPickerModal } from './components/ModelPickerModal';
import { WorkspaceModal } from './components/WorkspaceModal';

export const App: React.FC = () => {
  const [currentTab, setCurrentTab] = useState<'chat' | 'automations' | 'plugins' | 'memories'>('chat');
  const [theme, setTheme] = useState<'dark' | 'light'>('dark');
  const [sidebarOpen, setSidebarOpen] = useState(() =>
    typeof window !== 'undefined' ? window.innerWidth > 768 : true
  );

  const [sessions, setSessions] = useState<GuiSessionItem[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [messages, setMessages] = useState<TranscriptMessageDto[]>([]);
  const [turnRunning, setTurnRunning] = useState(false);

  const [workspaceRoot, setWorkspaceRoot] = useState('');
  const [sidebarAgents, setSidebarAgents] = useState<PluginAgentEntry[]>([]);
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [defaultProvider, setDefaultProvider] = useState('opencode');
  const [defaultModel, setDefaultModel] = useState('');

  const [pendingApproval, setPendingApproval] = useState<ApprovalRequestPayload | null>(null);
  const [pendingClarify, setPendingClarify] = useState<ClarifyRequestPayload | null>(null);

  const [schedules, setSchedules] = useState<ScheduleTask[]>([]);
  const [plugins, setPlugins] = useState<PluginSummary[]>([]);

  const [isSettingsOpen, setIsSettingsOpen] = useState(false);
  const [isModelPickerOpen, setIsModelPickerOpen] = useState(false);
  const [isWorkspacePickerOpen, setIsWorkspacePickerOpen] = useState(false);
  const [isPluginBuilderOpen, setIsPluginBuilderOpen] = useState(false);
  const [toastError, setToastError] = useState<string | null>(null);

  const activeSessionIdRef = useRef<string | null>(null);
  activeSessionIdRef.current = activeSessionId;

  // RAF-batched streaming delta buffer
  const pendingContentDeltaRef = useRef('');
  const pendingReasoningDeltaRef = useRef('');
  const rafIdRef = useRef<number | null>(null);
  const liveToolCallsRef = useRef<Array<{ id: string; name: string }>>([]);
  const liveToolSeqRef = useRef(0);
  const optimisticQueryRef = useRef<{ sid: string; query: string } | null>(null);

  const showError = useCallback((msg: string) => {
    setToastError(msg);
    setTimeout(() => {
      setToastError((curr) => (curr === msg ? null : curr));
    }, 5000);
  }, []);

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

  const loadTranscript = useCallback(async (sid: string) => {
    try {
      const hist = await api.getTranscript(sid, 150);
      if (activeSessionIdRef.current === sid) {
        setMessages(hist);
      }
    } catch (e) {
      console.error('Failed to load transcript', e);
    }
  }, []);

  useEffect(() => {
    api
      .bootstrap()
      .then((data) => {
        setWorkspaceRoot(data.workspace_root || '');
        setSessions(data.sessions);
        setSidebarAgents(data.sidebar_agents || []);
        setProviders(data.providers || []);
        setDefaultProvider(data.default_provider || 'opencode');
        setDefaultModel(data.default_model || '');
        setTheme(data.theme || 'dark');

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
          }
          break;

        case 'interrupted':
          if (event.session_id === currSid) {
            flushBufferedDeltas();
            setTurnRunning(false);
            setPendingApproval(null);
            setPendingClarify(null);
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
  }, [flushBufferedDeltas, loadTranscript, scheduleDeltaFlush]);

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
      if (window.innerWidth <= 768) {
        setSidebarOpen(false);
      }
    },
    [loadTranscript]
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
      } catch (e) {
        showError(`Failed to fork session: ${String(e)}`);
      }
    },
    [handleSelectSession, showError]
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
    async (text: string) => {
      const sid = activeSessionIdRef.current;
      if (!sid) return;
      optimisticQueryRef.current = { sid, query: text };
      setTurnRunning(true);
      setMessages((prev) => [...prev, { role: 'user', content: text, tool_calls: [] }]);
      try {
        await api.sendMessage(sid, text);
      } catch (e) {
        optimisticQueryRef.current = null;
        setTurnRunning(false);
        showError(`Failed to send message: ${String(e)}`);
      }
    },
    [showError]
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
    async (provider: string, model: string) => {
      await api.setDefaultModel(provider, model, activeSessionIdRef.current);
      setDefaultProvider(provider);
      setDefaultModel(model);
      const updated = await api.listSessions(150);
      setSessions(updated);
    },
    []
  );

  const handleRefreshProvider = useCallback(async (providerId: string) => {
    const models = await api.refreshProviderModels(providerId);
    setProviders((prev) =>
      prev.map((p) => (p.id === providerId ? { ...p, models } : p))
    );
  }, []);

  const handleSaveCredential = useCallback(async (provider: string, key: string) => {
    await api.saveCredential(provider, key);
    setProviders((prev) =>
      prev.map((p) => (p.id === provider ? { ...p, configured: true } : p))
    );
  }, []);

  const handleSelectWorkspace = useCallback(async (path: string) => {
    const res = await api.setWorkspace(path);
    setWorkspaceRoot(res.workspace_root);
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

  return (
    <div className="app-container">
      {toastError && (
        <div className="toast-error" role="alert">
          <span>{toastError}</span>
          <button
            type="button"
            className="toast-close"
            onClick={() => setToastError(null)}
            aria-label="Dismiss error"
          >
            ×
          </button>
        </div>
      )}

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
        onOpenSettings={() => setIsSettingsOpen(true)}
        theme={theme}
        onToggleTheme={() => setTheme((t) => (t === 'dark' ? 'light' : 'dark'))}
      />

      <main className="main-content">
        {currentTab === 'chat' && (
          <ChatView
            sessionId={activeSessionId}
            sessionTitle={activeTitle}
            workspaceRoot={workspaceRoot}
            provider={activeProvider}
            model={activeModel}
            messages={messages}
            turnRunning={turnRunning}
            sidebarOpen={sidebarOpen}
            onToggleSidebar={() => setSidebarOpen((v) => !v)}
            onOpenModelPicker={() => setIsModelPickerOpen(true)}
            onOpenWorkspacePicker={() => setIsWorkspacePickerOpen(true)}
            onNewSession={() => handleNewSession(null)}
            onSendMessage={handleSendMessage}
            onInterrupt={handleInterrupt}
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

      <PluginBuilderModal
        isOpen={isPluginBuilderOpen}
        onClose={() => setIsPluginBuilderOpen(false)}
        onSubmit={handleCreatePluginFromSpec}
      />

      <ModelPickerModal
        isOpen={isModelPickerOpen}
        onClose={() => setIsModelPickerOpen(false)}
        providers={providers}
        activeProvider={activeProvider}
        activeModel={activeModel}
        onSelectModel={handleSelectModel}
        onRefreshProvider={handleRefreshProvider}
        onSaveCredential={handleSaveCredential}
      />

      <WorkspaceModal
        isOpen={isWorkspacePickerOpen}
        onClose={() => setIsWorkspacePickerOpen(false)}
        currentWorkspace={workspaceRoot}
        onSelectWorkspace={handleSelectWorkspace}
      />

      <SettingsModal
        isOpen={isSettingsOpen}
        onClose={() => setIsSettingsOpen(false)}
        providers={providers}
        defaultProvider={activeProvider}
        defaultModel={activeModel}
        workspaceRoot={workspaceRoot}
        onOpenModelPicker={() => setIsModelPickerOpen(true)}
        onOpenWorkspacePicker={() => setIsWorkspacePickerOpen(true)}
        onSaveCredential={handleSaveCredential}
      />
    </div>
  );
};
