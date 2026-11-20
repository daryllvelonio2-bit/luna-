import React, { useMemo, useState } from 'react';
import type { DeliveryReceipt, DeliveryTarget, ScheduleTask } from '../types';
import {
  Clock,
  Plus,
  Trash2,
  PanelLeft,
  Sparkles,
  Play,
  Pencil,
  Search,
  RefreshCw,
  ChevronDown,
  ChevronRight,
  ExternalLink,
  CalendarClock,
  CheckCircle2,
  AlertCircle,
  Layers,
  History,
  X,
  ArrowRight,
  Repeat,
  Globe,
} from 'lucide-react';
import {
  describeScheduleRule,
  formatScheduleRule,
  parseScheduleRule,
  getDeviceTimezone,
  formatTime12h,
  type CadenceKind,
} from '../scheduleUtils';

interface AutomationsViewProps {
  tasks: ScheduleTask[];
  sidebarOpen: boolean;
  onToggleSidebar: () => void;
  onCreateTask: (payload: {
    name: string;
    instruction: string;
    rule: string;
    target?: 'new_session_each_run' | 'single_new_session' | 'existing_session';
    session_id?: string;
  }) => Promise<void> | void;
  onUpdateTask: (
    id: string,
    payload: {
      name?: string;
      instruction?: string;
      rule?: string;
      paused?: boolean;
      target?: 'new_session_each_run' | 'single_new_session' | 'existing_session';
    }
  ) => Promise<void> | void;
  onToggleTask: (id: string, paused: boolean) => Promise<void> | void;
  onRunTask: (id: string) => Promise<string | null>;
  onDeleteTask: (id: string) => Promise<void> | void;
  onStartAutomationConversation: (prefill?: string) => void;
  onOpenSession: (sessionId: string) => void;
  onRefresh: () => void;
}

type FilterTab = 'all' | 'active' | 'paused' | 'history';

const GUIDED_STARTERS = [
  {
    label: 'Daily 8:00 PM deliverable digest',
    prompt:
      'Every day at 8pm, review project progress, inspect open tasks, and summarize next steps into a concise brief.',
  },
  {
    label: 'Two-stage research & build pipeline',
    prompt:
      'Every day at 7pm, research top trends in my niche and save structured notes. Then every day at 8pm, read those notes and generate the next deliverable.',
  },
  {
    label: 'Daily morning summary brief',
    prompt:
      'Every morning at 8am, review recent updates, open tasks, and key metrics, then compile an action plan.',
  },
  {
    label: 'Weekday repository & PR triage',
    prompt:
      'Every weekday at 9am, inspect open issues and pull requests, highlight blockers or failing checks, and draft prioritized next steps.',
  },
];

function formatRelativeTime(iso?: string | null, future = false): string {
  if (!iso) return future ? 'Not scheduled' : 'Never run';
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  const diffMs = future ? date.getTime() - Date.now() : Date.now() - date.getTime();
  if (future && diffMs <= 0) return 'Due now';
  if (!future && diffMs < 60_000) return 'Just now';

  const mins = Math.floor(Math.abs(diffMs) / 60_000);
  const hours = Math.floor(mins / 60);
  const days = Math.floor(hours / 24);

  let span = '';
  if (days > 0) {
    const remH = hours % 24;
    span = remH > 0 ? `${days}d ${remH}h` : `${days}d`;
  } else if (hours > 0) {
    const remM = mins % 60;
    span = remM > 0 ? `${hours}h ${remM}m` : `${hours}h`;
  } else {
    span = `${Math.max(1, mins)}m`;
  }
  return future ? `in ${span}` : `${span} ago`;
}

function formatTargetLabel(target: DeliveryTarget): string {
  switch (target.kind) {
    case 'new_session_each_run':
      return 'Fresh session';
    case 'single_new_session':
      return 'Dedicated thread';
    case 'existing_session':
      return `Session ${target.session_id.slice(0, 8)}`;
  }
}

