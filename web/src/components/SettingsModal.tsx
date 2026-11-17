import React, { useState, useEffect } from 'react';
import type { ProviderInfo } from '../types';
import { X, Key, Cpu } from 'lucide-react';

interface SettingsModalProps {
  isOpen: boolean;
  onClose: () => void;
  providers: ProviderInfo[];
  defaultProvider: string;
  defaultModel: string;
  onSaveDefaultModel: (provider: string, model: string) => Promise<void>;
  onSaveCredential: (provider: string, key: string) => Promise<void>;
}

export const SettingsModal: React.FC<SettingsModalProps> = ({
  isOpen,
  onClose,
  providers,
  defaultProvider,
  defaultModel,
  onSaveDefaultModel,
  onSaveCredential,
}) => {
  const [selectedProvider, setSelectedProvider] = useState(defaultProvider);
  const [selectedModel, setSelectedModel] = useState(defaultModel);
  const [apiKey, setApiKey] = useState('');
  const [isSaving, setIsSaving] = useState(false);
  const [statusMsg, setStatusMsg] = useState<{ kind: 'ok' | 'err'; text: string } | null>(null);

  useEffect(() => {
    setSelectedProvider(defaultProvider);
    setSelectedModel(defaultModel);
  }, [defaultProvider, defaultModel, isOpen]);

  useEffect(() => {
    if (!isOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [isOpen, onClose]);

  if (!isOpen) return null;

  const currentProviderInfo = providers.find((p) => p.id === selectedProvider);

  const handleSaveModel = async () => {
    setIsSaving(true);
    setStatusMsg(null);
    try {
      await onSaveDefaultModel(selectedProvider, selectedModel);
      setStatusMsg({ kind: 'ok', text: `Default model updated to ${selectedProvider} / ${selectedModel}.` });
    } catch (e) {
      setStatusMsg({ kind: 'err', text: `Failed to save model: ${String(e)}` });
    } finally {
      setIsSaving(false);
    }
  };

  const handleSaveKey = async () => {
    if (!apiKey.trim()) return;
    setIsSaving(true);
    setStatusMsg(null);
    try {
      await onSaveCredential(selectedProvider, apiKey.trim());
      setApiKey('');
      setStatusMsg({ kind: 'ok', text: `Credential saved for ${selectedProvider}.` });
    } catch (e) {
      setStatusMsg({ kind: 'err', text: `Failed to save credential: ${String(e)}` });
    } finally {
      setIsSaving(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal-card"
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
            <div className={`inline-banner ${statusMsg.kind === 'err' ? 'inline-banner-err' : 'inline-banner-ok'}`}>
              {statusMsg.text}
            </div>
          )}

          <div className="settings-section">
            <div className="settings-section-title">
              <Cpu size={14} className="text-accent" />
              <span>Model & provider</span>
            </div>

            <div className="two-col-fields">
              <div className="form-group">
                <label className="form-label">Provider</label>
                <select
                  className="form-input"
                  value={selectedProvider}
                  onChange={(e) => {
                    const p = e.target.value;
                    setSelectedProvider(p);
                    const info = providers.find((pr) => pr.id === p);
                    if (info && info.models.length > 0) {
                      setSelectedModel(info.models[0]);
                    }
                  }}
                >
                  {providers.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.display_name} {p.configured ? '(Configured)' : ''}
                    </option>
                  ))}
                </select>
              </div>

              <div className="form-group">
                <label className="form-label">Model</label>
                <select
                  className="form-input"
                  value={selectedModel}
                  onChange={(e) => setSelectedModel(e.target.value)}
                >
                  {currentProviderInfo?.models.map((m) => (
                    <option key={m} value={m}>
                      {m}
                    </option>
                  ))}
                </select>
              </div>
            </div>

            <div>
              <button
                type="button"
                className="btn btn-secondary"
                onClick={handleSaveModel}
                disabled={isSaving}
              >
                Save default model
              </button>
            </div>
          </div>

          <div className="settings-divider" />

          <div className="settings-section">
            <div className="settings-section-title">
              <Key size={14} className="text-warning" />
              <span>Credentials ({selectedProvider})</span>
            </div>

            <div className="form-group">
              <label className="form-label">API key or token</label>
              <div className="inline-row">
                <input
                  type="password"
                  className="form-input flex-1"
                  placeholder={`Enter key for ${selectedProvider}…`}
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
                  Save key
                </button>
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
