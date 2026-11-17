import React from 'react';
import type { PluginSummary } from '../types';
import { ToggleLeft, ToggleRight, Hammer } from 'lucide-react';

interface PluginsViewProps {
  plugins: PluginSummary[];
  onTogglePlugin: (id: string, enabled: boolean) => void;
  onOpenBuilder: () => void;
}

export const PluginsView: React.FC<PluginsViewProps> = ({
  plugins,
  onTogglePlugin,
  onOpenBuilder,
}) => {
  return (
    <div style={{ flex: 1, padding: '24px', overflowY: 'auto' }}>
      <div
        style={{
          display: 'flex',
          justifyContent: 'space-between',
          alignItems: 'center',
          marginBottom: '20px',
        }}
      >
        <div>
          <h2 style={{ fontSize: '18px', fontWeight: 600 }}>TypeScript Plugins Engine</h2>
          <p style={{ color: 'var(--text-secondary)', fontSize: '13px', marginTop: '4px' }}>
            Cordis-style spatiotemporal composability: tools, agents, schedules, and commands.
          </p>
        </div>
        <button className="primary-btn" onClick={onOpenBuilder}>
          <Hammer size={16} />
          <span>Plugin Builder</span>
        </button>
      </div>

      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fill, minmax(320px, 1fr))', gap: '16px' }}>
        {plugins.map((p) => (
          <div
            key={p.id}
            style={{
              backgroundColor: 'var(--bg-secondary)',
              border: '1px solid var(--border-color)',
              borderRadius: '8px',
              padding: '16px',
              display: 'flex',
              flexDirection: 'column',
              justifyContent: 'space-between',
            }}
          >
            <div>
              <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
                <span style={{ fontWeight: 600, fontSize: '15px' }}>{p.name}</span>
                <span className="badge">{p.version}</span>
              </div>
              <div style={{ color: 'var(--text-secondary)', fontSize: '13px', margin: '8px 0' }}>
                {p.description || 'No description provided.'}
              </div>
              <div style={{ display: 'flex', gap: '8px', flexWrap: 'wrap', marginTop: '12px' }}>
                <span className="badge">Tools: {p.tools_count}</span>
                <span className="badge">Agents: {p.agents_count}</span>
                <span className="badge">Schedules: {p.schedules_count}</span>
                <span className="badge">{p.source_kind}</span>
              </div>
            </div>

            <div
              style={{
                display: 'flex',
                alignItems: 'center',
                justifyContent: 'space-between',
                marginTop: '16px',
                paddingTop: '12px',
                borderTop: '1px solid var(--border-color)',
              }}
            >
              <span style={{ fontSize: '12px', color: p.enabled ? 'var(--success-color)' : 'var(--text-muted)' }}>
                {p.enabled ? 'Active' : 'Disabled'}
              </span>
              <button
                className="action-btn"
                title={p.enabled ? 'Disable' : 'Enable'}
                onClick={() => onTogglePlugin(p.id, !p.enabled)}
              >
                {p.enabled ? (
                  <ToggleRight size={24} color="var(--accent-color)" />
                ) : (
                  <ToggleLeft size={24} />
                )}
              </button>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
};
