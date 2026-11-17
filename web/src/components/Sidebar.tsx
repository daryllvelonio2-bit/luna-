import React, { useState } from 'react';
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
  PanelLeftClose,
  Check,
  X,
} from 'lucide-react';

interface SidebarProps {
  isOpen: boolean;
  onClose: () => void;
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
  isOpen,
  onClose,
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
  const [editingId, setEditingId] = useState<string | null>(null);
  const [editingName, setEditingName] = useState('');
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);

  if (!isOpen) return null;

  const commitRename = (sid: string) => {
    const trimmed = editingName.trim();
    if (trimmed) {
      onRenameSession(sid, trimmed);
    }
    setEditingId(null);
  };

  return (
    <aside className="sidebar">
      <div className="sidebar-header">
        <div className="brand-title">
          <span className="brand-mark" aria-hidden="true">
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none">
              <path
                d="M20.5 14.5A8.5 8.5 0 0 1 9.5 3.5a8.5 8.5 0 1 0 11 11Z"
                fill="currentColor"
              />
            </svg>
          </span>
          <span>Luna</span>
        </div>

        <div className="sidebar-header-actions">
          <button
            type="button"
            className="icon-btn"
            onClick={onToggleTheme}
            title={`Switch to ${theme === 'dark' ? 'light' : 'dark'} theme`}
            aria-label="Toggle theme"
          >
            {theme === 'dark' ? <Sun size={15} /> : <Moon size={15} />}
          </button>
          <button
            type="button"
            className="icon-btn"
            onClick={onClose}
            title="Collapse sidebar"
            aria-label="Collapse sidebar"
          >
            <PanelLeftClose size={15} />
          </button>
        </div>
      </div>

      <div className="sidebar-top-action">
        <button
          type="button"
          className="new-chat-btn"
          onClick={() => {
            onNewSession(null);
            onSelectTab('chat');
          }}
        >
          <Plus size={15} />
          <span>New chat</span>
        </button>
      </div>

      <nav className="sidebar-nav" aria-label="Primary">
        <button
          type="button"
          className={`nav-item ${currentTab === 'chat' ? 'active' : ''}`}
          onClick={() => onSelectTab('chat')}
        >
          <MessageSquare size={15} />
          <span>Chat</span>
        </button>
        <button
          type="button"
          className={`nav-item ${currentTab === 'automations' ? 'active' : ''}`}
          onClick={() => onSelectTab('automations')}
        >
          <Clock size={15} />
          <span>Automations</span>
        </button>
        <button
          type="button"
          className={`nav-item ${currentTab === 'plugins' ? 'active' : ''}`}
          onClick={() => onSelectTab('plugins')}
        >
          <Layers size={15} />
          <span>Plugins</span>
        </button>
        <button
          type="button"
          className={`nav-item ${currentTab === 'memories' ? 'active' : ''}`}
          onClick={() => onSelectTab('memories')}
        >
          <BookOpen size={15} />
          <span>Memory & Notes</span>
        </button>
      </nav>

      {sidebarAgents.length > 0 && (
        <div className="sidebar-group">
          <div className="sidebar-group-label">Agents</div>
          <div className="sidebar-group-list">
            {sidebarAgents.map((ag) => (
              <button
                key={ag.id}
                type="button"
                className="session-item"
                onClick={() => {
                  onNewSession(ag.id);
                  onSelectTab('chat');
                }}
                title={ag.description}
              >
                <span className="session-item-agent">
                  <Bot size={14} className="text-accent" />
                  <span className="session-name">{ag.name}</span>
                </span>
                <Plus size={13} className="session-agent-plus" />
              </button>
            ))}
          </div>
        </div>
      )}

      <div className="sidebar-conversations">
        <div className="sidebar-group-label">Conversations</div>
        <div className="session-list">
          {sessions.length === 0 ? (
            <div className="sidebar-empty">No conversations yet</div>
          ) : (
            sessions.map((item) => {
              const sid = item.record.id;
              const displayName =
                item.record.name || item.record.title || `Session ${sid.slice(0, 8)}`;
              const isActive = activeSessionId === sid && currentTab === 'chat';
              const isEditing = editingId === sid;
              const isConfirmingDelete = confirmDeleteId === sid;

              if (isEditing) {
                return (
                  <div key={sid} className="session-item session-item-editing">
                    <input
                      type="text"
                      className="session-rename-input"
                      value={editingName}
                      autoFocus
                      onChange={(e) => setEditingName(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === 'Enter') commitRename(sid);
                        if (e.key === 'Escape') setEditingId(null);
                      }}
                    />
                    <button
                      type="button"
                      className="action-btn"
                      title="Save"
                      onClick={() => commitRename(sid)}
                    >
                      <Check size={13} />
                    </button>
                    <button
                      type="button"
                      className="action-btn"
                      title="Cancel"
                      onClick={() => setEditingId(null)}
                    >
                      <X size={13} />
                    </button>
                  </div>
                );
              }

              return (
                <div
                  key={sid}
                  className={`session-item ${isActive ? 'active' : ''}`}
                  onClick={() => {
                    setConfirmDeleteId(null);
                    onSelectSession(sid);
                    onSelectTab('chat');
                  }}
                >
                  <span className="session-name" title={displayName}>
                    {displayName}
                  </span>
                  <div
                    className={`session-actions ${isConfirmingDelete ? 'visible' : ''}`}
                    onClick={(e) => e.stopPropagation()}
                  >
                    {isConfirmingDelete ? (
                      <>
                        <button
                          type="button"
                          className="action-btn action-btn-danger"
                          title="Confirm delete"
                          onClick={() => {
                            setConfirmDeleteId(null);
                            onDeleteSession(sid);
                          }}
                        >
                          <Check size={13} />
                        </button>
                        <button
                          type="button"
                          className="action-btn"
                          title="Cancel"
                          onClick={() => setConfirmDeleteId(null)}
                        >
                          <X size={13} />
                        </button>
                      </>
                    ) : (
                      <>
                        <button
                          type="button"
                          className="action-btn"
                          title="Rename"
                          onClick={() => {
                            setEditingId(sid);
                            setEditingName(displayName);
                            setConfirmDeleteId(null);
                          }}
                        >
                          <Edit2 size={12} />
                        </button>
                        <button
                          type="button"
                          className="action-btn"
                          title="Fork conversation"
                          onClick={() => onForkSession(sid)}
                        >
                          <GitFork size={12} />
                        </button>
                        <button
                          type="button"
                          className="action-btn"
                          title="Delete"
                          onClick={() => setConfirmDeleteId(sid)}
                        >
                          <Trash2 size={12} />
                        </button>
                      </>
                    )}
                  </div>
                </div>
              );
            })
          )}
        </div>
      </div>

      <div className="sidebar-footer">
        <button type="button" className="nav-item w-full" onClick={onOpenSettings}>
          <Settings size={15} />
          <span>Settings</span>
        </button>
      </div>
    </aside>
  );
};
