import {
  Bot,
  CalendarClock,
  Database,
  GitFork,
  MessageSquare,
  Moon,
  Plus,
  Puzzle,
  Settings,
  Trash2,
} from "lucide-react";
import type { GuiSessionItem, PluginAgentEntry } from "../types";

export type ActiveTab = "chat" | "plugins" | "automations" | "memory";

interface SidebarProps {
  workspaceRoot: string;
  agents: PluginAgentEntry[];
  sessions: GuiSessionItem[];
  activeTab: ActiveTab;
  activeSessionId: string | null;
  activeAgentId: string | null;
  onSelectTab: (tab: ActiveTab) => void;
  onNewChat: (pluginAgentId: string | null) => void;
  onSelectSession: (item: GuiSessionItem) => void;
  onSelectAgent: (agent: PluginAgentEntry | null) => void;
  onForkSession: (id: string) => void;
  onDeleteSession: (id: string) => void;
  onOpenSettings: () => void;
}

export function Sidebar({
  workspaceRoot,
  agents,
  sessions,
  activeTab,
  activeSessionId,
  activeAgentId,
  onSelectTab,
  onNewChat,
  onSelectSession,
  onSelectAgent,
  onForkSession,
  onDeleteSession,
  onOpenSettings,
}: SidebarProps) {
  return (
    <aside className="sidebar">
      <div className="sidebar-header">
        <div className="brand-row">
          <div className="brand-title">
            <div className="brand-badge">
              <Moon size={15} />
            </div>
            <span>Luna</span>
          </div>
          <button
            className="icon-btn"
            title="Model & Account Settings"
            onClick={onOpenSettings}
          >
            <Settings size={16} />
          </button>
        </div>
        <div className="workspace-pill" title={workspaceRoot}>
          {workspaceRoot || "~"}
        </div>
        <button
          className="btn-primary"
          onClick={() => onNewChat(activeTab === "chat" ? activeAgentId : null)}
        >
          <Plus size={15} />
          <span>New Chat</span>
        </button>
      </div>

      <div className="sidebar-scroll">
        <div>
          <div className="section-label">
            <span>Agents</span>
            <span>{agents.length + 1}</span>
          </div>
          <button
            className={`nav-item ${
              activeTab === "chat" && activeAgentId === null ? "active" : ""
            }`}
            onClick={() => onSelectAgent(null)}
          >
            <div className="nav-item-main">
              <Moon size={15} color="#e5b84b" />
              <span className="nav-item-title">Luna Core</span>
            </div>
            <span className="nav-tag">Default</span>
          </button>

          {agents.map((agent) => (
            <button
              key={agent.id}
              className={`nav-item ${
                activeTab === "chat" && activeAgentId === agent.plugin_id
                  ? "active"
                  : ""
              }`}
              onClick={() => onSelectAgent(agent)}
              title={agent.description}
            >
              <div className="nav-item-main">
                <Bot size={15} color="#6aa9ff" />
                <span className="nav-item-title">{agent.name}</span>
              </div>
              <span className="nav-tag">
                {agent.registered_tools.length > 0
                  ? `${agent.registered_tools.length} tools`
                  : "Plugin"}
              </span>
            </button>
          ))}
        </div>

        <div>
          <div className="section-label">
            <span>Conversations</span>
            <span>{sessions.length}</span>
          </div>
          {sessions.length === 0 ? (
            <div style={{ padding: "6px 10px", fontSize: 12, color: "var(--text-muted)" }}>
              No conversations yet
            </div>
          ) : (
            sessions.map((item) => {
              const title =
                item.record.name ||
                item.record.title ||
                `Session ${item.record.id}`;
              const isSelected =
                activeTab === "chat" && activeSessionId === item.record.id;
              return (
                <div
                  key={item.record.id}
                  className={`nav-item ${isSelected ? "active" : ""}`}
                  onClick={() => onSelectSession(item)}
                  role="button"
                  tabIndex={0}
                >
                  <div className="nav-item-main">
                    <MessageSquare size={14} />
                    <span className="nav-item-title">{title}</span>
                  </div>
                  <div className="session-actions">
                    <button
                      className="icon-btn"
                      title="Fork conversation"
                      onClick={(e) => {
                        e.stopPropagation();
                        onForkSession(item.record.id);
                      }}
                    >
                      <GitFork size={13} />
                    </button>
                    <button
                      className="icon-btn"
                      title="Delete conversation"
                      onClick={(e) => {
                        e.stopPropagation();
                        onDeleteSession(item.record.id);
                      }}
                    >
                      <Trash2 size={13} />
                    </button>
                  </div>
                </div>
              );
            })
          )}
        </div>
      </div>

      <div className="sidebar-footer">
        <button
          className={`footer-nav-btn ${activeTab === "plugins" ? "active" : ""}`}
          onClick={() => onSelectTab("plugins")}
        >
          <Puzzle size={14} />
          <span>Plugins</span>
        </button>
        <button
          className={`footer-nav-btn ${activeTab === "automations" ? "active" : ""}`}
          onClick={() => onSelectTab("automations")}
        >
          <CalendarClock size={14} />
          <span>Schedules</span>
        </button>
        <button
          className={`footer-nav-btn ${activeTab === "memory" ? "active" : ""}`}
          onClick={() => onSelectTab("memory")}
        >
          <Database size={14} />
          <span>Memory</span>
        </button>
        <button className="footer-nav-btn" onClick={onOpenSettings}>
          <Settings size={14} />
          <span>Settings</span>
        </button>
      </div>
    </aside>
  );
}
