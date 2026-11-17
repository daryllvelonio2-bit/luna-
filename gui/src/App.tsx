import { useCallback, useEffect, useState } from "react";
import { api } from "./api";
import { AutomationsView } from "./components/AutomationsView";
import {
  ChatView,
  type LiveSubagentItem,
  type LiveToolItem,
  type PendingApproval,
  type PendingClarify,
  type PendingSudo,
} from "./components/ChatView";
import { MemoryNotesView } from "./components/MemoryNotesView";
import { PluginsView } from "./components/PluginsView";
import { SettingsModal } from "./components/SettingsModal";
import { Sidebar, type ActiveTab } from "./components/Sidebar";
import type {
  GuiSessionItem,
  PluginAgentEntry,
  PluginSummary,
  ScheduleTask,
  TranscriptMessageDto,
} from "./types";

export function App() {
  const [workspaceRoot, setWorkspaceRoot] = useState("");
  const [defaultProvider, setDefaultProvider] = useState("");
  const [defaultModel, setDefaultModel] = useState("");
  const [sessions, setSessions] = useState<GuiSessionItem[]>([]);
  const [agents, setAgents] = useState<PluginAgentEntry[]>([]);
  const [plugins, setPlugins] = useState<PluginSummary[]>([]);
  const [schedules, setSchedules] = useState<ScheduleTask[]>([]);

  const [activeTab, setActiveTab] = useState<ActiveTab>("chat");
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [activeAgentId, setActiveAgentId] = useState<string | null>(null);
  const [transcript, setTranscript] = useState<TranscriptMessageDto[]>([]);

  const [isRunning, setIsRunning] = useState(false);
  const [streamingThinking, setStreamingThinking] = useState("");
  const [streamingContent, setStreamingContent] = useState("");
  const [liveTools, setLiveTools] = useState<LiveToolItem[]>([]);
  const [liveSubagents, setLiveSubagents] = useState<LiveSubagentItem[]>([]);

  const [pendingApproval, setPendingApproval] = useState<PendingApproval | null>(null);
  const [pendingClarify, setPendingClarify] = useState<PendingClarify | null>(null);
  const [pendingSudo, setPendingSudo] = useState<PendingSudo | null>(null);
  const [showSettings, setShowSettings] = useState(false);

  const loadTranscript = useCallback(async (sessionId: string) => {
    const msgs = await api.getTranscript(sessionId);
    setTranscript(msgs);
  }, []);

  const refreshPlugins = useCallback(async () => {
    const [p, b] = await Promise.all([api.listPlugins(), api.bootstrap()]);
    setPlugins(p);
    setAgents(b.sidebar_agents);
    setSchedules(b.schedules);
  }, []);

  const refreshSchedules = useCallback(async () => {
    const list = await api.listSchedules();
    setSchedules(list);
  }, []);

  const refreshSessions = useCallback(async () => {
    const list = await api.listSessions();
    setSessions(list);
  }, []);

  useEffect(() => {
    api.bootstrap().then(async (b) => {
      setWorkspaceRoot(b.workspace_root);
      setDefaultProvider(b.default_provider);
      setDefaultModel(b.default_model);
      setSessions(b.sessions);
      setAgents(b.sidebar_agents);
      setPlugins(b.plugins);
      setSchedules(b.schedules);
      if (b.sessions.length > 0) {
        const first = b.sessions[0];
        setActiveSessionId(first.record.id);
        setActiveAgentId(first.plugin_agent_id);
        await loadTranscript(first.record.id);
      } else {
        const created = await api.createSession(null);
        setSessions([created]);
        setActiveSessionId(created.record.id);
      }
    });
  }, [loadTranscript]);

  useEffect(() => {
    const unlisteners: Array<() => void> = [];

    api.onSessionsUpdated(() => {
      void refreshSessions();
    }).then((u) => unlisteners.push(u));

    api.onPluginsUpdated(() => {
      void refreshPlugins();
    }).then((u) => unlisteners.push(u));

    api.onSchedulesUpdated(() => {
      void refreshSchedules();
    }).then((u) => unlisteners.push(u));

    api.onAppEvent((ev) => {
      if (ev.session_id !== activeSessionId) return;

      switch (ev.type) {
        case "turn_started":
          setIsRunning(true);
          setStreamingThinking("");
          setStreamingContent("");
          setLiveTools([]);
          setLiveSubagents([]);
          setTranscript((prev) => [
            ...prev,
            {
              role: "user",
              content: ev.query,
              reasoning: null,
              tool_calls: [],
              tool_call_id: null,
            },
          ]);
          break;
        case "thinking_delta":
          setStreamingThinking((prev) => prev + ev.text);
          break;
        case "content_delta":
          setStreamingContent((prev) => prev + ev.text);
          break;
        case "tool_call_started":
          setLiveTools((prev) => [
            ...prev,
            {
              id: `${ev.name}-${prev.length}`,
              name: ev.name,
              arguments: ev.arguments,
              diff: ev.diff,
              finished: false,
            },
          ]);
          break;
        case "tool_call_finished":
          setLiveTools((prev) => {
            const next = [...prev];
            const idx = next.findIndex((t) => !t.finished && t.name === ev.name);
            if (idx >= 0) {
              next[idx] = {
                ...next[idx],
                output: ev.output,
                is_error: ev.is_error,
                duration_ms: ev.duration_ms,
                finished: true,
              };
            }
            return next;
          });
          break;
        case "subagent_started":
          setLiveSubagents((prev) => [
            ...prev,
            {
              task_id: ev.task_id,
              goal: ev.goal,
              activity: "starting",
              finished: false,
            },
          ]);
          break;
        case "subagent_progress":
          setLiveSubagents((prev) =>
            prev.map((s) =>
              s.task_id === ev.task_id ? { ...s, activity: ev.activity } : s
            )
          );
          break;
        case "subagent_finished":
          setLiveSubagents((prev) =>
            prev.map((s) =>
              s.task_id === ev.task_id
                ? {
                    ...s,
                    finished: true,
                    success: ev.success,
                    summary: ev.summary,
                  }
                : s
            )
          );
          break;
        case "turn_finished":
        case "turn_failed":
          setIsRunning(false);
          setStreamingThinking("");
          setStreamingContent("");
          setLiveTools([]);
          void loadTranscript(ev.session_id);
          break;
        case "approval_requested":
          setPendingApproval({
            request_id: ev.request_id,
            session_id: ev.session_id,
            tool: ev.request.tool,
            target: ev.request.target,
            diff: ev.request.diff,
          });
          break;
        case "clarify_requested":
          setPendingClarify({
            request_id: ev.request_id,
            session_id: ev.session_id,
            questions: ev.request.questions,
          });
          break;
        case "sudo_requested":
          setPendingSudo({
            request_id: ev.request_id,
            session_id: ev.session_id,
            command: ev.command,
          });
          break;
        case "model_changed":
          setDefaultProvider(ev.provider);
          setDefaultModel(ev.model);
          break;
      }
    }).then((u) => unlisteners.push(u));

    return () => {
      unlisteners.forEach((u) => u());
    };
  }, [activeSessionId, loadTranscript, refreshPlugins, refreshSchedules, refreshSessions]);

  const handleNewChat = async (pluginAgentId: string | null) => {
    const item = await api.createSession(pluginAgentId);
    setSessions((prev) => [item, ...prev]);
    setActiveSessionId(item.record.id);
    setActiveAgentId(pluginAgentId);
    setTranscript([]);
    setActiveTab("chat");
  };

  const handleSelectAgent = async (agent: PluginAgentEntry | null) => {
    const pluginId = agent ? agent.plugin_id : null;
    setActiveAgentId(pluginId);
    setActiveTab("chat");
    const existing = sessions.find((s) => s.plugin_agent_id === pluginId);
    if (existing) {
      setActiveSessionId(existing.record.id);
      await loadTranscript(existing.record.id);
    } else {
      await handleNewChat(pluginId);
    }
  };

  const activeSession = sessions.find((s) => s.record.id === activeSessionId) ?? null;
  const activeAgent = agents.find((a) => a.plugin_id === activeAgentId) ?? null;
  const activePlugin = plugins.find((p) => p.id === activeAgentId) ?? null;
  const currentProvider = activeSession?.record.model?.[0] ?? defaultProvider;
  const currentModel = activeSession?.record.model?.[1] ?? defaultModel;

  return (
    <div className="app-shell">
      <Sidebar
        workspaceRoot={workspaceRoot}
        agents={agents}
        sessions={sessions}
        activeTab={activeTab}
        activeSessionId={activeSessionId}
        activeAgentId={activeAgentId}
        onSelectTab={setActiveTab}
        onNewChat={handleNewChat}
        onSelectSession={async (item) => {
          setActiveSessionId(item.record.id);
          setActiveAgentId(item.plugin_agent_id);
          setActiveTab("chat");
          await loadTranscript(item.record.id);
        }}
        onSelectAgent={handleSelectAgent}
        onForkSession={async (id) => {
          const forked = await api.forkSession(id);
          setSessions((prev) => [forked, ...prev]);
          setActiveSessionId(forked.record.id);
          setActiveAgentId(forked.plugin_agent_id);
          await loadTranscript(forked.record.id);
        }}
        onDeleteSession={async (id) => {
          await api.deleteSession(id);
          const next = sessions.filter((s) => s.record.id !== id);
          setSessions(next);
          if (activeSessionId === id) {
            if (next.length > 0) {
              setActiveSessionId(next[0].record.id);
              setActiveAgentId(next[0].plugin_agent_id);
              await loadTranscript(next[0].record.id);
            } else {
              await handleNewChat(null);
            }
          }
        }}
        onOpenSettings={() => setShowSettings(true)}
      />

      <main className="main-pane">
        {activeTab === "chat" && (
          <ChatView
            sessionId={activeSessionId}
            activeAgent={activeAgent}
            activePlugin={activePlugin}
            transcript={transcript}
            streamingThinking={streamingThinking}
            streamingContent={streamingContent}
            liveTools={liveTools}
            liveSubagents={liveSubagents}
            isRunning={isRunning}
            modelLabel={currentModel || "Select model"}
            schedules={schedules}
            pendingApproval={pendingApproval}
            pendingClarify={pendingClarify}
            pendingSudo={pendingSudo}
            onSend={async (text) => {
              let sid = activeSessionId;
              if (!sid) {
                const created = await api.createSession(activeAgentId);
                sid = created.record.id;
                setSessions((prev) => [created, ...prev]);
                setActiveSessionId(sid);
              }
              await api.sendMessage(sid, text, activeAgentId);
            }}
            onInterrupt={async () => {
              if (activeSessionId) {
                await api.interruptTurn(activeSessionId);
              }
            }}
            onOpenSettings={() => setShowSettings(true)}
            onClearApproval={() => setPendingApproval(null)}
            onClearClarify={() => setPendingClarify(null)}
            onClearSudo={() => setPendingSudo(null)}
            onRefreshSchedules={refreshSchedules}
          />
        )}

        {activeTab === "plugins" && (
          <PluginsView
            plugins={plugins}
            onRefresh={refreshPlugins}
            onOpenAgent={handleSelectAgent}
          />
        )}

        {activeTab === "automations" && (
          <AutomationsView
            schedules={schedules}
            onRefresh={refreshSchedules}
          />
        )}

        {activeTab === "memory" && <MemoryNotesView />}
      </main>

      {showSettings && (
        <SettingsModal
          sessionId={activeSessionId}
          currentProvider={currentProvider}
          currentModel={currentModel}
          onClose={() => setShowSettings(false)}
          onModelSwitched={(p, m) => {
            setDefaultProvider(p);
            setDefaultModel(m);
            void refreshSessions();
          }}
        />
      )}
    </div>
  );
}
