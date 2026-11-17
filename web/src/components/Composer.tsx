import React, { useState, useRef, useEffect } from 'react';
import { ArrowUp, Square } from 'lucide-react';

interface ComposerProps {
  sessionId: string | null;
  provider: string;
  model: string;
  turnRunning: boolean;
  onSend: (text: string) => void;
  onInterrupt: () => void;
  onOpenModelPicker: () => void;
}

export const Composer: React.FC<ComposerProps> = React.memo(
  ({ sessionId, provider, model, turnRunning, onSend, onInterrupt, onOpenModelPicker }) => {
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

    return (
      <div className="chat-composer-wrap">
        <div className="chat-composer">
          <textarea
            ref={textareaRef}
            className="chat-textarea"
            placeholder={sessionId ? 'Message Luna…' : 'Select or start a conversation first'}
            disabled={!sessionId}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            rows={1}
          />
          <div className="chat-composer-footer">
            <button
              type="button"
              className="composer-model-btn"
              onClick={onOpenModelPicker}
              title="Switch provider or model"
            >
              <span>{provider || 'default'}</span>
              <span className="model-pill-sep">/</span>
              <span className="composer-model-name">{model || 'default'}</span>
            </button>

            {turnRunning ? (
              <button
                type="button"
                className="composer-stop-btn"
                onClick={onInterrupt}
                title="Stop generating"
                aria-label="Stop generating"
              >
                <Square size={14} fill="currentColor" />
              </button>
            ) : (
              <button
                type="button"
                className="composer-send-btn"
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
