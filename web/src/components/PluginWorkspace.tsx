import React, { useEffect, useMemo, useRef, useState } from 'react';
import { api } from '../api';
import type { PluginSummary, PluginWidget } from '../types';
import { MarkdownContent } from './MarkdownContent';
import {
  AppWindow,
  ArrowLeft,
  Bot,
  Check,
  LayoutGrid,
  Play,
  RefreshCw,
  Settings,
  Sparkles,
  Terminal,
  X,
} from 'lucide-react';

type WorkspaceTab = 'ui' | 'panels' | 'runner' | 'settings';

interface PluginWorkspaceProps {
  plugin: PluginSummary;
  theme: 'dark' | 'light';
  compact?: boolean;
  onPluginUpdated: (updated: PluginSummary) => void;
  onBackOrClose: () => void;
  onChatWithAgent?: (agentId: string) => void;
  onEditWithLuna?: (plugin: PluginSummary) => void;
  onSendToChat?: (message: string) => void;
  onNotify: (msg: string, kind?: 'info' | 'err') => void;
}

export const PluginWorkspace: React.FC<PluginWorkspaceProps> = ({
  plugin,
  theme,
  compact = false,
  onPluginUpdated,
  onBackOrClose,
  onChatWithAgent,
  onEditWithLuna,
  onSendToChat,
  onNotify,
}) => {
  const hasUi = Boolean(plugin.has_custom_ui);
  const hasPanels = plugin.panels.length > 0;
  const hasRunner = plugin.commands.length > 0 || plugin.tools.length > 0;
  const hasSettings = (plugin.config_schema?.length ?? 0) > 0;

  const defaultTab: WorkspaceTab = hasUi
    ? 'ui'
    : hasPanels
    ? 'panels'
    : hasRunner
    ? 'runner'
    : 'settings';

  const [tab, setTab] = useState<WorkspaceTab>(defaultTab);
  const [refreshNonce, setRefreshNonce] = useState(0);
  const [busyAction, setBusyAction] = useState<string | null>(null);
  const [lastOutput, setLastOutput] = useState<string | null>(null);
  const [inputValues, setInputValues] = useState<Record<string, string>>({});

  // Runner state
  const [runnerKind, setRunnerKind] = useState<'command' | 'tool'>(
    plugin.commands.length > 0 ? 'command' : 'tool'
  );
  const [runnerTarget, setRunnerTarget] = useState<string>(
    plugin.commands[0]?.name || plugin.tools[0] || ''
  );
  const [runnerArgsJson, setRunnerArgsJson] = useState<string>('{}');

  // Settings draft
  const [settingsDraft, setSettingsDraft] = useState<Record<string, unknown>>(
    plugin.settings || {}
  );
  const [settingsSaved, setSettingsSaved] = useState(false);

  const iframeRef = useRef<HTMLIFrameElement>(null);

  useEffect(() => {
    setSettingsDraft(plugin.settings || {});
  }, [plugin.id, plugin.settings]);

  useEffect(() => {
    setTab(
      plugin.has_custom_ui
        ? 'ui'
        : plugin.panels.length > 0
        ? 'panels'
        : plugin.commands.length > 0 || plugin.tools.length > 0
        ? 'runner'
        : 'settings'
    );
    setLastOutput(null);
    setRunnerKind(plugin.commands.length > 0 ? 'command' : 'tool');
    setRunnerTarget(plugin.commands[0]?.name || plugin.tools[0] || '');
    setRunnerArgsJson('{}');
  }, [plugin.id]);

  // Forward theme changes into live custom UI iframe
  useEffect(() => {
    const win = iframeRef.current?.contentWindow;
    if (win) {
      win.postMessage({ type: 'luna:theme', theme }, '*');
    }
  }, [theme]);

  // Handle postMessage events from custom plugin iframe (`window.lunaPlugin.*`)
  useEffect(() => {
    const onMessage = async (ev: MessageEvent) => {
      if (!ev.data || typeof ev.data !== 'object') return;
      if (ev.data.pluginId && ev.data.pluginId !== plugin.id) return;

      if (ev.data.type === 'luna:plugin:notify' && typeof ev.data.message === 'string') {
        onNotify(ev.data.message, ev.data.kind === 'err' ? 'err' : 'info');
      } else if (ev.data.type === 'luna:plugin:sendToChat' && typeof ev.data.message === 'string') {
        onSendToChat?.(ev.data.message);
      } else if (ev.data.type === 'luna:plugin:refresh') {
        try {
          const fresh = await api.getPlugin(plugin.id);
          onPluginUpdated(fresh);
        } catch {
          // ignore
        }
      }
    };
    window.addEventListener('message', onMessage);
    return () => window.removeEventListener('message', onMessage);
  }, [plugin.id, onNotify, onPluginUpdated, onSendToChat]);

  const selectedToolSchema = useMemo(() => {
    if (runnerKind !== 'tool') return null;
    return plugin.tool_schemas?.find((t) => t.name === runnerTarget) || null;
  }, [plugin.tool_schemas, runnerKind, runnerTarget]);

  const handleReload = async () => {
    setBusyAction('reload');
    try {
      const updated = await api.reloadPlugin(plugin.id);
      onPluginUpdated(updated);
      setRefreshNonce((n) => n + 1);
      onNotify(`Reloaded '${plugin.name}'.`);
    } catch (e) {
      onNotify(String(e), 'err');
    } finally {
      setBusyAction(null);
    }
  };

  const handleRefreshSummary = async () => {
    setBusyAction('refresh');
    try {
      const updated = await api.getPlugin(plugin.id);
      onPluginUpdated(updated);
      setRefreshNonce((n) => n + 1);
    } catch (e) {
      onNotify(String(e), 'err');
    } finally {
      setBusyAction(null);
    }
  };

  const handleRunPanelCommand = async (
    action: string,
    args?: Record<string, unknown>
  ) => {
    setBusyAction(action);
    try {
      const res = await api.runPluginCommand(plugin.id, action, args || {});
      if (res.summary) onPluginUpdated(res.summary);
      if (res.output) setLastOutput(res.output);
      setRefreshNonce((n) => n + 1);
    } catch (e) {
      onNotify(`Command '${action}' failed: ${String(e)}`, 'err');
    } finally {
      setBusyAction(null);
    }
  };

  const handleRunManual = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!runnerTarget) return;
    let parsedArgs: Record<string, unknown> = {};
    try {
      const raw = runnerArgsJson.trim();
      parsedArgs = raw ? (JSON.parse(raw) as Record<string, unknown>) : {};
    } catch (err) {
      onNotify(`Invalid JSON arguments: ${String(err)}`, 'err');
      return;
    }

    setBusyAction('manual');
    try {
      const res =
        runnerKind === 'command'
          ? await api.runPluginCommand(plugin.id, runnerTarget, parsedArgs)
          : await api.runPluginTool(plugin.id, runnerTarget, parsedArgs);
      if (res.summary) onPluginUpdated(res.summary);
      setLastOutput(res.output || '(no output)');
      setRefreshNonce((n) => n + 1);
    } catch (err) {
      onNotify(String(err), 'err');
    } finally {
      setBusyAction(null);
    }
  };

  const handleSaveSettings = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusyAction('settings');
    try {
      const updated = await api.updatePluginSettings(plugin.id, settingsDraft);
      onPluginUpdated(updated);
      setSettingsSaved(true);
      setTimeout(() => setSettingsSaved(false), 1800);
      onNotify(`Saved settings for '${plugin.name}'.`);
    } catch (err) {
      onNotify(String(err), 'err');
    } finally {
      setBusyAction(null);
    }
  };

  const renderWidget = (w: PluginWidget) => {
    switch (w.kind) {
      case 'stat_card':
        return (
          <div key={w.id} className="plugin-widget-card plugin-stat-card">
            <div className="plugin-stat-label">{w.label}</div>
            <div className="plugin-stat-value tabular-nums">{w.value}</div>
            {w.subtitle && <div className="plugin-stat-sub">{w.subtitle}</div>}
          </div>
        );

      case 'progress': {
        const max = w.max > 0 ? w.max : 100;
        const pct = Math.max(0, Math.min(100, (w.value / max) * 100));
        return (
          <div key={w.id} className="plugin-widget-card">
            <div className="plugin-progress-head">
              <span className="plugin-widget-label">{w.label}</span>
              <span className="plugin-stat-sub tabular-nums">
                {w.value} / {max}
              </span>
            </div>
            <div className="context-meter-bar">
              <div className="context-meter-fill" style={{ width: `${pct}%` }} />
            </div>
            {w.subtitle && <div className="plugin-stat-sub">{w.subtitle}</div>}
          </div>
        );
      }

      case 'key_value':
        return (
          <div key={w.id} className="plugin-widget-card">
            <div className="plugin-kv-list">
              {(w.items || []).map(([k, v], i) => (
                <div key={`${w.id}-${i}`} className="plugin-kv-row">
                  <span className="plugin-kv-key">{k}</span>
                  <span className="plugin-kv-val">{v}</span>
                </div>
              ))}
            </div>
          </div>
        );

      case 'table':
        return (
          <div key={w.id} className="plugin-widget-card plugin-table-wrap">
            {w.title && <div className="plugin-widget-title">{w.title}</div>}
            <table className="plugin-table">
              {w.columns.length > 0 && (
                <thead>
                  <tr>
                    {w.columns.map((col, idx) => (
                      <th key={idx}>{col}</th>
                    ))}
                  </tr>
                </thead>
              )}
              <tbody>
                {w.rows.length === 0 ? (
                  <tr>
                    <td colSpan={Math.max(1, w.columns.length)} className="text-muted">
                      No rows
                    </td>
                  </tr>
                ) : (
                  w.rows.map((row, rIdx) => (
                    <tr key={rIdx}>
                      {row.map((cell, cIdx) => (
                        <td key={cIdx}>{cell}</td>
                      ))}
                    </tr>
                  ))
                )}
              </tbody>
            </table>
          </div>
        );

      case 'markdown':
        return (
          <div key={w.id} className="plugin-widget-card">
            <MarkdownContent content={w.content} />
          </div>
        );

      case 'action_button':
        return (
          <button
            key={w.id}
            type="button"
            className={`btn ${w.variant === 'primary' ? 'btn-primary' : 'btn-ghost'}`}
            disabled={busyAction !== null}
            onClick={() =>
              handleRunPanelCommand(
                w.action,
                (w.args && typeof w.args === 'object' ? w.args : {}) as Record<string, unknown>
              )
            }
          >
            <Play size={13} />
            <span>{busyAction === w.action ? 'Running…' : w.label}</span>
          </button>
        );

      case 'input_action': {
        const val = inputValues[w.id] ?? '';
        const argKey = w.arg_name || 'input';
        return (
          <form
            key={w.id}
            className="plugin-widget-card plugin-input-action"
            onSubmit={(e) => {
              e.preventDefault();
              handleRunPanelCommand(w.action, { [argKey]: val });
            }}
          >
            {w.label && <label className="plugin-widget-label">{w.label}</label>}
            <div className="plugin-input-action-row">
              <input
                type="text"
                className="text-input"
                placeholder={w.placeholder || ''}
                value={val}
                onChange={(e) =>
                  setInputValues((prev) => ({ ...prev, [w.id]: e.target.value }))
                }
              />
              <button
                type="submit"
                className={`btn ${w.variant === 'primary' ? 'btn-primary' : 'btn-ghost'}`}
                disabled={busyAction !== null}
              >
                {w.button_label || 'Run'}
              </button>
            </div>
          </form>
        );
      }

      case 'html':
        return (
          <div key={w.id} className="plugin-widget-card">
            <iframe
              title={w.id}
              className="plugin-widget-html-frame"
              sandbox="allow-scripts"
              srcDoc={w.content}
            />
          </div>
        );
    }
  };

  return (
    <div className={`plugin-workspace ${compact ? 'plugin-workspace-compact' : ''}`}>
      <div className="plugin-workspace-header">
        <div className="plugin-workspace-title-group">
          <button
            type="button"
            className="icon-btn"
            onClick={onBackOrClose}
            title={compact ? 'Close plugin panel' : 'Back to plugins'}
            aria-label={compact ? 'Close plugin panel' : 'Back to plugins'}
          >
            {compact ? <X size={15} /> : <ArrowLeft size={16} />}
          </button>
          <div className="plugin-workspace-meta">
            <div className="plugin-workspace-name">{plugin.name}</div>
            <div className="plugin-workspace-sub">{plugin.description || plugin.id}</div>
          </div>
        </div>

        <div className="plugin-workspace-actions">
          {!compact && plugin.agent && onChatWithAgent && (
            <button
              type="button"
              className="btn btn-ghost"
              onClick={() => onChatWithAgent(plugin.agent!.id)}
              title="Open chat with this plugin's agent"
            >
              <Bot size={14} />
              <span>Chat</span>
            </button>
          )}
          {onEditWithLuna && !plugin.builtin && (
            <button
              type="button"
              className="btn btn-ghost"
              onClick={() => onEditWithLuna(plugin)}
              title="Ask Luna to modify or extend this plugin"
            >
              <Sparkles size={14} />
              <span>Edit with Luna</span>
            </button>
          )}
          <button
            type="button"
            className="icon-btn"
            onClick={handleReload}
            disabled={busyAction !== null}
            title="Recompile & reload plugin"
            aria-label="Reload plugin"
          >
            <RefreshCw size={14} />
          </button>
        </div>
      </div>

      <div className="plugin-workspace-tabs">
        {hasUi && (
          <button
            type="button"
            className={`plugin-tab-btn ${tab === 'ui' ? 'active' : ''}`}
            onClick={() => setTab('ui')}
          >
            <AppWindow size={13} />
            <span>App UI</span>
          </button>
        )}
        {hasPanels && (
          <button
            type="button"
            className={`plugin-tab-btn ${tab === 'panels' ? 'active' : ''}`}
            onClick={() => {
              setTab('panels');
              handleRefreshSummary();
            }}
          >
            <LayoutGrid size={13} />
            <span>Panels ({plugin.panels.length})</span>
          </button>
        )}
        {hasRunner && (
          <button
            type="button"
            className={`plugin-tab-btn ${tab === 'runner' ? 'active' : ''}`}
            onClick={() => setTab('runner')}
          >
            <Terminal size={13} />
            <span>Commands & Tools</span>
          </button>
        )}
        {hasSettings && (
          <button
            type="button"
            className={`plugin-tab-btn ${tab === 'settings' ? 'active' : ''}`}
            onClick={() => setTab('settings')}
          >
            <Settings size={13} />
            <span>Settings</span>
          </button>
        )}
      </div>

      <div className="plugin-workspace-body">
        {tab === 'ui' && hasUi && (
          <iframe
            ref={iframeRef}
            key={`${plugin.id}-${refreshNonce}`}
            src={`/api/plugins/${encodeURIComponent(plugin.id)}/ui/?theme=${theme}&v=${refreshNonce}`}
            className="plugin-custom-iframe"
            title={`${plugin.name} UI`}
            sandbox="allow-scripts allow-forms allow-same-origin allow-downloads"
          />
        )}

        {tab === 'panels' && (
          <div className="plugin-panels-scroll">
            {plugin.panels.length === 0 ? (
              <div className="empty-inline-flat">No declarative panels registered.</div>
            ) : (
              plugin.panels.map((panel) => (
                <section key={panel.id} className="plugin-panel-section">
                  <div className="plugin-panel-head">
                    <h4>{panel.title}</h4>
                    <button
                      type="button"
                      className="icon-btn"
                      onClick={handleRefreshSummary}
                      title="Refresh panel"
                    >
                      <RefreshCw size={13} />
                    </button>
                  </div>
                  <div className="plugin-panel-widgets">
                    {panel.widgets.map((w) => renderWidget(w))}
                  </div>
                </section>
              ))
            )}
            {lastOutput !== null && (
              <div className="plugin-output-box">
                <div className="plugin-output-head">
                  <span>Output</span>
                  <button
                    type="button"
                    className="icon-btn"
                    onClick={() => setLastOutput(null)}
                  >
                    <X size={12} />
                  </button>
                </div>
                <pre>{lastOutput}</pre>
              </div>
            )}
          </div>
        )}

        {tab === 'runner' && (
          <div className="plugin-panels-scroll">
            <form className="plugin-runner-form" onSubmit={handleRunManual}>
              <div className="plugin-runner-controls">
                <select
                  className="text-input"
                  value={runnerKind}
                  onChange={(e) => {
                    const next = e.target.value as 'command' | 'tool';
                    setRunnerKind(next);
                    setRunnerTarget(
                      next === 'command'
                        ? plugin.commands[0]?.name || ''
                        : plugin.tools[0] || ''
                    );
                  }}
                >
                  {plugin.commands.length > 0 && <option value="command">Command</option>}
                  {plugin.tools.length > 0 && <option value="tool">Tool</option>}
                </select>

                <select
                  className="text-input"
                  value={runnerTarget}
                  onChange={(e) => setRunnerTarget(e.target.value)}
                >
                  {runnerKind === 'command'
                    ? plugin.commands.map((c) => (
                        <option key={c.name} value={c.name}>
                          /{c.name} — {c.description || 'command'}
                        </option>
                      ))
                    : plugin.tools.map((t) => (
                        <option key={t} value={t}>
                          {t}
                        </option>
                      ))}
                </select>
              </div>

              {selectedToolSchema && (
                <div className="plugin-tool-hint">
                  <div>{selectedToolSchema.description}</div>
                  {selectedToolSchema.parameters?.properties &&
                    typeof selectedToolSchema.parameters.properties === 'object' ? (
                      <code>
                        params:{' '}
                        {Object.keys(
                          selectedToolSchema.parameters.properties as Record<string, unknown>
                        ).join(', ') || 'none'}
                      </code>
                    ) : null}
                </div>
              )}

              <label className="plugin-widget-label">JSON arguments</label>
              <textarea
                className="text-input plugin-json-input"
                rows={4}
                value={runnerArgsJson}
                onChange={(e) => setRunnerArgsJson(e.target.value)}
                placeholder="{}"
              />

              <div className="plugin-runner-submit">
                <button
                  type="submit"
                  className="btn btn-primary"
                  disabled={!runnerTarget || busyAction !== null}
                >
                  <Play size={13} />
                  <span>{busyAction === 'manual' ? 'Running…' : 'Execute'}</span>
                </button>
              </div>
            </form>

            {lastOutput !== null && (
              <div className="plugin-output-box">
                <div className="plugin-output-head">
                  <span>Result</span>
                </div>
                <pre>{lastOutput}</pre>
              </div>
            )}
          </div>
        )}

        {tab === 'settings' && (
          <div className="plugin-panels-scroll">
            {!hasSettings ? (
              <div className="empty-inline-flat">
                This plugin does not declare any configuration fields.
              </div>
            ) : (
              <form className="plugin-settings-form" onSubmit={handleSaveSettings}>
                {(plugin.config_schema || []).map((field) => {
                  const val = settingsDraft[field.key] ?? field.default_value ?? '';
                  const ftype = field.field_type.toLowerCase();
                  return (
                    <div key={field.key} className="plugin-setting-row">
                      <label className="plugin-widget-label" htmlFor={`cfg-${field.key}`}>
                        {field.label}
                      </label>
                      {field.description && (
                        <div className="plugin-stat-sub">{field.description}</div>
                      )}
                      {ftype === 'boolean' ? (
                        <label className="plugin-checkbox-row">
                          <input
                            id={`cfg-${field.key}`}
                            type="checkbox"
                            checked={Boolean(val)}
                            onChange={(e) =>
                              setSettingsDraft((prev) => ({
                                ...prev,
                                [field.key]: e.target.checked,
                              }))
                            }
                          />
                          <span>{Boolean(val) ? 'Enabled' : 'Disabled'}</span>
                        </label>
                      ) : (
                        <input
                          id={`cfg-${field.key}`}
                          type={
                            ftype === 'secret'
                              ? 'password'
                              : ftype === 'number'
                              ? 'number'
                              : 'text'
                          }
                          className="text-input"
                          value={String(val ?? '')}
                          onChange={(e) =>
                            setSettingsDraft((prev) => ({
                              ...prev,
                              [field.key]:
                                ftype === 'number' ? Number(e.target.value) : e.target.value,
                            }))
                          }
                        />
                      )}
                    </div>
                  );
                })}
                <div>
                  <button
                    type="submit"
                    className="btn btn-primary"
                    disabled={busyAction !== null}
                  >
                    {settingsSaved ? <Check size={13} /> : null}
                    <span>{settingsSaved ? 'Saved' : 'Save settings'}</span>
                  </button>
                </div>
              </form>
            )}
          </div>
        )}

        {!hasUi && !hasPanels && !hasRunner && !hasSettings && (
          <div className="empty-state">
            <p>This plugin has no UI panels, commands, or settings yet.</p>
          </div>
        )}
      </div>
    </div>
  );
};
