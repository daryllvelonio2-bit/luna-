import React, { useState, useEffect } from 'react';
import type { PluginBuilderSpec } from '../types';
import { Hammer, X } from 'lucide-react';

interface PluginBuilderModalProps {
  isOpen: boolean;
  onClose: () => void;
  onSubmit: (spec: PluginBuilderSpec) => Promise<void>;
}

export const PluginBuilderModal: React.FC<PluginBuilderModalProps> = ({
  isOpen,
  onClose,
  onSubmit,
}) => {
  const [name, setName] = useState('');
  const [description, setDescription] = useState('');
  const [author] = useState('User');
  const [version, setVersion] = useState('0.1.0');

  const [toolName, setToolName] = useState('my_custom_tool');
  const [toolDescription, setToolDescription] = useState('Executes custom TypeScript logic');
  const [handlerCode, setHandlerCode] = useState(
    `export default async function handle(args) {\n  return "Processed with " + JSON.stringify(args);\n}`
  );

  const [includeAgent, setIncludeAgent] = useState(true);
  const [agentName, setAgentName] = useState('Specialist Agent');
  const [systemPrompt, setSystemPrompt] = useState('You are an expert specialist plugin agent.');

  const [isSubmitting, setIsSubmitting] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  useEffect(() => {
    if (!isOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [isOpen, onClose]);

  if (!isOpen) return null;

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!name.trim()) return;

    const spec: PluginBuilderSpec = {
      name: name.trim(),
      description: description.trim(),
      author: author.trim(),
      version: version.trim(),
      tools: [
        {
          name: toolName.trim(),
          description: toolDescription.trim(),
          parameters_schema: {
            type: 'object',
            properties: {
              input: { type: 'string', description: 'Input parameter' },
            },
          },
          handler_code: handlerCode,
        },
      ],
      agents: includeAgent
        ? [
            {
              name: agentName.trim(),
              description: description.trim(),
              system_prompt: systemPrompt.trim(),
              tools: [toolName.trim()],
            },
          ]
        : [],
      schedules: [],
    };

    setIsSubmitting(true);
    setErrorMsg(null);
    try {
      await onSubmit(spec);
      onClose();
    } catch (err) {
      setErrorMsg(`Failed to build plugin: ${String(err)}`);
    } finally {
      setIsSubmitting(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <form
        className="modal-card modal-card-wide"
        onSubmit={handleSubmit}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-header">
          <div className="modal-title-group">
            <Hammer size={16} className="text-accent" />
            <span className="modal-title">Plugin builder</span>
          </div>
          <button type="button" className="icon-btn" onClick={onClose} aria-label="Close">
            <X size={16} />
          </button>
        </div>

        <div className="modal-body">
          {errorMsg && <div className="inline-banner inline-banner-err">{errorMsg}</div>}

          <div className="two-col-fields">
            <div className="form-group">
              <label className="form-label">Plugin name</label>
              <input
                type="text"
                className="form-input"
                required
                placeholder="e.g. data-analyzer"
                value={name}
                onChange={(e) => setName(e.target.value)}
              />
            </div>
            <div className="form-group">
              <label className="form-label">Version</label>
              <input
                type="text"
                className="form-input"
                value={version}
                onChange={(e) => setVersion(e.target.value)}
              />
            </div>
          </div>

          <div className="form-group">
            <label className="form-label">Description</label>
            <input
              type="text"
              className="form-input"
              placeholder="What this plugin does…"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
            />
          </div>

          <div className="settings-divider" />

          <div className="settings-section">
            <div className="settings-section-title">
              <span>Custom tool</span>
            </div>
            <div className="two-col-fields">
              <div className="form-group">
                <label className="form-label">Tool name</label>
                <input
                  type="text"
                  className="form-input"
                  value={toolName}
                  onChange={(e) => setToolName(e.target.value)}
                />
              </div>
              <div className="form-group">
                <label className="form-label">Tool description</label>
                <input
                  type="text"
                  className="form-input"
                  value={toolDescription}
                  onChange={(e) => setToolDescription(e.target.value)}
                />
              </div>
            </div>

            <div className="form-group">
              <label className="form-label">TypeScript handler</label>
              <textarea
                className="form-input mono-textarea"
                rows={4}
                value={handlerCode}
                onChange={(e) => setHandlerCode(e.target.value)}
              />
            </div>
          </div>

          <div className="settings-divider" />

          <div className="settings-section">
            <label className="radio-row">
              <input
                type="checkbox"
                checked={includeAgent}
                onChange={(e) => setIncludeAgent(e.target.checked)}
              />
              <span>Expose dedicated sidebar agent</span>
            </label>

            {includeAgent && (
              <div className="settings-section">
                <div className="form-group">
                  <label className="form-label">Agent name</label>
                  <input
                    type="text"
                    className="form-input"
                    value={agentName}
                    onChange={(e) => setAgentName(e.target.value)}
                  />
                </div>
                <div className="form-group">
                  <label className="form-label">System prompt</label>
                  <textarea
                    className="form-input"
                    rows={2}
                    value={systemPrompt}
                    onChange={(e) => setSystemPrompt(e.target.value)}
                  />
                </div>
              </div>
            )}
          </div>
        </div>

        <div className="modal-footer">
          <button type="button" className="btn btn-secondary" onClick={onClose}>
            Cancel
          </button>
          <button
            type="submit"
            className="btn btn-primary"
            disabled={isSubmitting || !name.trim()}
          >
            {isSubmitting ? 'Building…' : 'Build & register'}
          </button>
        </div>
      </form>
    </div>
  );
};
