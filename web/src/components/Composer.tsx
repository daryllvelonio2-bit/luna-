import React, { useState, useRef, useEffect, useMemo } from 'react';
import type {
  AgentMode,
  SkillSummary,
  WorkerProfile,
} from '../types';
import {
  ArrowUp,
  Square,
  Folder,
  ChevronDown,
  ImagePlus,
  X,
  Brain,
  CornerDownRight,
  ListPlus,
} from 'lucide-react';

export interface SlashItem {
  cmd: string;
  desc: string;
  category: 'command' | 'worker' | 'skill';
  hasArg?: boolean;
}

const BUILTIN_SLASH_COMMANDS: SlashItem[] = [
  { cmd: '/model', desc: 'Pick provider, account, and model', category: 'command' },
  { cmd: '/thinking', desc: 'Set thinking / reasoning effort', category: 'command', hasArg: true },
  { cmd: '/plan', desc: 'Toggle read-only plan mode', category: 'command' },
  { cmd: '/mode', desc: 'Switch build / auto / plan mode', category: 'command', hasArg: true },
  { cmd: '/compact', desc: 'Compact conversation history now', category: 'command' },
  { cmd: '/summarize', desc: 'Summarize and start a fresh chat', category: 'command' },
  { cmd: '/usage', desc: 'View session and global token/cost usage', category: 'command' },
  { cmd: '/todo', desc: 'Open session todo checklist', category: 'command' },
  { cmd: '/diff', desc: 'Review file changes per turn', category: 'command' },
  { cmd: '/rewind', desc: 'Restore workspace to a checkpoint', category: 'command' },
  { cmd: '/goal', desc: 'Start or stop autonomous goal loop', category: 'command', hasArg: true },
  { cmd: '/subagents', desc: 'Inspect delegated subagents & background jobs', category: 'command' },
  { cmd: '/remember', desc: 'Run memory distillation pass now', category: 'command' },
  { cmd: '/memory', desc: 'Set project memory scope or reset', category: 'command', hasArg: true },
  { cmd: '/dir', desc: 'Switch workspace directory', category: 'command' },
  { cmd: '/connect', desc: 'Manage provider accounts & API keys', category: 'command' },
  { cmd: '/worker', desc: 'Configure background workers', category: 'command' },
  { cmd: '/heartbeat', desc: 'View live system & tool health', category: 'command' },
  { cmd: '/settings', desc: 'Open settings', category: 'command' },
  { cmd: '/fork', desc: 'Fork this conversation', category: 'command' },
  { cmd: '/new', desc: 'Start a new conversation', category: 'command' },
  { cmd: '/name', desc: 'Rename this conversation', category: 'command', hasArg: true },
  { cmd: '/delete', desc: 'Delete this conversation', category: 'command' },
];

const EFFORTS: Array<{ value: string; label: string }> = [
  { value: 'off', label: 'Off' },
  { value: 'minimal', label: 'Minimal' },
  { value: 'low', label: 'Low' },
  { value: 'medium', label: 'Medium' },
  { value: 'high', label: 'High' },
  { value: 'xhigh', label: 'XHigh' },
  { value: 'max', label: 'Max' },
];

interface ComposerProps {
  sessionId: string | null;
  workspaceRoot: string;
  showWorkspacePicker: boolean;
  provider: string;
  model: string;
  accountLabel?: string | null;
  mode: AgentMode;
  reasoningEffort: string | null;
  contextTokens: number;
  contextWindow: number;
  queuedCount: number;
  turnRunning: boolean;
  workers: Record<string, WorkerProfile>;
  skills: SkillSummary[];
  onSend: (text: string, images?: string[], sendMode?: 'steer' | 'queue') => void;
  onSlashCommand: (raw: string, images?: string[]) => boolean;
  onInterrupt: () => void;
  onChangeMode: (mode: AgentMode) => void;
  onChangeReasoning: (effort: string | null) => void;
  onOpenModelPicker: () => void;
  onOpenWorkspacePicker: () => void;
  onOpenUsage: () => void;
}

