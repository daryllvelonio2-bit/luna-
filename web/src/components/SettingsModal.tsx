import React, { useState, useEffect, useMemo, useCallback } from 'react';
import { api } from '../api';
import type {
  AutoCompactSettings,
  DisplaySettings,
  FeatureFlags,
  McpServerSummary,
  ModelTarget,
  ProfileItem,
  ProviderInfo,
  RotationStrategy,
  SkillSummary,
  UsagePayload,
  WorkerProfile,
} from '../types';
import {
  X,
  Key,
  Cpu,
  Sliders,
  Users,
  BarChart3,
  Shield,
  Search,
  Check,
  Trash2,
  Plus,
  Download,
  RotateCcw,
  Sparkles,
  ChevronRight,
} from 'lucide-react';

export type SettingsTab =
  | 'general'
  | 'models'
  | 'accounts'
  | 'features'
  | 'workers'
  | 'usage';

interface SettingsModalProps {
  isOpen: boolean;
  initialTab?: SettingsTab;
  onClose: () => void;
  sessionId: string | null;
  providers: ProviderInfo[];
  defaultProvider: string;
  defaultModel: string;
  compactProvider?: string | null;
  compactModel?: string | null;
  backgroundProvider?: string | null;
  backgroundModel?: string | null;
  approvalProvider?: string | null;
  approvalModel?: string | null;
  workspaceRoot: string;
  launchWorkspaceRoot?: string;
  display: DisplaySettings;
  autoCompact: AutoCompactSettings;
  features: FeatureFlags;
  memoryProjectFilter: string | null;
  workers: Record<string, WorkerProfile>;
  skills: SkillSummary[];
  mcpServers: McpServerSummary[];
  onOpenModelPicker: (target: ModelTarget) => void;
  onOpenWorkspacePicker: () => void;
  onResetWorkspace: () => Promise<void>;
  onSaveCredential: (provider: string, key: string, label?: string) => Promise<void>;
  onRemoveCredential: (provider: string, credId: string) => Promise<void>;
  onSetPriorityAccount: (
    provider: string,
    credId: string,
    scope: 'session' | 'global'
  ) => Promise<void>;
  onSetRotationStrategy: (provider: string, strategy: RotationStrategy) => Promise<void>;
  onResetCooldowns: (provider?: string) => Promise<void>;
  onRefreshProviders: () => Promise<void>;
  onUpdateSettings: (patch: Record<string, unknown>) => Promise<void>;
  onRunRemember: () => Promise<void>;
  onSetMemoryScope: (project: string | null) => Promise<void>;
  onWorkersChange: (workers: Record<string, WorkerProfile>) => void;
}

const STRATEGIES: Array<{ value: RotationStrategy; label: string }> = [
  { value: 'round_robin', label: 'Round robin' },
  { value: 'fill_first', label: 'Fill first' },
  { value: 'least_used', label: 'Least used' },
  { value: 'random', label: 'Random' },
  { value: 'never', label: 'Priority only' },
];

