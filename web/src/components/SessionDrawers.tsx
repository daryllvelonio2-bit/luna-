import React, { useState, useEffect, useCallback } from 'react';
import { api } from '../api';
import type {
  GoalStatus,
  HeartbeatPayload,
  JobSummary,
  SubagentTaskRecord,
  TodoItem,
  TurnDiffItem,
  UsagePayload,
  WorkerProfile,
} from '../types';
import {
  X,
  CheckCircle2,
  Circle,
  PlayCircle,
  Trash2,
  RotateCcw,
  ChevronDown,
  ChevronRight,
  Square,
  Play,
  Activity,
  Cpu,
  Plus,
} from 'lucide-react';

export type ActiveDrawerTab = 'todos' | 'checkpoints' | 'tasks' | 'usage' | null;

interface SessionDrawersProps {
  activeTab: ActiveDrawerTab;
  onClose: () => void;
  sessionId: string | null;
  todos: TodoItem[];
  onTodosChange: (items: TodoItem[]) => void;
  goal: GoalStatus | null;
  onGoalChange: (goal: GoalStatus | null) => void;
  workers: Record<string, WorkerProfile>;
  contextTokens: number;
  contextWindow: number;
  onNotify: (msg: string) => void;
  onError: (msg: string) => void;
}

export const SessionDrawers: React.FC<SessionDrawersProps> = ({
  activeTab,
  onClose,
  sessionId,
  todos,
  onTodosChange,
  goal,
  onGoalChange,
  workers,
  contextTokens,
  contextWindow,
  onNotify,
  onError,
}) => {
  const [newTodoText, setNewTodoText] = useState('');

  const [turns, setTurns] = useState<TurnDiffItem[]>([]);
  const [expandedDiffs, setExpandedDiffs] = useState<Record<number, boolean>>({});
  const [confirmRewindSeq, setConfirmRewindSeq] = useState<number | null>(null);
  const [loadingCheckpoints, setLoadingCheckpoints] = useState(false);

  const [subagents, setSubagents] = useState<SubagentTaskRecord[]>([]);
  const [jobs, setJobs] = useState<JobSummary[]>([]);
  const [goalObjective, setGoalObjective] = useState('');
  const [goalMaxIter, setGoalMaxIter] = useState(10);
  const [workerName, setWorkerName] = useState('');
  const [workerTask, setWorkerTask] = useState('');

  const [usage, setUsage] = useState<UsagePayload | null>(null);
  const [heartbeat, setHeartbeat] = useState<HeartbeatPayload | null>(null);

  const refreshCheckpoints = useCallback(async () => {
    if (!sessionId) return;
    setLoadingCheckpoints(true);
    try {
      const res = await api.listCheckpoints(sessionId);
      setTurns(res.turns || []);
    } catch (e) {
      onError(String(e));
    } finally {
      setLoadingCheckpoints(false);
    }
  }, [sessionId, onError]);

  const refreshTasks = useCallback(async () => {
    if (!sessionId) return;
    try {
      const res = await api.listTasks(sessionId);
      setSubagents(res.subagents || []);
      setJobs(res.jobs || []);
      onGoalChange(res.goal || null);
    } catch (e) {
      onError(String(e));
    }
  }, [sessionId, onGoalChange, onError]);

  const refreshUsageAndHeartbeat = useCallback(async () => {
    try {
      const [u, h] = await Promise.all([
        api.getUsage(sessionId),
        api.getHeartbeat(sessionId),
      ]);
      setUsage(u);
      setHeartbeat(h);
    } catch (e) {
      onError(String(e));
    }
  }, [sessionId, onError]);

  useEffect(() => {
    if (!activeTab) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [activeTab, onClose]);

  useEffect(() => {
    if (!activeTab || !sessionId) return;
    if (activeTab === 'todos') {
      api.getTodos(sessionId).then((r) => onTodosChange(r.items || [])).catch(() => {});
    } else if (activeTab === 'checkpoints') {
      refreshCheckpoints();
    } else if (activeTab === 'tasks') {
      refreshTasks();
      const firstWorker = Object.keys(workers)[0] || '';
      if (!workerName && firstWorker) setWorkerName(firstWorker);
    } else if (activeTab === 'usage') {
      refreshUsageAndHeartbeat();
    }
  }, [
    activeTab,
    sessionId,
    onTodosChange,
    refreshCheckpoints,
    refreshTasks,
    refreshUsageAndHeartbeat,
    workers,
    workerName,
  ]);

  if (!activeTab) return null;

  const handleAddTodo = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!sessionId || !newTodoText.trim()) return;
    try {
      const res = await api.mutateTodo(sessionId, {
        action: 'add',
        text: newTodoText.trim(),
      });
      onTodosChange(res.items || []);
      setNewTodoText('');
    } catch (err) {
      onError(String(err));
    }
  };

  const handleCycleTodo = async (item: TodoItem) => {
    if (!sessionId) return;
    const nextStatus =
      item.status === 'pending'
        ? 'in_progress'
        : item.status === 'in_progress'
        ? 'done'
        : 'pending';
    try {
      const res = await api.mutateTodo(sessionId, {
        action: 'update',
        id: item.id,
        status: nextStatus,
      });
      onTodosChange(res.items || []);
    } catch (err) {
      onError(String(err));
    }
  };

  const handleRemoveTodo = async (id: number) => {
    if (!sessionId) return;
    try {
      const res = await api.mutateTodo(sessionId, { action: 'remove', id });
      onTodosChange(res.items || []);
    } catch (err) {
      onError(String(err));
    }
  };

  const handleClearDone = async () => {
    if (!sessionId) return;
    try {
      const res = await api.mutateTodo(sessionId, { action: 'clear_done' });
      onTodosChange(res.items || []);
    } catch (err) {
      onError(String(err));
    }
  };

  const handleRewind = async (seq: number) => {
    if (!sessionId) return;
    try {
      const rep = await api.rewindCheckpoint(sessionId, seq);
      setConfirmRewindSeq(null);
      onNotify(
        `Rewound turn ${seq}: ${rep.restored.length} restored, ${rep.deleted.length} removed.`
      );
      refreshCheckpoints();
    } catch (err) {
      onError(String(err));
    }
  };

  const handleStartGoal = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!sessionId || !goalObjective.trim()) return;
    try {
      const res = await api.manageGoal(
        sessionId,
        'start',
        goalObjective.trim(),
        goalMaxIter
      );
      if (res.goal) onGoalChange(res.goal);
      setGoalObjective('');
      onClose();
    } catch (err) {
      onError(String(err));
    }
  };

  const handleStopGoal = async () => {
    if (!sessionId) return;
    try {
      await api.manageGoal(sessionId, 'stop');
      onGoalChange(null);
      onNotify('Goal loop stop requested.');
    } catch (err) {
      onError(String(err));
    }
  };

  const handleRunWorker = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!sessionId || !workerName || !workerTask.trim()) return;
    try {
      const res = await api.runWorker(sessionId, workerName, workerTask.trim());
      setWorkerTask('');
      onNotify(`Worker "${res.worker}" started (${res.model}).`);
      refreshTasks();
    } catch (err) {
      onError(String(err));
    }
  };

  const formatSubagentStatus = (status: SubagentTaskRecord['status']): string => {
    if (typeof status === 'string') return status;
    if (status && typeof status === 'object') return status.status;
    return 'unknown';
  };

  const titles: Record<Exclude<ActiveDrawerTab, null>, string> = {
    todos: 'Session Checklist',
    checkpoints: 'File Changes & Rewind',
    tasks: 'Goal Loop, Subagents & Jobs',
    usage: 'Usage, Cost & System Health',
  };

  const ctxPct =
    contextWindow > 0 ? Math.min(100, Math.round((contextTokens / contextWindow) * 100)) : 0;

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="drawer-panel"
        role="dialog"
        aria-modal="true"
        aria-label={titles[activeTab]}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="drawer-header">
          <span className="modal-title">{titles[activeTab]}</span>
          <button type="button" className="icon-btn" onClick={onClose} aria-label="Close">
            <X size={16} />
          </button>
        </div>

        <div className="drawer-body">
          {activeTab === 'todos' && (
            <div className="drawer-section">
              <form className="inline-row" onSubmit={handleAddTodo}>
                <input
                  type="text"
                  className="form-input flex-1"
                  placeholder="Add a task for this conversation…"
                  value={newTodoText}
                  onChange={(e) => setNewTodoText(e.target.value)}
                />
                <button type="submit" className="btn btn-primary" disabled={!newTodoText.trim()}>
                  <Plus size={14} />
                  <span>Add</span>
                </button>
              </form>

              {todos.length === 0 ? (
                <div className="empty-inline-flat">No tasks tracked in this session yet.</div>
              ) : (
                <div className="hairline-list">
                  {todos.map((item) => (
                    <div key={item.id} className="hairline-row">
                      <button
                        type="button"
                        className="todo-status-btn"
                        onClick={() => handleCycleTodo(item)}
                        title="Click to cycle status"
                      >
                        {item.status === 'done' ? (
                          <CheckCircle2 size={16} className="text-success" />
                        ) : item.status === 'in_progress' ? (
                          <PlayCircle size={16} className="text-accent" />
                        ) : (
                          <Circle size={16} className="text-muted" />
                        )}
                      </button>
                      <span
                        className={`todo-text ${item.status === 'done' ? 'todo-done' : ''}`}
                      >
                        {item.text}
                      </span>
                      <button
                        type="button"
                        className="icon-btn"
                        onClick={() => handleRemoveTodo(item.id)}
                        aria-label="Remove task"
                      >
                        <Trash2 size={13} />
                      </button>
                    </div>
                  ))}
                </div>
              )}

              {todos.some((t) => t.status === 'done') && (
                <div className="drawer-footer-action">
                  <button type="button" className="btn btn-secondary" onClick={handleClearDone}>
                    Clear completed
                  </button>
                </div>
              )}
            </div>
          )}

          {activeTab === 'checkpoints' && (
            <div className="drawer-section">
              {loadingCheckpoints ? (
                <div className="empty-inline-flat">Loading checkpoints…</div>
              ) : turns.length === 0 ? (
                <div className="empty-inline-flat">
                  No file checkpoints recorded for this session yet.
                </div>
              ) : (
                <div className="hairline-list">
                  {[...turns].reverse().map((turn) => {
                    const open = Boolean(expandedDiffs[turn.seq]);
                    const changedFiles = turn.files.filter((f) => f.kind !== 'unchanged');
                    return (
                      <div key={turn.seq} className="checkpoint-row">
                        <div className="checkpoint-row-head">
                          <button
                            type="button"
                            className="checkpoint-toggle"
                            onClick={() =>
                              setExpandedDiffs((prev) => ({
                                ...prev,
                                [turn.seq]: !prev[turn.seq],
                              }))
                            }
                          >
                            {open ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                            <span className="checkpoint-seq">Turn #{turn.seq}</span>
                            <span className="checkpoint-prompt" title={turn.prompt || ''}>
                              {turn.prompt || 'Edit'}
                            </span>
                            <span className="checkpoint-stat tabular-nums">
                              {changedFiles.length} file{changedFiles.length === 1 ? '' : 's'}
                            </span>
                          </button>

                          {confirmRewindSeq === turn.seq ? (
                            <div className="inline-row">
                              <button
                                type="button"
                                className="btn btn-danger btn-xs"
                                onClick={() => handleRewind(turn.seq)}
                              >
                                Confirm rewind
                              </button>
                              <button
                                type="button"
                                className="btn btn-secondary btn-xs"
                                onClick={() => setConfirmRewindSeq(null)}
                              >
                                Cancel
                              </button>
                            </div>
                          ) : (
                            <button
                              type="button"
                              className="picker-action-chip"
                              onClick={() => setConfirmRewindSeq(turn.seq)}
                              title="Restore workspace files to before this turn"
                            >
                              <RotateCcw size={12} />
                              <span>Rewind</span>
                            </button>
                          )}
                        </div>

                        {open && (
                          <div className="checkpoint-diff-body">
                            {changedFiles.length === 0 ? (
                              <div className="empty-inline-flat">No net file changes.</div>
                            ) : (
                              changedFiles.map((f) => (
                                <div key={f.path} className="diff-file-block">
                                  <div className="diff-file-header">
                                    <span className="mono-text">{f.path}</span>
                                    <span className="tabular-nums text-muted">
                                      +{f.additions} -{f.deletions} ({f.kind})
                                    </span>
                                  </div>
                                  {f.diff && <pre className="diff-view">{f.diff}</pre>}
                                </div>
                              ))
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

          {activeTab === 'tasks' && (
            <div className="drawer-section">
              <div className="drawer-subhead">Autonomous Goal Loop</div>
              {goal && goal.running ? (
                <div className="goal-active-bar">
                  <div className="goal-active-info">
                    <span className="pulse-dot" />
                    <div>
                      <div className="goal-active-title">{goal.objective}</div>
                      <div className="goal-active-meta tabular-nums">
                        Iteration {goal.iteration} of {goal.max_iterations}
                      </div>
                    </div>
                  </div>
                  <button type="button" className="btn btn-danger btn-xs" onClick={handleStopGoal}>
                    <Square size={12} />
                    <span>Stop</span>
                  </button>
                </div>
              ) : (
                <form className="goal-form" onSubmit={handleStartGoal}>
                  <input
                    type="text"
                    className="form-input flex-1"
                    placeholder="Objective for autonomous multi-turn loop…"
                    value={goalObjective}
                    onChange={(e) => setGoalObjective(e.target.value)}
                  />
                  <input
                    type="number"
                    className="form-input goal-iter-input tabular-nums"
                    min={1}
                    max={50}
                    value={goalMaxIter}
                    onChange={(e) => setGoalMaxIter(Number(e.target.value) || 10)}
                    title="Max iterations"
                  />
                  <button
                    type="submit"
                    className="btn btn-primary"
                    disabled={!goalObjective.trim()}
                  >
                    <Play size={13} />
                    <span>Start</span>
                  </button>
                </form>
              )}

              {Object.keys(workers).length > 0 && (
                <>
                  <div className="drawer-subhead">Run Named Worker</div>
                  <form className="goal-form" onSubmit={handleRunWorker}>
                    <select
                      className="form-input worker-select"
                      value={workerName}
                      onChange={(e) => setWorkerName(e.target.value)}
                    >
                      {Object.entries(workers).map(([name, w]) => (
                        <option key={name} value={name}>
                          /{name} ({w.model})
                        </option>
                      ))}
                    </select>
                    <input
                      type="text"
                      className="form-input flex-1"
                      placeholder="Task for worker (runs on session fork)…"
                      value={workerTask}
                      onChange={(e) => setWorkerTask(e.target.value)}
                    />
                    <button
                      type="submit"
                      className="btn btn-secondary"
                      disabled={!workerTask.trim()}
                    >
                      Run
                    </button>
                  </form>
                </>
              )}

              <div className="drawer-subhead">
                Delegated Subagents <span className="tabular-nums">({subagents.length})</span>
              </div>
              {subagents.length === 0 ? (
                <div className="empty-inline-flat">No subagents spawned in this session.</div>
              ) : (
                <div className="hairline-list">
                  {subagents.map((s) => {
                    const st = formatSubagentStatus(s.status);
                    return (
                      <div key={s.id} className="hairline-row">
                        <div className="hairline-row-main">
                          <div className="hairline-row-title">{s.goal}</div>
                          <div className="hairline-row-meta">
                            {s.id} · {st}
                          </div>
                        </div>
                        {st === 'running' && sessionId && (
                          <button
                            type="button"
                            className="btn btn-danger btn-xs"
                            onClick={() =>
                              api.killTask(sessionId, s.id, 'subagent').then(refreshTasks)
                            }
                          >
                            Stop
                          </button>
                        )}
                      </div>
                    );
                  })}
                </div>
              )}

              <div className="drawer-subhead">
                Background Shell Jobs <span className="tabular-nums">({jobs.length})</span>
              </div>
              {jobs.length === 0 ? (
                <div className="empty-inline-flat">No background shell jobs.</div>
              ) : (
                <div className="hairline-list">
                  {jobs.map((j) => (
                    <div key={j.id} className="hairline-row">
                      <div className="hairline-row-main">
                        <div className="hairline-row-title mono-text">{j.command}</div>
                        <div className="hairline-row-meta tabular-nums">
                          {j.status} · {j.elapsed_secs}s
                        </div>
                      </div>
                      {j.status === 'running' && sessionId && (
                        <button
                          type="button"
                          className="btn btn-danger btn-xs"
                          onClick={() =>
                            api.killTask(sessionId, j.id, 'job').then(refreshTasks)
                          }
                        >
                          Kill
                        </button>
                      )}
                    </div>
                  ))}
                </div>
              )}
            </div>
          )}

          {activeTab === 'usage' && (
            <div className="drawer-section">
              <div className="drawer-subhead">
                <Cpu size={13} />
                <span>Context Window</span>
              </div>
              <div className="context-meter-bar">
                <div className="context-meter-fill" style={{ width: `${ctxPct}%` }} />
              </div>
              <div className="context-meter-caption tabular-nums">
                {contextTokens.toLocaleString()} / {contextWindow.toLocaleString()} tokens ({ctxPct}%)
              </div>

              {usage && (
                <>
                  <div className="drawer-subhead">Session Usage</div>
                  {usage.session ? (
                    <div className="metrics-grid tabular-nums">
                      <div className="metric-item">
                        <span className="metric-label">Turns</span>
                        <span className="metric-val">{usage.session.turns}</span>
                      </div>
                      <div className="metric-item">
                        <span className="metric-label">Input / Cached</span>
                        <span className="metric-val">
                          {usage.session.prompt_tokens.toLocaleString()} /{' '}
                          {usage.session.cached_tokens.toLocaleString()}
                        </span>
                      </div>
                      <div className="metric-item">
                        <span className="metric-label">Output</span>
                        <span className="metric-val">
                          {usage.session.completion_tokens.toLocaleString()}
                        </span>
                      </div>
                      <div className="metric-item">
                        <span className="metric-label">Est. Cost</span>
                        <span className="metric-val">
                          {usage.session.unpriced_turns === usage.session.turns &&
                          usage.session.turns > 0
                            ? 'Unpriced'
                            : `$${usage.session.cost_usd.toFixed(4)}`}
                        </span>
                      </div>
                    </div>
                  ) : (
                    <div className="empty-inline-flat">No turns recorded yet.</div>
                  )}

                  <div className="drawer-subhead">All-Time Ledger</div>
                  <div className="metrics-grid tabular-nums">
                    <div className="metric-item">
                      <span className="metric-label">Total Turns</span>
                      <span className="metric-val">{usage.global.turns}</span>
                    </div>
                    <div className="metric-item">
                      <span className="metric-label">Total Tokens</span>
                      <span className="metric-val">
                        {usage.global.total_tokens.toLocaleString()}
                      </span>
                    </div>
                    <div className="metric-item">
                      <span className="metric-label">Total Cost</span>
                      <span className="metric-val">${usage.global.cost_usd.toFixed(4)}</span>
                    </div>
                  </div>
                </>
              )}

              {heartbeat && (
                <>
                  <div className="drawer-subhead">
                    <Activity size={13} />
                    <span>System Heartbeat</span>
                  </div>
                  <div className="metrics-grid tabular-nums">
                    <div className="metric-item">
                      <span className="metric-label">PID / Memory</span>
                      <span className="metric-val">
                        {heartbeat.pid}
                        {heartbeat.rss_mb ? ` · ${heartbeat.rss_mb.toFixed(1)} MB` : ''}
                      </span>
                    </div>
                    <div className="metric-item">
                      <span className="metric-label">Git</span>
                      <span className="metric-val">
                        {heartbeat.git
                          ? `${heartbeat.git.branch} (${heartbeat.git.dirty} modified)`
                          : 'Non-git'}
                      </span>
                    </div>
                    <div className="metric-item">
                      <span className="metric-label">Tools / Skills / MCP</span>
                      <span className="metric-val">
                        {heartbeat.tools_count} / {heartbeat.skills_count} /{' '}
                        {heartbeat.mcp_servers_count}
                      </span>
                    </div>
                  </div>
                </>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
};
