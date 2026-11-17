import React, { useState, useEffect, useRef, useMemo } from 'react';
import type {
  ApprovalRequestPayload,
  ClarifyRequestPayload,
  ToolCallItem,
  TranscriptMessageDto,
} from '../types';
import {
  ArrowUp,
  Square,
  ChevronDown,
  ChevronRight,
  ShieldAlert,
  HelpCircle,
  Terminal,
  FileText,
  Search,
  Wrench,
  Sparkles,
  Copy,
  Check,
  PanelLeft,
  Plus,
} from 'lucide-react';
import { MarkdownContent } from './MarkdownContent';

interface ChatViewProps {
  sessionId: string | null;
  sessionTitle: string;
  provider: string;
  model: string;
  messages: TranscriptMessageDto[];
  turnRunning: boolean;
  sidebarOpen: boolean;
  onToggleSidebar: () => void;
  onOpenSettings: () => void;
  onNewSession: () => void;
  onSendMessage: (text: string) => void;
  onInterrupt: () => void;
  pendingApproval: ApprovalRequestPayload | null;
  onResolveApproval: (
    decision: 'run' | 'deny' | 'run_with_args' | 'deny_with_feedback',
    args?: unknown,
    feedback?: string
  ) => void;
  pendingClarify: ClarifyRequestPayload | null;
  onResolveClarify: (answers: Array<string | string[]>) => void;
  onAskApprovalQuestion: (
    toolName: string,
    target: string | null,
    question: string
  ) => Promise<string>;
}

interface PairedToolStep {
  id: string;
  name: string;
  arguments: unknown;
  output?: string;
  hasOutput: boolean;
}

interface AssistantSegment {
  reasoning?: string;
  tools: PairedToolStep[];
  content?: string;
}

type RenderTurn =
  | { kind: 'user'; content: string; key: string }
  | { kind: 'assistant'; segments: AssistantSegment[]; orphanOutputs: string[]; key: string };

function summarizeToolArgs(name: string, args: unknown): string {
  if (!args || typeof args !== 'object') return '';
  const obj = args as Record<string, unknown>;
  if (typeof obj.command === 'string') return obj.command;
  if (typeof obj.path === 'string') return obj.path;
  if (typeof obj.pattern === 'string') return obj.pattern;
  if (typeof obj.query === 'string') return obj.query;
  if (typeof obj.url === 'string') return obj.url;
  if (typeof obj.goal === 'string') return obj.goal;
  if (name === 'list_files' && typeof obj.depth === 'number') return `depth ${obj.depth}`;
  const firstStr = Object.values(obj).find((v) => typeof v === 'string');
  return typeof firstStr === 'string' ? firstStr : '';
}

function getToolIcon(name: string) {
  const lower = name.toLowerCase();
  if (lower.includes('terminal') || lower.includes('shell') || lower.includes('exec')) {
    return <Terminal size={13} />;
  }
  if (lower.includes('read') || lower.includes('write') || lower.includes('patch') || lower.includes('file')) {
    return <FileText size={13} />;
  }
  if (lower.includes('search') || lower.includes('find') || lower.includes('list')) {
    return <Search size={13} />;
  }
  return <Wrench size={13} />;
}

function formatToolArgs(args: unknown): string {
  if (args === undefined || args === null) return '';
  if (typeof args === 'string') return args;
  try {
    return JSON.stringify(args, null, 2);
  } catch {
    return String(args);
  }
}