export const SettingsModal: React.FC<SettingsModalProps> = ({
  isOpen,
  initialTab = 'general',
  onClose,
  sessionId,
  providers,
  defaultProvider,
  defaultModel,
  compactProvider,
  compactModel,
  backgroundProvider,
  backgroundModel,
  approvalProvider,
  approvalModel,
  workspaceRoot,
  launchWorkspaceRoot,
  display,
  autoCompact,
  features,
  memoryProjectFilter,
  workers,
  skills,
  mcpServers,
  onOpenModelPicker,
  onOpenWorkspacePicker,
  onResetWorkspace,
  onSaveCredential,
  onRemoveCredential,
  onSetPriorityAccount,
  onSetRotationStrategy,
  onResetCooldowns,
  onRefreshProviders,
  onUpdateSettings,
  onRunRemember,
  onSetMemoryScope,
  onWorkersChange,
}) => {
  const [tab, setTab] = useState<SettingsTab>(initialTab);
  const [selectedProvider, setSelectedProvider] = useState(defaultProvider);
  const [providerQuery, setProviderQuery] = useState('');
  const [accLabel, setAccLabel] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [isSaving, setIsSaving] = useState(false);
  const [statusMsg, setStatusMsg] = useState<{ kind: 'ok' | 'err'; text: string } | null>(null);

  const [customProvId, setCustomProvId] = useState('');
  const [customProvUrl, setCustomProvUrl] = useState('');
  const [customProvKey, setCustomProvKey] = useState('');
  const [showCustomProv, setShowCustomProv] = useState(false);

  const [compactThreshold, setCompactThreshold] = useState(autoCompact.threshold);
  const [compactPercent, setCompactPercent] = useState(autoCompact.percent);
  const [memScopeInput, setMemScopeInput] = useState(memoryProjectFilter || '');

  const [newWorkerName, setNewWorkerName] = useState('');
  const [newWorkerProvider, setNewWorkerProvider] = useState(defaultProvider);
  const [newWorkerModel, setNewWorkerModel] = useState(defaultModel);
  const [newWorkerEffort, setNewWorkerEffort] = useState('');

  const [profiles, setProfiles] = useState<ProfileItem[]>([]);
  const [newProfName, setNewProfName] = useState('');
  const [newProfSummary, setNewProfSummary] = useState('');

  const [usage, setUsage] = useState<UsagePayload | null>(null);

  useEffect(() => {
    if (!isOpen) return;
    setTab(initialTab);
    setSelectedProvider(defaultProvider || providers[0]?.id || '');
    setProviderQuery('');
    setAccLabel('');
    setApiKey('');
    setStatusMsg(null);
    setCompactThreshold(autoCompact.threshold);
    setCompactPercent(autoCompact.percent);
    setMemScopeInput(memoryProjectFilter || '');
    setNewWorkerProvider(defaultProvider);
    setNewWorkerModel(defaultModel);
  }, [
    isOpen,
    initialTab,
    defaultProvider,
    defaultModel,
    providers,
    autoCompact,
    memoryProjectFilter,
  ]);

  const loadProfiles = useCallback(() => {
    api
      .listProfiles()
      .then((res) => setProfiles(res.profiles || []))
      .catch(() => {});
  }, []);

  useEffect(() => {
    if (!isOpen) return;
    if (tab === 'workers') loadProfiles();
    if (tab === 'usage') {
      api
        .getUsage(sessionId)
        .then(setUsage)
        .catch(() => {});
    }
  }, [isOpen, tab, sessionId, loadProfiles]);

  useEffect(() => {
    if (!isOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [isOpen, onClose]);

  const filteredProviders = useMemo(() => {
    const q = providerQuery.trim().toLowerCase();
    return [...providers]
      .sort((a, b) => {
        if (a.configured !== b.configured) return a.configured ? -1 : 1;
        return a.display_name.localeCompare(b.display_name);
      })
      .filter(
        (p) =>
          !q ||
          p.display_name.toLowerCase().includes(q) ||
          p.id.toLowerCase().includes(q)
      );
  }, [providers, providerQuery]);

  if (!isOpen) return null;

  const activeProviderObj = providers.find((p) => p.id === selectedProvider);

  const handleSaveKey = async () => {
    if (!apiKey.trim() || !selectedProvider) return;
    setIsSaving(true);
    setStatusMsg(null);
    try {
      await onSaveCredential(selectedProvider, apiKey.trim(), accLabel.trim() || undefined);
      setApiKey('');
      setAccLabel('');
      setStatusMsg({
        kind: 'ok',
        text: `Added account for ${activeProviderObj?.display_name || selectedProvider}.`,
      });
    } catch (e) {
      setStatusMsg({ kind: 'err', text: String(e) });
    } finally {
      setIsSaving(false);
    }
  };

  const handleImport = async (kind: 'antigravity' | 'opencode' | 'codex') => {
    setIsSaving(true);
    setStatusMsg(null);
    try {
      const res = await api.importCredentials(kind);
      await onRefreshProviders();
      setStatusMsg({
        kind: 'ok',
        text:
          res.imported.length > 0
            ? `Imported: ${res.imported.join(', ')}`
            : `No local ${kind} credentials found.`,
      });
    } catch (e) {
      setStatusMsg({ kind: 'err', text: String(e) });
    } finally {
      setIsSaving(false);
    }
  };

  const handleAddCustomProv = async () => {
    if (!customProvId.trim() || !customProvUrl.trim()) return;
    setIsSaving(true);
    setStatusMsg(null);
    try {
      const res = await api.addCustomProvider({
        id: customProvId.trim(),
        base_url: customProvUrl.trim(),
        api_key: customProvKey.trim() || undefined,
      });
      await onRefreshProviders();
      setSelectedProvider(res.id);
      setCustomProvId('');
      setCustomProvUrl('');
      setCustomProvKey('');
      setShowCustomProv(false);
      setStatusMsg({ kind: 'ok', text: `Added custom provider "${res.id}".` });
    } catch (e) {
      setStatusMsg({ kind: 'err', text: String(e) });
    } finally {
      setIsSaving(false);
    }
  };

  const handleCreateWorker = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newWorkerName.trim() || !newWorkerProvider.trim() || !newWorkerModel.trim()) return;
    setIsSaving(true);
    setStatusMsg(null);
    try {
      const res = await api.upsertWorker({
        name: newWorkerName.trim(),
        provider: newWorkerProvider.trim(),
        model: newWorkerModel.trim(),
        reasoning_effort: newWorkerEffort || null,
      });
      onWorkersChange(res.workers);
      setNewWorkerName('');
      setStatusMsg({ kind: 'ok', text: `Saved worker /${newWorkerName.trim().toLowerCase()}.` });
    } catch (err) {
      setStatusMsg({ kind: 'err', text: String(err) });
    } finally {
      setIsSaving(false);
    }
  };

  const handleCreateProfile = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!newProfName.trim()) return;
    setIsSaving(true);
    setStatusMsg(null);
    try {
      await api.createProfile({
        name: newProfName.trim(),
        summary: newProfSummary.trim() || undefined,
      });
      setNewProfName('');
      setNewProfSummary('');
      loadProfiles();
      setStatusMsg({ kind: 'ok', text: `Created profile "${newProfName.trim()}".` });
    } catch (err) {
      setStatusMsg({ kind: 'err', text: String(err) });
    } finally {
      setIsSaving(false);
    }
  };

  const NAV_TABS: Array<{ id: SettingsTab; label: string; icon: React.ReactNode }> = [
    { id: 'general', label: 'General', icon: <Sliders size={14} /> },
    { id: 'models', label: 'Models & Compact', icon: <Cpu size={14} /> },
    { id: 'accounts', label: 'Accounts & Keys', icon: <Key size={14} /> },
    { id: 'features', label: 'Features & Memory', icon: <Shield size={14} /> },
    { id: 'workers', label: 'Workers & Profiles', icon: <Users size={14} /> },
    { id: 'usage', label: 'Usage & Cost', icon: <BarChart3 size={14} /> },
  ];

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="settings-shell"
        role="dialog"
        aria-modal="true"
        aria-label="Settings"
        onClick={(e) => e.stopPropagation()}
      >
        <aside className="settings-rail">
          <div className="settings-rail-title">Settings</div>
          <nav className="settings-rail-nav">
            {NAV_TABS.map((t) => (
              <button
                key={t.id}
                type="button"
                className={`settings-rail-item ${tab === t.id ? 'active' : ''}`}
                onClick={() => {
                  setTab(t.id);
                  setStatusMsg(null);
                }}
              >
                {t.icon}
                <span>{t.label}</span>
              </button>
            ))}
          </nav>
        </aside>

        <div className="settings-content">
          <div className="settings-content-head">
            <span className="modal-title">
              {NAV_TABS.find((t) => t.id === tab)?.label}
            </span>
            <button type="button" className="icon-btn" onClick={onClose} aria-label="Close">
              <X size={16} />
            </button>
          </div>

          <div className="settings-content-body">
            {statusMsg && (
              <div
                className={`inline-banner ${
                  statusMsg.kind === 'err' ? 'inline-banner-err' : 'inline-banner-ok'
                }`}
              >
                {statusMsg.text}
              </div>
            )}

            {tab === 'general' && (
              <div className="settings-group">
                <div className="setting-row">
                  <div className="setting-row-info">
                    <div className="setting-row-label">Theme</div>
                    <div className="setting-row-desc">Persisted across CLI, TUI, and Web</div>
                  </div>
                  <div className="segmented-toggle">
                    <button
                      type="button"
                      className={`segmented-btn ${display.theme === 'dark' ? 'active' : ''}`}
                      onClick={() => onUpdateSettings({ theme: 'dark' })}
                    >
                      Dark
                    </button>
                    <button
                      type="button"
                      className={`segmented-btn ${display.theme === 'light' ? 'active' : ''}`}
                      onClick={() => onUpdateSettings({ theme: 'light' })}
                    >
                      Light
                    </button>
                  </div>
                </div>

                <div className="setting-row">
                  <div className="setting-row-info">
                    <div className="setting-row-label">Workspace directory</div>
                    <div className="setting-row-desc mono-text">{workspaceRoot || '.'}</div>
                  </div>
                  <div className="inline-row">
                    {launchWorkspaceRoot && workspaceRoot !== launchWorkspaceRoot && (
                      <button
                        type="button"
                        className="btn btn-secondary btn-xs"
                        onClick={onResetWorkspace}
                      >
                        Reset
                      </button>
                    )}
                    <button
                      type="button"
                      className="btn btn-secondary btn-xs"
                      onClick={() => {
                        onClose();
                        onOpenWorkspacePicker();
                      }}
                    >
                      Change
                    </button>
                  </div>
                </div>

                <div className="setting-row">
                  <div className="setting-row-info">
                    <div className="setting-row-label">Auto-collapse tool trace</div>
                    <div className="setting-row-desc">
                      Collapse activity summary automatically when a turn completes
                    </div>
                  </div>
                  <input
                    type="checkbox"
                    checked={display.auto_collapse}
                    onChange={(e) => onUpdateSettings({ auto_collapse: e.target.checked })}
                  />
                </div>

                <div className="setting-row">
                  <div className="setting-row-info">
                    <div className="setting-row-label">Auto-hide thinking during tools</div>
                    <div className="setting-row-desc">
                      Keep live execution view focused on active tool output
                    </div>
                  </div>
                  <input
                    type="checkbox"
                    checked={display.auto_hide_thinking}
                    onChange={(e) => onUpdateSettings({ auto_hide_thinking: e.target.checked })}
                  />
                </div>
              </div>
            )}

            {tab === 'models' && (
              <div className="settings-group">
                <button
                  type="button"
                  className="setting-row setting-row-clickable"
                  onClick={() => {
                    onClose();
                    onOpenModelPicker('default');
                  }}
                >
                  <div className="setting-row-info">
                    <div className="setting-row-label">Default provider & model</div>
                    <div className="setting-row-desc mono-text">
                      {defaultProvider} / {defaultModel || 'none'}
                    </div>
                  </div>
                  <ChevronRight size={15} className="text-muted" />
                </button>

                <button
                  type="button"
                  className="setting-row setting-row-clickable"
                  onClick={() => {
                    onClose();
                    onOpenModelPicker('compact');
                  }}
                >
                  <div className="setting-row-info">
                    <div className="setting-row-label">Compaction model</div>
                    <div className="setting-row-desc mono-text">
                      {compactProvider && compactModel
                        ? `${compactProvider} / ${compactModel}`
                        : 'Same as session model'}
                    </div>
                  </div>
                  <ChevronRight size={15} className="text-muted" />
                </button>

                <button
                  type="button"
                  className="setting-row setting-row-clickable"
                  onClick={() => {
                    onClose();
                    onOpenModelPicker('bgsaver');
                  }}
                >
                  <div className="setting-row-info">
                    <div className="setting-row-label">Background memory saver model</div>
                    <div className="setting-row-desc mono-text">
                      {backgroundProvider && backgroundModel
                        ? `${backgroundProvider} / ${backgroundModel}`
                        : 'Same as session model'}
                    </div>
                  </div>
                  <ChevronRight size={15} className="text-muted" />
                </button>

                <button
                  type="button"
                  className="setting-row setting-row-clickable"
                  onClick={() => {
                    onClose();
                    onOpenModelPicker('approval');
                  }}
                >
                  <div className="setting-row-info">
                    <div className="setting-row-label">Approval assistant model</div>
                    <div className="setting-row-desc mono-text">
                      {approvalProvider && approvalModel
                        ? `${approvalProvider} / ${approvalModel}`
                        : 'Same as default model'}
                    </div>
                  </div>
                  <ChevronRight size={15} className="text-muted" />
                </button>

                <div className="setting-row">
                  <div className="setting-row-info">
                    <div className="setting-row-label">Auto-compact context</div>
                    <div className="setting-row-desc">
                      Automatically summarize older turns when crossing threshold
                    </div>
                  </div>
                  <input
                    type="checkbox"
                    checked={autoCompact.enabled}
                    onChange={(e) =>
                      onUpdateSettings({
                        auto_compact_enabled: e.target.checked,
                        session_id: sessionId,
                      })
                    }
                  />
                </div>

                <div className="setting-row">
                  <div className="setting-row-info">
                    <div className="setting-row-label">Compaction threshold & ratio</div>
                    <div className="setting-row-desc">
                      Threshold (100k–700k tokens) and compact target (1%–90%)
                    </div>
                  </div>
                  <div className="inline-row">
                    <input
                      type="number"
                      className="form-input tabular-nums"
                      style={{ width: 110 }}
                      min={100000}
                      max={700000}
                      step={10000}
                      value={compactThreshold}
                      onChange={(e) => setCompactThreshold(Number(e.target.value))}
                    />
                    <input
                      type="number"
                      className="form-input tabular-nums"
                      style={{ width: 72 }}
                      min={1}
                      max={90}
                      value={compactPercent}
                      onChange={(e) => setCompactPercent(Number(e.target.value))}
                    />
                    <button
                      type="button"
                      className="btn btn-secondary btn-xs"
                      onClick={() =>
                        onUpdateSettings({
                          auto_compact_threshold: compactThreshold,
                          auto_compact_percent: compactPercent,
                          session_id: sessionId,
                        }).then(() =>
                          setStatusMsg({ kind: 'ok', text: 'Updated auto-compact settings.' })
                        )
                      }
                    >
                      Apply
                    </button>
                  </div>
                </div>
              </div>
            )}

            {tab === 'accounts' && (
              <div className="settings-group">
                <div className="connect-imports-bar">
                  <span className="connect-imports-label">Import local logins:</span>
                  <button
                    type="button"
                    className="picker-action-chip"
                    disabled={isSaving}
                    onClick={() => handleImport('antigravity')}
                  >
                    <Download size={12} />
                    <span>Antigravity</span>
                  </button>
                  <button
                    type="button"
                    className="picker-action-chip"
                    disabled={isSaving}
                    onClick={() => handleImport('opencode')}
                  >
                    <Download size={12} />
                    <span>OpenCode</span>
                  </button>
                  <button
                    type="button"
                    className="picker-action-chip"
                    disabled={isSaving}
                    onClick={() => handleImport('codex')}
                  >
                    <Download size={12} />
                    <span>Codex</span>
                  </button>
                  <button
                    type="button"
                    className="picker-action-chip"
                    onClick={() => setShowCustomProv((v) => !v)}
                  >
                    <Plus size={12} />
                    <span>Custom provider</span>
                  </button>
                </div>

                {showCustomProv && (
                  <div className="inline-custom-prov">
                    <input
                      type="text"
                      className="form-input"
                      placeholder="id (e.g. vllm)"
                      value={customProvId}
                      onChange={(e) => setCustomProvId(e.target.value)}
                    />
                    <input
                      type="text"
                      className="form-input flex-1"
                      placeholder="Base URL (e.g. http://localhost:8000/v1)"
                      value={customProvUrl}
                      onChange={(e) => setCustomProvUrl(e.target.value)}
                    />
                    <input
                      type="password"
                      className="form-input"
                      placeholder="API key (optional)"
                      value={customProvKey}
                      onChange={(e) => setCustomProvKey(e.target.value)}
                    />
                    <button
                      type="button"
                      className="btn btn-primary btn-xs"
                      onClick={handleAddCustomProv}
                      disabled={isSaving || !customProvId.trim() || !customProvUrl.trim()}
                    >
                      Save
                    </button>
                  </div>
                )}

                <div className="cred-manager">
                  <div className="cred-provider-list">
                    <div className="cred-search-wrap">
                      <Search size={13} className="cred-search-icon" />
                      <input
                        type="text"
                        className="cred-search-input"
                        placeholder="Filter providers…"
                        value={providerQuery}
                        onChange={(e) => setProviderQuery(e.target.value)}
                      />
                    </div>
                    <div className="cred-scroll">
                      {filteredProviders.map((p) => (
                        <button
                          key={p.id}
                          type="button"
                          className={`picker-provider-row ${
                            p.id === selectedProvider ? 'active' : ''
                          }`}
                          onClick={() => setSelectedProvider(p.id)}
                        >
                          <span className="picker-provider-left">
                            <span
                              className={`picker-status-dot ${p.configured ? 'configured' : ''}`}
                            />
                            <span className="picker-provider-name">{p.display_name}</span>
                          </span>
                          {p.accounts.length > 0 && (
                            <span className="picker-count">{p.accounts.length}</span>
                          )}
                        </button>
                      ))}
                    </div>
                  </div>

                  <div className="cred-editor">
                    <div className="cred-editor-head">
                      <span className="cred-editor-title">
                        {activeProviderObj?.display_name || selectedProvider}
                      </span>
                      <div className="inline-row">
                        <select
                          className="picker-inline-select"
                          value={activeProviderObj?.rotation_strategy || 'round_robin'}
                          onChange={(e) =>
                            activeProviderObj &&
                            onSetRotationStrategy(
                              activeProviderObj.id,
                              e.target.value as RotationStrategy
                            )
                          }
                        >
                          {STRATEGIES.map((s) => (
                            <option key={s.value} value={s.value}>
                              {s.label}
                            </option>
                          ))}
                        </select>
                        <button
                          type="button"
                          className="picker-mini-btn"
                          onClick={() =>
                            onResetCooldowns(activeProviderObj?.id).then(() =>
                              setStatusMsg({ kind: 'ok', text: 'Cooldowns reset.' })
                            )
                          }
                          title="Reset cooldowns"
                        >
                          <RotateCcw size={11} />
                        </button>
                      </div>
                    </div>

                    {activeProviderObj && activeProviderObj.accounts.length > 0 && (
                      <div className="hairline-list">
                        {activeProviderObj.accounts.map((acc) => (
                          <div key={acc.id} className="hairline-row">
                            <div className="hairline-row-main">
                              <div className="hairline-row-title">
                                {acc.label}{' '}
                                {acc.is_priority && (
                                  <span className="text-accent">· global priority</span>
                                )}
                                {acc.is_session_priority && (
                                  <span className="text-success">· session priority</span>
                                )}
                              </div>
                              <div className="hairline-row-meta tabular-nums">
                                {acc.usage_count} uses
                                {acc.is_temporarily_disabled ? ' · cooldown' : ''}
                              </div>
                            </div>
                            <div className="inline-row">
                              {sessionId && (
                                <button
                                  type="button"
                                  className="picker-mini-btn"
                                  onClick={() =>
                                    onSetPriorityAccount(
                                      activeProviderObj.id,
                                      acc.id,
                                      'session'
                                    )
                                  }
                                  title="Use for this session"
                                >
                                  Session
                                </button>
                              )}
                              <button
                                type="button"
                                className="picker-mini-btn"
                                onClick={() =>
                                  onSetPriorityAccount(
                                    activeProviderObj.id,
                                    acc.id,
                                    'global'
                                  )
                                }
                                title="Set global priority"
                              >
                                <Check size={11} />
                                <span>Default</span>
                              </button>
                              <button
                                type="button"
                                className="icon-btn"
                                onClick={() =>
                                  onRemoveCredential(activeProviderObj.id, acc.id)
                                }
                                aria-label="Remove account"
                              >
                                <Trash2 size={13} />
                              </button>
                            </div>
                          </div>
                        ))}
                      </div>
                    )}

                    <div className="form-group">
                      <label className="form-label">Add account / API key</label>
                      <div className="inline-row">
                        <input
                          type="text"
                          className="form-input"
                          style={{ width: 120 }}
                          placeholder="Label"
                          value={accLabel}
                          onChange={(e) => setAccLabel(e.target.value)}
                        />
                        <input
                          type="password"
                          className="form-input flex-1"
                          placeholder="API key or token…"
                          value={apiKey}
                          onChange={(e) => setApiKey(e.target.value)}
                          onKeyDown={(e) => e.key === 'Enter' && handleSaveKey()}
                        />
                        <button
                          type="button"
                          className="btn btn-primary"
                          onClick={handleSaveKey}
                          disabled={isSaving || !apiKey.trim()}
                        >
                          Add
                        </button>
                      </div>
                    </div>
                  </div>
                </div>
              </div>
            )}

            {tab === 'features' && (
              <div className="settings-group">
                {(
                  [
                    ['memory_enabled', 'Saved memory', features.memory],
                    ['notes_enabled', 'Workspace notes', features.notes],
                    ['skills_enabled', 'Skills', features.skills],
                    ['mcp_enabled', 'MCP servers', features.mcp],
                  ] as const
                ).map(([key, label, val]) => (
                  <div key={key} className="setting-row">
                    <div className="setting-row-label">{label}</div>
                    <input
                      type="checkbox"
                      checked={val}
                      onChange={(e) =>
                        onUpdateSettings({
                          [key]: e.target.checked,
                          session_id: sessionId,
                        })
                      }
                    />
                  </div>
                ))}

                <div className="setting-row">
                  <div className="setting-row-info">
                    <div className="setting-row-label">Memory saver mode</div>
                    <div className="setting-row-desc">
                      Session writer (in-turn tools) vs Background saver (post-turn distillation)
                    </div>
                  </div>
                  <div className="inline-row">
                    <div className="segmented-toggle">
                      <button
                        type="button"
                        className={`segmented-btn ${
                          features.memory_mode === 'session' ? 'active' : ''
                        }`}
                        onClick={() =>
                          onUpdateSettings({
                            memory_mode: 'session',
                            session_id: sessionId,
                          })
                        }
                      >
                        Session
                      </button>
                      <button
                        type="button"
                        className={`segmented-btn ${
                          features.memory_mode === 'background' ? 'active' : ''
                        }`}
                        onClick={() =>
                          onUpdateSettings({
                            memory_mode: 'background',
                            session_id: sessionId,
                          })
                        }
                      >
                        Background
                      </button>
                    </div>
                    {sessionId && features.memory_mode === 'background' && (
                      <button
                        type="button"
                        className="btn btn-secondary btn-xs"
                        onClick={() =>
                          onRunRemember().then(() =>
                            setStatusMsg({ kind: 'ok', text: 'Memory pass complete.' })
                          )
                        }
                      >
                        <Sparkles size={12} />
                        <span>Remember now</span>
                      </button>
                    )}
                  </div>
                </div>

                {sessionId && (
                  <div className="setting-row">
                    <div className="setting-row-info">
                      <div className="setting-row-label">Project memory scope</div>
                      <div className="setting-row-desc">
                        Filter active session memories to one project (blank = all projects)
                      </div>
                    </div>
                    <div className="inline-row">
                      <input
                        type="text"
                        className="form-input"
                        style={{ width: 170 }}
                        placeholder="All projects"
                        value={memScopeInput}
                        onChange={(e) => setMemScopeInput(e.target.value)}
                      />
                      <button
                        type="button"
                        className="btn btn-secondary btn-xs"
                        onClick={() =>
                          onSetMemoryScope(memScopeInput.trim() || null).then(() =>
                            setStatusMsg({ kind: 'ok', text: 'Updated memory scope.' })
                          )
                        }
                      >
                        Set
                      </button>
                    </div>
                  </div>
                )}

                <div className="drawer-subhead">
                  Installed Skills ({skills.length}) & MCP Servers ({mcpServers.length})
                </div>
                {skills.length === 0 && mcpServers.length === 0 ? (
                  <div className="empty-inline-flat">No skills or MCP servers configured.</div>
                ) : (
                  <div className="hairline-list">
                    {mcpServers.map((m) => (
                      <div key={m.name} className="hairline-row">
                        <div className="hairline-row-main">
                          <div className="hairline-row-title">MCP · {m.name}</div>
                          <div className="hairline-row-meta mono-text">{m.command}</div>
                        </div>
                        <span className="text-muted">{m.enabled ? 'Enabled' : 'Off'}</span>
                      </div>
                    ))}
                    {skills.map((s) => (
                      <div key={s.name} className="hairline-row">
                        <div className="hairline-row-main">
                          <div className="hairline-row-title">/{s.name}</div>
                          <div className="hairline-row-meta">{s.description}</div>
                        </div>
                        <span className="text-muted">{s.source}</span>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            )}

            {tab === 'workers' && (
              <div className="settings-group">
                <div className="drawer-subhead">Named Background Workers (/worker)</div>
                <form className="inline-custom-prov" onSubmit={handleCreateWorker}>
                  <input
                    type="text"
                    className="form-input"
                    style={{ width: 100 }}
                    placeholder="name"
                    value={newWorkerName}
                    onChange={(e) => setNewWorkerName(e.target.value)}
                  />
                  <input
                    type="text"
                    className="form-input"
                    style={{ width: 110 }}
                    placeholder="provider"
                    value={newWorkerProvider}
                    onChange={(e) => setNewWorkerProvider(e.target.value)}
                  />
                  <input
                    type="text"
                    className="form-input flex-1 mono-input"
                    placeholder="model"
                    value={newWorkerModel}
                    onChange={(e) => setNewWorkerModel(e.target.value)}
                  />
                  <select
                    className="picker-inline-select"
                    value={newWorkerEffort}
                    onChange={(e) => setNewWorkerEffort(e.target.value)}
                  >
                    <option value="">inherit effort</option>
                    <option value="low">low</option>
                    <option value="medium">medium</option>
                    <option value="high">high</option>
                  </select>
                  <button
                    type="submit"
                    className="btn btn-primary btn-xs"
                    disabled={isSaving || !newWorkerName.trim()}
                  >
                    Save
                  </button>
                </form>

                {Object.keys(workers).length === 0 ? (
                  <div className="empty-inline-flat">
                    No workers configured. Add one above and invoke with /&lt;name&gt; &lt;task&gt;.
                  </div>
                ) : (
                  <div className="hairline-list">
                    {Object.entries(workers).map(([name, w]) => (
                      <div key={name} className="hairline-row">
                        <div className="hairline-row-main">
                          <div className="hairline-row-title mono-text">/{name}</div>
                          <div className="hairline-row-meta mono-text">
                            {w.provider} / {w.model}
                            {w.reasoning_effort ? ` · ${w.reasoning_effort}` : ''}
                          </div>
                        </div>
                        <button
                          type="button"
                          className="icon-btn"
                          onClick={() =>
                            api.deleteWorker(name).then((r) => onWorkersChange(r.workers))
                          }
                          aria-label="Delete worker"
                        >
                          <Trash2 size={13} />
                        </button>
                      </div>
                    ))}
                  </div>
                )}

                <div className="drawer-subhead">Profiles</div>
                <form className="inline-custom-prov" onSubmit={handleCreateProfile}>
                  <input
                    type="text"
                    className="form-input"
                    style={{ width: 130 }}
                    placeholder="profile name"
                    value={newProfName}
                    onChange={(e) => setNewProfName(e.target.value)}
                  />
                  <input
                    type="text"
                    className="form-input flex-1"
                    placeholder="Short summary (inherits default persona/behavior)"
                    value={newProfSummary}
                    onChange={(e) => setNewProfSummary(e.target.value)}
                  />
                  <button
                    type="submit"
                    className="btn btn-secondary btn-xs"
                    disabled={isSaving || !newProfName.trim()}
                  >
                    Create
                  </button>
                </form>

                <div className="hairline-list">
                  {profiles.map((prof) => (
                    <div key={prof.name} className="hairline-row">
                      <div className="hairline-row-main">
                        <div className="hairline-row-title">
                          {prof.name}{' '}
                          {prof.builtin && <span className="text-muted">· built-in</span>}
                        </div>
                        <div className="hairline-row-meta">{prof.summary}</div>
                      </div>
                      {!prof.builtin && (
                        <button
                          type="button"
                          className="icon-btn"
                          onClick={() => api.deleteProfile(prof.name).then(loadProfiles)}
                          aria-label="Delete profile"
                        >
                          <Trash2 size={13} />
                        </button>
                      )}
                    </div>
                  ))}
                </div>
              </div>
            )}

            {tab === 'usage' && (
              <div className="settings-group">
                {!usage ? (
                  <div className="empty-inline-flat">Loading usage ledger…</div>
                ) : (
                  <>
                    <div className="metrics-grid tabular-nums">
                      <div className="metric-item">
                        <span className="metric-label">Session Turns</span>
                        <span className="metric-val">{usage.session?.turns ?? 0}</span>
                      </div>
                      <div className="metric-item">
                        <span className="metric-label">Session Cost</span>
                        <span className="metric-val">
                          ${(usage.session?.cost_usd ?? 0).toFixed(4)}
                        </span>
                      </div>
                      <div className="metric-item">
                        <span className="metric-label">All-Time Turns</span>
                        <span className="metric-val">{usage.global.turns}</span>
                      </div>
                      <div className="metric-item">
                        <span className="metric-label">All-Time Cost</span>
                        <span className="metric-val">${usage.global.cost_usd.toFixed(4)}</span>
                      </div>
                    </div>

                    <div className="drawer-subhead">By Model</div>
                    {usage.by_model.length === 0 ? (
                      <div className="empty-inline-flat">No model usage recorded yet.</div>
                    ) : (
                      <div className="hairline-list">
                        {usage.by_model.map((row) => (
                          <div key={row.key} className="hairline-row tabular-nums">
                            <div className="hairline-row-main">
                              <div className="hairline-row-title mono-text">{row.key}</div>
                              <div className="hairline-row-meta">
                                {row.totals.turns} turns ·{' '}
                                {row.totals.total_tokens.toLocaleString()} tokens
                              </div>
                            </div>
                            <span className="mono-text">${row.totals.cost_usd.toFixed(4)}</span>
                          </div>
                        ))}
                      </div>
                    )}
                  </>
                )}
              </div>
            )}
          </div>
        </div>
      </div>
    </div>
  );
};
