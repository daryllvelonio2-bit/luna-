import React from 'react';
import type { PluginSummary } from '../types';
import {
  ToggleLeft,
  ToggleRight,
  Sparkles,
  Layers,
  PanelLeft,
  RefreshCw,
  Trash2,
  AlertCircle,
} from 'lucide-react';

interface PluginsViewProps {
  plugins: PluginSummary[];
  sidebarOpen: boolean;
  onToggleSidebar: () => void;
  onTogglePlugin: (id: string, enabled: boolean) => void;
  onDeletePlugin: (id: string) => void;
  onRefresh: () => void;
  onStartPluginConversation: () => void;
}

export const PluginsView: React.FC<PluginsViewProps> = ({
  plugins,
  sidebarOpen,
  onToggleSidebar,
  onTogglePlugin,
  onDeletePlugin,
  onRefresh,
  onStartPluginConversation,
}) => {
  return (
    <div className="page-view">
      <header className="chat-header">
        <div className="chat-header-left">
          {!sidebarOpen && (
            <button
              type="button"
              className="icon-btn"
              onClick={onToggleSidebar}
              title="Open sidebar"
              aria-label="Open sidebar"
            >
              <PanelLeft size={16} />
            </button>
          )}
          <span className="chat-header-title">Plugins</span>
        </div>
        <div className="chat-header-right" style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
          <button
            type="button"
            className="icon-btn"
            onClick={onRefresh}
            title="Refresh plugins"
            aria-label="Refresh plugins"
          >
            <RefreshCw size={14} />
          </button>
          <button type="button" className="btn btn-primary" onClick={onStartPluginConversation}>
            <Sparkles size={14} />
            <span>New plugin</span>
          </button>
        </div>
      </header>

      <div className="page-scroll">
        <div className="page-container">
          <div className="page-intro">
            <h1 className="page-title">Extensions & plugins</h1>
            <p className="page-subtitle">
              Describe a plugin for Luna to build, compile, and load — or use <code>/plugin</code> in
              chat.
            </p>
          </div>

          {plugins.length === 0 ? (
            <div className="empty-state">
              <Layers size={24} className="empty-state-icon" />
              <div className="empty-state-title">No plugins installed</div>
              <p className="empty-state-subtitle">
                Tell Luna what tools, commands, panels, or sidebar agent to build in natural
                language.
              </p>
              <button type="button" className="btn btn-primary" onClick={onStartPluginConversation}>
                <Sparkles size={14} />
                <span>Create plugin with Luna</span>
              </button>
            </div>
          ) : (
            <div className="surface-list">
              {plugins.map((p) => {
                const sourceKind = p.builtin
                  ? 'builtin'
                  : p.path?.includes('.luna/plugins') && !p.path?.includes('/.luna/plugins/' + p.id)
                    ? 'workspace'
                    : p.path?.includes('/.luna/plugins/')
                      ? 'global'
                      : 'plugin';
                const toolsCount = p.tools?.length ?? 0;
                const agentsCount = p.agent ? 1 : 0;
                const schedulesCount = p.schedules?.length ?? 0;
                const commandsCount = p.commands?.length ?? 0;
                return (
                  <div key={p.id} className="surface-row">
                    <div className="surface-row-main">
                      <div className="surface-row-title-line">
                        <span className="surface-row-title">{p.name}</span>
                        <span className="pill">{p.version}</span>
                        <span className="pill">{sourceKind}</span>
                        {p.enabled && !p.loaded && (
                          <span className="pill" style={{ color: 'var(--danger, #f87171)' }}>
                            load error
                          </span>
                        )}
                      </div>
                      <div className="surface-row-meta">
                        {p.description || 'No description'}
                        {' · '}
                        <span className="tabular-nums">
                          {toolsCount} {toolsCount === 1 ? 'tool' : 'tools'}
                          {commandsCount > 0
                            ? `, ${commandsCount} ${commandsCount === 1 ? 'command' : 'commands'}`
                            : ''}
                          {`, ${agentsCount} ${agentsCount === 1 ? 'agent' : 'agents'}`}
                          {`, ${schedulesCount} ${schedulesCount === 1 ? 'schedule' : 'schedules'}`}
                        </span>
                      </div>
                      {p.last_error && (
                        <div
                          className="surface-row-meta"
                          style={{
                            color: 'var(--danger, #f87171)',
                            display: 'flex',
                            alignItems: 'center',
                            gap: '5px',
                            marginTop: '4px',
                          }}
                        >
                          <AlertCircle size={12} />
                          <span>{p.last_error}</span>
                        </div>
                      )}
                    </div>

                    <div className="surface-row-actions">
                      <span
                        className={`status-label ${
                          p.enabled && p.loaded ? 'text-success' : 'text-muted'
                        }`}
                      >
                        {p.enabled ? (p.loaded ? 'Active' : 'Error') : 'Off'}
                      </span>
                      <button
                        type="button"
                        className="icon-btn"
                        title={p.enabled ? 'Disable' : 'Enable'}
                        onClick={() => onTogglePlugin(p.id, !p.enabled)}
                      >
                        {p.enabled ? (
                          <ToggleRight size={22} className="text-accent" />
                        ) : (
                          <ToggleLeft size={22} />
                        )}
                      </button>
                      {!p.builtin && (
                        <button
                          type="button"
                          className="icon-btn"
                          title="Delete plugin"
                          aria-label={`Delete ${p.name}`}
                          onClick={() => onDeletePlugin(p.id)}
                        >
                          <Trash2 size={15} />
                        </button>
                      )}
                    </div>
                  </div>
                );
              })}
            </div>
          )}
        </div>
      </div>
    </div>
  );
};
