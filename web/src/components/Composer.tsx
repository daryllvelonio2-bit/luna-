import React, { useState, useRef, useEffect } from 'react';
import { ArrowUp, Square, Folder, ChevronDown } from 'lucide-react';

interface ComposerProps {
  sessionId: string | null;
  workspaceRoot: string;
  showWorkspacePicker: boolean;
  provider: string;
  model: string;
  turnRunning: boolean;
  onSend: (text: string) => void;
  onInterrupt: () => void;
  onOpenModelPicker: () => void;
  onOpenWorkspacePicker: () => void;
}

export const Composer: React.FC<ComposerProps> = React.memo(
  ({
    sessionId,
    workspaceRoot,
    showWorkspacePicker,
    provider,
    model,
    turnRunning,
    onSend,
    onInterrupt,
    onOpenModelPicker,
    onOpenWorkspacePicker,
  }) => {
    const [input, setInput] = useState('');
    const textareaRef = useRef<HTMLTextAreaElement>(null);

    useEffect(() => {
      const el = textareaRef.current;
      if (!el) return;
      el.style.height = 'auto';
      el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
    }, [input]);

    const submit = () => {
      const text = input.trim();
      if (!text || !sessionId) return;
      setInput('');
      onSend(text);
    };

    const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault();
        submit();
      }
    };

    const workspaceFolder =
      workspaceRoot.split('/').filter(Boolean).pop() || workspaceRoot || 'Workspace';

    return (
      <div className="composer-region">
        {sessionId && showWorkspacePicker && (
          <div className="composer-workspace-row">
            <button
              type="button"
              className="composer-workspace-btn"
              onClick={onOpenWorkspacePicker}
              title={`Workspace: ${workspaceRoot}`}
            >
              <Folder size={13} className="composer-workspace-icon" />
              <span className="composer-workspace-name">{workspaceFolder}</span>
              <span className="composer-workspace-path">{workspaceRoot}</span>
              <ChevronDown size={12} className="composer-workspace-chevron" />
            </button>
          </div>
        )}

        <div className="composer-shell">
          <textarea
            ref={textareaRef}
            className="composer-textarea"
            placeholder={sessionId ? 'Message Luna…' : 'Select or start a conversation first'}
            disabled={!sessionId}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            rows={1}
          />
          <div className="composer-footer">
            <div className="composer-meta">
              <button
                type="button"
                className="composer-model-btn"
                onClick={onOpenModelPicker}
                title="Switch provider or model"
              >
                <span className="composer-model-provider">{provider || 'default'}</span>
                <span className="model-pill-sep">/</span>
                <span className="composer-model-name">{model || 'default'}</span>
              </button>
            </div>

            {turnRunning ? (
              <button
                type="button"
                className="send-circle send-circle-stop"
                onClick={onInterrupt}
                title="Stop generating"
                aria-label="Stop generating"
              >
                <Square size={13} fill="currentColor" />
              </button>
            ) : (
              <button
                type="button"
                className="send-circle"
                onClick={submit}
                disabled={!sessionId || !input.trim()}
                title="Send message"
                aria-label="Send message"
              >
                <ArrowUp size={16} />
              </button>
            )}
          </div>
        </div>
      </div>
    );
  }
);
