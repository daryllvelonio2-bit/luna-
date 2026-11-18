import React, { useState, useEffect, useMemo, useRef } from 'react';
import type {
  AccountInfo,
  ModelScope,
  ModelTarget,
  ProviderInfo,
  RotationStrategy,
} from '../types';
import {
  Search,
  Check,
  RefreshCw,
  Plus,
  X,
  UserCheck,
  Trash2,
  RotateCcw,
} from 'lucide-react';

interface ModelPickerModalProps {
  isOpen: boolean;
  onClose: () => void;
  providers: ProviderInfo[];
  activeProvider: string;
  activeModel: string;
  sessionId: string | null;
  initialTarget?: ModelTarget;
  initialScope?: ModelScope;
  onSelectModel: (
    provider: string,
    model: string,
    scope: ModelScope,
    target: ModelTarget
  ) => Promise<void>;
  onRefreshProvider: (providerId: string) => Promise<void>;
  onSaveCredential: (provider: string, key: string, label?: string) => Promise<void>;
  onRemoveCredential: (provider: string, credId: string) => Promise<void>;
  onSetPriorityAccount: (
    provider: string,
    credId: string,
    scope: ModelScope
  ) => Promise<void>;
  onSetRotationStrategy: (provider: string, strategy: RotationStrategy) => Promise<void>;
  onResetCooldowns: (provider: string) => Promise<void>;
  onAddCustomProvider: (payload: {
    id: string;
    base_url: string;
    api_key?: string;
    label?: string;
  }) => Promise<void>;
}

const TARGET_LABEL: Record<ModelTarget, string> = {
  session: 'Session model',
  default: 'Default model',
  compact: 'Compaction model',
  bgsaver: 'Background saver',
  approval: 'Approval assistant',
};

const STRATEGIES: Array<{ value: RotationStrategy; label: string }> = [
  { value: 'round_robin', label: 'Round robin' },
  { value: 'fill_first', label: 'Fill first' },
  { value: 'least_used', label: 'Least used' },
  { value: 'random', label: 'Random' },
  { value: 'never', label: 'Priority only' },
];

