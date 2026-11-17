import React, { useState } from 'react';
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
  const [author, setAuthor] = useState('User');
  const [version, setVersion] = useState('0.1.0');

  // Tool fields
  const [toolName, setToolName] = useState('my_custom_tool');
  const [toolDescription, setToolDescription] = useState('Executes custom TypeScript logic');
  const [handlerCode, setHandlerCode] = useState(
    `export default async function handle(args) {\n  return "Processed with " + JSON.stringify(args);\n}`
  );

  // Agent fields
  const [includeAgent, setIncludeAgent] = useState(true);
  const [agentName, setAgentName] = useState('Specialist Agent');
  const [systemPrompt, setSystemPrompt] = useState('You are an expert specialist plugin agent.');

  const [isSubmitting, setIsSubmitting] = useState(false);

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
    try {
      await onSubmit(spec);
      onClose();
    } catch (err) {
      alert(`Failed to build plugin: ${String(err)}`);
    } finally {
      setIsSubmitting(false);
    }
  };

  return (
    <div className="modal-backdrop">
      <form className="modal-card" style={{ width: '700px' }} onSubmit={handleSubmit}>
        <div className="modal-header">
          <div style={{ display: 'flex', alignItems: 'center', gap: '8px' }}>
            <Hammer size={18} color="var(--accent-color)" />
            <span style={{ fontWeight: 600 }}>Interactive Plugin Builder</span>
          </div>
          <button type="button" className="action-btn" onClick={onClose}>
            <X size={18} />
          </button>
        </div>

        <div className="modal-body">
          <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '12px' }}>
            <div className="form-group">
              <label className="form-label">Plugin Name (identifier)</label>
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
              placeholder="e.g. Analyzes CSV metrics and formats reporting tables"
              value={description}
              onChange={(e) => setDescription(e.target.value)}
            />
          </div>

          <div style={{ borderTop: '1px solid var(--border-color)', paddingTop: '12px', marginTop: '4px' }}>
            <h4 style={{ fontSize: '13px', fontWeight: 600, marginBottom: '8px' }}>Custom Tool</h4>
            <div style={{ display: 'grid', gridTemplateColumns: '1fr 2fr', gap: '12px', marginBottom: '8px' }}>
              <div className="form-group">
                <label className="form-label">Tool Name</label>
                <input
                  type="text"
                  className="form-input"
                  value={toolName}
                  onChange={(e) => setToolName(e.target.value)}
                />
              </div>
              <div className="form-group">
                <label className="form-label">Tool Description</label>
                <input
                  type="text"
                  className="form-input"
                  value={toolDescription}
                  onChange={(e) => setToolDescription(e.target.value)}
                />
              </div>
            </div>

            <div className="form-group">
              <label className="form-label">TypeScript Execution Handler</label>
              <textarea
                className="form-input"
                style={{ fontFamily: 'monospace', fontSize: '12px' }}
                rows={4}
                value={handlerCode}
                onChange={(e) => setHandlerCode(e.target.value)}
              />
            </div>
          </div>

          <div style={{ borderTop: '1px solid var(--border-color)', paddingTop: '12px', marginTop: '4px' }}>
            <label style={{ display: 'flex', alignItems: 'center', gap: '8px', cursor: 'pointer', marginBottom: '10px' }}>
              <input
                type="checkbox"
                checked={includeAgent}
                onChange={(e) => setIncludeAgent(e.target.checked)}
              />
              <span style={{ fontWeight: 600, fontSize: '13px' }}>Expose Dedicated Sidebar Agent</span>
            </label>

            {includeAgent && (
              <div style={{ display: 'flex', flexDirection: 'column', gap: '10px' }}>
                <div className="form-group">
                  <label className="form-label">Agent Display Name</label>
                  <input
                    type="text"
                    className="form-input"
                    value={agentName}
                    onChange={(e) => setAgentName(e.target.value)}
                  />
                </div>
                <div className="form-group">
                  <label className="form-label">System Prompt Persona</label>
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
          <button type="button" className="secondary-btn" onClick={onClose}>
            Cancel
          </button>
          <button type="submit" className="primary-btn" disabled={isSubmitting || !name.trim()}>
            {isSubmitting ? 'Compiling & Bundling...' : 'Build & Register Plugin'}
          </button>
        </div>
      </form>
    </div>
  );
};
