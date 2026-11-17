import React, { useState } from 'react';
import type { ScheduleTask } from '../types';
import { Clock, Plus, Trash2, ToggleLeft, ToggleRight, PanelLeft } from 'lucide-react';

interface AutomationsViewProps {
  tasks: ScheduleTask[];
  sidebarOpen: boolean;
  onToggleSidebar: () => void;
  onCreateTask: (
    title: string,
    prompt: string,
    scheduleType: 'cron' | 'interval',
    cronExpr?: string,
    intervalSecs?: number
  ) => void;
  onToggleTask: (id: string, enabled: boolean) => void;
  onDeleteTask: (id: string) => void;
}

export const AutomationsView: React.FC<AutomationsViewProps> = ({
  tasks,
  sidebarOpen,
  onToggleSidebar,
  onCreateTask,
  onToggleTask,
  onDeleteTask,
}) => {
  const [showCreateModal, setShowCreateModal] = useState(false);
  const [title, setTitle] = useState('');
  const [prompt, setPrompt] = useState('');
  const [scheduleType, setScheduleType] = useState<'cron' | 'interval'>('cron');
  const [cronExpr, setCronExpr] = useState('0 9 * * 1-5');
  const [intervalSecs, setIntervalSecs] = useState(3600);

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!title.trim() || !prompt.trim()) return;
    onCreateTask(
      title.trim(),
      prompt.trim(),
      scheduleType,
      scheduleType === 'cron' ? cronExpr.trim() : undefined,
      scheduleType === 'interval' ? Number(intervalSecs) : undefined
    );
    setShowCreateModal(false);
    setTitle('');
    setPrompt('');
  };

  return (
    <div className="page-view">
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
          <span className="chat-header-title">Automations</span>
        </div>
        <div className="chat-header-right">
          <button
            type="button"
            className="btn btn-primary"
            onClick={() => setShowCreateModal(true)}
          >
            <Plus size={15} />
            <span>New automation</span>
          </button>
        </div>
      </header>

      <div className="page-scroll">
        <div className="page-container">
          <div className="page-intro">
            <h1 className="page-title">Scheduled automations</h1>
            <p className="page-subtitle">
              Cron and interval tasks executed in isolated sessions.
            </p>
          </div>

          {tasks.length === 0 ? (
            <div className="empty-state">
              <Clock size={24} className="empty-state-icon" />
              <div className="empty-state-title">No scheduled tasks</div>
              <p className="empty-state-desc">
                Create a cron or interval automation to run prompts automatically.
              </p>
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => setShowCreateModal(true)}
              >
                <Plus size={14} />
                <span>Create automation</span>
              </button>
            </div>
          ) : (
            <div className="surface-list">
              {tasks.map((t) => (
                <div key={t.id} className="surface-row">
                  <div className="surface-row-main">
                    <div className="surface-row-title-line">
                      <span className="surface-row-title">{t.title}</span>
                      <span className={`pill ${t.enabled ? 'pill-accent' : ''}`}>
                        {t.schedule_type === 'cron'
                          ? t.cron_expr
                          : `Every ${t.interval_secs}s`}
                      </span>
                    </div>
                    <div className="surface-row-meta">{t.prompt}</div>
                  </div>

                  <div className="surface-row-actions">
                    <button
                      type="button"
                      className="icon-btn"
                      title={t.enabled ? 'Disable' : 'Enable'}
                      onClick={() => onToggleTask(t.id, !t.enabled)}
                    >
                      {t.enabled ? (
                        <ToggleRight size={22} className="text-accent" />
                      ) : (
                        <ToggleLeft size={22} />
                      )}
                    </button>
                    <button
                      type="button"
                      className="icon-btn"
                      title="Delete"
                      onClick={() => onDeleteTask(t.id)}
                    >
                      <Trash2 size={15} />
                    </button>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
      </div>

      {showCreateModal && (
        <div
          className="modal-backdrop"
          onClick={() => setShowCreateModal(false)}
        >
          <form
            className="modal-card"
            onSubmit={handleSubmit}
            onClick={(e) => e.stopPropagation()}
          >
            <div className="modal-header">
              <span className="modal-title">Create scheduled task</span>
            </div>
            <div className="modal-body">
              <div className="form-group">
                <label className="form-label">Title</label>
                <input
                  type="text"
                  className="form-input"
                  required
                  placeholder="e.g. Daily repository triage"
                  value={title}
                  onChange={(e) => setTitle(e.target.value)}
                />
              </div>

              <div className="form-group">
                <label className="form-label">Prompt</label>
                <textarea
                  className="form-input"
                  rows={3}
                  required
                  placeholder="Describe what Luna should do on each run…"
                  value={prompt}
                  onChange={(e) => setPrompt(e.target.value)}
                />
              </div>

              <div className="form-group">
                <label className="form-label">Cadence</label>
                <div className="radio-inline-group">
                  <label className="radio-row">
                    <input
                      type="radio"
                      name="sched_type"
                      checked={scheduleType === 'cron'}
                      onChange={() => setScheduleType('cron')}
                    />
                    <span>Cron expression</span>
                  </label>
                  <label className="radio-row">
                    <input
                      type="radio"
                      name="sched_type"
                      checked={scheduleType === 'interval'}
                      onChange={() => setScheduleType('interval')}
                    />
                    <span>Interval (seconds)</span>
                  </label>
                </div>

                {scheduleType === 'cron' ? (
                  <input
                    type="text"
                    className="form-input"
                    value={cronExpr}
                    onChange={(e) => setCronExpr(e.target.value)}
                    placeholder="0 9 * * 1-5"
                  />
                ) : (
                  <input
                    type="number"
                    className="form-input"
                    value={intervalSecs}
                    onChange={(e) => setIntervalSecs(Number(e.target.value))}
                    min={10}
                  />
                )}
              </div>
            </div>
            <div className="modal-footer">
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => setShowCreateModal(false)}
              >
                Cancel
              </button>
              <button type="submit" className="btn btn-primary">
                Schedule
              </button>
            </div>
          </form>
        </div>
      )}
    </div>
  );
};
