import { useState } from "react";
import { Bot, Code2, Plus, Power, RefreshCw, Trash2, X } from "lucide-react";
import { api } from "../api";
import type { PluginAgentEntry, PluginSummary } from "../types";
import { PluginBuilderModal } from "./PluginBuilderModal";

interface PluginsViewProps {
  plugins: PluginSummary[];
  onRefresh: () => void;
  onOpenAgent: (agent: PluginAgentEntry) => void;
}

export function PluginsView({
  plugins,
  onRefresh,
  onOpenAgent,
}: PluginsViewProps) {
  const [showBuilder, setShowBuilder] = useState(false);
  const [inspectingSource, setInspectingSource] = useState<{
    name: string;
    code: string;
  } | null>(null);

  return (
    <div className="page-scroll">
      <div className="page-header">
        <div>
          <h1>Plugins &amp; Agents</h1>
          <div style={{ fontSize: 13, color: "var(--text-muted)" }}>
            DeepSeek-style TypeScript plugins running inside Luna&apos;s embedded QuickJS runtime. Every plugin gets an automatic sidebar agent.
          </div>
        </div>
        <button className="btn-primary" onClick={() => setShowBuilder(true)}>
          <Plus size={15} />
          <span>Create Plugin</span>
        </button>
      </div>

      <div className="grid-cards">
        {plugins.map((p) => (
          <div key={p.id} className="surface-card">
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", gap: 8 }}>
              <div>
                <div style={{ fontWeight: 650, fontSize: 15 }}>
                  {p.name}{" "}
                  <span style={{ fontSize: 11, color: "var(--text-muted)" }}>
                    v{p.version}
                  </span>
                </div>
                <div style={{ fontSize: 12, color: "var(--text-secondary)", marginTop: 2 }}>
                  {p.description}
                </div>
              </div>
              <span className={p.error ? "badge-err" : p.enabled ? "badge-ok" : "badge-muted"}>
                {p.error ? "Error" : p.enabled ? "Active" : "Disabled"}
              </span>
            </div>

            {p.error && (
              <pre className="mono-pre" style={{ color: "var(--red)" }}>
                {p.error}
              </pre>
            )}

            <div style={{ fontSize: 12, color: "var(--text-muted)" }}>
              Source: <code>{p.source_kind}</code> &bull;{" "}
              {p.agent ? `${p.agent.registered_tools.length} tool(s)` : "No agent"}
            </div>

            <div style={{ display: "flex", gap: 6, flexWrap: "wrap", marginTop: "auto" }}>
              {p.agent && p.enabled && (
                <button
                  className="btn-primary"
                  onClick={() => onOpenAgent(p.agent!)}
                >
                  <Bot size={13} />
                  <span>Open Agent</span>
                </button>
              )}
              <button
                className="btn-secondary"
                onClick={async () => {
                  await api.togglePlugin(p.id, !p.enabled);
                  onRefresh();
                }}
              >
                <Power size={13} />
                <span>{p.enabled ? "Disable" : "Enable"}</span>
              </button>
              <button
                className="btn-secondary"
                onClick={async () => {
                  await api.reloadPlugin(p.id);
                  onRefresh();
                }}
              >
                <RefreshCw size={13} />
                <span>Reload</span>
              </button>
              <button
                className="btn-secondary"
                onClick={async () => {
                  const code = await api.readPluginSource(p.id);
                  setInspectingSource({ name: p.name, code });
                }}
              >
                <Code2 size={13} />
                <span>TS Source</span>
              </button>
              {!p.builtin && (
                <button
                  className="btn-danger"
                  onClick={async () => {
                    await api.deletePlugin(p.id);
                    onRefresh();
                  }}
                >
                  <Trash2 size={13} />
                </button>
              )}
            </div>
          </div>
        ))}
      </div>

      {showBuilder && (
        <PluginBuilderModal
          onClose={() => setShowBuilder(false)}
          onCreated={() => {
            setShowBuilder(false);
            onRefresh();
          }}
        />
      )}

      {inspectingSource && (
        <div className="modal-backdrop">
          <div className="modal-card">
            <div className="modal-header">
              <span>{inspectingSource.name} — TypeScript Source</span>
              <button
                className="icon-btn"
                onClick={() => setInspectingSource(null)}
              >
                <X size={16} />
              </button>
            </div>
            <div className="modal-body">
              <pre className="mono-pre">{inspectingSource.code}</pre>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
