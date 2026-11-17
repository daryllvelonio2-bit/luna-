import React, { useState } from 'react';
import type { ProviderInfo } from '../types';
import { X, Key, Cpu, ShieldCheck } from 'lucide-react';

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

  if (!isOpen) return null;

  const currentProviderInfo = providers.find((p) => p.id === selectedProvider);

  const handleSaveModel = async () => {
    setIsSaving(true);
    try {
      await onSaveDefaultModel(selectedProvider, selectedModel);
      alert('Default model preferences saved.');
    } catch (e) {
      alert(`Failed to save model: ${String(e)}`);
    } finally {
      setIsSaving(false);
    }
  };

  const handleSaveKey = async () => {
    if (!apiKey.trim()) return;
    setIsSaving(true);
    try {
      await onSaveCredential(selectedProvider, apiKey.trim());
      setApiKey('');
      alert(`API credential for ${selectedProvider} stored successfully.`);
    } catch (e) {
      alert(`Failed to save credential: ${String(e)}`);
    } finally {
      setIsSaving(false);
    }
  };

  return (
    <div className="modal-backdrop">
      <div className="modal-card">
        <div className="modal-header">
          <span style={{ fontWeight: 600 }}>Luna Settings</span>
          <button className="action-btn" onClick={onClose}>
            <X size={18} />
          </button>
        </div>

        <div className="modal-body">
          <div style={{ display: 'flex', flexDirection: 'column', gap: '16px' }}>
            <div>
              <div style={{ display: 'flex', alignItems: 'center', gap: '8px', marginBottom: '8px' }}>
                <Cpu size={16} color="var(--accent-color)" />
                <span style={{ fontWeight: 600, fontSize: '14px' }}>Active Model & Provider</span>
              </div>

              <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '12px' }}>
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
                        {p.display_name} {p.configured ? '✓' : ''}
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

              <button
                className="secondary-btn"
                style={{ marginTop: '10px' }}
                onClick={handleSaveModel}
                disabled={isSaving}
              >
                Set as Default Model
              </button>
            </div>

            <div style={{ borderTop: '1px solid var(--border-color)', paddingTop: '16px' }}>
              <div style={{ display: 'flex', alignItems: 'center', gap: '8px', marginBottom: '8px' }}>
                <Key size={16} color="var(--warning-color)" />
                <span style={{ fontWeight: 600, fontSize: '14px' }}>
                  Manage Credentials ({selectedProvider})
                </span>
              </div>

              <div className="form-group">
                <label className="form-label">API Key / Token</label>
                <div style={{ display: 'flex', gap: '8px' }}>
                  <input
                    type="password"
                    className="form-input"
                    style={{ flex: 1 }}
                    placeholder={`Enter key for ${selectedProvider}...`}
                    value={apiKey}
                    onChange={(e) => setApiKey(e.target.value)}
                  />
                  <button
                    className="primary-btn"
                    onClick={handleSaveKey}
                    disabled={isSaving || !apiKey.trim()}
                  >
                    Save Key
                  </button>
                </div>
              </div>
            </div>
          </div>
        </div>

        <div className="modal-footer">
          <button className="primary-btn" onClick={onClose}>
            Done
          </button>
        </div>
      </div>
    </div>
  );
};
