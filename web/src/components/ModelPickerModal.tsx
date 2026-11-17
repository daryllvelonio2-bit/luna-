import React, { useState, useEffect, useMemo, useRef } from 'react';
import type { ProviderInfo } from '../types';
import { Search, Check, RefreshCw, Key, X } from 'lucide-react';

interface ModelPickerModalProps {
  isOpen: boolean;
  onClose: () => void;
  providers: ProviderInfo[];
  activeProvider: string;
  activeModel: string;
  onSelectModel: (provider: string, model: string) => Promise<void>;
  onRefreshProvider: (providerId: string) => Promise<void>;
  onSaveCredential: (provider: string, key: string) => Promise<void>;
}

export const ModelPickerModal: React.FC<ModelPickerModalProps> = ({
  isOpen,
  onClose,
  providers,
  activeProvider,
  activeModel,
  onSelectModel,
  onRefreshProvider,
  onSaveCredential,
}) => {
  const [query, setQuery] = useState('');
  const [selectedProvider, setSelectedProvider] = useState(activeProvider);
  const [refreshing, setRefreshing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [keyInput, setKeyInput] = useState('');
  const [showKeyInput, setShowKeyInput] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!isOpen) return;
    setQuery('');
    setSelectedProvider(activeProvider || providers[0]?.id || '');
    setShowKeyInput(false);
    setKeyInput('');
    setErrorMsg(null);
    setTimeout(() => searchRef.current?.focus(), 20);
  }, [isOpen, activeProvider, providers]);

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

  // When searching across all providers vs browsing selected provider
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
        p.models.some((m) => m.toLowerCase().includes(normalizedQuery))
    );
  }, [sortedProviders, normalizedQuery]);

  if (!isOpen) return null;

  const handleChooseModel = async (providerId: string, modelId: string) => {
    setSaving(true);
    setErrorMsg(null);
    try {
      await onSelectModel(providerId, modelId);
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

  const handleSaveKey = async () => {
    if (!currentProviderObj || !keyInput.trim()) return;
    setSaving(true);
    setErrorMsg(null);
    try {
      await onSaveCredential(currentProviderObj.id, keyInput.trim());
      setKeyInput('');
      setShowKeyInput(false);
    } catch (e) {
      setErrorMsg(`Failed to save key: ${String(e)}`);
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="picker-modal"
        role="dialog"
        aria-modal="true"
        aria-label="Choose model and provider"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="picker-search-bar">
          <Search size={15} className="picker-search-icon" />
          <input
            ref={searchRef}
            type="text"
            className="picker-search-input"
            placeholder="Search providers or models…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
          {query && (
            <button
              type="button"
              className="icon-btn"
              onClick={() => setQuery('')}
              aria-label="Clear search"
            >
              <X size={14} />
            </button>
          )}
          <button type="button" className="icon-btn" onClick={onClose} aria-label="Close">
            <X size={15} />
          </button>
        </div>

        {errorMsg && <div className="picker-error">{errorMsg}</div>}

        <div className="picker-panes">
          <div className="picker-providers">
            <div className="picker-pane-label">Providers</div>
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
                      setShowKeyInput(false);
                    }}
                  >
                    <span className="picker-provider-left">
                      <span
                        className={`picker-status-dot ${p.configured ? 'configured' : ''}`}
                        title={p.configured ? 'Configured' : 'Not configured'}
                      />
                      <span className="picker-provider-name">{p.display_name}</span>
                    </span>
                    <span className="picker-count">{p.models.length}</span>
                  </button>
                );
              })}
            </div>
          </div>

          <div className="picker-models">
            <div className="picker-models-header">
              <span className="picker-pane-label">
                {currentProviderObj?.display_name || 'Models'}
                {currentProviderObj?.configured ? ' · Ready' : ' · Key required'}
              </span>
              <div className="picker-header-actions">
                <button
                  type="button"
                  className="picker-action-chip"
                  onClick={() => setShowKeyInput((v) => !v)}
                >
                  <Key size={12} />
                  <span>{currentProviderObj?.configured ? 'Update key' : 'Add key'}</span>
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

            {showKeyInput && currentProviderObj && (
              <div className="picker-key-bar">
                <input
                  type="password"
                  className="form-input flex-1"
                  placeholder={`Paste API key for ${currentProviderObj.display_name}…`}
                  value={keyInput}
                  onChange={(e) => setKeyInput(e.target.value)}
                  onKeyDown={(e) => e.key === 'Enter' && handleSaveKey()}
                />
                <button
                  type="button"
                  className="btn btn-primary"
                  onClick={handleSaveKey}
                  disabled={saving || !keyInput.trim()}
                >
                  Save
                </button>
              </div>
            )}

            <div className="picker-list">
              {filteredModels.length === 0 ? (
                <div className="picker-empty">
                  {currentProviderObj?.models.length === 0
                    ? 'No cached models yet. Click Refresh or type a custom model ID below.'
                    : 'No models match your search.'}
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
          </div>
        </div>
      </div>
    </div>
  );
};
