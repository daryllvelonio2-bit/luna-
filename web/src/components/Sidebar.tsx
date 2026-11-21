import React, { useState, useMemo, useRef } from 'react';
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
  PanelLeftClose,
  Check,
  X,
  FolderOpen,
  Search,
  Calendar,
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
  workspaceRoot: string;
  onOpenWorkspacePicker: () => void;
  onOpenSettings: () => void;
  theme: 'dark' | 'light';
  onToggleTheme: () => void;
}

export const Sidebar: React.FC<SidebarProps> = React.memo(
  ({
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
    workspaceRoot,
    onOpenWorkspacePicker,
    onOpenSettings,
    theme,
    onToggleTheme,
  }) => {
    const [editingId, setEditingId] = useState<string | null>(null);
    const [editingName, setEditingName] = useState('');
    const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);
    const [filter, setFilter] = useState('');
    const [visibleLimit, setVisibleLimit] = useState(40);
    const filterInputRef = useRef<HTMLInputElement>(null);
    void theme;
    void onToggleTheme;

    const filteredSessions = useMemo(() => {
      const q = filter.trim().toLowerCase();
      if (!q) return sessions;
      return sessions.filter((s) => {
        const label = (s.record.name || s.record.title || s.record.id).toLowerCase();
        return label.includes(q);
      });
    }, [sessions, filter]);

    const visibleSessions = useMemo(
      () => filteredSessions.slice(0, visibleLimit),
      [filteredSessions, visibleLimit]
    );

    if (!isOpen) return null;

    const commitRename = (sid: string) => {
      const trimmed = editingName.trim();
      if (trimmed) {
        onRenameSession(sid, trimmed);
      }
      setEditingId(null);
    };

    const workspaceFolderName =
      workspaceRoot.split('/').filter(Boolean).pop() || workspaceRoot || 'Workspace';

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
              onClick={() => filterInputRef.current?.focus()}
              title="Search conversations"
              aria-label="Search conversations"
            >
              <Search size={15} />
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
            <span>New Chat</span>
          </button>
        </div>

        <nav className="sidebar-nav" aria-label="Main navigation">
          <button
            type="button"
            className="nav-item"
            onClick={onOpenWorkspacePicker}
            title={`Workspace: ${workspaceRoot}`}
          >
            <FolderOpen size={15} />
            <span className="sidebar-nav-truncate">{workspaceFolderName}</span>
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
              {sidebarAgents.map((agent) => (
                <button
                  key={agent.id}
                  type="button"
                  className="session-item sidebar-agent-item"
                  title={agent.description}
                  onClick={() => {
                    onNewSession(agent.id);
                    onSelectTab('chat');
                  }}
                >
                  <span className="session-item-agent">
                    {agent.id.includes('scheduler') ? <Calendar size={14} /> : <Bot size={14} />}
                    <span className="session-name">{agent.name}</span>
                  </span>
                  <Plus size={13} className="session-agent-plus" />
                </button>
              ))}
            </div>
          </div>
        )}

        <div className="sidebar-conversations">
          <div className="sidebar-group-label">Conversations</div>
          <div className="sidebar-search-wrap">
            <MessageSquare size={13} className="sidebar-search-icon" />
            <input
              ref={filterInputRef}
              type="text"
              className="sidebar-search-input"
              placeholder="Filter chats..."
              value={filter}
              onChange={(e) => setFilter(e.target.value)}
            />
          </div>
          <div className="session-list">
            {visibleSessions.map((item) => {
              const sid = item.record.id;
              const isActive = sid === activeSessionId && currentTab === 'chat';
              const displayName =
                item.record.name || item.record.title || `Session ${sid.slice(0, 7)}`;

              if (editingId === sid) {
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
                      onBlur={() => commitRename(sid)}
                    />
                  </div>
                );
              }

              return (
                <div
                  key={sid}
                  className={`session-item ${isActive ? 'active' : ''}`}
                  onClick={() => {
                    onSelectSession(sid);
                    onSelectTab('chat');
                    setConfirmDeleteId(null);
                  }}
                >
                  <span className="session-name" title={displayName}>
                    {displayName}
                  </span>
                  <div
                    className={`session-actions ${confirmDeleteId === sid ? 'visible' : ''}`}
                    onClick={(e) => e.stopPropagation()}
                  >
                    {confirmDeleteId === sid ? (
                      <>
                        <button
                          type="button"
                          className="action-btn action-btn-danger"
                          title="Confirm delete"
                          aria-label="Confirm delete"
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
                          aria-label="Cancel delete"
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
                          aria-label="Rename conversation"
                          onClick={() => {
                            setEditingId(sid);
                            setEditingName(displayName);
                          }}
                        >
                          <Edit2 size={12} />
                        </button>
                        <button
                          type="button"
                          className="action-btn"
                          title="Fork"
                          aria-label="Fork conversation"
                          onClick={() => onForkSession(sid)}
                        >
                          <GitFork size={12} />
                        </button>
                        <button
                          type="button"
                          className="action-btn"
                          title="Delete"
                          aria-label="Delete conversation"
                          onClick={() => setConfirmDeleteId(sid)}
                        >
                          <Trash2 size={12} />
                        </button>
                      </>
                    )}
                  </div>
                </div>
              );
            })}

            {filteredSessions.length > visibleLimit && (
              <button
                type="button"
                className="sidebar-show-more"
                onClick={() => setVisibleLimit((v) => v + 50)}
              >
                Show more ({filteredSessions.length - visibleLimit})
              </button>
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
  }
);
