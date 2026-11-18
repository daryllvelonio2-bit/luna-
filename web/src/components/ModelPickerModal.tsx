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
  ChevronLeft,
  ChevronRight,
  Trash2,
  RotateCcw,
} from 'lucide-react';

interface ModelPickerModalProps {
  isOpen: boolean;
  onClose: () => void;
  anchorRect?: DOMRect | null;
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
  onRefreshProvider: (
    providerId: string,
    credentialId?: string,
    force?: boolean
  ) => Promise<string[]>;
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

type PickerStep = 'provider' | 'account' | 'model';

const TARGET_LABEL: Record<ModelTarget, string> = {
  session: 'Session',
  default: 'Default',
  compact: 'Compact',
  bgsaver: 'Memory saver',
  approval: 'Approval',
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
  anchorRect,
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
  const [step, setStep] = useState<PickerStep>('provider');
  const [query, setQuery] = useState('');
  const [selectedProvider, setSelectedProvider] = useState(activeProvider);
  const [selectedAccountId, setSelectedAccountId] = useState<string | null>(null);
  const [accountModels, setAccountModels] = useState<string[] | null>(null);
  const [loadingModels, setLoadingModels] = useState(false);

  const [scope, setScope] = useState<ModelScope>(initialScope);
  const [target, setTarget] = useState<ModelTarget>(initialTarget);
  const [saving, setSaving] = useState(false);

  const [showAddAccount, setShowAddAccount] = useState(false);
  const [accLabel, setAccLabel] = useState('');
  const [accKey, setAccKey] = useState('');

  const [showCustomProvider, setShowCustomProvider] = useState(false);
  const [customProvId, setCustomProvId] = useState('');
  const [customProvUrl, setCustomProvUrl] = useState('');
  const [customProvKey, setCustomProvKey] = useState('');

  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!isOpen) return;
    setStep('provider');
    setQuery('');
    setSelectedProvider(activeProvider || providers[0]?.id || '');
    setSelectedAccountId(null);
    setAccountModels(null);
    setLoadingModels(false);
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
    setErrorMsg(null);
    setTimeout(() => searchRef.current?.focus(), 20);
  }, [isOpen, activeProvider, initialTarget, initialScope, sessionId]);

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

  const filteredProviders = useMemo(() => {
    if (!normalizedQuery) return sortedProviders;
    return sortedProviders.filter(
      (p) =>
        p.display_name.toLowerCase().includes(normalizedQuery) ||
        p.id.toLowerCase().includes(normalizedQuery)
    );
  }, [sortedProviders, normalizedQuery]);

  const rawModelsForStep = useMemo(() => {
    if (accountModels !== null) return accountModels;
    return currentProviderObj?.models || [];
  }, [accountModels, currentProviderObj]);

  const filteredModels = useMemo(() => {
    if (!normalizedQuery) return rawModelsForStep;
    return rawModelsForStep.filter((m) => m.toLowerCase().includes(normalizedQuery));
  }, [rawModelsForStep, normalizedQuery]);

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

  const accounts = currentProviderObj?.accounts || [];
  const prioritizedAcc = activeAccountForProvider(accounts);
  const selectedAccount = accounts.find((a) => a.id === selectedAccountId) || null;
  const hasCooldowns = accounts.some((a) => a.is_temporarily_disabled);

  const loadModelsForAccount = async (
    providerId: string,
    credentialId: string | null,
    force: boolean
  ) => {
    setLoadingModels(true);
    setErrorMsg(null);
    try {
      const models = await onRefreshProvider(providerId, credentialId || undefined, force);
      setAccountModels(models);
    } catch (e) {
      setAccountModels(null);
      setErrorMsg(`Could not refresh account models: ${String(e)}`);
    } finally {
      setLoadingModels(false);
    }
  };

  const handleSelectProvider = (providerId: string) => {
    setSelectedProvider(providerId);
    setSelectedAccountId(null);
    setAccountModels(null);
    setShowAddAccount(false);
    setShowCustomProvider(false);
    setErrorMsg(null);
    setQuery('');
    setStep('account');
  };

  const handleSelectAccount = async (acc: AccountInfo) => {
    if (!currentProviderObj) return;
    setSaving(true);
    setErrorMsg(null);
    try {
      await onSetPriorityAccount(currentProviderObj.id, acc.id, effectiveScope);
      setSelectedAccountId(acc.id);
      setQuery('');
      setStep('model');
      setTimeout(() => searchRef.current?.focus(), 20);
      await loadModelsForAccount(currentProviderObj.id, acc.id, false);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setSaving(false);
    }
  };

  const handleSkipAccount = async () => {
    if (!currentProviderObj) return;
    setSelectedAccountId(null);
    setAccountModels(null);
    setQuery('');
    setStep('model');
    setTimeout(() => searchRef.current?.focus(), 20);
  };

  const handleChooseModel = async (modelId: string) => {
    if (!currentProviderObj || !modelId.trim()) return;
    setSaving(true);
    setErrorMsg(null);
    try {
      await onSelectModel(currentProviderObj.id, modelId.trim(), effectiveScope, target);
      onClose();
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setSaving(false);
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
      const nextId = customProvId.trim().toLowerCase();
      await onAddCustomProvider({
        id: customProvId.trim(),
        base_url: customProvUrl.trim(),
        api_key: customProvKey.trim() || undefined,
      });
      setSelectedProvider(nextId);
      setCustomProvId('');
      setCustomProvUrl('');
      setCustomProvKey('');
      setShowCustomProvider(false);
      setStep('account');
    } catch (e) {
      setErrorMsg(`Failed to add provider: ${String(e)}`);
    } finally {
      setSaving(false);
    }
  };

  const popoverStyle: React.CSSProperties | undefined = anchorRect
    ? (() => {
        const width = 360;
        const left = Math.max(
          12,
          Math.min(anchorRect.left, (typeof window !== 'undefined' ? window.innerWidth : 1024) - width - 12)
        );
        const bottom = Math.max(
          12,
          (typeof window !== 'undefined' ? window.innerHeight : 768) - anchorRect.top + 8
        );
        return {
          position: 'fixed',
          left: `${left}px`,
          bottom: `${bottom}px`,
          width: `${width}px`,
          maxHeight: `min(400px, calc(100vh - ${bottom + 16}px))`,
        };
      })()
    : undefined;

  const content = (
    <div
      className={`model-popover ${anchorRect ? '' : 'model-popover-centered'}`}
      style={popoverStyle}
      role="dialog"
      aria-modal="true"
      aria-label="Select provider, account, and model"
      onClick={(e) => e.stopPropagation()}
    >
      {/* Compact Header / Breadcrumb */}
      <div className="model-popover-head">
        <div className="model-popover-title">
          {step !== 'provider' && (
            <button
              type="button"
              className="popover-back-btn"
              onClick={() => {
                setQuery('');
                setErrorMsg(null);
                setShowCustomProvider(false);
                setStep(step === 'model' ? 'account' : 'provider');
              }}
              title="Back"
            >
              <ChevronLeft size={14} />
            </button>
          )}
          <span className="model-popover-heading">
            {step === 'provider'
              ? 'Provider'
              : step === 'account'
              ? `${currentProviderObj?.display_name || 'Provider'} · Account`
              : `${currentProviderObj?.display_name || 'Models'}${
                  selectedAccount ? ` · ${selectedAccount.label}` : ''
                }`}
          </span>
          {target !== 'session' && (
            <span className="popover-target-badge">{TARGET_LABEL[target]}</span>
          )}
        </div>

        <div className="model-popover-head-right">
          {!isGlobalOnlyTarget && sessionId && (
            <div className="segmented-toggle segmented-toggle-xs" role="group" aria-label="Scope">
              <button
                type="button"
                className={`segmented-btn ${effectiveScope === 'session' ? 'active' : ''}`}
                onClick={() => setScope('session')}
                title="Apply to this session only"
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
            <X size={14} />
          </button>
        </div>
      </div>

      {errorMsg && <div className="popover-error">{errorMsg}</div>}

      {/* Step 1: Provider Picker */}
      {step === 'provider' && (
        <>
          <div className="popover-search-row">
            <Search size={13} className="picker-search-icon" />
            <input
              ref={searchRef}
              type="text"
              className="popover-search-input"
              placeholder="Filter providers…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
            <button
              type="button"
              className="picker-mini-btn"
              onClick={() => setShowCustomProvider((v) => !v)}
              title="Add custom OpenAI-compatible provider"
            >
              <Plus size={11} />
              <span>Custom</span>
            </button>
          </div>

          {showCustomProvider ? (
            <div className="popover-form-body">
              <input
                type="text"
                className="form-input"
                placeholder="Provider id (e.g. vllm, lmstudio)"
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
                placeholder="API key (optional)"
                value={customProvKey}
                onChange={(e) => setCustomProvKey(e.target.value)}
              />
              <div className="inline-row">
                <button
                  type="button"
                  className="btn btn-secondary btn-xs"
                  onClick={() => setShowCustomProvider(false)}
                >
                  Cancel
                </button>
                <button
                  type="button"
                  className="btn btn-primary btn-xs"
                  disabled={saving || !customProvId.trim() || !customProvUrl.trim()}
                  onClick={handleCreateCustomProvider}
                >
                  Save
                </button>
              </div>
            </div>
          ) : (
            <div className="popover-list">
              {filteredProviders.map((p) => {
                const isActive = p.id === activeProvider;
                return (
                  <button
                    key={p.id}
                    type="button"
                    className={`popover-row ${isActive ? 'active' : ''}`}
                    onClick={() => handleSelectProvider(p.id)}
                  >
                    <span className="picker-provider-left">
                      <span className={`picker-status-dot ${p.configured ? 'configured' : ''}`} />
                      <span className="picker-provider-name">{p.display_name}</span>
                    </span>
                    <span className="popover-row-right">
                      <span className="picker-count">
                        {p.accounts.length > 0 ? `${p.accounts.length}a` : `${p.models.length}m`}
                      </span>
                      <ChevronRight size={13} className="text-muted" />
                    </span>
                  </button>
                );
              })}
            </div>
          )}
        </>
      )}

      {/* Step 2: Account Picker */}
      {step === 'account' && currentProviderObj && (
        <>
          <div className="popover-subbar">
            <div className="inline-row">
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
                  title="Reset cooldowns"
                >
                  <RotateCcw size={11} />
                  <span>Reset</span>
                </button>
              )}
            </div>
            <button
              type="button"
              className="picker-mini-btn"
              onClick={() => setShowAddAccount((v) => !v)}
            >
              <Plus size={11} />
              <span>Add key</span>
            </button>
          </div>

          {(showAddAccount || accounts.length === 0) && (
            <div className="popover-form-inline">
              <input
                type="text"
                className="form-input"
                placeholder="Label (e.g. work)"
                value={accLabel}
                onChange={(e) => setAccLabel(e.target.value)}
              />
              <input
                type="password"
                className="form-input"
                placeholder={`API key for ${currentProviderObj.display_name}…`}
                value={accKey}
                onChange={(e) => setAccKey(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && handleSaveAccount()}
              />
              <button
                type="button"
                className="btn btn-primary btn-xs"
                onClick={handleSaveAccount}
                disabled={saving || !accKey.trim()}
              >
                Add
              </button>
            </div>
          )}

          <div className="popover-list">
            {accounts.length === 0 ? (
              <div className="popover-empty-block">
                <div className="text-muted">No accounts saved for {currentProviderObj.display_name}.</div>
                {currentProviderObj.models.length > 0 && (
                  <button
                    type="button"
                    className="btn btn-secondary btn-xs"
                    onClick={handleSkipAccount}
                  >
                    <span>Browse {currentProviderObj.models.length} models</span>
                    <ChevronRight size={12} />
                  </button>
                )}
              </div>
            ) : (
              accounts.map((acc) => {
                const isSelected = prioritizedAcc?.id === acc.id;
                return (
                  <div key={acc.id} className={`popover-row-wrap ${isSelected ? 'active' : ''}`}>
                    <button
                      type="button"
                      className="popover-row-btn"
                      onClick={() => handleSelectAccount(acc)}
                      disabled={saving}
                    >
                      <span className="picker-provider-left">
                        <span
                          className={`picker-status-dot ${
                            acc.is_temporarily_disabled ? '' : 'configured'
                          }`}
                        />
                        <span className="picker-provider-name">{acc.label}</span>
                      </span>
                      <span className="popover-row-right">
                        <span className="picker-count">
                          {acc.is_temporarily_disabled ? 'cooldown' : `${acc.usage_count}×`}
                        </span>
                        {isSelected && <Check size={12} className="text-accent" />}
                        <ChevronRight size={13} className="text-muted" />
                      </span>
                    </button>
                    <button
                      type="button"
                      className="popover-del-btn"
                      title="Remove account"
                      onClick={() => onRemoveCredential(currentProviderObj.id, acc.id)}
                    >
                      <Trash2 size={12} />
                    </button>
                  </div>
                );
              })
            )}
          </div>
        </>
      )}

      {/* Step 3: Models available for that account */}
      {step === 'model' && currentProviderObj && (
        <>
          <div className="popover-search-row">
            <Search size={13} className="picker-search-icon" />
            <input
              ref={searchRef}
              type="text"
              className="popover-search-input"
              placeholder="Filter models…"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
            <button
              type="button"
              className="picker-mini-btn"
              onClick={() =>
                loadModelsForAccount(currentProviderObj.id, selectedAccountId, true)
              }
              disabled={loadingModels}
              title="Refresh models for this account"
            >
              <RefreshCw size={11} className={loadingModels ? 'spin' : ''} />
              <span>{loadingModels ? 'Loading' : 'Refresh'}</span>
            </button>
          </div>

          <div className="popover-list">
            {loadingModels && filteredModels.length === 0 ? (
              <div className="popover-empty-block text-muted">
                Loading models for {selectedAccount?.label || currentProviderObj.display_name}…
              </div>
            ) : filteredModels.length === 0 ? (
              <div className="popover-empty-block text-muted">
                {normalizedQuery
                  ? `No models matching "${query.trim()}"`
                  : 'No models returned for this account.'}
              </div>
            ) : (
              filteredModels.map((m) => {
                const isActive =
                  currentProviderObj.id === activeProvider && m === activeModel;
                return (
                  <button
                    key={m}
                    type="button"
                    className={`popover-row ${isActive ? 'active' : ''}`}
                    disabled={saving}
                    onClick={() => handleChooseModel(m)}
                  >
                    <span className="popover-model-name">{m}</span>
                    {isActive && <Check size={13} className="text-accent" />}
                  </button>
                );
              })
            )}

            {normalizedQuery && !filteredModels.includes(query.trim()) && (
              <button
                type="button"
                className="popover-row popover-row-custom"
                disabled={saving}
                onClick={() => handleChooseModel(query.trim())}
              >
                <span className="popover-model-name">Use "{query.trim()}"</span>
              </button>
            )}
          </div>
        </>
      )}
    </div>
  );

  if (anchorRect) {
    return (
      <div className="popover-backdrop" onClick={onClose}>
        {content}
      </div>
    );
  }

  return (
    <div className="modal-backdrop" onClick={onClose}>
      {content}
    </div>
  );
};
