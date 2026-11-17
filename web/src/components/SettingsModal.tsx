import React, { useState, useEffect, useMemo } from 'react';
import type { ProviderInfo } from '../types';
import { X, Key, Cpu, Folder, ChevronRight, Search } from 'lucide-react';

interface SettingsModalProps {
  isOpen: boolean;
  onClose: () => void;
  providers: ProviderInfo[];
  defaultProvider: string;
  defaultModel: string;
  workspaceRoot: string;
  onOpenModelPicker: () => void;
  onOpenWorkspacePicker: () => void;
  onSaveCredential: (provider: string, key: string) => Promise<void>;
}

export const SettingsModal: React.FC<SettingsModalProps> = ({
  isOpen,
  onClose,
  providers,
  defaultProvider,
  defaultModel,
  workspaceRoot,
  onOpenModelPicker,
  onOpenWorkspacePicker,
  onSaveCredential,
}) => {
  const [selectedProvider, setSelectedProvider] = useState(defaultProvider);
  const [providerQuery, setProviderQuery] = useState('');
  const [apiKey, setApiKey] = useState('');
  const [isSaving, setIsSaving] = useState(false);
  const [statusMsg, setStatusMsg] = useState<{ kind: 'ok' | 'err'; text: string } | null>(null);

  useEffect(() => {
    if (!isOpen) return;
    setSelectedProvider(defaultProvider || providers[0]?.id || '');
    setProviderQuery('');
    setApiKey('');
    setStatusMsg(null);
  }, [defaultProvider, isOpen, providers]);

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
      await onSaveCredential(selectedProvider, apiKey.trim());
      setApiKey('');
      setStatusMsg({
        kind: 'ok',
        text: `Saved credential for ${activeProviderObj?.display_name || selectedProvider}.`,
      });
    } catch (e) {
      setStatusMsg({ kind: 'err', text: `Failed to save credential: ${String(e)}` });
    } finally {
      setIsSaving(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal-card modal-card-wide"
        role="dialog"
        aria-modal="true"
        aria-label="Settings"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-header">
          <span className="modal-title">Settings</span>
          <button type="button" className="icon-btn" onClick={onClose} aria-label="Close">
            <X size={16} />
          </button>
        </div>

        <div className="modal-body">
          {statusMsg && (
            <div
              className={`inline-banner ${
                statusMsg.kind === 'err' ? 'inline-banner-err' : 'inline-banner-ok'
              }`}
            >
              {statusMsg.text}
            </div>
          )}

          <div className="settings-section">
            <div className="settings-section-title">
              <Folder size={14} className="text-accent" />
              <span>Workspace</span>
            </div>
            <button
              type="button"
              className="settings-action-card"
              onClick={() => {
                onClose();
                onOpenWorkspacePicker();
              }}
            >
              <div className="settings-action-info">
                <span className="settings-action-label">Active directory</span>
                <span className="settings-action-value mono-text">{workspaceRoot || '.'}</span>
              </div>
              <span className="settings-action-cta">
                <span>Change</span>
                <ChevronRight size={14} />
              </span>
            </button>
          </div>

          <div className="settings-divider" />

          <div className="settings-section">
            <div className="settings-section-title">
              <Cpu size={14} className="text-accent" />
              <span>Default model & provider</span>
            </div>
            <button
              type="button"
              className="settings-action-card"
              onClick={() => {
                onClose();
                onOpenModelPicker();
              }}
            >
              <div className="settings-action-info">
                <span className="settings-action-label">
                  {providers.find((p) => p.id === defaultProvider)?.display_name ||
                    defaultProvider ||
                    'Not selected'}
                </span>
                <span className="settings-action-value mono-text">
                  {defaultModel || 'Select a model'}
                </span>
              </div>
              <span className="settings-action-cta">
                <span>Choose</span>
                <ChevronRight size={14} />
              </span>
            </button>
          </div>

          <div className="settings-divider" />

          <div className="settings-section">
            <div className="settings-section-title">
              <Key size={14} className="text-warning" />
              <span>API credentials</span>
            </div>

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
                    </button>
                  ))}
                </div>
              </div>

              <div className="cred-editor">
                <div className="cred-editor-head">
                  <span className="cred-editor-title">
                    {activeProviderObj?.display_name || selectedProvider}
                  </span>
                  <span
                    className={`badge ${activeProviderObj?.configured ? 'badge-ok' : ''}`}
                  >
                    {activeProviderObj?.configured ? 'Configured' : 'Not configured'}
                  </span>
                </div>

                <div className="form-group">
                  <label className="form-label">API key or token</label>
                  <div className="inline-row">
                    <input
                      type="password"
                      className="form-input flex-1"
                      placeholder={`Enter key for ${
                        activeProviderObj?.display_name || selectedProvider
                      }…`}
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
                      Save
                    </button>
                  </div>
                </div>
              </div>
            </div>
          </div>
        </div>

        <div className="modal-footer">
          <button type="button" className="btn btn-primary" onClick={onClose}>
            Done
          </button>
        </div>
      </div>
    </div>
  );
};
