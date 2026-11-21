import React, { useState, useEffect, useRef, useMemo, useCallback } from 'react';
import type {
  AgentMode,
  ApprovalRequestPayload,
  ClarifyRequestPayload,
  GoalStatus,
  PluginSummary,
  SkillSummary,
  TodoItem,
  ToolCallItem,
  TranscriptMessageDto,
  WorkerProfile,
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
  ListTodo,
  GitBranch,
  Layers,
  MoreHorizontal,
  Minimize2,
  GitFork,
  Activity,
  AppWindow,
  Laptop,
} from 'lucide-react';
import { MarkdownContent } from './MarkdownContent';
import { Composer } from './Composer';
import { PluginWorkspace } from './PluginWorkspace';
import type { ActiveDrawerTab } from './SessionDrawers';

interface ChatViewProps {
  sessionId: string | null;
  sessionTitle: string;
  workspaceRoot: string;
  provider: string;
  model: string;
  accountLabel?: string | null;
  mode: AgentMode;
  reasoningEffort: string | null;
  contextTokens: number;
  contextWindow: number;
  queuedCount: number;
  todos: TodoItem[];
  goal: GoalStatus | null;
  workers: Record<string, WorkerProfile>;
  skills: SkillSummary[];
  pluginCommands?: Array<{ pluginId: string; name: string; description: string }>;
  activePlugin?: PluginSummary | null;
  pluginPanelOpen?: boolean;
  onTogglePluginPanel?: () => void;
  onPluginUpdated?: (updated: PluginSummary) => void;
  onEditPluginWithLuna?: (plugin: PluginSummary) => void;
  onNotify?: (msg: string, kind?: 'info' | 'err') => void;
  theme?: 'dark' | 'light';
  composerPrefill?: string | null;
  onClearComposerPrefill?: () => void;
  notices: Array<{ id: string; text: string; kind: 'info' | 'err' }>;
  onDismissNotice: (id: string) => void;
  messages: TranscriptMessageDto[];
  turnRunning: boolean;
  sidebarOpen: boolean;
  onToggleSidebar: () => void;
  onOpenThinkingPicker: (anchorRect: DOMRect) => void;
  onOpenModelPicker: (anchorRect?: DOMRect) => void;
  onOpenWorkspacePicker: () => void;
  onOpenDrawer: (tab: ActiveDrawerTab) => void;
  onRenameSession: (name: string) => void;
  onCompactSession: () => void;
  onSummarizeSession: () => void;
  onRememberSession: () => void;
  onForkSession: () => void;
  onNewSession: () => void;
  onSendMessage: (text: string, images?: string[], sendMode?: 'steer' | 'queue') => void;
  onSlashCommand: (raw: string, images?: string[]) => boolean;
  onInterrupt: () => void;
  onChangeMode: (mode: AgentMode) => void;
  onChangeReasoning: (effort: string | null) => void;
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

const STARTER_PROMPTS: Array<{ text: string; kind: 'doc' | 'term' | 'wrench' }> = [
  { text: 'Inspect this workspace and summarize its architecture', kind: 'doc' },
  { text: 'Run git status and explain recent changes', kind: 'term' },
  { text: 'Check failing tests or compiler warnings', kind: 'wrench' },
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

function stripInternalUserPrefixes(raw: string): string {
  let text = raw.trim();
  const oobMatch = text.match(
    /^\[OUT-OF-BAND USER MESSAGE[^\]]*\]\s*([\s\S]*?)\s*\[\/OUT-OF-BAND USER MESSAGE\]$/
  );
  if (oobMatch) {
    text = oobMatch[1].trim();
  }
  while (text.startsWith('[PLAN MODE ACTIVE:')) {
    const closeIdx = text.indexOf(']');
    if (closeIdx === -1) break;
    text = text.slice(closeIdx + 1).trimStart();
  }
  return text.trim();
}

function parseUserMessageParts(content: string): { text: string; images: string[] } {
  const cleaned = stripInternalUserPrefixes(content);
  const images: string[] = [];
  const textLines: string[] = [];
  for (const line of cleaned.split('\n')) {
    const trimmed = line.trim();
    if (trimmed.startsWith('data:image/') && trimmed.includes(';base64,')) {
      images.push(trimmed);
    } else {
      textLines.push(line);
    }
  }
  return { text: textLines.join('\n').trim(), images };
}

export const ChatView: React.FC<ChatViewProps> = ({
  sessionId,
  sessionTitle,
  workspaceRoot,
  provider,
  model,
  accountLabel,
  mode,
  reasoningEffort,
  contextTokens,
  contextWindow,
  queuedCount,
  todos,
  goal,
  workers,
  skills,
  pluginCommands,
  activePlugin,
  pluginPanelOpen,
  onTogglePluginPanel,
  onPluginUpdated,
  onEditPluginWithLuna,
  onNotify,
  theme = 'dark',
  composerPrefill,
  onClearComposerPrefill,
  notices,
  onDismissNotice,
  messages,
  turnRunning,
  sidebarOpen,
  onToggleSidebar,
  onOpenModelPicker,
  onOpenWorkspacePicker,
  onOpenDrawer,
  onRenameSession,
  onCompactSession,
  onSummarizeSession,
  onRememberSession,
  onForkSession,
  onNewSession,
  onSendMessage,
  onSlashCommand,
  onInterrupt,
  onChangeMode,
  onChangeReasoning,
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

  const [renamingTitle, setRenamingTitle] = useState(false);
  const [titleDraft, setTitleDraft] = useState(sessionTitle);
  const [moreMenuOpen, setMoreMenuOpen] = useState(false);

  // Approval state
  const [approvalQuestion, setApprovalQuestion] = useState('');
  const [approvalReplies, setApprovalReplies] = useState<Array<{ q: string; a: string }>>([]);
  const [askingApproval, setAskingApproval] = useState(false);
  const [editingArgs, setEditingArgs] = useState(false);
  const [argsDraft, setArgsDraft] = useState('');
  const [argsError, setArgsError] = useState<string | null>(null);
  const [denyingWithFeedback, setDenyingWithFeedback] = useState(false);
  const [denyFeedback, setDenyFeedback] = useState('');

  // Clarify state
  const [clarifyAnswers, setClarifyAnswers] = useState<Record<number, string>>({});
  const [clarifyCustom, setClarifyCustom] = useState<Record<number, string>>({});

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
    setRenamingTitle(false);
    setMoreMenuOpen(false);
  }, [sessionId]);

