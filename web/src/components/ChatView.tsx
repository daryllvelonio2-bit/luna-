import React, { useState, useEffect, useRef, useMemo, useCallback } from 'react';
import type {
  ApprovalRequestPayload,
  ClarifyRequestPayload,
  ToolCallItem,
  TranscriptMessageDto,
} from '../types';
import {
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
import { Composer } from './Composer';

interface ChatViewProps {
  sessionId: string | null;
  sessionTitle: string;
  workspaceRoot: string;
  provider: string;
  model: string;
  messages: TranscriptMessageDto[];
  turnRunning: boolean;
  sidebarOpen: boolean;
  onToggleSidebar: () => void;
  onOpenModelPicker: () => void;
  onOpenWorkspacePicker: () => void;
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

type ActivityEntry =
  | { kind: 'reasoning'; key: string; text: string }
  | { kind: 'tool'; step: PairedToolStep }
  | { kind: 'orphan'; key: string; output: string }
  | { kind: 'note'; key: string; text: string };

type RenderTurn =
  | { kind: 'user'; content: string; key: string }
  | {
      kind: 'assistant';
      activities: ActivityEntry[];
      finalContents: string[];
      reasoningCount: number;
      toolCount: number;
      key: string;
    };

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
    if (!currentAssistant) return;
    turns.push(currentAssistant);
    currentAssistant = null;
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
            activities: [],
            finalContents: [],
            reasoningCount: 0,
            toolCount: 0,
            key: `a-${idx}`,
          };
        }
        currentAssistant.activities.push({
          kind: 'orphan',
          key: `orphan-${idx}`,
          output,
        });
        currentAssistant.toolCount += 1;
      }
      return;
    }

    if (!currentAssistant) {
      currentAssistant = {
        kind: 'assistant',
        activities: [],
        finalContents: [],
        reasoningCount: 0,
        toolCount: 0,
        key: `a-${idx}`,
      };
    }

    const hasReasoning = Boolean(m.reasoning && m.reasoning.trim().length > 0);
    if (hasReasoning && m.reasoning) {
      currentAssistant.activities.push({
        kind: 'reasoning',
        key: `r-${idx}`,
        text: m.reasoning,
      });
      currentAssistant.reasoningCount += 1;
    }

    const rawTools = m.tool_calls || [];
    rawTools.forEach((tc: ToolCallItem, tIdx: number) => {
      const step: PairedToolStep = {
        id: tc.id || `tc-${idx}-${tIdx}`,
        name: tc.name,
        arguments: tc.arguments,
        hasOutput: false,
      };
      if (tc.id) {
        toolStepById.set(tc.id, step);
      }
      currentAssistant!.activities.push({ kind: 'tool', step });
      currentAssistant!.toolCount += 1;
    });

    const hasContent = Boolean(m.content && m.content.trim().length > 0);
    if (hasContent && m.content) {
      if (rawTools.length > 0) {
        currentAssistant.activities.push({
          kind: 'note',
          key: `note-${idx}`,
          text: m.content,
        });
      } else {
        currentAssistant.finalContents.push(m.content);
      }
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

function formatActivityLabel(
  turn: Extract<RenderTurn, { kind: 'assistant' }>,
  isLive: boolean
): string {
  const { reasoningCount, toolCount } = turn;
  if (isLive) {
    if (toolCount > 0) {
      return `Working · ${toolCount} ${toolCount === 1 ? 'tool call' : 'tool calls'}`;
    }
    if (reasoningCount > 0) {
      return 'Thinking…';
    }
    return 'Working…';
  }
  if (reasoningCount > 0 && toolCount > 0) {
    return `Thought and used ${toolCount} ${toolCount === 1 ? 'tool' : 'tools'}`;
  }
  if (toolCount > 0) {
    return `Used ${toolCount} ${toolCount === 1 ? 'tool' : 'tools'}`;
  }
  return 'Thought process';
}

interface AssistantTurnRowProps {
  turn: Extract<RenderTurn, { kind: 'assistant' }>;
  isLive: boolean;
  isActivityOpen: boolean;
  expandedReasoning: Record<string, boolean>;
  expandedTools: Record<string, boolean>;
  copied: boolean;
  onToggleActivity: (turnKey: string, isLive: boolean) => void;
  onToggleReasoning: (key: string) => void;
  onToggleTool: (id: string) => void;
  onCopy: (turn: Extract<RenderTurn, { kind: 'assistant' }>) => void;
}

const AssistantTurnRow: React.FC<AssistantTurnRowProps> = React.memo(
  ({
    turn,
    isLive,
    isActivityOpen,
    expandedReasoning,
    expandedTools,
    copied,
    onToggleActivity,
    onToggleReasoning,
    onToggleTool,
    onCopy,
  }) => {
    const hasActivity = turn.activities.length > 0;
    const hasProse = turn.finalContents.length > 0;

    return (
      <div className="turn-assistant">
        {hasActivity && (
          <div className="activity-group">
            <button
              type="button"
              className="activity-summary-btn"
              onClick={() => onToggleActivity(turn.key, isLive)}
            >
              {isLive ? (
                <span className="pulse-dot" />
              ) : (
                <Sparkles size={13} className="thinking-icon" />
              )}
              <span>{formatActivityLabel(turn, isLive)}</span>
              {isActivityOpen ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
            </button>

            {isActivityOpen && (
              <div className="activity-trace">
                {turn.activities.map((entry, entryIdx) => {
                  if (entry.kind === 'reasoning') {
                    const rKey = `${turn.key}-${entry.key}`;
                    const isLastEntry = entryIdx === turn.activities.length - 1;
                    const rOpen =
                      expandedReasoning[rKey] !== undefined
                        ? expandedReasoning[rKey]
                        : isLive && isLastEntry;

                    return (
                      <div key={rKey} className="thinking-block">
                        <button
                          type="button"
                          className="thinking-toggle"
                          onClick={() => onToggleReasoning(rKey)}
                        >
                          <Sparkles size={12} className="thinking-icon" />
                          <span>Thought process</span>
                          {rOpen ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                        </button>
                        {rOpen && <div className="thinking-body">{entry.text}</div>}
                      </div>
                    );
                  }

                  if (entry.kind === 'note') {
                    return (
                      <div key={`${turn.key}-${entry.key}`} className="activity-note">
                        {entry.text}
                      </div>
                    );
                  }

                  if (entry.kind === 'orphan') {
                    const oKey = `${turn.key}-${entry.key}`;
                    const open = Boolean(expandedTools[oKey]);
                    const preview = entry.output.split('\n')[0]?.slice(0, 72) || 'Tool output';
                    return (
                      <div key={oKey} className="tool-step">
                        <button
                          type="button"
                          className="tool-step-row"
                          onClick={() => onToggleTool(oKey)}
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
                            <pre className="tool-drawer-pre">{entry.output}</pre>
                          </div>
                        )}
                      </div>
                    );
                  }

                  const tc = entry.step;
                  const open = Boolean(expandedTools[tc.id]);
                  const summary = summarizeToolArgs(tc.name, tc.arguments);
                  const isActiveRunning = !tc.hasOutput && isLive;

                  return (
                    <div key={tc.id} className="tool-step">
                      <button
                        type="button"
                        className="tool-step-row"
                        onClick={() => onToggleTool(tc.id)}
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
                          {open ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
                        </span>
                      </button>

                      {open && (
                        <div className="tool-step-drawer">
                          {tc.arguments !== undefined && (
                            <div className="tool-drawer-section">
                              <div className="tool-drawer-label">Input</div>
                              <pre className="tool-drawer-pre">{formatToolArgs(tc.arguments)}</pre>
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
          </div>
        )}

        {turn.finalContents.map((prose, pIdx) => (
          <div key={`${turn.key}-p-${pIdx}`} className="assistant-prose">
            <MarkdownContent content={prose} />
          </div>
        ))}

        {hasProse && !isLive && (
          <div className="turn-actions">
            <button
              type="button"
              className="turn-action-btn"
              onClick={() => onCopy(turn)}
              title="Copy response"
            >
              {copied ? (
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
  }
);

const INITIAL_VISIBLE_TURNS = 35;

export const ChatView: React.FC<ChatViewProps> = ({
  sessionId,
  sessionTitle,
  workspaceRoot,
  provider,
  model,
  messages,
  turnRunning,
  sidebarOpen,
  onToggleSidebar,
  onOpenModelPicker,
  onOpenWorkspacePicker,
  onNewSession,
  onSendMessage,
  onInterrupt,
  pendingApproval,
  onResolveApproval,
  pendingClarify,
  onResolveClarify,
  onAskApprovalQuestion,
}) => {
  const [expandedActivity, setExpandedActivity] = useState<Record<string, boolean>>({});
  const [collapsedLive, setCollapsedLive] = useState<Record<string, boolean>>({});
  const [expandedReasoning, setExpandedReasoning] = useState<Record<string, boolean>>({});
  const [expandedTools, setExpandedTools] = useState<Record<string, boolean>>({});
  const [copiedTurnKey, setCopiedTurnKey] = useState<string | null>(null);
  const [visibleTurnLimit, setVisibleTurnLimit] = useState(INITIAL_VISIBLE_TURNS);

  // Approval Q&A state
  const [approvalQuestion, setApprovalQuestion] = useState('');
  const [approvalReplies, setApprovalReplies] = useState<Array<{ q: string; a: string }>>([]);
  const [askingApproval, setAskingApproval] = useState(false);

  // Clarify state
  const [clarifyAnswers, setClarifyAnswers] = useState<Record<number, string>>({});
  const [customClarifyInput, setCustomClarifyInput] = useState('');

  const messagesAreaRef = useRef<HTMLDivElement>(null);
  const prevTurnRunningRef = useRef(turnRunning);

  const turns = useMemo(() => buildRenderTurns(messages), [messages]);
  const hiddenTurnCount = Math.max(0, turns.length - visibleTurnLimit);
  const visibleTurns = useMemo(
    () => (hiddenTurnCount > 0 ? turns.slice(hiddenTurnCount) : turns),
    [turns, hiddenTurnCount]
  );

  // Auto-collapse all activity groups, reasoning blocks, and tool drawers when a turn finishes
  useEffect(() => {
    if (prevTurnRunningRef.current && !turnRunning) {
      setExpandedActivity({});
      setCollapsedLive({});
      setExpandedReasoning({});
      setExpandedTools({});
    }
    prevTurnRunningRef.current = turnRunning;
  }, [turnRunning]);

  // Reset expansion state and windowing when switching conversations
  useEffect(() => {
    setExpandedActivity({});
    setCollapsedLive({});
    setExpandedReasoning({});
    setExpandedTools({});
    setVisibleTurnLimit(INITIAL_VISIBLE_TURNS);
  }, [sessionId]);

  // Keep viewport anchored to bottom during live turns without laggy smooth-scroll fighting tokens
  useEffect(() => {
    const el = messagesAreaRef.current;
    if (!el) return;
    if (turnRunning) {
      const distanceFromBottom = el.scrollHeight - el.scrollTop - el.clientHeight;
      if (distanceFromBottom < 260) {
        el.scrollTop = el.scrollHeight;
      }
    } else if (messages.length > 0) {
      el.scrollTop = el.scrollHeight;
    }
  }, [messages, turnRunning, sessionId]);

  const toggleActivity = useCallback((turnKey: string, isLive: boolean) => {
    if (isLive) {
      setCollapsedLive((prev) => ({ ...prev, [turnKey]: !prev[turnKey] }));
    } else {
      setExpandedActivity((prev) => ({ ...prev, [turnKey]: !prev[turnKey] }));
    }
  }, []);

  const toggleReasoning = useCallback((key: string) => {
    setExpandedReasoning((prev) => ({ ...prev, [key]: !prev[key] }));
  }, []);

  const toggleTool = useCallback((id: string) => {
    setExpandedTools((prev) => ({ ...prev, [id]: !prev[id] }));
  }, []);

  const handleCopyAssistantTurn = useCallback(
    (turn: Extract<RenderTurn, { kind: 'assistant' }>) => {
      const text = turn.finalContents.join('\n\n').trim();
      if (!text) return;
      navigator.clipboard.writeText(text).then(() => {
        setCopiedTurnKey(turn.key);
        setTimeout(() => setCopiedTurnKey(null), 1800);
      });
    },
    []
  );

  const approvalToolName =
    pendingApproval?.tool || pendingApproval?.tool_name || 'tool';

  const handleAskApproval = async () => {
    if (!approvalQuestion.trim() || !pendingApproval) return;
    setAskingApproval(true);
    try {
      const q = approvalQuestion.trim();
      const a = await onAskApprovalQuestion(
        approvalToolName,
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
        : [{ question: pendingClarify.question || '', choices: pendingClarify.choices || [] }];

    const answers = questions.map((_, idx) => {
      return clarifyAnswers[idx] || customClarifyInput || 'Skipped';
    });
    onResolveClarify(answers);
    setClarifyAnswers({});
    setCustomClarifyInput('');
  };

  const modelLabel = model || provider || 'default';
  const workspaceFolder =
    workspaceRoot.split('/').filter(Boolean).pop() || workspaceRoot || 'Workspace';
  const hasUserOrAssistantMessages = messages.length > 0;

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
        </div>
      </header>

      <div className="messages-area" ref={messagesAreaRef}>
        <div className="messages-column">
          {hiddenTurnCount > 0 && (
            <div className="load-earlier-wrap">
              <button
                type="button"
                className="load-earlier-btn"
                onClick={() => setVisibleTurnLimit((v) => v + 40)}
              >
                Show earlier messages ({hiddenTurnCount} hidden)
              </button>
            </div>
          )}

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
                {workspaceFolder} · {provider || 'default'} / {modelLabel}
              </p>
              <div className="starter-grid">
                {STARTER_PROMPTS.map((prompt) => (
                  <button
                    key={prompt}
                    type="button"
                    className="starter-chip"
                    onClick={() => onSendMessage(prompt)}
                  >
                    {prompt}
                  </button>
                ))}
              </div>
            </div>
          ) : (
            visibleTurns.map((turn, turnIdx) => {
              if (turn.kind === 'user') {
                return (
                  <div key={turn.key} className="turn-user">
                    <div className="user-bubble">{turn.content}</div>
                  </div>
                );
              }

              const isLastTurn = turnIdx === visibleTurns.length - 1;
              const isLive = isLastTurn && turnRunning;
              const isActivityOpen = isLive
                ? !collapsedLive[turn.key]
                : Boolean(expandedActivity[turn.key]);

              return (
                <AssistantTurnRow
                  key={turn.key}
                  turn={turn}
                  isLive={isLive}
                  isActivityOpen={isActivityOpen}
                  expandedReasoning={expandedReasoning}
                  expandedTools={expandedTools}
                  copied={copiedTurnKey === turn.key}
                  onToggleActivity={toggleActivity}
                  onToggleReasoning={toggleReasoning}
                  onToggleTool={toggleTool}
                  onCopy={handleCopyAssistantTurn}
                />
              );
            })
          )}
        </div>
      </div>

      {pendingApproval && (
        <div className="modal-backdrop">
          <div className="modal-card">
            <div className="modal-header">
              <div className="modal-title-row">
                <ShieldAlert size={17} className="text-warning" />
                <span>Approve tool execution</span>
              </div>
              <span className="badge badge-mono">{approvalToolName}</span>
            </div>

            <div className="modal-body">
              {pendingApproval.target && (
                <div className="form-group">
                  <span className="form-label">Target</span>
                  <div className="code-inline-block">{pendingApproval.target}</div>
                </div>
              )}

              <div className="form-group">
                <span className="form-label">
                  {pendingApproval.diff ? 'Proposed diff' : 'Arguments'}
                </span>
                <pre className="diff-view">
                  {pendingApproval.diff || JSON.stringify(pendingApproval.arguments, null, 2)}
                </pre>
              </div>

              {approvalReplies.length > 0 && (
                <div className="form-group">
                  <span className="form-label">Model explanation</span>
                  <div className="qa-list">
                    {approvalReplies.map((item, i) => (
                      <div key={i} className="qa-item">
                        <div className="qa-question">Q: {item.q}</div>
                        <div className="qa-answer">{item.a}</div>
                      </div>
                    ))}
                  </div>
                </div>
              )}

              <div className="form-group">
                <span className="form-label">Ask about this command before deciding</span>
                <div className="inline-input-row">
                  <input
                    type="text"
                    className="form-input"
                    placeholder="What does this command modify?"
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
                onClick={() => {
                  const reason = window.prompt('Optional denial reason or feedback:');
                  if (reason && reason.trim()) {
                    onResolveApproval('deny_with_feedback', undefined, reason.trim());
                  } else {
                    onResolveApproval('deny');
                  }
                  setApprovalReplies([]);
                }}
              >
                Deny
              </button>
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => {
                  onResolveApproval('run');
                  setApprovalReplies([]);
                }}
              >
                Allow
              </button>
            </div>
          </div>
        </div>
      )}

      {pendingClarify && (
        <div className="modal-backdrop">
          <div className="modal-card">
            <div className="modal-header">
              <div className="modal-title-row">
                <HelpCircle size={17} className="text-accent" />
                <span>Clarification requested</span>
              </div>
            </div>

            <div className="modal-body">
              {pendingClarify.questions && pendingClarify.questions.length > 0 ? (
                pendingClarify.questions.map((q, idx) => (
                  <div key={idx} className="form-group">
                    <span className="form-label form-label-strong">{q.question}</span>
                    <div className="radio-list">
                      {(q.choices || []).map((choice) => (
                        <label key={choice} className="radio-row">
                          <input
                            type="radio"
                            name={`q-${idx}`}
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
                ))
              ) : (
                <div className="form-group">
                  <span className="form-label form-label-strong">{pendingClarify.question}</span>
                  <div className="radio-list">
                    {(pendingClarify.choices || []).map((choice) => (
                      <label key={choice} className="radio-row">
                        <input
                          type="radio"
                          name="choice-0"
                          checked={clarifyAnswers[0] === choice}
                          onChange={() => setClarifyAnswers((prev) => ({ ...prev, [0]: choice }))}
                        />
                        <span>{choice}</span>
                      </label>
                    ))}
                  </div>
                </div>
              )}

              <div className="form-group">
                <span className="form-label">Or enter a custom answer</span>
                <input
                  type="text"
                  className="form-input"
                  placeholder="Type response…"
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

      <Composer
        sessionId={sessionId}
        workspaceRoot={workspaceRoot}
        showWorkspacePicker={!hasUserOrAssistantMessages}
        provider={provider}
        model={model}
        turnRunning={turnRunning}
        onSend={onSendMessage}
        onInterrupt={onInterrupt}
        onOpenModelPicker={onOpenModelPicker}
        onOpenWorkspacePicker={onOpenWorkspacePicker}
      />
    </div>
  );
};
