import React, { useEffect, useState } from 'react';
import type { PluginSummary } from '../types';
import { PluginWorkspace } from './PluginWorkspace';
import {
  ToggleLeft,
  ToggleRight,
  Sparkles,
  Layers,
  PanelLeft,
  RefreshCw,
  Trash2,
  AlertCircle,
  AppWindow,
  Bot,
} from 'lucide-react';

interface PluginsViewProps {
  plugins: PluginSummary[];
  theme: 'dark' | 'light';
  sidebarOpen: boolean;
  onToggleSidebar: () => void;
  onTogglePlugin: (id: string, enabled: boolean) => void;
  onReloadPlugin: (id: string) => void;
  onDeletePlugin: (id: string) => void;
  onPluginUpdated: (updated: PluginSummary) => void;
  onRefresh: () => void;
  onStartPluginConversation: () => void;
  onChatWithAgent: (agentId: string) => void;
  onEditWithLuna: (plugin: PluginSummary) => void;
  onNotify: (msg: string, kind?: 'info' | 'err') => void;
}

export const PluginsView: React.FC<PluginsViewProps> = ({
  plugins,
  theme,
  sidebarOpen,
  onToggleSidebar,
  onTogglePlugin,
  onReloadPlugin,
  onDeletePlugin,
  onPluginUpdated,
  onRefresh,
  onStartPluginConversation,
  onChatWithAgent,
  onEditWithLuna,
  onNotify,
}) => {
  const [selectedPluginId, setSelectedPluginId] = useState<string | null>(null);

  const selectedPlugin = selectedPluginId
    ? plugins.find((p) => p.id === selectedPluginId) || null
    : null;

  useEffect(() => {
    if (selectedPluginId && !selectedPlugin) {
      setSelectedPluginId(null);
    }
  }, [selectedPluginId, selectedPlugin]);

  if (selectedPlugin) {
    return (
      <div className="page-view">
        <PluginWorkspace
          plugin={selectedPlugin}
          theme={theme}
          onPluginUpdated={onPluginUpdated}
          onBackOrClose={() => setSelectedPluginId(null)}
          onChatWithAgent={onChatWithAgent}
          onEditWithLuna={onEditWithLuna}
          onNotify={onNotify}
        />
      </div>
    );
  }

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
                const sourceKind = p.scope || (p.builtin ? 'builtin' : 'global');
                const toolsCount = p.tools?.length ?? 0;
                const agentsCount = p.agent ? 1 : 0;
                const schedulesCount = p.schedules?.length ?? 0;
                const commandsCount = p.commands?.length ?? 0;
                const panelsCount = p.panels?.length ?? 0;
                const hasOpenableUi =
                  p.enabled &&
                  p.loaded &&
                  (Boolean(p.has_custom_ui) ||
                    panelsCount > 0 ||
                    toolsCount > 0 ||
                    commandsCount > 0 ||
                    (p.config_schema?.length ?? 0) > 0);
                return (
                  <div key={p.id} className="surface-row">
                    <div className="surface-row-main">
                      <div className="surface-row-title-line">
                        <span className="surface-row-title">{p.name}</span>
                        <span className="pill">{p.version}</span>
                        <span className="pill">{sourceKind}</span>
                        {p.has_custom_ui && <span className="pill">Custom UI</span>}
                        {panelsCount > 0 && (
                          <span className="pill">
                            {panelsCount} {panelsCount === 1 ? 'panel' : 'panels'}
                          </span>
                        )}
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
                      {hasOpenableUi && (
                        <button
                          type="button"
                          className="btn btn-secondary btn-sm"
                          onClick={() => setSelectedPluginId(p.id)}
                          title="Open plugin UI, panels, runner & settings"
                        >
                          <AppWindow size={13} />
                          <span>{p.has_custom_ui || panelsCount > 0 ? 'Open UI' : 'Open'}</span>
                        </button>
                      )}
                      {p.enabled && p.loaded && p.agent && (
                        <button
                          type="button"
                          className="icon-btn"
                          title={`Chat with ${p.agent.name}`}
                          onClick={() => onChatWithAgent(p.agent!.id)}
                        >
                          <Bot size={15} />
                        </button>
                      )}
                      <button
                        type="button"
                        className="icon-btn"
                        title="Reload plugin"
                        aria-label={`Reload ${p.name}`}
                        onClick={() => onReloadPlugin(p.id)}
                      >
                        <RefreshCw size={14} />
                      </button>
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