export const AutomationsView: React.FC<AutomationsViewProps> = ({
  tasks,
  sidebarOpen,
  onToggleSidebar,
  onCreateTask,
  onUpdateTask,
  onToggleTask,
  onRunTask,
  onDeleteTask,
  onStartAutomationConversation,
  onOpenSession,
  onRefresh,
}) => {
  const [filter, setFilter] = useState<FilterTab>('all');
  const [search, setSearch] = useState('');
  const [expandedPrompts, setExpandedPrompts] = useState<Record<string, boolean>>({});
  const [expandedHistory, setExpandedHistory] = useState<Record<string, boolean>>({});
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);
  const [runningSessions, setRunningSessions] = useState<Record<string, string>>({});
  const [startingRunId, setStartingRunId] = useState<string | null>(null);

  // Structured Editor modal (creates new or modifies existing automations)
  const [showEditorModal, setShowEditorModal] = useState(false);
  const [editingTask, setEditingTask] = useState<ScheduleTask | null>(null);
  const [name, setName] = useState('');
  const [instruction, setInstruction] = useState('');
  const [cadenceKind, setCadenceKind] = useState<CadenceKind>('daily');
  const [isLocal, setIsLocal] = useState(true);
  const [dailyTime, setDailyTime] = useState('20:00');
  const [intervalValue, setIntervalValue] = useState(1);
  const [intervalUnit, setIntervalUnit] = useState<'minutes' | 'hours' | 'seconds'>('hours');
  const [cronExpr, setCronExpr] = useState('0 20 * * *');
  const [onceValue, setOnceValue] = useState('');
  const [targetKind, setTargetKind] = useState<'new_session_each_run' | 'single_new_session'>(
    'new_session_each_run'
  );
  const [saving, setSaving] = useState(false);

  const deviceTz = useMemo(() => getDeviceTimezone(), []);

  const counts = useMemo(() => {
    let active = 0;
    let paused = 0;
    let runs = 0;
    for (const t of tasks) {
      if (t.paused || t.completed) paused += 1;
      else active += 1;
      runs += t.history?.length ?? 0;
    }
    return { all: tasks.length, active, paused, runs };
  }, [tasks]);

  const filteredTasks = useMemo(() => {
    const q = search.trim().toLowerCase();
    return tasks.filter((t) => {
      if (filter === 'active' && (t.paused || t.completed)) return false;
      if (filter === 'paused' && !t.paused && !t.completed) return false;
      if (!q) return true;
      return (
        t.name.toLowerCase().includes(q) ||
        t.instruction.toLowerCase().includes(q) ||
        t.rule.toLowerCase().includes(q) ||
        t.id.toLowerCase().includes(q)
      );
    });
  }, [tasks, filter, search]);

  const allHistory = useMemo(() => {
    const entries: Array<{ task: ScheduleTask; receipt: DeliveryReceipt }> = [];
    for (const task of tasks) {
      for (const receipt of task.history || []) {
        entries.push({ task, receipt });
      }
    }
    entries.sort(
      (a, b) =>
        new Date(b.receipt.delivered_at).getTime() - new Date(a.receipt.delivered_at).getTime()
    );
    const q = search.trim().toLowerCase();
    if (!q) return entries;
    return entries.filter(
      ({ task, receipt }) =>
        task.name.toLowerCase().includes(q) ||
        receipt.summary.toLowerCase().includes(q) ||
        receipt.status.toLowerCase().includes(q)
    );
  }, [tasks, search]);

  const computedRule = useMemo(() => {
    if (cadenceKind === 'daily') {
      return formatScheduleRule({
        kind: 'daily',
        isLocal,
        dailyTime: dailyTime || '20:00',
      });
    }
    if (cadenceKind === 'interval') {
      const mult = intervalUnit === 'hours' ? 3600 : intervalUnit === 'minutes' ? 60 : 1;
      const secs = Math.max(10, Math.round((Number(intervalValue) || 1) * mult));
      return formatScheduleRule({
        kind: 'interval',
        intervalSecs: secs,
      });
    }
    if (cadenceKind === 'once') {
      if (!onceValue) return 'once:';
      const dt = new Date(onceValue);
      return formatScheduleRule({
        kind: 'once',
        onceIso: Number.isNaN(dt.getTime()) ? onceValue : dt.toISOString(),
      });
    }
    return formatScheduleRule({
      kind: 'cron',
      isLocal,
      cronExpr: cronExpr.trim() || '0 20 * * *',
    });
  }, [cadenceKind, isLocal, dailyTime, intervalValue, intervalUnit, cronExpr, onceValue]);

  const openCreateModal = () => {
    setEditingTask(null);
    setName('');
    setInstruction('');
    setCadenceKind('daily');
    setIsLocal(true);
    setDailyTime('20:00');
    setIntervalValue(1);
    setIntervalUnit('hours');
    setCronExpr('0 20 * * *');
    setOnceValue('');
    setTargetKind('new_session_each_run');
    setShowEditorModal(true);
  };

  const openEditModal = (task: ScheduleTask) => {
    setEditingTask(task);
    setName(task.name);
    setInstruction(task.instruction);
    const parsed = parseScheduleRule(task.rule);
    setCadenceKind(parsed.kind);
    setIsLocal(parsed.isLocal);
    if (parsed.dailyTime) setDailyTime(parsed.dailyTime);
    if (parsed.intervalSecs) {
      if (parsed.intervalSecs % 3600 === 0) {
        setIntervalValue(parsed.intervalSecs / 3600);
        setIntervalUnit('hours');
      } else if (parsed.intervalSecs % 60 === 0) {
        setIntervalValue(parsed.intervalSecs / 60);
        setIntervalUnit('minutes');
      } else {
        setIntervalValue(parsed.intervalSecs);
        setIntervalUnit('seconds');
      }
    }
    if (parsed.cronExpr) setCronExpr(parsed.cronExpr);
    if (parsed.onceIso) {
      const dt = new Date(parsed.onceIso);
      const localIso = Number.isNaN(dt.getTime())
        ? ''
        : new Date(dt.getTime() - dt.getTimezoneOffset() * 60_000).toISOString().slice(0, 16);
      setOnceValue(localIso);
    }
    setTargetKind(
      task.delivery_target?.kind === 'single_new_session'
        ? 'single_new_session'
        : 'new_session_each_run'
    );
    setShowEditorModal(true);
  };

  const handleSaveEditor = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!name.trim() || !instruction.trim()) return;
    setSaving(true);
    try {
      if (editingTask) {
        await onUpdateTask(editingTask.id, {
          name: name.trim(),
          instruction: instruction.trim(),
          rule: computedRule,
          target: targetKind,
        });
      } else {
        await onCreateTask({
          name: name.trim(),
          instruction: instruction.trim(),
          rule: computedRule,
          target: targetKind,
        });
      }
      setShowEditorModal(false);
    } finally {
      setSaving(false);
    }
  };

  const handleTriggerRun = async (taskId: string) => {
    setStartingRunId(taskId);
    try {
      const sid = await onRunTask(taskId);
      if (sid) {
        setRunningSessions((prev) => ({ ...prev, [taskId]: sid }));
      }
    } finally {
      setStartingRunId(null);
    }
  };

  return (
    <div className="page-view auto-view">
      <header className="chat-header auto-header">
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
          <div className="auto-header-title">
            <span className="auto-header-icon" aria-hidden="true">
              <Clock size={15} />
            </span>
            <span className="chat-header-title">Automations</span>
          </div>
        </div>

        <div className="auto-header-actions">
          <div className="auto-search-box">
            <Search size={13} className="auto-search-icon" aria-hidden="true" />
            <input
              type="text"
              className="auto-search-input"
              placeholder="Search automations..."
              value={search}
              onChange={(e) => setSearch(e.target.value)}
              aria-label="Search automations"
            />
            {search && (
              <button
                type="button"
                className="auto-search-clear"
                onClick={() => setSearch('')}
                aria-label="Clear search"
              >
                <X size={12} />
              </button>
            )}
          </div>

          <button
            type="button"
            className="icon-btn"
            onClick={onRefresh}
            title="Refresh automations"
            aria-label="Refresh automations"
          >
            <RefreshCw size={14} />
          </button>

          <button
            type="button"
            className="btn btn-secondary"
            onClick={() => onStartAutomationConversation()}
            title="Design a workflow conversationally in chat"
          >
            <Sparkles size={13} />
            <span>Guide in chat</span>
          </button>

          <button
            type="button"
            className="btn btn-primary"
            onClick={openCreateModal}
          >
            <Plus size={14} />
            <span>New automation</span>
          </button>
        </div>
      </header>

      <div className="page-scroll">
        <div className="auto-container">
          <div className="auto-hero">
            <div className="auto-hero-copy">
              <h1 className="auto-title">Scheduled Automations</h1>
              <p className="auto-subtitle">
                Autonomous background agents running on schedule. Configured times run directly
                at your device's local clock time (<code>{deviceTz}</code>).
              </p>
            </div>

            <div className="auto-stats-strip">
              <div className="auto-stat-pill">
                <span className="auto-dot auto-dot-active" />
                <span className="auto-stat-val">{counts.active}</span>
                <span className="auto-stat-label">Active</span>
              </div>
              <div className="auto-stat-pill">
                <span className="auto-dot auto-dot-paused" />
                <span className="auto-stat-val">{counts.paused}</span>
                <span className="auto-stat-label">Paused</span>
              </div>
              <div className="auto-stat-pill">
                <History size={12} className="text-muted" />
                <span className="auto-stat-val">{counts.runs}</span>
                <span className="auto-stat-label">Runs</span>
              </div>
            </div>
          </div>

          <div className="auto-filter-bar" role="tablist" aria-label="Automation filters">
            <button
              type="button"
              role="tab"
              aria-selected={filter === 'all'}
              className={`auto-filter-tab ${filter === 'all' ? 'active' : ''}`}
              onClick={() => setFilter('all')}
            >
              <span>All</span>
              <span className="auto-filter-count">{counts.all}</span>
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={filter === 'active'}
              className={`auto-filter-tab ${filter === 'active' ? 'active' : ''}`}
              onClick={() => setFilter('active')}
            >
              <span className="auto-dot auto-dot-active" />
              <span>Active</span>
              <span className="auto-filter-count">{counts.active}</span>
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={filter === 'paused'}
              className={`auto-filter-tab ${filter === 'paused' ? 'active' : ''}`}
              onClick={() => setFilter('paused')}
            >
              <span>Paused</span>
              <span className="auto-filter-count">{counts.paused}</span>
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={filter === 'history'}
              className={`auto-filter-tab ${filter === 'history' ? 'active' : ''}`}
              onClick={() => setFilter('history')}
            >
              <History size={13} />
              <span>Run History</span>
              <span className="auto-filter-count">{counts.runs}</span>
            </button>
          </div>

          {filter === 'history' ? (
            allHistory.length === 0 ? (
              <div className="auto-empty">
                <div className="auto-empty-badge">
                  <History size={22} />
                </div>
                <div className="auto-empty-title">No automation runs yet</div>
                <p className="auto-empty-desc">
                  Execution logs appear here once an automation runs on schedule or via Run now.
                </p>
              </div>
            ) : (
              <div className="auto-timeline">
                {allHistory.map(({ task, receipt }, idx) => {
                  const ok = receipt.status !== 'failed';
                  return (
                    <div
                      key={`${task.id}-${receipt.delivered_at}-${idx}`}
                      className="auto-timeline-row"
                    >
                      <div className="auto-timeline-left">
                        <span className={`auto-status-chip ${ok ? 'is-ok' : 'is-err'}`}>
                          {ok ? <CheckCircle2 size={12} /> : <AlertCircle size={12} />}
                          <span>{ok ? 'Delivered' : 'Failed'}</span>
                        </span>
                        <div className="auto-timeline-main">
                          <div className="auto-timeline-head">
                            <span className="auto-timeline-name">{task.name}</span>
                            <span className="auto-timeline-time">
                              {formatRelativeTime(receipt.delivered_at, false)} ·{' '}
                              {new Date(receipt.delivered_at).toLocaleString()}
                            </span>
                          </div>
                          <p className="auto-timeline-summary">{receipt.summary}</p>
                        </div>
                      </div>
                      {receipt.session_id && (
                        <button
                          type="button"
                          className="btn btn-secondary auto-open-session-btn"
                          onClick={() => onOpenSession(receipt.session_id)}
                        >
                          <span>Open session</span>
                          <ExternalLink size={12} />
                        </button>
                      )}
                    </div>
                  );
                })}
              </div>
            )
          ) : filteredTasks.length === 0 ? (
            <div className="auto-empty">
              <div className="auto-empty-badge">
                <CalendarClock size={24} />
              </div>
              <div className="auto-empty-title">
                {tasks.length === 0 ? 'Create your first automation' : 'No matching automations'}
              </div>
              <p className="auto-empty-desc">
                {tasks.length === 0
                  ? 'Schedule unattended tasks to run at exact local clock times. Say "every day at 8pm" and Luna runs it at 8:00 PM.'
                  : 'Try adjusting your filter tab or search query.'}
              </p>
              {tasks.length === 0 && (
                <div className="auto-empty-actions" style={{ display: 'flex', gap: '8px' }}>
                  <button
                    type="button"
                    className="btn btn-primary"
                    onClick={openCreateModal}
                  >
                    <Plus size={14} />
                    <span>New automation</span>
                  </button>
                  <button
                    type="button"
                    className="btn btn-secondary"
                    onClick={() => onStartAutomationConversation()}
                  >
                    <Sparkles size={14} />
                    <span>Guide with chat</span>
                  </button>
                </div>
              )}
              {tasks.length === 0 && (
                <div className="auto-starters" style={{ marginTop: '20px', maxWidth: '640px' }}>
                  <div className="auto-starters-label">Starter workflows</div>
                  <div className="auto-starter-chips">
                    {GUIDED_STARTERS.map((s) => (
                      <button
                        key={s.label}
                        type="button"
                        className="auto-starter-chip"
                        onClick={() =>
                          onStartAutomationConversation(
                            `/automation guide me with creating an automation to ${s.prompt}`
                          )
                        }
                      >
                        <Sparkles size={11} />
                        <span>{s.label}</span>
                      </button>
                    ))}
                  </div>
                </div>
              )}
            </div>
          ) : (
            <div className="auto-grid">
              {filteredTasks.map((task) => {
                const isActive = !task.paused && !task.completed;
                const promptExpanded = !!expandedPrompts[task.id];
                const historyExpanded = !!expandedHistory[task.id];
                const recentRuns = [...(task.history || [])].reverse();
                const latestRun = recentRuns[0];
                const activeSession = runningSessions[task.id];
                const isStarting = startingRunId === task.id;
                const isConfirmDelete = confirmDeleteId === task.id;
                const ruleDesc = describeScheduleRule(task.rule);

                return (
                  <article
                    key={task.id}
                    className={`auto-card ${isActive ? 'auto-card-active' : 'auto-card-paused'}`}
                  >
                    <div className="auto-card-top">
                      <div className="auto-card-status-group">
                        <span
                          className={`auto-status-badge ${
                            task.completed
                              ? 'status-completed'
                              : isActive
                                ? 'status-active'
                                : 'status-paused'
                          }`}
                        >
                          <span
                            className={`auto-dot ${
                              isActive ? 'auto-dot-active auto-dot-pulse' : 'auto-dot-paused'
                            }`}
                          />
                          <span>
                            {task.completed ? 'Completed' : isActive ? 'Active' : 'Paused'}
                          </span>
                        </span>
                        <span className="auto-id-chip" title={task.id}>
                          #{task.id.slice(0, 8)}
                        </span>
                      </div>

                      <div className="auto-card-controls">
                        <button
                          type="button"
                          className="auto-action-btn auto-action-run"
                          title="Run now"
                          disabled={isStarting}
                          onClick={() => handleTriggerRun(task.id)}
                        >
                          <Play size={12} />
                          <span>{isStarting ? 'Starting…' : 'Run now'}</span>
                        </button>

                        <button
                          type="button"
                          className="auto-icon-btn"
                          title="Edit automation"
                          onClick={() => openEditModal(task)}
                        >
                          <Pencil size={13} />
                        </button>

                        <button
                          type="button"
                          role="switch"
                          aria-checked={isActive}
                          aria-label={isActive ? 'Pause automation' : 'Resume automation'}
                          className={`auto-switch ${isActive ? 'checked' : ''}`}
                          onClick={() => onToggleTask(task.id, !task.paused)}
                        >
                          <span className="auto-switch-thumb" />
                        </button>

                        {isConfirmDelete ? (
                          <button
                            type="button"
                            className="auto-action-btn auto-action-danger"
                            onClick={() => {
                              setConfirmDeleteId(null);
                              onDeleteTask(task.id);
                            }}
                            onBlur={() => setConfirmDeleteId(null)}
                          >
                            Confirm
                          </button>
                        ) : (
                          <button
                            type="button"
                            className="auto-icon-btn auto-icon-danger"
                            title="Delete automation"
                            onClick={() => setConfirmDeleteId(task.id)}
                          >
                            <Trash2 size={13} />
                          </button>
                        )}
                      </div>
                    </div>

                    <div className="auto-card-heading">
                      <h3 className="auto-card-title">{task.name}</h3>
                      <div className="auto-cadence-row">
                        <span className="auto-cadence-human">
                          <Repeat size={12} />
                          <span>{ruleDesc.summary}</span>
                        </span>
                        {ruleDesc.isLocal ? (
                          <span className="auto-badge-local" title={ruleDesc.detail}>
                            <Globe size={10} />
                            <span>Device local</span>
                          </span>
                        ) : (
                          <span className="auto-badge-utc" title={ruleDesc.detail}>
                            <span>UTC</span>
                          </span>
                        )}
                        <code className="auto-rule-code">{task.rule}</code>
                      </div>
                    </div>

                    <div className="auto-prompt-box">
                      <div
                        className={`auto-prompt-text ${promptExpanded ? 'expanded' : 'clamped'}`}
                      >
                        {task.instruction}
                      </div>
                      {task.instruction.length > 160 && (
                        <button
                          type="button"
                          className="auto-prompt-more"
                          onClick={() =>
                            setExpandedPrompts((prev) => ({
                              ...prev,
                              [task.id]: !prev[task.id],
                            }))
                          }
                        >
                          {promptExpanded ? 'Show less' : 'Show full prompt'}
                        </button>
                      )}
                    </div>

                    {activeSession && (
                      <div className="auto-live-banner">
                        <span className="auto-live-left">
                          <span className="auto-dot auto-dot-active auto-dot-pulse" />
                          <span>Manual run launched</span>
                        </span>
                        <button
                          type="button"
                          className="auto-live-link"
                          onClick={() => onOpenSession(activeSession)}
                        >
                          <span>Watch live</span>
                          <ArrowRight size={12} />
                        </button>
                      </div>
                    )}

                    <div className="auto-card-meta">
                      <div className="auto-meta-item">
                        <Clock size={12} />
                        <span className="auto-meta-label">Next run:</span>
                        <span className="auto-meta-value">
                          {task.paused
                            ? 'Paused'
                            : formatRelativeTime(task.next_run_at, true)}
                        </span>
                      </div>
                      <div className="auto-meta-item">
                        <Layers size={12} />
                        <span className="auto-meta-value">
                          {formatTargetLabel(task.delivery_target)}
                        </span>
                      </div>
                    </div>

                    <div className="auto-card-footer">
                      <button
                        type="button"
                        className="auto-history-toggle"
                        onClick={() =>
                          setExpandedHistory((prev) => ({
                            ...prev,
                            [task.id]: !prev[task.id],
                          }))
                        }
                      >
                        {historyExpanded ? <ChevronDown size={13} /> : <ChevronRight size={13} />}
                        <span>Recent runs ({recentRuns.length})</span>
                        {latestRun && (
                          <span
                            className={`auto-last-status ${
                              latestRun.status === 'failed' ? 'is-err' : 'is-ok'
                            }`}
                          >
                            · {formatRelativeTime(latestRun.delivered_at, false)}
                          </span>
                        )}
                      </button>

                      {latestRun?.session_id && (
                        <button
                          type="button"
                          className="auto-footer-link"
                          onClick={() => onOpenSession(latestRun.session_id)}
                        >
                          <span>Last session</span>
                          <ExternalLink size={11} />
                        </button>
                      )}
                    </div>

                    {historyExpanded && (
                      <div className="auto-history-drawer">
                        {recentRuns.length === 0 ? (
                          <div className="auto-history-empty">
                            No runs recorded yet. Click Run now to test immediately.
                          </div>
                        ) : (
                          recentRuns.slice(0, 6).map((r, i) => {
                            const ok = r.status !== 'failed';
                            return (
                              <div key={`${r.delivered_at}-${i}`} className="auto-receipt-row">
                                <div className="auto-receipt-top">
                                  <span
                                    className={`auto-status-chip ${ok ? 'is-ok' : 'is-err'}`}
                                  >
                                    {ok ? (
                                      <CheckCircle2 size={11} />
                                    ) : (
                                      <AlertCircle size={11} />
                                    )}
                                    <span>{ok ? 'Delivered' : 'Failed'}</span>
                                  </span>
                                  <span className="auto-receipt-time">
                                    {new Date(r.delivered_at).toLocaleString()}
                                  </span>
                                  {r.session_id && (
                                    <button
                                      type="button"
                                      className="auto-footer-link"
                                      onClick={() => onOpenSession(r.session_id)}
                                    >
                                      <span>Open</span>
                                      <ExternalLink size={11} />
                                    </button>
                                  )}
                                </div>
                                <div className="auto-receipt-summary">{r.summary}</div>
                              </div>
                            );
                          })
                        )}
                      </div>
                    )}
                  </article>
                );
              })}
            </div>
          )}
        </div>
      </div>

      {/* Structured Modal (for creating or editing scheduled automations) */}
      {showEditorModal && (
        <div className="modal-backdrop" onClick={() => setShowEditorModal(false)}>
          <form
            className="modal-card auto-modal-wide"
            onSubmit={handleSaveEditor}
            onClick={(e) => e.stopPropagation()}
          >
            <div className="modal-header auto-modal-header">
              <span className="modal-title">
                {editingTask ? `Edit automation: ${editingTask.name}` : 'New scheduled automation'}
              </span>
              <button
                type="button"
                className="icon-btn"
                onClick={() => setShowEditorModal(false)}
                aria-label="Close"
              >
                <X size={15} />
              </button>
            </div>

            <div className="modal-body">
              <div className="form-group">
                <label className="form-label">Name</label>
                <input
                  type="text"
                  className="form-input"
                  required
                  autoFocus
                  placeholder="e.g. Daily 8:00 PM deliverable digest"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                />
              </div>

              <div className="form-group">
                <label className="form-label">Schedule cadence</label>
                <div className="auto-cadence-tabs">
                  <button
                    type="button"
                    className={`auto-cadence-tab ${cadenceKind === 'daily' ? 'active' : ''}`}
                    onClick={() => {
                      setCadenceKind('daily');
                      setIsLocal(true);
                    }}
                  >
                    Daily
                  </button>
                  <button
                    type="button"
                    className={`auto-cadence-tab ${cadenceKind === 'interval' ? 'active' : ''}`}
                    onClick={() => setCadenceKind('interval')}
                  >
                    Interval
                  </button>
                  <button
                    type="button"
                    className={`auto-cadence-tab ${cadenceKind === 'cron' ? 'active' : ''}`}
                    onClick={() => setCadenceKind('cron')}
                  >
                    Cron (Advanced)
                  </button>
                  <button
                    type="button"
                    className={`auto-cadence-tab ${cadenceKind === 'once' ? 'active' : ''}`}
                    onClick={() => setCadenceKind('once')}
                  >
                    One-shot
                  </button>
                </div>

                {cadenceKind === 'daily' && (
                  <div className="auto-cadence-block">
                    <div className="auto-cadence-field">
                      <input
                        type="time"
                        className="form-input"
                        value={dailyTime}
                        onChange={(e) => setDailyTime(e.target.value)}
                        required
                      />
                      <div className="auto-tz-toggle">
                        <button
                          type="button"
                          className={`auto-tz-btn ${isLocal ? 'active' : ''}`}
                          onClick={() => setIsLocal(true)}
                        >
                          Device local ({deviceTz})
                        </button>
                        <button
                          type="button"
                          className={`auto-tz-btn ${!isLocal ? 'active' : ''}`}
                          onClick={() => setIsLocal(false)}
                        >
                          UTC (Fixed)
                        </button>
                      </div>
                    </div>
                    <span className="auto-field-hint">
                      {isLocal
                        ? `Runs every day at ${formatTime12h(dailyTime || '20:00')} using Luna device local time`
                        : `Runs every day at ${dailyTime || '20:00'} UTC`}
                    </span>
                  </div>
                )}

                {cadenceKind === 'interval' && (
                  <div className="auto-cadence-field">
                    <input
                      type="number"
                      min={1}
                      className="form-input"
                      value={intervalValue}
                      onChange={(e) => setIntervalValue(Number(e.target.value))}
                    />
                    <select
                      className="form-input"
                      value={intervalUnit}
                      onChange={(e) =>
                        setIntervalUnit(e.target.value as 'minutes' | 'hours' | 'seconds')
                      }
                    >
                      <option value="hours">Hours</option>
                      <option value="minutes">Minutes</option>
                      <option value="seconds">Seconds</option>
                    </select>
                  </div>
                )}

                {cadenceKind === 'cron' && (
                  <div className="auto-cadence-block">
                    <div className="auto-cadence-field">
                      <input
                        type="text"
                        className="form-input"
                        value={cronExpr}
                        onChange={(e) => setCronExpr(e.target.value)}
                        placeholder="0 20 * * *"
                        required
                      />
                      <div className="auto-tz-toggle">
                        <button
                          type="button"
                          className={`auto-tz-btn ${isLocal ? 'active' : ''}`}
                          onClick={() => setIsLocal(true)}
                        >
                          Device local
                        </button>
                        <button
                          type="button"
                          className={`auto-tz-btn ${!isLocal ? 'active' : ''}`}
                          onClick={() => setIsLocal(false)}
                        >
                          UTC
                        </button>
                      </div>
                    </div>
                    <span className="auto-field-hint">
                      {isLocal
                        ? `5 fields (min hour dom mon dow) evaluated in device local time (${deviceTz})`
                        : '5 fields (min hour dom mon dow) evaluated in UTC'}
                    </span>
                  </div>
                )}

                {cadenceKind === 'once' && (
                  <div className="auto-cadence-field">
                    <input
                      type="datetime-local"
                      className="form-input"
                      value={onceValue}
                      onChange={(e) => setOnceValue(e.target.value)}
                      required
                    />
                  </div>
                )}

                <div className="auto-rule-preview">
                  <span>Rule:</span>
                  <code>{computedRule}</code>
                  <span className="auto-rule-preview-human">
                    · {describeScheduleRule(computedRule).summary}
                  </span>
                </div>
              </div>

              <div className="form-group">
                <label className="form-label">Session target</label>
                <div className="auto-cadence-tabs">
                  <button
                    type="button"
                    className={`auto-cadence-tab ${
                      targetKind === 'new_session_each_run' ? 'active' : ''
                    }`}
                    onClick={() => setTargetKind('new_session_each_run')}
                  >
                    Fresh session each run
                  </button>
                  <button
                    type="button"
                    className={`auto-cadence-tab ${
                      targetKind === 'single_new_session' ? 'active' : ''
                    }`}
                    onClick={() => setTargetKind('single_new_session')}
                  >
                    Single dedicated thread
                  </button>
                </div>
              </div>

              <div className="form-group">
                <label className="form-label">Agent instruction</label>
                <textarea
                  className="form-input"
                  rows={5}
                  required
                  placeholder="Describe the exact steps, inputs, tools, and outputs for each scheduled run..."
                  value={instruction}
                  onChange={(e) => setInstruction(e.target.value)}
                />
              </div>
            </div>

            <div
              className="modal-footer"
              style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}
            >
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => {
                  setShowEditorModal(false);
                  onStartAutomationConversation(
                    editingTask
                      ? `/automation guide me with updating automation "${editingTask.name}" `
                      : '/automation guide me with creating an automation '
                  );
                }}
              >
                <Sparkles size={13} />
                <span>Guide in chat</span>
              </button>

              <div style={{ display: 'flex', gap: '8px' }}>
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={() => setShowEditorModal(false)}
                >
                  Cancel
                </button>
                <button type="submit" className="btn btn-primary" disabled={saving}>
                  {saving ? 'Saving…' : editingTask ? 'Save changes' : 'Create automation'}
                </button>
              </div>
            </div>
          </form>
        </div>
      )}
    </div>
  );
};
