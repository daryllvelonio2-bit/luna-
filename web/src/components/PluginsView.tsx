import React from 'react';
import type { PluginSummary } from '../types';
import { ToggleLeft, ToggleRight, Hammer, Layers, PanelLeft } from 'lucide-react';

interface PluginsViewProps {
  plugins: PluginSummary[];
  sidebarOpen: boolean;
  onToggleSidebar: () => void;
  onTogglePlugin: (id: string, enabled: boolean) => void;
  onOpenBuilder: () => void;
}

export const PluginsView: React.FC<PluginsViewProps> = ({
  plugins,
  sidebarOpen,
  onToggleSidebar,
  onTogglePlugin,
  onOpenBuilder,
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
        <div className="chat-header-right">
          <button type="button" className="btn btn-primary" onClick={onOpenBuilder}>
            <Hammer size={15} />
            <span>New plugin</span>
          </button>
        </div>
      </header>

      <div className="page-scroll">
        <div className="page-container">
          <div className="page-intro">
            <h1 className="page-title">Extensions & plugins</h1>
            <p className="page-subtitle">
              Composable tools, specialized agents, and scheduled hooks.
            </p>
          </div>

          {plugins.length === 0 ? (
            <div className="empty-state">
              <Layers size={24} className="empty-state-icon" />
              <div className="empty-state-title">No plugins installed</div>
              <p className="empty-state-desc">
                Build a TypeScript plugin to register custom tools or sidebar agents.
              </p>
              <button type="button" className="btn btn-secondary" onClick={onOpenBuilder}>
                <Hammer size={14} />
                <span>Open plugin builder</span>
              </button>
            </div>
          ) : (
            <div className="surface-list">
              {plugins.map((p) => (
                <div key={p.id} className="surface-row">
                  <div className="surface-row-main">
                    <div className="surface-row-title-line">
                      <span className="surface-row-title">{p.name}</span>
                      <span className="pill">{p.version}</span>
                      <span className="pill">{p.source_kind}</span>
                    </div>
                    <div className="surface-row-meta">
                      {p.description || 'No description'}
                      {' · '}
                      <span className="tabular-nums">
                        {p.tools_count} tools, {p.agents_count} agents, {p.schedules_count} schedules
                      </span>
                    </div>
                  </div>

                  <div className="surface-row-actions">
                    <span className={`status-label ${p.enabled ? 'text-success' : 'text-muted'}`}>
                      {p.enabled ? 'Active' : 'Off'}
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
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>
    </div>
  );
};