export const ModelPickerModal: React.FC<ModelPickerModalProps> = ({
  isOpen,
  onClose,
  providers,
  activeProvider,
  activeModel,
  sessionId,
  initialTarget = 'session',
  initialScope = 'session',
  onSelectModel,
  onRefreshProvider,
  onSaveCredential,
  onRemoveCredential,
  onSetPriorityAccount,
  onSetRotationStrategy,
  onResetCooldowns,
  onAddCustomProvider,
}) => {
  const [query, setQuery] = useState('');
  const [selectedProvider, setSelectedProvider] = useState(activeProvider);
  const [scope, setScope] = useState<ModelScope>(initialScope);
  const [target, setTarget] = useState<ModelTarget>(initialTarget);
  const [refreshing, setRefreshing] = useState(false);
  const [saving, setSaving] = useState(false);

  const [showAddAccount, setShowAddAccount] = useState(false);
  const [accLabel, setAccLabel] = useState('');
  const [accKey, setAccKey] = useState('');

  const [showCustomProvider, setShowCustomProvider] = useState(false);
  const [customProvId, setCustomProvId] = useState('');
  const [customProvUrl, setCustomProvUrl] = useState('');
  const [customProvKey, setCustomProvKey] = useState('');

  const [customModelInput, setCustomModelInput] = useState('');
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!isOpen) return;
    setQuery('');
    setSelectedProvider(activeProvider || providers[0]?.id || '');
    setTarget(initialTarget);
    setScope(
      initialTarget === 'default' || initialTarget === 'bgsaver' || initialTarget === 'approval'
        ? 'global'
        : sessionId
        ? initialScope
        : 'global'
    );
    setShowAddAccount(false);
    setShowCustomProvider(false);
    setAccLabel('');
    setAccKey('');
    setCustomModelInput('');
    setErrorMsg(null);
    setTimeout(() => searchRef.current?.focus(), 20);
  }, [isOpen, activeProvider, providers, initialTarget, initialScope, sessionId]);

  useEffect(() => {
    if (!isOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [isOpen, onClose]);

  const sortedProviders = useMemo(() => {
    return [...providers].sort((a, b) => {
      if (a.id === activeProvider && b.id !== activeProvider) return -1;
      if (b.id === activeProvider && a.id !== activeProvider) return 1;
      if (a.configured !== b.configured) return a.configured ? -1 : 1;
      return a.display_name.localeCompare(b.display_name);
    });
  }, [providers, activeProvider]);

  const currentProviderObj = useMemo(
    () => providers.find((p) => p.id === selectedProvider) || sortedProviders[0],
    [providers, selectedProvider, sortedProviders]
  );

  const normalizedQuery = query.trim().toLowerCase();

  const filteredModels = useMemo(() => {
    if (!currentProviderObj) return [];
    const list = currentProviderObj.models || [];
    if (!normalizedQuery) return list;
    return list.filter((m) => m.toLowerCase().includes(normalizedQuery));
  }, [currentProviderObj, normalizedQuery]);

  const filteredProviders = useMemo(() => {
    if (!normalizedQuery) return sortedProviders;
    return sortedProviders.filter(
      (p) =>
        p.display_name.toLowerCase().includes(normalizedQuery) ||
        p.id.toLowerCase().includes(normalizedQuery) ||
        p.models.some((m) => m.toLowerCase().includes(normalizedQuery)) ||
        p.accounts.some((a) => a.label.toLowerCase().includes(normalizedQuery))
    );
  }, [sortedProviders, normalizedQuery]);

  if (!isOpen) return null;

  const isGlobalOnlyTarget =
    target === 'default' || target === 'bgsaver' || target === 'approval';
  const effectiveScope: ModelScope = isGlobalOnlyTarget || !sessionId ? 'global' : scope;

  const activeAccountForProvider = (accounts: AccountInfo[]): AccountInfo | undefined => {
    if (effectiveScope === 'session') {
      return accounts.find((a) => a.is_session_priority) || accounts.find((a) => a.is_priority);
    }
    return accounts.find((a) => a.is_priority);
  };

  const handleChooseModel = async (providerId: string, modelId: string) => {
    if (!modelId.trim()) return;
    setSaving(true);
    setErrorMsg(null);
    try {
      await onSelectModel(providerId, modelId.trim(), effectiveScope, target);
      onClose();
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setSaving(false);
    }
  };

  const handleRefresh = async () => {
    if (!currentProviderObj || refreshing) return;
    setRefreshing(true);
    setErrorMsg(null);
    try {
      await onRefreshProvider(currentProviderObj.id);
    } catch (e) {
      setErrorMsg(`Refresh failed: ${String(e)}`);
    } finally {
      setRefreshing(false);
    }
  };

  const handleSaveAccount = async () => {
    if (!currentProviderObj || !accKey.trim()) return;
    setSaving(true);
    setErrorMsg(null);
    try {
      await onSaveCredential(
        currentProviderObj.id,
        accKey.trim(),
        accLabel.trim() || undefined
      );
      setAccLabel('');
      setAccKey('');
      setShowAddAccount(false);
    } catch (e) {
      setErrorMsg(`Failed to add account: ${String(e)}`);
    } finally {
      setSaving(false);
    }
  };

  const handleCreateCustomProvider = async () => {
    if (!customProvId.trim() || !customProvUrl.trim()) return;
    setSaving(true);
    setErrorMsg(null);
    try {
      await onAddCustomProvider({
        id: customProvId.trim(),
        base_url: customProvUrl.trim(),
        api_key: customProvKey.trim() || undefined,
      });
      setSelectedProvider(customProvId.trim().toLowerCase());
      setCustomProvId('');
      setCustomProvUrl('');
      setCustomProvKey('');
      setShowCustomProvider(false);
    } catch (e) {
      setErrorMsg(`Failed to add provider: ${String(e)}`);
    } finally {
      setSaving(false);
    }
  };

  const accounts = currentProviderObj?.accounts || [];
  const prioritizedAcc = activeAccountForProvider(accounts);
  const hasCooldowns = accounts.some((a) => a.is_temporarily_disabled);

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="picker-modal picker-modal-tall"
        role="dialog"
        aria-modal="true"
        aria-label="Choose provider, account, and model"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="picker-search-bar">
          <Search size={15} className="picker-search-icon" />
          <input
            ref={searchRef}
            type="text"
            className="picker-search-input"
            placeholder="Search providers, accounts, or models…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          <span className="picker-target-tag">{TARGET_LABEL[target]}</span>
          {!isGlobalOnlyTarget && sessionId && (
            <div className="segmented-toggle" role="group" aria-label="Apply scope">
              <button
                type="button"
                className={`segmented-btn ${effectiveScope === 'session' ? 'active' : ''}`}
                onClick={() => setScope('session')}
                title="Apply to this conversation only"
              >
                Session
              </button>
              <button
                type="button"
                className={`segmented-btn ${effectiveScope === 'global' ? 'active' : ''}`}
                onClick={() => setScope('global')}
                title="Save as global default"
              >
                Global
              </button>
            </div>
          )}
          <button type="button" className="icon-btn" onClick={onClose} aria-label="Close">
            <X size={15} />
          </button>
        </div>

        {errorMsg && <div className="picker-error">{errorMsg}</div>}

        <div className="picker-panes">
          <div className="picker-providers">
            <div className="picker-pane-head">
              <span className="picker-pane-label">Providers</span>
              <button
                type="button"
                className="picker-mini-btn"
                onClick={() => setShowCustomProvider((v) => !v)}
                title="Add custom OpenAI-compatible provider"
              >
                <Plus size={12} />
                <span>Custom</span>
              </button>
            </div>
            <div className="picker-list">
              {filteredProviders.map((p) => {
                const isSelected = p.id === currentProviderObj?.id;
                return (
                  <button
                    key={p.id}
                    type="button"
                    className={`picker-provider-row ${isSelected ? 'active' : ''}`}
                    onClick={() => {
                      setSelectedProvider(p.id);
                      setShowAddAccount(false);
                      setShowCustomProvider(false);
                    }}
                  >
                    <span className="picker-provider-left">
                      <span
                        className={`picker-status-dot ${p.configured ? 'configured' : ''}`}
                        title={p.configured ? 'Configured' : 'Not configured'}
                      />
                      <span className="picker-provider-name">{p.display_name}</span>
                    </span>
                    <span className="picker-count">
                      {p.accounts.length > 1 ? `${p.accounts.length}a · ` : ''}
                      {p.models.length}
                    </span>
                  </button>
                );
              })}
            </div>
          </div>

          <div className="picker-models">
            {showCustomProvider ? (
              <div className="picker-custom-form">
                <div className="picker-pane-label">Add custom OpenAI-compatible provider</div>
                <input
                  type="text"
                  className="form-input"
                  placeholder="Provider id (e.g. vllm, lmstudio, coarena)"
                  value={customProvId}
                  onChange={(e) => setCustomProvId(e.target.value)}
                />
                <input
                  type="text"
                  className="form-input"
                  placeholder="Base URL (e.g. http://localhost:8000/v1)"
                  value={customProvUrl}
                  onChange={(e) => setCustomProvUrl(e.target.value)}
                />
                <input
                  type="password"
                  className="form-input"
                  placeholder="API key (optional for local)"
                  value={customProvKey}
                  onChange={(e) => setCustomProvKey(e.target.value)}
                />
                <div className="inline-row">
                  <button
                    type="button"
                    className="btn btn-secondary"
                    onClick={() => setShowCustomProvider(false)}
                  >
                    Cancel
                  </button>
                  <button
                    type="button"
                    className="btn btn-primary"
                    disabled={saving || !customProvId.trim() || !customProvUrl.trim()}
                    onClick={handleCreateCustomProvider}
                  >
                    Save provider
                  </button>
                </div>
              </div>
            ) : (
              <>
                <div className="picker-models-header">
                  <span className="picker-pane-label">
                    {currentProviderObj?.display_name || 'Models'}
                    {currentProviderObj?.base_url ? ` · ${currentProviderObj.base_url}` : ''}
                  </span>
                  <div className="picker-header-actions">
                    <button
                      type="button"
                      className="picker-action-chip"
                      onClick={() => setShowAddAccount((v) => !v)}
                    >
                      <Plus size={12} />
                      <span>Account</span>
                    </button>
                    <button
                      type="button"
                      className="picker-action-chip"
                      onClick={handleRefresh}
                      disabled={refreshing}
                      title="Fetch latest model list"
                    >
                      <RefreshCw size={12} className={refreshing ? 'spin' : ''} />
                      <span>{refreshing ? 'Refreshing' : 'Refresh'}</span>
                    </button>
                  </div>
                </div>

                {/* Account selector strip */}
                {currentProviderObj && (
                  <div className="picker-accounts-strip">
                    <div className="picker-accounts-top">
                      <span className="picker-accounts-label">
                        <UserCheck size={12} />
                        <span>
                          Account ({effectiveScope === 'session' ? 'this session' : 'global'})
                        </span>
                      </span>
                      <div className="picker-accounts-controls">
                        {accounts.length > 1 && (
                          <select
                            className="picker-inline-select"
                            value={currentProviderObj.rotation_strategy || 'round_robin'}
                            onChange={(e) =>
                              onSetRotationStrategy(
                                currentProviderObj.id,
                                e.target.value as RotationStrategy
                              )
                            }
                            title="Credential rotation strategy"
                          >
                            {STRATEGIES.map((s) => (
                              <option key={s.value} value={s.value}>
                                {s.label}
                              </option>
                            ))}
                          </select>
                        )}
                        {hasCooldowns && (
                          <button
                            type="button"
                            className="picker-mini-btn"
                            onClick={() => onResetCooldowns(currentProviderObj.id)}
                            title="Reset rate-limit cooldowns"
                          >
                            <RotateCcw size={11} />
                            <span>Reset cooldown</span>
                          </button>
                        )}
                      </div>
                    </div>

                    {accounts.length === 0 ? (
                      <div className="picker-accounts-empty">
                        No accounts connected for {currentProviderObj.display_name}. Click + Account above.
                      </div>
                    ) : (
                      <div className="picker-account-pills">
                        {accounts.map((acc) => {
                          const isSelected = prioritizedAcc?.id === acc.id;
                          return (
                            <div
                              key={acc.id}
                              className={`picker-account-pill ${isSelected ? 'active' : ''} ${
                                acc.is_temporarily_disabled ? 'cooldown' : ''
                              }`}
                            >
                              <button
                                type="button"
                                className="picker-account-btn"
                                onClick={() =>
                                  onSetPriorityAccount(
                                    currentProviderObj.id,
                                    acc.id,
                                    effectiveScope
                                  )
                                }
                                title={`Use "${acc.label}" as priority (${acc.usage_count} uses)`}
                              >
                                <span
                                  className={`picker-status-dot ${
                                    acc.is_temporarily_disabled ? '' : 'configured'
                                  }`}
                                />
                                <span className="picker-account-name">{acc.label}</span>
                                <span className="picker-account-meta">
                                  {acc.is_temporarily_disabled
                                    ? 'cooldown'
                                    : `${acc.usage_count}×`}
                                </span>
                                {isSelected && <Check size={12} className="text-accent" />}
                              </button>
                              <button
                                type="button"
                                className="picker-account-del"
                                title="Remove account"
                                onClick={() =>
                                  onRemoveCredential(currentProviderObj.id, acc.id)
                                }
                              >
                                <Trash2 size={11} />
                              </button>
                            </div>
                          );
                        })}
                      </div>
                    )}
                  </div>
                )}

                {showAddAccount && currentProviderObj && (
                  <div className="picker-key-bar">
                    <input
                      type="text"
                      className="form-input picker-label-input"
                      placeholder="Account label (e.g. work)"
                      value={accLabel}
                      onChange={(e) => setAccLabel(e.target.value)}
                    />
                    <input
                      type="password"
                      className="form-input flex-1"
                      placeholder={`API key / token for ${currentProviderObj.display_name}…`}
                      value={accKey}
                      onChange={(e) => setAccKey(e.target.value)}
                      onKeyDown={(e) => e.key === 'Enter' && handleSaveAccount()}
                    />
                    <button
                      type="button"
                      className="btn btn-primary"
                      onClick={handleSaveAccount}
                      disabled={saving || !accKey.trim()}
                    >
                      Add
                    </button>
                  </div>
                )}

                <div className="picker-list">
                  {query.trim() &&
                    currentProviderObj &&
                    !filteredModels.some(
                      (m) => m.toLowerCase() === query.trim().toLowerCase()
                    ) && (
                      <button
                        type="button"
                        className="picker-model-row picker-custom-model-row"
                        disabled={saving}
                        onClick={() => handleChooseModel(currentProviderObj.id, query.trim())}
                      >
                        <span className="picker-model-name">
                          Use custom model "{query.trim()}"
                        </span>
                      </button>
                    )}

                  {filteredModels.length === 0 ? (
                    <div className="picker-empty">
                      <p>
                        {currentProviderObj?.models.length === 0
                          ? 'No cached models yet. Click Refresh or enter a model ID below.'
                          : 'No cached models match your search.'}
                      </p>
                      {currentProviderObj && (
                        <div className="inline-row picker-manual-model">
                          <input
                            type="text"
                            className="form-input flex-1 mono-input"
                            placeholder="Enter custom model ID…"
                            value={customModelInput}
                            onChange={(e) => setCustomModelInput(e.target.value)}
                            onKeyDown={(e) =>
                              e.key === 'Enter' &&
                              handleChooseModel(currentProviderObj.id, customModelInput)
                            }
                          />
                          <button
                            type="button"
                            className="btn btn-primary"
                            disabled={saving || !customModelInput.trim()}
                            onClick={() =>
                              handleChooseModel(currentProviderObj.id, customModelInput)
                            }
                          >
                            Use
                          </button>
                        </div>
                      )}
                    </div>
                  ) : (
                    filteredModels.map((m) => {
                      const isCurrent =
                        currentProviderObj?.id === activeProvider && m === activeModel;
                      return (
                        <button
                          key={m}
                          type="button"
                          className={`picker-model-row ${isCurrent ? 'active' : ''}`}
                          disabled={saving}
                          onClick={() =>
                            currentProviderObj && handleChooseModel(currentProviderObj.id, m)
                          }
                        >
                          <span className="picker-model-name" title={m}>
                            {m}
                          </span>
                          {isCurrent && <Check size={14} className="text-accent" />}
                        </button>
                      );
                    })
                  )}
                </div>
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
};