function buildRenderTurns(messages: TranscriptMessageDto[]): RenderTurn[] {
  const turns: RenderTurn[] = [];
  let currentAssistant: Extract<RenderTurn, { kind: 'assistant' }> | null = null;
  const toolStepById = new Map<string, PairedToolStep>();

  const flushAssistant = () => {
    if (currentAssistant) {
      turns.push(currentAssistant);
      currentAssistant = null;
    }
  };

  messages.forEach((m, idx) => {
    if (m.role === 'user') {
      flushAssistant();
      turns.push({
        kind: 'user',
        content: m.content || '',
        key: `u-${idx}`,
      });
      return;
    }

    if (m.role === 'tool') {
      const output = m.content || '';
      if (m.tool_call_id && toolStepById.has(m.tool_call_id)) {
        const step = toolStepById.get(m.tool_call_id)!;
        step.output = output;
        step.hasOutput = true;
      } else {
        if (!currentAssistant) {
          currentAssistant = {
            kind: 'assistant',
            segments: [],
            orphanOutputs: [],
            key: `a-${idx}`,
          };
        }
        currentAssistant.orphanOutputs.push(output);
      }
      return;
    }

    // Assistant or system summary message
    if (!currentAssistant) {
      currentAssistant = {
        kind: 'assistant',
        segments: [],
        orphanOutputs: [],
        key: `a-${idx}`,
      };
    }

    const pairedTools: PairedToolStep[] = (m.tool_calls || []).map((tc: ToolCallItem) => {
      const step: PairedToolStep = {
        id: tc.id || `tc-${idx}-${Math.random()}`,
        name: tc.name,
        arguments: tc.arguments,
        hasOutput: false,
      };
      if (tc.id) {
        toolStepById.set(tc.id, step);
      }
      return step;
    });

    const hasContent = Boolean(m.content && m.content.trim().length > 0);
    const hasReasoning = Boolean(m.reasoning && m.reasoning.trim().length > 0);

    if (hasReasoning || pairedTools.length > 0 || hasContent) {
      currentAssistant.segments.push({
        reasoning: m.reasoning || undefined,
        tools: pairedTools,
        content: m.content || undefined,
      });
    }
  });

  flushAssistant();
  return turns;
}

const STARTER_PROMPTS = [
  'Inspect this workspace and summarize its architecture',
  'Run git status and explain recent changes',
  'Check failing tests or compiler warnings',
];