  useEffect(() => {
    if (pendingApproval) {
      setEditingArgs(false);
      setArgsDraft(JSON.stringify(pendingApproval.arguments ?? {}, null, 2));
      setArgsError(null);
      setDenyingWithFeedback(false);
      setDenyFeedback('');
    }
  }, [pendingApproval]);

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
      const custom = (clarifyCustom[idx] || '').trim();
      if (custom) return custom;
      return clarifyAnswers[idx] || 'Skipped';
    });
    onResolveClarify(answers);
    setClarifyAnswers({});
    setClarifyCustom({});
  };

  const modelLabel = model || provider || 'default';
  const workspaceFolder =
    workspaceRoot.split('/').filter(Boolean).pop() || workspaceRoot || 'Workspace';
  const isLanding = !sessionId || turns.length === 0;
  const openTodosCount = todos.filter((t) => t.status !== 'done').length;

  return (
    <div className="chat-view">
      {!isLanding ? (
        <header className="chat-header">
        <div className="chat-header-left">
          {!sidebarOpen && (
            <button
              type="button"
              className="icon-btn header-sidebar-toggle"
              onClick={onToggleSidebar}
              title="Open sidebar"
              aria-label="Open sidebar"
            >
              <PanelLeft size={16} />
            </button>
          )}
          {renamingTitle && sessionId ? (
            <input
              type="text"
              className="header-rename-input"
              autoFocus
              value={titleDraft}
              onChange={(e) => setTitleDraft(e.target.value)}
              onBlur={() => {
                setRenamingTitle(false);
                if (titleDraft.trim() && titleDraft.trim() !== sessionTitle) {
                  onRenameSession(titleDraft.trim());
                }
              }}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.currentTarget.blur();
                } else if (e.key === 'Escape') {
                  setTitleDraft(sessionTitle);
                  setRenamingTitle(false);
                }
              }}
            />
          ) : (
            <button
              type="button"
              className="chat-header-title-btn"
              onClick={() => {
                if (!sessionId) return;
                setTitleDraft(sessionTitle || '');
                setRenamingTitle(true);
              }}
              title={sessionId ? 'Click to rename conversation' : undefined}
            >
              {sessionTitle || 'New conversation'}
            </button>
          )}
        </div>

        <div className="chat-header-right">
          {goal && goal.running && (
            <button
              type="button"
              className="status-chip status-chip-live"
              onClick={() => onOpenDrawer('tasks')}
              title={`Goal loop: ${goal.objective}`}
            >
              <span className="pulse-dot" />
              <span className="tabular-nums">
                Goal {goal.iteration}/{goal.max_iterations}
              </span>
            </button>
          )}

          {turnRunning && (
            <span className="status-chip status-chip-live">
              <span className="pulse-dot" />
              <span>Working</span>
            </span>
          )}

          {sessionId && (
            <div className="header-actions-group">
              {activePlugin && onTogglePluginPanel && (
                <button
                  type="button"
                  className={`header-action-btn ${pluginPanelOpen ? 'active' : ''}`}
                  onClick={onTogglePluginPanel}
                  title={`Toggle ${activePlugin.name} UI`}
                >
                  <AppWindow size={14} />
                  <span style={{ fontSize: '12px', marginLeft: '4px' }}>UI</span>
                </button>
              )}

              <button
                type="button"
                className="header-action-btn"
                onClick={() => onOpenDrawer('todos')}
                title="Session checklist (/todo)"
              >
                <ListTodo size={14} />
                {openTodosCount > 0 && (
                  <span className="header-badge-num tabular-nums">{openTodosCount}</span>
                )}
              </button>

              <button
                type="button"
                className="header-action-btn"
                onClick={() => onOpenDrawer('checkpoints')}
                title="File changes & rewind (/diff, /rewind)"
              >
                <GitBranch size={14} />
              </button>

              <button
                type="button"
                className="header-action-btn"
                onClick={() => onOpenDrawer('tasks')}
                title="Subagents, jobs & goal loop (/subagents, /goal)"
              >
                <Layers size={14} />
              </button>

              <div className="header-more-wrap">
                <button
                  type="button"
                  className="header-action-btn"
                  onClick={() => setMoreMenuOpen((v) => !v)}
                  title="More session actions"
                  aria-label="More session actions"
                >
                  <MoreHorizontal size={15} />
                </button>
                {moreMenuOpen && (
                  <div
                    className="header-dropdown"
                    onMouseLeave={() => setMoreMenuOpen(false)}
                  >
                    <button
                      type="button"
                      className="header-dropdown-item"
                      onClick={() => {
                        setMoreMenuOpen(false);
                        onCompactSession();
                      }}
                    >
                      <Minimize2 size={13} />
                      <span>Compact history</span>
                    </button>
                    <button
                      type="button"
                      className="header-dropdown-item"
                      onClick={() => {
                        setMoreMenuOpen(false);
                        onSummarizeSession();
                      }}
                    >
                      <Sparkles size={13} />
                      <span>Summarize & new chat</span>
                    </button>
                    <button
                      type="button"
                      className="header-dropdown-item"
                      onClick={() => {
                        setMoreMenuOpen(false);
                        onRememberSession();
                      }}
                    >
                      <Sparkles size={13} />
                      <span>Remember now</span>
                    </button>
                    <button
                      type="button"
                      className="header-dropdown-item"
                      onClick={() => {
                        setMoreMenuOpen(false);
                        onForkSession();
                      }}
                    >
                      <GitFork size={13} />
                      <span>Fork conversation</span>
                    </button>
                    <button
                      type="button"
                      className="header-dropdown-item"
                      onClick={() => {
                        setMoreMenuOpen(false);
                        onOpenDrawer('usage');
                      }}
                    >
                      <Activity size={13} />
                      <span>Usage & system health</span>
                    </button>
                  </div>
                )}
              </div>
            </div>
          )}
        </div>
      </header>
      ) : (
        !sidebarOpen && (
          <div className="landing-floating-bar header-sidebar-toggle">
            <button
              type="button"
              className="icon-btn"
              onClick={onToggleSidebar}
              title="Open sidebar"
              aria-label="Open sidebar"
            >
              <PanelLeft size={16} />
            </button>
          </div>
        )
      )}

      {notices.length > 0 && (
        <div className="notices-strip">
          {notices.map((n) => (
            <div
              key={n.id}
              className={`notice-pill ${n.kind === 'err' ? 'notice-pill-err' : ''}`}
            >
              <span>{n.text}</span>
              <button
                type="button"
                className="notice-close"
                onClick={() => onDismissNotice(n.id)}
                aria-label="Dismiss"
              >
                ×
              </button>
            </div>
          ))}
        </div>
      )}

      <div className={`chat-split-layout ${activePlugin && pluginPanelOpen ? 'split-open' : ''}`}>
        <div className="chat-main-pane">
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
            <div className="chat-landing-stage">
              <div className="chat-landing-hero">
                <h1 className="chat-empty-title">What can I help with?</h1>
                <p className="chat-empty-subtitle">
                  {workspaceFolder} : {provider || 'default'} / {modelLabel}
                </p>
                <div style={{ marginTop: '14px' }}>
                  <button type="button" className="btn btn-primary" onClick={onNewSession}>
                    <Plus size={15} />
                    <span>New Chat</span>
                  </button>
                </div>
              </div>
            </div>
          ) : turns.length === 0 ? (
            <div className="chat-landing-stage">
              <div className="chat-landing-hero">
                <h1 className="chat-empty-title">What can I help with?</h1>
                <p className="chat-empty-subtitle">
                  <button
                    type="button"
                    className="landing-subtitle-btn"
                    onClick={onOpenWorkspacePicker}
                    title={`Workspace: ${workspaceRoot}`}
                  >
                    {workspaceFolder}
                  </button>
                  {' : '}
                  <button
                    type="button"
                    className="landing-subtitle-btn"
                    onClick={(e) => onOpenModelPicker(e.currentTarget.getBoundingClientRect())}
                    title="Switch provider or model"
                  >
                    {provider || 'default'} / {modelLabel}
                  </button>
                </p>

                <div className="chat-landing-composer-wrap">
                  <Composer
                    sessionId={sessionId}
                    workspaceRoot={workspaceRoot}
                    showWorkspacePicker={false}
                    landingMode={true}
                    provider={provider}
                    model={model}
                    accountLabel={accountLabel}
                    mode={mode}
                    reasoningEffort={reasoningEffort}
                    contextTokens={contextTokens}
                    contextWindow={contextWindow}
                    queuedCount={queuedCount}
                    turnRunning={turnRunning}
                    workers={workers}
                    skills={skills}
                    pluginCommands={pluginCommands}
                    composerPrefill={composerPrefill}
                    onClearComposerPrefill={onClearComposerPrefill}
                    onSend={onSendMessage}
                    onSlashCommand={onSlashCommand}
                    onInterrupt={onInterrupt}
                    onChangeMode={onChangeMode}
                    onChangeReasoning={onChangeReasoning}
                    onOpenModelPicker={onOpenModelPicker}
                    onOpenWorkspacePicker={onOpenWorkspacePicker}
                    onOpenUsage={() => onOpenDrawer('usage')}
                    onNotify={onNotify}
                  />
                </div>

                <div className="landing-mode-toggle" role="group" aria-label="Agent mode">
                  <button
                    type="button"
                    className={`landing-mode-btn ${mode === 'plan' ? 'active' : ''}`}
                    onClick={() => onChangeMode('plan')}
                    title="Conversational / read-only chat mode"
                  >
                    Chat
                  </button>
                  <button
                    type="button"
                    className={`landing-mode-btn ${mode !== 'plan' ? 'active' : ''}`}
                    onClick={() => onChangeMode('build')}
                    title="Full workspace build & tool execution mode"
                  >
                    {mode === 'auto' ? 'Build · Auto' : 'Build'}
                  </button>
                </div>

                <div className="starter-stack">
                  {STARTER_PROMPTS.map((item) => (
                    <button
                      key={item.text}
                      type="button"
                      className="starter-row"
                      onClick={() => onSendMessage(item.text)}
                    >
                      <span className="starter-row-icon">
                        {item.kind === 'doc' ? (
                          <FileText size={15} />
                        ) : item.kind === 'term' ? (
                          <Laptop size={15} />
                        ) : (
                          <Wrench size={15} />
                        )}
                      </span>
                      <span className="starter-row-text">{item.text}</span>
                    </button>
                  ))}
                </div>
              </div>
            </div>
          ) : (
            visibleTurns.map((turn, turnIdx) => {
              if (turn.kind === 'user') {
                const parsed = parseUserMessageParts(turn.content);
                return (
                  <div key={turn.key} className="turn-user">
                    <div className="user-bubble">
                      {parsed.images.length > 0 && (
                        <div className="user-bubble-images">
                          {parsed.images.map((src, i) => (
                            <img key={i} src={src} alt={`Attachment ${i + 1}`} />
                          ))}
                        </div>
                      )}
                      {parsed.text && <div>{parsed.text}</div>}
                    </div>
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
                <div className="form-label-row">
                  <span className="form-label">
                    {pendingApproval.diff && !editingArgs ? 'Proposed diff' : 'Arguments'}
                  </span>
                  <button
                    type="button"
                    className="picker-mini-btn"
                    onClick={() => setEditingArgs((v) => !v)}
                  >
                    {editingArgs ? 'Cancel edit' : 'Edit arguments'}
                  </button>
                </div>
                {editingArgs ? (
                  <>
                    <textarea
                      className="form-textarea mono-input"
                      rows={6}
                      value={argsDraft}
                      onChange={(e) => {
                        setArgsDraft(e.target.value);
                        setArgsError(null);
                      }}
                    />
                    {argsError && <div className="text-danger">{argsError}</div>}
                  </>
                ) : (
                  <pre className="diff-view">
                    {pendingApproval.diff || JSON.stringify(pendingApproval.arguments, null, 2)}
                  </pre>
                )}
              </div>

              {denyingWithFeedback && (
                <div className="form-group">
                  <span className="form-label">Feedback for model (why you denied)</span>
                  <input
                    type="text"
                    className="form-input"
                    autoFocus
                    placeholder="e.g. Use read-only check instead…"
                    value={denyFeedback}
                    onChange={(e) => setDenyFeedback(e.target.value)}
                    onKeyDown={(e) => {
                      if (e.key === 'Enter') {
                        const clean = denyFeedback.trim();
                        if (clean) {
                          onResolveApproval('deny_with_feedback', undefined, clean);
                        } else {
                          onResolveApproval('deny');
                        }
                        setApprovalReplies([]);
                      }
                    }}
                  />
                </div>
              )}

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

            <div className="modal-footer modal-footer-between">
              <div className="inline-row">
                {!denyingWithFeedback ? (
                  <>
                    <button
                      type="button"
                      className="btn btn-danger"
                      onClick={() => {
                        onResolveApproval('deny');
                        setApprovalReplies([]);
                      }}
                    >
                      Deny
                    </button>
                    <button
                      type="button"
                      className="btn btn-secondary"
                      onClick={() => setDenyingWithFeedback(true)}
                    >
                      Deny + feedback
                    </button>
                  </>
                ) : (
                  <button
                    type="button"
                    className="btn btn-danger"
                    onClick={() => {
                      const clean = denyFeedback.trim();
                      onResolveApproval(
                        clean ? 'deny_with_feedback' : 'deny',
                        undefined,
                        clean || undefined
                      );
                      setApprovalReplies([]);
                    }}
                  >
                    Submit denial
                  </button>
                )}
              </div>

              <div className="inline-row">
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={() => {
                    onChangeMode('auto');
                    onResolveApproval('run');
                    setApprovalReplies([]);
                  }}
                  title="Switch this session to Auto mode and allow"
                >
                  Allow session
                </button>
                {editingArgs ? (
                  <button
                    type="button"
                    className="btn btn-primary"
                    onClick={() => {
                      try {
                        const parsed = JSON.parse(argsDraft);
                        onResolveApproval('run_with_args', parsed);
                        setApprovalReplies([]);
                      } catch (err) {
                        setArgsError(`Invalid JSON: ${String(err)}`);
                      }
                    }}
                  >
                    Run edited
                  </button>
                ) : (
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
                )}
              </div>
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
              {(pendingClarify.questions && pendingClarify.questions.length > 0
                ? pendingClarify.questions
                : [{ question: pendingClarify.question || '', choices: pendingClarify.choices || [] }]
              ).map((q, idx) => (
                <div key={idx} className="form-group">
                  <span className="form-label form-label-strong">{q.question}</span>
                  {(q.choices || []).length > 0 && (
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
                  )}
                  <input
                    type="text"
                    className="form-input"
                    placeholder="Or type a custom answer…"
                    value={clarifyCustom[idx] || ''}
                    onChange={(e) =>
                      setClarifyCustom((prev) => ({ ...prev, [idx]: e.target.value }))
                    }
                    onKeyDown={(e) => e.key === 'Enter' && handleClarifySubmit()}
                  />
                </div>
              ))}
            </div>

            <div className="modal-footer">
              <button type="button" className="btn btn-primary" onClick={handleClarifySubmit}>
                Submit
              </button>
            </div>
          </div>
        </div>
      )}

          {!isLanding && (
            <Composer
              sessionId={sessionId}
              workspaceRoot={workspaceRoot}
              showWorkspacePicker={false}
              landingMode={false}
              provider={provider}
              model={model}
              accountLabel={accountLabel}
              mode={mode}
              reasoningEffort={reasoningEffort}
              contextTokens={contextTokens}
              contextWindow={contextWindow}
              queuedCount={queuedCount}
              turnRunning={turnRunning}
              workers={workers}
              skills={skills}
              pluginCommands={pluginCommands}
              composerPrefill={composerPrefill}
              onClearComposerPrefill={onClearComposerPrefill}
              onSend={onSendMessage}
              onSlashCommand={onSlashCommand}
              onInterrupt={onInterrupt}
              onChangeMode={onChangeMode}
              onChangeReasoning={onChangeReasoning}
              onOpenModelPicker={onOpenModelPicker}
              onOpenWorkspacePicker={onOpenWorkspacePicker}
              onOpenUsage={() => onOpenDrawer('usage')}
              onNotify={onNotify}
            />
          )}
        </div>

        {activePlugin && pluginPanelOpen && (
          <aside className="chat-plugin-pane">
            <PluginWorkspace
              plugin={activePlugin}
              theme={theme}
              compact={true}
              onPluginUpdated={(updated) => onPluginUpdated?.(updated)}
              onBackOrClose={() => onTogglePluginPanel?.()}
              onEditWithLuna={onEditPluginWithLuna}
              onSendToChat={(msg) => onSendMessage(msg)}
              onNotify={(msg, kind) => onNotify?.(msg, kind)}
            />
          </aside>
        )}
      </div>
    </div>
  );
};
