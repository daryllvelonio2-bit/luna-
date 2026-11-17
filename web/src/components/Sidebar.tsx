import React from 'react';
import type { GuiSessionItem, PluginAgentEntry } from '../types';
import {
  MessageSquare,
  Clock,
  Layers,
  BookOpen,
  Settings,
  Plus,
  Trash2,
  GitFork,
  Edit2,
  Bot,
  Sun,
  Moon,
} from 'lucide-react';

interface SidebarProps {
  currentTab: 'chat' | 'automations' | 'plugins' | 'memories';
  onSelectTab: (tab: 'chat' | 'automations' | 'plugins' | 'memories') => void;
  sessions: GuiSessionItem[];
  activeSessionId: string | null;
  onSelectSession: (id: string) => void;
  onNewSession: (agentId?: string | null) => void;
  onForkSession: (id: string) => void;
  onDeleteSession: (id: string) => void;
  onRenameSession: (id: string, name: string) => void;
  sidebarAgents: PluginAgentEntry[];
  onOpenSettings: () => void;
  theme: 'dark' | 'light';
  onToggleTheme: () => void;
}

export const Sidebar: React.FC<SidebarProps> = ({
  currentTab,
  onSelectTab,
  sessions,
  activeSessionId,
  onSelectSession,
  onNewSession,
  onForkSession,
  onDeleteSession,
  onRenameSession,
  sidebarAgents,
  onOpenSettings,
  theme,
  onToggleTheme,
}) => {
  return (
    <div className="sidebar">
      <div className="sidebar-header">
        <div className="brand-title">
          <span>🌙 Luna</span>
        </div>
        <button
          className="action-btn"
          onClick={onToggleTheme}
          title={`Switch to ${theme === 'dark' ? 'light' : 'dark'} theme`}
        >
          {theme === 'dark' ? <Sun size={16} /> : <Moon size={16} />}
        </button>
      </div>

      <div className="sidebar-nav">
        <button
          className={`nav-item ${currentTab === 'chat' ? 'active' : ''}`}
          onClick={() => onSelectTab('chat')}
        >
          <MessageSquare size={16} />
          <span>Chat</span>
        </button>
        <button
          className={`nav-item ${currentTab === 'automations' ? 'active' : ''}`}
          onClick={() => onSelectTab('automations')}
        >
          <Clock size={16} />
          <span>Automations</span>
        </button>
        <button
          className={`nav-item ${currentTab === 'plugins' ? 'active' : ''}`}
          onClick={() => onSelectTab('plugins')}
        >
          <Layers size={16} />
          <span>Plugins</span>
        </button>
        <button
          className={`nav-item ${currentTab === 'memories' ? 'active' : ''}`}
          onClick={() => onSelectTab('memories')}
        >
          <BookOpen size={16} />
          <span>Memory & Notes</span>
        </button>
      </div>

      {sidebarAgents.length > 0 && (
        <div className="session-section" style={{ flex: 'none', maxHeight: '160px' }}>
          <div className="session-section-header">
            <span>Specialized Agents</span>
          </div>
          <div className="session-list">
            {sidebarAgents.map((ag) => (
              <div
                key={ag.id}
                className="session-item"
                onClick={() => onNewSession(ag.id)}
                title={ag.description}
              >
                <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
                  <Bot size={14} color="var(--accent-color)" />
                  <span className="session-name">{ag.name}</span>
                </div>
                <Plus size={14} />
              </div>
            ))}
          </div>
        </div>
      )}

      <div className="session-section">
        <div className="session-section-header">
          <span>Conversations</span>
          <button className="new-btn" onClick={() => onNewSession(null)} title="New chat">
            <Plus size={16} />
          </button>
        </div>
        <div className="session-list">
          {sessions.map((item) => {
            const sid = item.record.id;
            const displayName =
              item.record.name || item.record.title || `Session ${sid.slice(0, 8)}`;
            const isActive = activeSessionId === sid;

            return (
              <div
                key={sid}
                className={`session-item ${isActive ? 'active' : ''}`}
                onClick={() => {
                  onSelectSession(sid);
                  onSelectTab('chat');
                }}
              >
                <span className="session-name" title={displayName}>
                  {displayName}
                </span>
                <div className="session-actions" onClick={(e) => e.stopPropagation()}>
                  <button
                    className="action-btn"
                    title="Rename"
                    onClick={() => {
                      const newName = window.prompt('Rename session:', displayName);
                      if (newName && newName.trim()) {
                        onRenameSession(sid, newName.trim());
                      }
                    }}
                  >
                    <Edit2 size={13} />
                  </button>
                  <button
                    className="action-btn"
                    title="Fork"
                    onClick={() => onForkSession(sid)}
                  >
                    <GitFork size={13} />
                  </button>
                  <button
                    className="action-btn"
                    title="Delete"
                    onClick={() => {
                      if (window.confirm('Delete this session?')) {
                        onDeleteSession(sid);
                      }
                    }}
                  >
                    <Trash2 size={13} />
                  </button>
                </div>
              </div>
            );
          })}
        </div>
      </div>

      <div className="sidebar-footer">
        <button
          className="nav-item"
          style={{ width: '100%', padding: '6px 8px' }}
          onClick={onOpenSettings}
        >
          <Settings size={16} />
          <span>Settings</span>
        </button>
      </div>
    </div>
  );
};
