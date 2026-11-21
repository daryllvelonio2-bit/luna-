import React, { useState, useEffect, useRef } from 'react';
import { Sparkles, X, ArrowRight, Globe, Folder } from 'lucide-react';

interface PluginBuilderModalProps {
  isOpen: boolean;
  onClose: () => void;
  onStartConversation: (
    prompt: string,
    options: { scope: 'global' | 'workspace'; autoSend: boolean }
  ) => Promise<void>;
}

const PLUGIN_STARTERS = [
  {
    label: 'API / Webhook connector',
    prompt:
      'Create a plugin that wraps a REST API (let me specify the URL and auth) with tools to query and post data, plus a sidebar agent.',
  },
  {
    label: 'Git & repo workflow helper',
    prompt:
      'Create a workspace plugin with tools to inspect recent commits, list stale branches, and summarize working tree status.',
  },
  {
    label: 'Persistent KV tracker',
    prompt:
      'Create a plugin using ctx.storage to save, search, and manage structured items/bookmarks across sessions with a summary UI panel.',
  },
  {
    label: 'Scheduled digest plugin',
    prompt:
      'Create a plugin with a custom tool and a recurring daily schedule that gathers updates and saves a concise report.',
  },
];

export const PluginBuilderModal: React.FC<PluginBuilderModalProps> = ({
  isOpen,
  onClose,
  onStartConversation,
}) => {
  const [prompt, setPrompt] = useState('');
  const [scope, setScope] = useState<'global' | 'workspace'>('global');
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    if (!isOpen) return;
    setErrorMsg(null);
    const focusTimer = window.setTimeout(() => {
      textareaRef.current?.focus();
    }, 30);
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => {
      window.clearTimeout(focusTimer);
      window.removeEventListener('keydown', onKey);
    };
  }, [isOpen, onClose]);

  if (!isOpen) return null;

  const submitPrompt = async (autoSend: boolean) => {
    const trimmed = prompt.trim();
    if (!trimmed && autoSend) return;
    setErrorMsg(null);
    setIsSubmitting(true);
    try {
      await onStartConversation(trimmed, { scope, autoSend });
      setPrompt('');
      onClose();
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setIsSubmitting(false);
    }
  };

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    void submitPrompt(true);
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      if (prompt.trim() && !isSubmitting) {
        void submitPrompt(true);
      }
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <form
        className="modal-dialog modal-lg"
        onSubmit={handleSubmit}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-header">
          <div className="modal-title-group">
            <Sparkles size={17} />
            <span className="modal-title">Create plugin with Luna</span>
          </div>
          <button
            type="button"
            className="icon-btn"
            onClick={onClose}
            aria-label="Close"
          >
            <X size={16} />
          </button>
        </div>

        <div className="modal-body">
          {errorMsg && <div className="error-banner">{errorMsg}</div>}

          <p className="text-muted" style={{ fontSize: '13px', margin: 0, lineHeight: 1.5 }}>
            Describe what you want your plugin to do in plain language. Luna will load the
            built-in plugin architecture skill, ask any clarifying questions if needed, and
            write, compile, and register the TypeScript plugin automatically.
          </p>

          <div className="form-group">
            <label className="form-label" htmlFor="plugin-prompt-input">
              What plugin should Luna create?
            </label>
            <textarea
              id="plugin-prompt-input"
              ref={textareaRef}
              className="form-input"
              rows={5}
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder="Example: Create a GitHub PR helper plugin with tools to list open pull requests, fetch diffs for a PR number, and register a Code Reviewer sidebar agent."
            />
          </div>

          <div className="form-group">
            <span className="form-label">Install scope</span>
            <div className="auto-cadence-tabs" style={{ marginBottom: 0 }}>
              <button
                type="button"
                className={`auto-starter-chip ${scope === 'global' ? 'is-selected' : ''}`}
                style={
                  scope === 'global'
                    ? {
                        borderColor: 'var(--accent)',
                        backgroundColor: 'var(--accent-soft)',
                        color: 'var(--text-primary)',
                      }
                    : undefined
                }
                onClick={() => setScope('global')}
              >
                <Globe size={12} />
                <span>Global (~/.luna/plugins)</span>
              </button>
              <button
                type="button"
                className={`auto-starter-chip ${scope === 'workspace' ? 'is-selected' : ''}`}
                style={
                  scope === 'workspace'
                    ? {
                        borderColor: 'var(--accent)',
                        backgroundColor: 'var(--accent-soft)',
                        color: 'var(--text-primary)',
                      }
                    : undefined
                }
                onClick={() => setScope('workspace')}
              >
                <Folder size={12} />
                <span>Workspace (.luna/plugins)</span>
              </button>
            </div>
          </div>

          <div className="auto-starters">
            <div className="auto-starters-label">Starter ideas</div>
            <div className="auto-starter-chips">
              {PLUGIN_STARTERS.map((starter) => (
                <button
                  key={starter.label}
                  type="button"
                  className="auto-starter-chip"
                  onClick={() => {
                    setPrompt(starter.prompt);
                    textareaRef.current?.focus();
                  }}
                >
                  <span>{starter.label}</span>
                  <ArrowRight size={11} />
                </button>
              ))}
            </div>
          </div>
        </div>

        <div className="modal-footer">
          <button type="button" className="btn btn-secondary" onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            className="btn btn-secondary"
            disabled={isSubmitting}
            onClick={() => void submitPrompt(false)}
          >
            Prefill in chat
          </button>
          <button
            type="submit"
            className="btn btn-primary"
            disabled={isSubmitting || !prompt.trim()}
          >
            <Sparkles size={14} />
            <span>{isSubmitting ? 'Starting…' : 'Create with Luna'}</span>
          </button>
        </div>
      </form>
    </div>
  );
};