function formatCompactTokens(n: number): string {
  if (n >= 1000) return `${(n / 1000).toFixed(n >= 10000 ? 0 : 1)}k`;
  return String(n);
}

export const Composer: React.FC<ComposerProps> = React.memo(
  ({
    sessionId,
    workspaceRoot,
    showWorkspacePicker,
    provider,
    model,
    accountLabel,
    mode,
    reasoningEffort,
    contextTokens,
    contextWindow,
    queuedCount,
    turnRunning,
    workers,
    skills,
    onSend,
    onSlashCommand,
    onInterrupt,
    onChangeMode,
    onChangeReasoning,
    onOpenModelPicker,
    onOpenWorkspacePicker,
    onOpenUsage,
  }) => {
    const [input, setInput] = useState('');
    const [images, setImages] = useState<string[]>([]);
    const [slashIndex, setSlashIndex] = useState(0);
    const [slashDismissed, setSlashDismissed] = useState(false);
    const textareaRef = useRef<HTMLTextAreaElement>(null);
    const fileInputRef = useRef<HTMLInputElement>(null);

    useEffect(() => {
      const el = textareaRef.current;
      if (!el) return;
      el.style.height = 'auto';
      el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
    }, [input]);

    const allSlashItems = useMemo(() => {
      const items: SlashItem[] = [...BUILTIN_SLASH_COMMANDS];
      Object.entries(workers || {}).forEach(([name, w]) => {
        items.push({
          cmd: `/${name}`,
          desc: `Worker · ${w.provider}/${w.model}`,
          category: 'worker',
          hasArg: true,
        });
      });
      (skills || []).forEach((s) => {
        items.push({
          cmd: `/${s.name}`,
          desc: s.description || `Skill (${s.source})`,
          category: 'skill',
          hasArg: true,
        });
      });
      return items;
    }, [workers, skills]);

    const slashQuery = useMemo(() => {
      if (slashDismissed || !input.startsWith('/') || input.includes('\n')) return null;
      const firstWord = input.split(/\s+/)[0];
      if (input.includes(' ')) return null;
      return firstWord.toLowerCase();
    }, [input, slashDismissed]);

    const slashMatches = useMemo(() => {
      if (slashQuery === null) return [];
      return allSlashItems
        .filter((item) => item.cmd.toLowerCase().startsWith(slashQuery))
        .slice(0, 10);
    }, [allSlashItems, slashQuery]);

    useEffect(() => {
      setSlashIndex(0);
    }, [slashQuery]);

    const attachFiles = (fileList: FileList | null) => {
      if (!fileList) return;
      Array.from(fileList).forEach((file) => {
        if (!file.type.startsWith('image/')) return;
        const reader = new FileReader();
        reader.onload = () => {
          const result = reader.result;
          if (typeof result === 'string' && result.startsWith('data:image/')) {
            setImages((prev) => [...prev, result].slice(0, 6));
          }
        };
        reader.readAsDataURL(file);
      });
    };

    const handlePaste = (e: React.ClipboardEvent<HTMLTextAreaElement>) => {
      const items = e.clipboardData?.items;
      if (!items) return;
      for (let i = 0; i < items.length; i++) {
        const item = items[i];
        if (item.type.startsWith('image/')) {
          const file = item.getAsFile();
          if (file) {
            e.preventDefault();
            const reader = new FileReader();
            reader.onload = () => {
              if (typeof reader.result === 'string') {
                setImages((prev) => [...prev, reader.result as string].slice(0, 6));
              }
            };
            reader.readAsDataURL(file);
          }
        }
      }
    };

    const submit = (sendMode?: 'steer' | 'queue') => {
      const text = input.trim();
      if ((!text && images.length === 0) || !sessionId) return;

      if (text.startsWith('/') && onSlashCommand(text, images)) {
        setInput('');
        setImages([]);
        setSlashDismissed(false);
        return;
      }

      const imgs = images.length > 0 ? [...images] : undefined;
      setInput('');
      setImages([]);
      setSlashDismissed(false);
      onSend(text, imgs, sendMode);
    };

    const applySlashPick = (item: SlashItem) => {
      if (item.hasArg) {
        setInput(`${item.cmd} `);
        setSlashDismissed(true);
        textareaRef.current?.focus();
      } else {
        setInput('');
        setSlashDismissed(false);
        onSlashCommand(item.cmd, images);
        setImages([]);
      }
    };

    const cycleMode = () => {
      const next: AgentMode =
        mode === 'build' ? 'auto' : mode === 'auto' ? 'plan' : 'build';
      onChangeMode(next);
    };

    const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
      if (e.key === 'Tab' && e.shiftKey) {
        e.preventDefault();
        cycleMode();
        return;
      }

      if (slashMatches.length > 0) {
        if (e.key === 'ArrowDown') {
          e.preventDefault();
          setSlashIndex((i) => (i + 1) % slashMatches.length);
          return;
        }
        if (e.key === 'ArrowUp') {
          e.preventDefault();
          setSlashIndex((i) => (i - 1 + slashMatches.length) % slashMatches.length);
          return;
        }
        if (e.key === 'Tab') {
          e.preventDefault();
          const chosen = slashMatches[slashIndex];
          if (chosen) applySlashPick(chosen);
          return;
        }
        if (e.key === 'Escape') {
          e.preventDefault();
          setSlashDismissed(true);
          return;
        }
        if (e.key === 'Enter' && !e.shiftKey) {
          e.preventDefault();
          const chosen = slashMatches[slashIndex];
          if (chosen) applySlashPick(chosen);
          return;
        }
      }

      if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault();
        if (turnRunning && (e.ctrlKey || e.metaKey)) {
          submit('queue');
        } else {
          submit(turnRunning ? 'steer' : undefined);
        }
      }
    };

    const workspaceFolder =
      workspaceRoot.split('/').filter(Boolean).pop() || workspaceRoot || 'Workspace';
    const pct =
      contextWindow > 0 ? Math.min(100, Math.round((contextTokens / contextWindow) * 100)) : 0;

    return (
      <div
        className="composer-region"
        onDragOver={(e) => e.preventDefault()}
        onDrop={(e) => {
          e.preventDefault();
          attachFiles(e.dataTransfer.files);
        }}
      >
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

        <div className="composer-anchor">
          {slashMatches.length > 0 && (
            <div className="slash-popover" role="listbox" aria-label="Slash commands">
              {slashMatches.map((item, idx) => (
                <button
                  key={item.cmd}
                  type="button"
                  className={`slash-row ${idx === slashIndex ? 'active' : ''}`}
                  onMouseEnter={() => setSlashIndex(idx)}
                  onClick={() => applySlashPick(item)}
                >
                  <span className="slash-row-cmd">{item.cmd}</span>
                  <span className="slash-row-desc">{item.desc}</span>
                  {item.category !== 'command' && (
                    <span className="slash-row-tag">{item.category}</span>
                  )}
                </button>
              ))}
            </div>
          )}

          <div className={`composer-shell ${mode === 'plan' ? 'composer-shell-plan' : ''}`}>
            {images.length > 0 && (
              <div className="composer-attachments">
                {images.map((src, idx) => (
                  <div key={idx} className="attachment-thumb">
                    <img src={src} alt={`Attachment ${idx + 1}`} />
                    <button
                      type="button"
                      className="attachment-remove"
                      onClick={() => setImages((prev) => prev.filter((_, i) => i !== idx))}
                      aria-label="Remove attachment"
                    >
                      <X size={11} />
                    </button>
                  </div>
                ))}
              </div>
            )}

            <textarea
              ref={textareaRef}
              className="composer-textarea"
              placeholder={
                !sessionId
                  ? 'Select or start a conversation first'
                  : turnRunning
                  ? 'Steer this turn, queue next message, or type /…'
                  : mode === 'plan'
                  ? 'Plan mode (read-only) — describe what to blueprint, or type /…'
                  : 'Message Luna, paste an image, or type / for commands…'
              }
              disabled={!sessionId}
              value={input}
              onChange={(e) => {
                setInput(e.target.value);
                setSlashDismissed(false);
              }}
              onKeyDown={handleKeyDown}
              onPaste={handlePaste}
              rows={1}
            />

            <div className="composer-footer">
              <div className="composer-meta">
                <button
                  type="button"
                  className={`composer-ghost-pill mode-pill-${mode}`}
                  onClick={cycleMode}
                  title="Click or press Shift+Tab to switch mode (Build / Auto / Plan)"
                >
                  <span className={`mode-dot mode-dot-${mode}`} />
                  <span>{mode === 'build' ? 'Build' : mode === 'auto' ? 'Auto' : 'Plan'}</span>
                </button>

                <label className="composer-ghost-select" title="Reasoning / thinking effort">
                  <Brain size={12} />
                  <select
                    value={reasoningEffort || 'off'}
                    onChange={(e) =>
                      onChangeReasoning(e.target.value === 'off' ? null : e.target.value)
                    }
                    disabled={!sessionId}
                    aria-label="Reasoning effort"
                  >
                    {EFFORTS.map((ef) => (
                      <option key={ef.value} value={ef.value}>
                        {ef.label}
                      </option>
                    ))}
                  </select>
                </label>

                <button
                  type="button"
                  className="composer-ghost-pill composer-model-pill"
                  onClick={onOpenModelPicker}
                  title="Choose provider, account, and model"
                >
                  <span className="composer-model-provider">{provider || 'default'}</span>
                  <span className="model-pill-sep">/</span>
                  <span className="composer-model-name">{model || 'default'}</span>
                  {accountLabel && (
                    <span className="composer-account-badge">@{accountLabel}</span>
                  )}
                </button>

                {contextWindow > 0 && contextTokens > 0 && (
                  <button
                    type="button"
                    className="composer-ghost-pill composer-ctx-pill tabular-nums"
                    onClick={onOpenUsage}
                    title={`Context: ${contextTokens.toLocaleString()} / ${contextWindow.toLocaleString()} tokens (${pct}%)`}
                  >
                    <span>
                      {formatCompactTokens(contextTokens)}/{formatCompactTokens(contextWindow)}
                    </span>
                    <span className={pct >= 75 ? 'text-warning' : 'text-muted'}>({pct}%)</span>
                  </button>
                )}

                {queuedCount > 0 && (
                  <span className="composer-queued-badge tabular-nums">
                    {queuedCount} queued
                  </span>
                )}
              </div>

              <div className="composer-actions">
                <input
                  ref={fileInputRef}
                  type="file"
                  accept="image/*"
                  multiple
                  hidden
                  onChange={(e) => {
                    attachFiles(e.target.files);
                    e.target.value = '';
                  }}
                />
                <button
                  type="button"
                  className="icon-btn"
                  disabled={!sessionId}
                  onClick={() => fileInputRef.current?.click()}
                  title="Attach image (or paste/drop)"
                  aria-label="Attach image"
                >
                  <ImagePlus size={15} />
                </button>

                {turnRunning ? (
                  <>
                    {(input.trim() || images.length > 0) && (
                      <>
                        <button
                          type="button"
                          className="composer-run-btn"
                          onClick={() => submit('queue')}
                          title="Queue for next turn (Ctrl+Enter)"
                        >
                          <ListPlus size={13} />
                          <span>Queue</span>
                        </button>
                        <button
                          type="button"
                          className="composer-run-btn composer-run-btn-accent"
                          onClick={() => submit('steer')}
                          title="Steer active turn (Enter)"
                        >
                          <CornerDownRight size={13} />
                          <span>Steer</span>
                        </button>
                      </>
                    )}
                    <button
                      type="button"
                      className="send-circle send-circle-stop"
                      onClick={onInterrupt}
                      title="Stop generating"
                      aria-label="Stop generating"
                    >
                      <Square size={13} fill="currentColor" />
                    </button>
                  </>
                ) : (
                  <button
                    type="button"
                    className="send-circle"
                    onClick={() => submit()}
                    disabled={!sessionId || (!input.trim() && images.length === 0)}
                    title="Send message"
                    aria-label="Send message"
                  >
                    <ArrowUp size={16} />
                  </button>
                )}
              </div>
            </div>
          </div>
        </div>
      </div>
    );
  }
);
