import React, { useState, useEffect, useCallback } from 'react';
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

export const App: React.FC = () => {
  const [currentTab, setCurrentTab] = useState<'chat' | 'automations' | 'plugins' | 'memories'>('chat');
  const [theme, setTheme] = useState<'dark' | 'light'>('dark');

  const [sessions, setSessions] = useState<GuiSessionItem[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [messages, setMessages] = useState<TranscriptMessageDto[]>([]);
  const [turnRunning, setTurnRunning] = useState(false);

  const [sidebarAgents, setSidebarAgents] = useState<PluginAgentEntry[]>([]);
  const [providers, setProviders] = useState<ProviderInfo[]>([]);
  const [defaultProvider, setDefaultProvider] = useState('opencode');
  const [defaultModel, setDefaultModel] = useState('');

  const [pendingApproval, setPendingApproval] = useState<ApprovalRequestPayload | null>(null);
  const [pendingClarify, setPendingClarify] = useState<ClarifyRequestPayload | null>(null);

  const [schedules, setSchedules] = useState<ScheduleTask[]>([]);
  const [plugins, setPlugins] = useState<PluginSummary[]>([]);

  const [isSettingsOpen, setIsSettingsOpen] = useState(false);
  const [isPluginBuilderOpen, setIsPluginBuilderOpen] = useState(false);

  // Apply theme to document
  useEffect(() => {
    document.documentElement.setAttribute('data-theme', theme);
  }, [theme]);

  const loadTranscript = useCallback(async (sid: string) => {
    try {
      const hist = await api.getTranscript(sid);
      setMessages(hist);
    } catch (e) {
      console.error('Failed to load transcript', e);
    }
  }, []);

  // Bootstrap initial state
  useEffect(() => {
    api
      .bootstrap()
      .then((data) => {
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
      .catch((e) => console.error('Bootstrap failed', e));

    // Load initial schedules and plugins
    api.listSchedules().then(setSchedules).catch(() => {});
    api.listPlugins().then(setPlugins).catch(() => {});
  }, [loadTranscript]);

  // Subscribe to WebSocket events
  useEffect(() => {
    const unsub = api.subscribeEvents((event) => {
      switch (event.type) {
        case 'TurnStarted':
          if (event.session_id === activeSessionId) {
            setTurnRunning(true);
            setMessages((prev) => [...prev, { role: 'user', content: event.query, tool_calls: [] }]);
          }
          break;

        case 'StreamDelta':
          if (event.session_id === activeSessionId) {
            setMessages((prev) => {
              const last = prev[prev.length - 1];
              if (last && last.role === 'assistant') {
                return [
                  ...prev.slice(0, -1),
                  { ...last, content: (last.content || '') + event.delta },
                ];
              }
              return [...prev, { role: 'assistant', content: event.delta, tool_calls: [] }];
            });
          }
          break;

        case 'StreamReasoningDelta':
          if (event.session_id === activeSessionId) {
            setMessages((prev) => {
              const last = prev[prev.length - 1];
              if (last && last.role === 'assistant') {
                return [
                  ...prev.slice(0, -1),
                  { ...last, reasoning: (last.reasoning || '') + event.delta },
                ];
              }
              return [
                ...prev,
                { role: 'assistant', content: '', reasoning: event.delta, tool_calls: [] },
              ];
            });
          }
          break;

        case 'ToolCallStarted':
          if (event.session_id === activeSessionId) {
            setMessages((prev) => {
              const last = prev[prev.length - 1];
              const tc = { id: event.call_id, name: event.tool_name, arguments: event.arguments };
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

        case 'ApprovalRequested':
          if (event.session_id === activeSessionId) {
            setPendingApproval(event.request);
          }
          break;

        case 'ApprovalResolved':
          if (event.session_id === activeSessionId) {
            setPendingApproval(null);
          }
          break;

        case 'ClarifyRequested':
          if (event.session_id === activeSessionId) {
            setPendingClarify(event.request);
          }
          break;

        case 'ClarifyResolved':
          if (event.session_id === activeSessionId) {
            setPendingClarify(null);
          }
          break;

        case 'TurnCompleted':
          if (event.session_id === activeSessionId) {
            setTurnRunning(false);
          }
          // Refresh sessions list message count
          api.listSessions().then(setSessions).catch(() => {});
          break;

        case 'TurnError':
          if (event.session_id === activeSessionId) {
            setTurnRunning(false);
            setMessages((prev) => [
              ...prev,
              { role: 'assistant', content: `⚠️ Error: ${event.error}`, tool_calls: [] },
            ]);
          }
          break;
      }
    });

    return () => unsub();
  }, [activeSessionId]);

  const handleSelectSession = (sid: string) => {
    setActiveSessionId(sid);
    setPendingApproval(null);
    setPendingClarify(null);
    setTurnRunning(false);
    loadTranscript(sid);
  };

  const handleNewSession = async (agentId?: string | null) => {
    try {
      const sid = await api.createSession(agentId);
      const updated = await api.listSessions();
      setSessions(updated);
      handleSelectSession(sid);
      setCurrentTab('chat');
    } catch (e) {
      alert(`Failed to create session: ${String(e)}`);
    }
  };

  const handleForkSession = async (sid: string) => {
    try {
      const newSid = await api.forkSession(sid);
      const updated = await api.listSessions();
      setSessions(updated);
      handleSelectSession(newSid);
    } catch (e) {
      alert(`Failed to fork session: ${String(e)}`);
    }
  };

  const handleDeleteSession = async (sid: string) => {
    try {
      await api.deleteSession(sid);
      const updated = await api.listSessions();
      setSessions(updated);
      if (activeSessionId === sid) {
        if (updated.length > 0) {
          handleSelectSession(updated[0].record.id);
        } else {
          setActiveSessionId(null);
          setMessages([]);
        }
      }
    } catch (e) {
      alert(`Failed to delete session: ${String(e)}`);
    }
  };

  const handleRenameSession = async (sid: string, name: string) => {
    try {
      await api.renameSession(sid, name);
      const updated = await api.listSessions();
      setSessions(updated);
    } catch (e) {
      alert(`Failed to rename: ${String(e)}`);
    }
  };

  const handleSendMessage = async (text: string) => {
    if (!activeSessionId) return;
    try {
      await api.sendMessage(activeSessionId, text);
    } catch (e) {
      alert(`Failed to send: ${String(e)}`);
    }
  };

  const handleInterrupt = async () => {
    if (!activeSessionId) return;
    try {
      await api.interrupt(activeSessionId);
    } catch (e) {
      console.error(e);
    }
  };

  const handleResolveApproval = async (
    decision: 'run' | 'deny' | 'run_with_args' | 'deny_with_feedback',
    args?: unknown,
    feedback?: string
  ) => {
    if (!activeSessionId) return;
    try {
      await api.resolveApproval(activeSessionId, decision, args, feedback);
      setPendingApproval(null);
    } catch (e) {
      alert(`Failed to resolve approval: ${String(e)}`);
    }
  };

  const handleResolveClarify = async (answers: Array<string | string[]>) => {
    if (!activeSessionId) return;
    try {
      await api.resolveClarify(activeSessionId, answers);
      setPendingClarify(null);
    } catch (e) {
      alert(`Failed to resolve clarification: ${String(e)}`);
    }
  };

  // Automations handlers
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
      alert(`Failed to create schedule: ${String(e)}`);
    }
  };

  const handleToggleSchedule = async (taskId: string, enabled: boolean) => {
    try {
      await api.toggleSchedule(taskId, enabled);
      setSchedules((prev) =>
        prev.map((s) => (s.id === taskId ? { ...s, enabled } : s))
      );
    } catch (e) {
      alert(`Failed to toggle: ${String(e)}`);
    }
  };

  const handleDeleteSchedule = async (taskId: string) => {
    try {
      await api.deleteSchedule(taskId);
      setSchedules((prev) => prev.filter((s) => s.id !== taskId));
    } catch (e) {
      alert(`Failed to delete schedule: ${String(e)}`);
    }
  };

  // Plugin handlers
  const handleTogglePlugin = async (pluginId: string, enabled: boolean) => {
    try {
      await api.togglePlugin(pluginId, enabled);
      setPlugins((prev) =>
        prev.map((p) => (p.id === pluginId ? { ...p, enabled } : p))
      );
    } catch (e) {
      alert(`Failed to toggle plugin: ${String(e)}`);
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
    ? activeSessionItem.record.name || activeSessionItem.record.title || `Session ${activeSessionItem.record.id.slice(0, 8)}`
    : '';

  return (
    <div className="app-container">
      <Sidebar
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
        onOpenSettings={() => setIsSettingsOpen(true)}
        theme={theme}
        onToggleTheme={() => setTheme((t) => (t === 'dark' ? 'light' : 'dark'))}
      />

      <div className="main-content">
        {currentTab === 'chat' && (
          <ChatView
            sessionId={activeSessionId}
            sessionTitle={activeTitle}
            provider={defaultProvider}
            model={defaultModel}
            messages={messages}
            turnRunning={turnRunning}
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
            onCreateTask={handleCreateSchedule}
            onToggleTask={handleToggleSchedule}
            onDeleteTask={handleDeleteSchedule}
          />
        )}

        {currentTab === 'plugins' && (
          <PluginsView
            plugins={plugins}
            onTogglePlugin={handleTogglePlugin}
            onOpenBuilder={() => setIsPluginBuilderOpen(true)}
          />
        )}

        {currentTab === 'memories' && (
          <MemoryNotesView
            onSearchMemories={api.searchMemories.bind(api)}
            onSearchNotes={api.searchNotes.bind(api)}
          />
        )}
      </div>

      <PluginBuilderModal
        isOpen={isPluginBuilderOpen}
        onClose={() => setIsPluginBuilderOpen(false)}
        onSubmit={handleCreatePluginFromSpec}
      />

      <SettingsModal
        isOpen={isSettingsOpen}
        onClose={() => setIsSettingsOpen(false)}
        providers={providers}
        defaultProvider={defaultProvider}
        defaultModel={defaultModel}
        onSaveDefaultModel={async (p, m) => {
          await api.setDefaultModel(p, m);
          setDefaultProvider(p);
          setDefaultModel(m);
        }}
        onSaveCredential={api.saveCredential.bind(api)}
      />
    </div>
  );
};