export const ChatView: React.FC<ChatViewProps> = ({
  sessionId,
  sessionTitle,
  provider,
  model,
  messages,
  turnRunning,
  sidebarOpen,
  onToggleSidebar,
  onOpenSettings,
  onNewSession,
  onSendMessage,
  onInterrupt,
  pendingApproval,
  onResolveApproval,
  pendingClarify,
  onResolveClarify,
  onAskApprovalQuestion,
}) => {
  const [input, setInput] = useState('');
  const [expandedReasoning, setExpandedReasoning] = useState<Record<string, boolean>>({});
  const [expandedTools, setExpandedTools] = useState<Record<string, boolean>>({});
  const [copiedTurnKey, setCopiedTurnKey] = useState<string | null>(null);

  // Approval Q&A state
  const [approvalQuestion, setApprovalQuestion] = useState('');
  const [approvalReplies, setApprovalReplies] = useState<Array<{ q: string; a: string }>>([]);
  const [askingApproval, setAskingApproval] = useState(false);

  // Clarify state
  const [clarifyAnswers, setClarifyAnswers] = useState<Record<number, string>>({});
  const [customClarifyInput, setCustomClarifyInput] = useState('');

  const messagesEndRef = useRef<HTMLDivElement>(null);
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  const turns = useMemo(() => buildRenderTurns(messages), [messages]);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages.length, turnRunning]);

  useEffect(() => {
    const el = textareaRef.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = `${Math.min(el.scrollHeight, 200)}px`;
  }, [input]);

  const handleSend = (overrideText?: string) => {
    const text = (overrideText ?? input).trim();
    if (!text || !sessionId) return;
    onSendMessage(text);
    if (!overrideText) setInput('');
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const toggleReasoning = (key: string) => {
    setExpandedReasoning((prev) => ({ ...prev, [key]: !prev[key] }));
  };

  const toggleTool = (id: string) => {
    setExpandedTools((prev) => ({ ...prev, [id]: !prev[id] }));
  };

  const handleCopyAssistantTurn = (turn: Extract<RenderTurn, { kind: 'assistant' }>) => {
    const text = turn.segments
      .map((s) => s.content || '')
      .filter(Boolean)
      .join('\n\n');
    if (!text) return;
    navigator.clipboard.writeText(text).then(() => {
      setCopiedTurnKey(turn.key);
      setTimeout(() => setCopiedTurnKey(null), 1800);
    });
  };

  const handleAskApproval = async () => {
    if (!approvalQuestion.trim() || !pendingApproval) return;
    setAskingApproval(true);
    try {
      const q = approvalQuestion.trim();
      const a = await onAskApprovalQuestion(
        pendingApproval.tool_name,
        pendingApproval.target || null,
        q
      );
      setApprovalReplies((prev) => [...prev, { q, a }]);
      setApprovalQuestion('');
    } catch (e) {
      setApprovalReplies((prev) => [
        ...prev,
        { q: approvalQuestion.trim(), a: `Failed to query model: ${String(e)}` },
      ]);
    } finally {
      setAskingApproval(false);
    }
  };

  const handleClarifySubmit = () => {
    if (!pendingClarify) return;
    const questions =
      pendingClarify.questions && pendingClarify.questions.length > 0
        ? pendingClarify.questions
        : [{ question: pendingClarify.question, choices: pendingClarify.choices }];

    const answers = questions.map((_, idx) => {
      return clarifyAnswers[idx] || customClarifyInput || 'Skipped';
    });
    onResolveClarify(answers);
    setClarifyAnswers({});
    setCustomClarifyInput('');
  };

  const modelLabel = model || provider || 'default';

  return (
    <div className="chat-view">
      <header className="chat-header">
        <div className="chat-header-left">
          {!sidebarOpen && (
            <button
              type="button"
              className="icon-btn"
              onClick={onToggleSidebar}
              title="Open sidebar"
              aria-label="Open sidebar"
            >
              <PanelLeft size={16} />
            </button>
          )}
          <span className="chat-header-title" title={sessionTitle}>
            {sessionTitle || 'New conversation'}
          </span>
        </div>

        <div className="chat-header-right">
          {turnRunning && (
            <span className="status-chip status-chip-live">
              <span className="pulse-dot" />
              <span>Working</span>
            </span>
          )}
          <button
            type="button"
            className="model-pill"
            onClick={onOpenSettings}
            title="Configure provider & model"
          >
            <span className="model-pill-provider">{provider || 'default'}</span>
            <span className="model-pill-sep">/</span>
            <span className="model-pill-name">{modelLabel}</span>
          </button>
        </div>
      </header>

      <div className="messages-area">
        <div className="messages-column">
          {!sessionId ? (
            <div className="chat-empty">
              <div className="chat-empty-mark">
                <svg width="26" height="26" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                  <path
                    d="M20.5 14.5A8.5 8.5 0 0 1 9.5 3.5a8.5 8.5 0 1 0 11 11Z"
                    fill="currentColor"
                  />
                </svg>
              </div>
              <h1 className="chat-empty-title">Luna</h1>
              <p className="chat-empty-subtitle">
                Start a conversation to write code, inspect files, or run tasks.
              </p>
              <button type="button" className="btn btn-primary" onClick={onNewSession}>
                <Plus size={15} />
                <span>New chat</span>
              </button>
            </div>
          ) : turns.length === 0 ? (
            <div className="chat-empty">
              <div className="chat-empty-mark">
                <svg width="26" height="26" viewBox="0 0 24 24" fill="none" aria-hidden="true">
                  <path
                    d="M20.5 14.5A8.5 8.5 0 0 1 9.5 3.5a8.5 8.5 0 1 0 11 11Z"
                    fill="currentColor"
                  />
                </svg>
              </div>
              <h1 className="chat-empty-title">What can I help with?</h1>
              <p className="chat-empty-subtitle">
                Using {provider || 'default'} · {modelLabel}
              </p>
              <div className="starter-grid">
                {STARTER_PROMPTS.map((prompt) => (
                  <button
                    key={prompt}
                    type="button"
                    className="starter-chip"
                    onClick={() => handleSend(prompt)}
                  >
                    {prompt}
                  </button>
                ))}
              </div>
            </div>
          ) : (
            turns.map((turn, turnIdx) => {
              if (turn.kind === 'user') {
                return (
                  <div key={turn.key} className="turn-user">
                    <div className="user-bubble">{turn.content}</div>
                  </div>
                );
              }

              const isLastTurn = turnIdx === turns.length - 1;
              const hasProse = turn.segments.some((s) => Boolean(s.content && s.content.trim()));

              return (
                <div key={turn.key} className="turn-assistant">
                  {turn.segments.map((seg, segIdx) => {
                    const rKey = `${turn.key}-r-${segIdx}`;
                    const isReasoningOpen = Boolean(expandedReasoning[rKey]);

                    return (
                      <div key={segIdx} className="assistant-segment">
                        {seg.reasoning && (
                          <div className="thinking-block">
                            <button
                              type="button"
                              className="thinking-toggle"
                              onClick={() => toggleReasoning(rKey)}
                            >
                              <Sparkles size={13} className="thinking-icon" />
                              <span>Thought process</span>
                              {isReasoningOpen ? (
                                <ChevronDown size={13} />
                              ) : (
                                <ChevronRight size={13} />
                              )}
                            </button>
                            {isReasoningOpen && (
                              <div className="thinking-body">{seg.reasoning}</div>
                            )}
                          </div>
                        )}

                        {seg.tools.length > 0 && (
                          <div className="tool-steps">
                            {seg.tools.map((tc) => {
                              const open = Boolean(expandedTools[tc.id]);
                              const summary = summarizeToolArgs(tc.name, tc.arguments);
                              const isActiveRunning = !tc.hasOutput && isLastTurn && turnRunning;

                              return (
                                <div key={tc.id} className="tool-step">
                                  <button
                                    type="button"
                                    className="tool-step-row"
                                    onClick={() => toggleTool(tc.id)}
                                  >
                                    <span className="tool-step-left">
                                      <span className="tool-step-icon">{getToolIcon(tc.name)}</span>
                                      <span className="tool-step-name">{tc.name}</span>
                                      {summary && (
                                        <span className="tool-step-summary" title={summary}>
                                          {summary}
                                        </span>
                                      )}
                                    </span>
                                    <span className="tool-step-right">
                                      {isActiveRunning ? (
                                        <span className="tool-status tool-status-running">Running</span>
                                      ) : tc.hasOutput ? (
                                        <span className="tool-status">Done</span>
                                      ) : null}
                                      {open ? (
                                        <ChevronDown size={13} />
                                      ) : (
                                        <ChevronRight size={13} />
                                      )}
                                    </span>
                                  </button>

                                  {open && (
                                    <div className="tool-step-drawer">
                                      {tc.arguments !== undefined && (
                                        <div className="tool-drawer-section">
                                          <div className="tool-drawer-label">Input</div>
                                          <pre className="tool-drawer-pre">
                                            {formatToolArgs(tc.arguments)}
                                          </pre>
                                        </div>
                                      )}
                                      {tc.hasOutput && (
                                        <div className="tool-drawer-section">
                                          <div className="tool-drawer-label">Output</div>
                                          <pre className="tool-drawer-pre">
                                            {tc.output || '(empty output)'}
                                          </pre>
                                        </div>
                                      )}
                                    </div>
                                  )}
                                </div>
                              );
                            })}
                          </div>
                        )}

                        {seg.content && seg.content.trim().length > 0 && (
                          <div className="assistant-prose">
                            <MarkdownContent content={seg.content} />
                          </div>
                        )}
                      </div>
                    );
                  })}

                  {turn.orphanOutputs.length > 0 && (
                    <div className="tool-steps">
                      {turn.orphanOutputs.map((out, oi) => {
                        const oKey = `${turn.key}-orphan-${oi}`;
                        const open = Boolean(expandedTools[oKey]);
                        const preview = out.split('\n')[0]?.slice(0, 72) || 'Tool output';
                        return (
                          <div key={oKey} className="tool-step">
                            <button
                              type="button"
                              className="tool-step-row"
                              onClick={() => toggleTool(oKey)}
                            >
                              <span className="tool-step-left">
                                <span className="tool-step-icon">
                                  <Terminal size={13} />
                                </span>
                                <span className="tool-step-name">output</span>
                                <span className="tool-step-summary">{preview}</span>
                              </span>
                              <span className="tool-step-right">
                                {open ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
                              </span>
                            </button>
                            {open && (
                              <div className="tool-step-drawer">
                                <pre className="tool-drawer-pre">{out}</pre>
                              </div>
                            )}
                          </div>
                        );
                      })}
                    </div>
                  )}

                  {hasProse && !(isLastTurn && turnRunning) && (
                    <div className="turn-actions">
                      <button
                        type="button"
                        className="turn-action-btn"
                        onClick={() => handleCopyAssistantTurn(turn)}
                        title="Copy response"
                      >
                        {copiedTurnKey === turn.key ? (
                          <>
                            <Check size={13} />
                            <span>Copied</span>
                          </>
                        ) : (
                          <>
                            <Copy size={13} />
                            <span>Copy</span>
                          </>
                        )}
                      </button>
                    </div>
                  )}
                </div>
              );
            })
          )}

          {turnRunning &&
            (turns.length === 0 || turns[turns.length - 1].kind === 'user') && (
              <div className="turn-assistant">
                <div className="thinking-indicator">
                  <span className="pulse-dot" />
                  <span>Luna is thinking…</span>
                </div>
              </div>
            )}

          <div ref={messagesEndRef} />
        </div>
      </div>

      {pendingApproval && (
        <div className="modal-backdrop">
          <div className="modal-card" role="dialog" aria-modal="true" aria-label="Tool approval">
            <div className="modal-header">
              <div className="modal-title-group">
                <ShieldAlert size={17} className="text-warning" />
                <span className="modal-title">Approve tool execution</span>
              </div>
              <span className="pill">{pendingApproval.tool_name}</span>
            </div>

            <div className="modal-body">
              {pendingApproval.target && (
                <div className="form-group">
                  <span className="form-label">Target</span>
                  <div className="mono-inline-box">{pendingApproval.target}</div>
                </div>
              )}

              {pendingApproval.diff ? (
                <div className="form-group">
                  <span className="form-label">Proposed diff</span>
                  <div className="diff-view">{pendingApproval.diff}</div>
                </div>
              ) : (
                <div className="form-group">
                  <span className="form-label">Arguments</span>
                  <div className="diff-view">
                    {JSON.stringify(pendingApproval.arguments, null, 2)}
                  </div>
                </div>
              )}

              {approvalReplies.length > 0 && (
                <div className="form-group">
                  <span className="form-label">Model Q&A</span>
                  <div className="qa-list">
                    {approvalReplies.map((r, i) => (
                      <div key={i} className="qa-item">
                        <div className="qa-question">Q: {r.q}</div>
                        <div className="qa-answer">{r.a}</div>
                      </div>
                    ))}
                  </div>
                </div>
              )}

              <div className="form-group">
                <span className="form-label">Ask about this change before approving</span>
                <div className="inline-row">
                  <input
                    type="text"
                    className="form-input flex-1"
                    placeholder="Ask a question or request a narrower edit…"
                    value={approvalQuestion}
                    onChange={(e) => setApprovalQuestion(e.target.value)}
                    onKeyDown={(e) => e.key === 'Enter' && handleAskApproval()}
                  />
                  <button
                    type="button"
                    className="btn btn-secondary"
                    onClick={handleAskApproval}
                    disabled={askingApproval || !approvalQuestion.trim()}
                  >
                    {askingApproval ? 'Asking…' : 'Ask'}
                  </button>
                </div>
              </div>
            </div>

            <div className="modal-footer">
              <button
                type="button"
                className="btn btn-danger"
                onClick={() => onResolveApproval('deny')}
              >
                Deny
              </button>
              {approvalReplies.length > 0 && (
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={() =>
                    onResolveApproval(
                      'deny_with_feedback',
                      undefined,
                      approvalReplies[approvalReplies.length - 1].q
                    )
                  }
                >
                  Deny with feedback
                </button>
              )}
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => onResolveApproval('run')}
              >
                Approve once
              </button>
            </div>
          </div>
        </div>
      )}

      {pendingClarify && (
        <div className="modal-backdrop">
          <div className="modal-card" role="dialog" aria-modal="true" aria-label="Clarification">
            <div className="modal-header">
              <div className="modal-title-group">
                <HelpCircle size={17} className="text-accent" />
                <span className="modal-title">Clarification needed</span>
              </div>
            </div>

            <div className="modal-body">
              {(pendingClarify.questions && pendingClarify.questions.length > 0
                ? pendingClarify.questions
                : [{ question: pendingClarify.question, choices: pendingClarify.choices }]
              ).map((q, idx) => (
                <div key={idx} className="form-group">
                  <span className="clarify-question">{q.question}</span>
                  <div className="radio-stack">
                    {q.choices?.map((choice) => (
                      <label key={choice} className="radio-row">
                        <input
                          type="radio"
                          name={`choice-${idx}`}
                          checked={clarifyAnswers[idx] === choice}
                          onChange={() =>
                            setClarifyAnswers((prev) => ({ ...prev, [idx]: choice }))
                          }
                        />
                        <span>{choice}</span>
                      </label>
                    ))}
                  </div>
                </div>
              ))}

              <div className="form-group">
                <span className="form-label">Or enter a custom answer</span>
                <input
                  type="text"
                  className="form-input"
                  placeholder="Custom response…"
                  value={customClarifyInput}
                  onChange={(e) => setCustomClarifyInput(e.target.value)}
                  onKeyDown={(e) => e.key === 'Enter' && handleClarifySubmit()}
                />
              </div>
            </div>

            <div className="modal-footer">
              <button type="button" className="btn btn-primary" onClick={handleClarifySubmit}>
                Submit
              </button>
            </div>
          </div>
        </div>
      )}

      <div className="composer-region">
        <div className="composer-shell">
          <textarea
            ref={textareaRef}
            className="composer-textarea"
            placeholder={sessionId ? 'Message Luna…' : 'Create or select a conversation to start'}
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
                onClick={onOpenSettings}
                title="Change model or provider"
              >
                {modelLabel}
              </button>
              <span className="composer-hint">Enter to send · Shift+Enter for newline</span>
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
                onClick={() => handleSend()}
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
    </div>
  );
};
