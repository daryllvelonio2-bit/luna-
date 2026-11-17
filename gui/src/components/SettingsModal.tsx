import { useEffect, useState } from "react";
import { X } from "lucide-react";
import { api } from "../api";
import type { CredentialItem, GuiProviderInfo } from "../types";

interface SettingsModalProps {
  sessionId: string | null;
  currentProvider: string;
  currentModel: string;
  onClose: () => void;
  onModelSwitched: (provider: string, model: string) => void;
}

export function SettingsModal({
  sessionId,
  currentProvider,
  currentModel,
  onClose,
  onModelSwitched,
}: SettingsModalProps) {
  const [providers, setProviders] = useState<GuiProviderInfo[]>([]);
  const [selectedProvider, setSelectedProvider] = useState(currentProvider);
  const [selectedModel, setSelectedModel] = useState(currentModel);
  const [credentials, setCredentials] = useState<CredentialItem[]>([]);
  const [scope, setScope] = useState<"session" | "global">("session");
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    api.listProvidersAndModels().then((list) => {
      setProviders(list);
    });
  }, []);

  useEffect(() => {
    if (selectedProvider) {
      api.listCredentials(selectedProvider).then(setCredentials);
    }
  }, [selectedProvider]);

  const activeProvider = providers.find((p) => p.id === selectedProvider);

  return (
    <div className="modal-backdrop">
      <div className="modal-card" style={{ width: "min(560px, 100%)" }}>
        <div className="modal-header">
          <span>Model &amp; Credential Priority</span>
          <button className="icon-btn" onClick={onClose}>
            <X size={16} />
          </button>
        </div>

        <div className="modal-body">
          <label className="field-label">
            <span>Provider</span>
            <select
              className="select-input"
              value={selectedProvider}
              onChange={(e) => {
                const pid = e.target.value;
                setSelectedProvider(pid);
                const found = providers.find((p) => p.id === pid);
                if (found && found.models.length > 0) {
                  setSelectedModel(found.models[0]);
                }
              }}
            >
              {providers.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.display_name} {p.configured ? "(configured)" : "(no auth)"}
                </option>
              ))}
            </select>
          </label>

          <label className="field-label">
            <span>Model</span>
            {activeProvider && activeProvider.models.length > 0 ? (
              <select
                className="select-input"
                value={selectedModel}
                onChange={(e) => setSelectedModel(e.target.value)}
              >
                {activeProvider.models.map((m) => (
                  <option key={m} value={m}>
                    {m}
                  </option>
                ))}
              </select>
            ) : (
              <input
                className="text-input"
                value={selectedModel}
                onChange={(e) => setSelectedModel(e.target.value)}
              />
            )}
          </label>

          {credentials.length > 0 && (
            <div>
              <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 6 }}>
                <span style={{ fontSize: 12, color: "var(--text-secondary)" }}>
                  Account / Credential Priority
                </span>
                <select
                  className="select-input"
                  value={scope}
                  onChange={(e) => setScope(e.target.value as "session" | "global")}
                >
                  <option value="session">This session only</option>
                  <option value="global">Global default</option>
                </select>
              </div>
              <div style={{ display: "flex", flexDirection: "column", gap: 6 }}>
                {credentials.map((c) => (
                  <button
                    key={c.id}
                    className="btn-secondary"
                    style={{ justifyContent: "space-between" }}
                    onClick={async () => {
                      if (!sessionId) return;
                      await api.setCredentialPriority(
                        sessionId,
                        selectedProvider,
                        c.id,
                        scope
                      );
                      const updated = await api.listCredentials(selectedProvider);
                      setCredentials(updated);
                    }}
                  >
                    <span>{c.label || c.id}</span>
                    {c.is_priority && <span className="badge-ok">Priority</span>}
                  </button>
                ))}
              </div>
            </div>
          )}
        </div>

        <div className="modal-footer">
          <button className="btn-secondary" onClick={onClose}>
            Cancel
          </button>
          <button
            className="btn-primary"
            disabled={saving || !sessionId}
            onClick={async () => {
              if (!sessionId) return;
              setSaving(true);
              try {
                await api.switchSessionModel(
                  sessionId,
                  selectedProvider,
                  selectedModel
                );
                onModelSwitched(selectedProvider, selectedModel);
                onClose();
              } finally {
                setSaving(false);
              }
            }}
          >
            {saving ? "Switching..." : "Apply Model"}
          </button>
        </div>
      </div>
    </div>
  );
}
