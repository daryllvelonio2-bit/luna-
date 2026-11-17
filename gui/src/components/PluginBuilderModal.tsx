import { useState } from "react";
import { Plus, Trash2, X } from "lucide-react";
import { api } from "../api";
import type {
  BuilderPanelWidgetDef,
  BuilderScheduleDef,
  BuilderToolDef,
  PluginBuilderSpec,
  PluginSummary,
} from "../types";

interface PluginBuilderModalProps {
  onClose: () => void;
  onCreated: (plugin: PluginSummary) => void;
}

export function PluginBuilderModal({
  onClose,
  onCreated,
}: PluginBuilderModalProps) {
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [systemPrompt, setSystemPrompt] = useState(
    "You are a focused specialist assistant using your plugin tools."
  );
  const [tools, setTools] = useState<BuilderToolDef[]>([
    {
      name: "run_action",
      description: "Execute the plugin's primary action",
      params: [
        {
          name: "target",
          param_type: "string",
          description: "Target value or argument",
          required: true,
        },
      ],
      action: {
        kind: "shell",
        command_template: "echo Running on {{target}}",
      },
    },
  ]);
  const [schedules, setSchedules] = useState<BuilderScheduleDef[]>([]);
  const [widgets, setWidgets] = useState<BuilderPanelWidgetDef[]>([
    {
      kind: "status_card",
      title: "Plugin Status",
      body: "Ready — sidebar agent active",
      command: null,
    },
  ]);
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleCreate = async () => {
    if (!name.trim()) {
      setError("Plugin name is required");
      return;
    }
    setSubmitting(true);
    setError(null);
    try {
      const spec: PluginBuilderSpec = {
        id: "",
        name: name.trim(),
        description: description.trim() || `${name.trim()} automation plugin`,
        icon: "puzzle",
        system_prompt: systemPrompt.trim(),
        model: null,
        tools,
        schedules,
        widgets,
      };
      const created = await api.createPluginFromBuilder(spec);
      onCreated(created);
    } catch (e) {
      setError(String(e));
    } finally {
      setSubmitting(false);
    }
  };

  return (
    <div className="modal-backdrop">
      <div className="modal-card">
        <div className="modal-header">
          <span>Guided Plugin Builder (compiles to TypeScript)</span>
          <button className="icon-btn" onClick={onClose}>
            <X size={16} />
          </button>
        </div>

        <div className="modal-body">
          {error && <div className="badge-err">{error}</div>}

          <label className="field-label">
            <span>Plugin Name (automatically gets a sidebar Agent)</span>
            <input
              className="text-input"
              placeholder="e.g. Daily Repo Monitor"
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </label>

          <label className="field-label">
            <span>Description</span>
            <input
              className="text-input"
              placeholder="What this plugin does..."
              value={description}
              onChange={(e) => setDescription(e.target.value)}
            />
          </label>

          <label className="field-label">
            <span>Sidebar Agent System Prompt</span>
            <textarea
              className="area-input"
              rows={3}
              value={systemPrompt}
              onChange={(e) => setSystemPrompt(e.target.value)}
            />
          </label>

          <div>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
              <strong style={{ fontSize: 13 }}>Custom Tools ({tools.length})</strong>
              <button
                className="btn-secondary"
                onClick={() =>
                  setTools((prev) => [
                    ...prev,
                    {
                      name: `tool_${prev.length + 1}`,
                      description: "Custom plugin action",
                      params: [
                        {
                          name: "input",
                          param_type: "string",
                          description: "Input parameter",
                          required: true,
                        },
                      ],
                      action: {
                        kind: "kv_set",
                        key_template: "last_input",
                        value_template: "{{input}}",
                      },
                    },
                  ])
                }
              >
                <Plus size={13} />
                <span>Add Tool</span>
              </button>
            </div>

            {tools.map((t, idx) => (
              <div
                key={idx}
                className="surface-card"
                style={{ marginBottom: 8, padding: 12 }}
              >
                <div style={{ display: "flex", gap: 8 }}>
                  <input
                    className="text-input"
                    style={{ flex: 1 }}
                    placeholder="tool_name"
                    value={t.name}
                    onChange={(e) => {
                      const next = [...tools];
                      next[idx] = { ...t, name: e.target.value };
                      setTools(next);
                    }}
                  />
                  <select
                    className="select-input"
                    value={t.action.kind}
                    onChange={(e) => {
                      const kind = e.target.value;
                      const next = [...tools];
                      if (kind === "shell") {
                        next[idx] = {
                          ...t,
                          action: { kind: "shell", command_template: "echo {{target}}" },
                        };
                      } else if (kind === "http") {
                        next[idx] = {
                          ...t,
                          action: {
                            kind: "http",
                            method: "GET",
                            url_template: "https://httpbin.org/get?q={{target}}",
                          },
                        };
                      } else if (kind === "read_file") {
                        next[idx] = {
                          ...t,
                          action: { kind: "read_file", path_template: "{{target}}" },
                        };
                      } else if (kind === "kv_get") {
                        next[idx] = {
                          ...t,
                          action: { kind: "kv_get", key_template: "{{target}}" },
                        };
                      } else {
                        next[idx] = {
                          ...t,
                          action: {
                            kind: "kv_set",
                            key_template: "note",
                            value_template: "{{target}}",
                          },
                        };
                      }
                      setTools(next);
                    }}
                  >
                    <option value="shell">Shell Command</option>
                    <option value="http">HTTP Request</option>
                    <option value="read_file">Read File</option>
                    <option value="kv_get">Plugin KV Get</option>
                    <option value="kv_set">Plugin KV Set</option>
                  </select>
                  <button
                    className="icon-btn"
                    onClick={() => setTools(tools.filter((_, i) => i !== idx))}
                  >
                    <Trash2 size={14} />
                  </button>
                </div>

                <input
                  className="text-input"
                  placeholder="Tool description"
                  value={t.description}
                  onChange={(e) => {
                    const next = [...tools];
                    next[idx] = { ...t, description: e.target.value };
                    setTools(next);
                  }}
                />

                {t.action.kind === "shell" && (
                  <input
                    className="text-input"
                    placeholder="Shell command template (use {{param}})"
                    value={t.action.command_template}
                    onChange={(e) => {
                      const next = [...tools];
                      next[idx] = {
                        ...t,
                        action: { kind: "shell", command_template: e.target.value },
                      };
                      setTools(next);
                    }}
                  />
                )}
                {t.action.kind === "http" && (
                  <input
                    className="text-input"
                    placeholder="URL template (use {{param}})"
                    value={t.action.url_template}
                    onChange={(e) => {
                      const next = [...tools];
                      if (t.action.kind === "http") {
                        next[idx] = {
                          ...t,
                          action: { ...t.action, url_template: e.target.value },
                        };
                      }
                      setTools(next);
                    }}
                  />
                )}
              </div>
            ))}
          </div>

          <div>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
              <strong style={{ fontSize: 13 }}>Automatic Schedules ({schedules.length})</strong>
              <button
                className="btn-secondary"
                onClick={() =>
                  setSchedules((prev) => [
                    ...prev,
                    {
                      name: "Daily check",
                      rule: "daily:09:00",
                      instruction: "Run the plugin check and summarize results.",
                    },
                  ])
                }
              >
                <Plus size={13} />
                <span>Add Schedule</span>
              </button>
            </div>
            {schedules.map((s, idx) => (
              <div key={idx} className="surface-card" style={{ marginBottom: 8, padding: 12 }}>
                <div style={{ display: "flex", gap: 8 }}>
                  <input
                    className="text-input"
                    style={{ flex: 1 }}
                    placeholder="Schedule name"
                    value={s.name}
                    onChange={(e) => {
                      const next = [...schedules];
                      next[idx] = { ...s, name: e.target.value };
                      setSchedules(next);
                    }}
                  />
                  <input
                    className="text-input"
                    style={{ width: 170 }}
                    placeholder="daily:09:00 or every:3600"
                    value={s.rule}
                    onChange={(e) => {
                      const next = [...schedules];
                      next[idx] = { ...s, rule: e.target.value };
                      setSchedules(next);
                    }}
                  />
                  <button
                    className="icon-btn"
                    onClick={() => setSchedules(schedules.filter((_, i) => i !== idx))}
                  >
                    <Trash2 size={14} />
                  </button>
                </div>
                <input
                  className="text-input"
                  placeholder="Instruction to execute when triggered"
                  value={s.instruction}
                  onChange={(e) => {
                    const next = [...schedules];
                    next[idx] = { ...s, instruction: e.target.value };
                    setSchedules(next);
                  }}
                />
              </div>
            ))}
          </div>

          <div>
            <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
              <strong style={{ fontSize: 13 }}>Panel Widgets ({widgets.length})</strong>
              <button
                className="btn-secondary"
                onClick={() =>
                  setWidgets((prev) => [
                    ...prev,
                    {
                      kind: "markdown",
                      title: "Guide",
                      body: "Use this agent from the sidebar anytime.",
                      command: null,
                    },
                  ])
                }
              >
                <Plus size={13} />
                <span>Add Widget</span>
              </button>
            </div>
          </div>
        </div>

        <div className="modal-footer">
          <button className="btn-secondary" onClick={onClose}>
            Cancel
          </button>
          <button
            className="btn-primary"
            disabled={submitting}
            onClick={handleCreate}
          >
            {submitting ? "Compiling TypeScript..." : "Build & Install Plugin"}
          </button>
        </div>
      </div>
    </div>
  );
}
