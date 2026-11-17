import React, { useState } from 'react';
import type { ScheduleTask } from '../types';
import { Clock, Plus, Trash2, ToggleLeft, ToggleRight, Play } from 'lucide-react';

interface AutomationsViewProps {
  tasks: ScheduleTask[];
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
    <div style={{ flex: 1, padding: '24px', overflowY: 'auto' }}>
      <div
        style={{
          display: 'flex',
          justifyContent: 'space-between',
          alignItems: 'center',
          marginBottom: '20px',
        }}
      >
        <div>
          <h2 style={{ fontSize: '18px', fontWeight: 600 }}>Scheduled Automations</h2>
          <p style={{ color: 'var(--text-secondary)', fontSize: '13px', marginTop: '4px' }}>
            Cron and interval agent jobs with isolated execution and delivery.
          </p>
        </div>
        <button className="primary-btn" onClick={() => setShowCreateModal(true)}>
          <Plus size={16} />
          <span>New Automation</span>
        </button>
      </div>

      <div style={{ display: 'flex', flexDirection: 'column', gap: '12px' }}>
        {tasks.length === 0 ? (
          <div
            style={{
              padding: '40px',
              textAlign: 'center',
              backgroundColor: 'var(--bg-secondary)',
              borderRadius: '8px',
              border: '1px solid var(--border-color)',
              color: 'var(--text-secondary)',
            }}
          >
            <Clock size={32} style={{ margin: '0 auto 12px', opacity: 0.5 }} />
            <p>No scheduled tasks registered yet. Click "New Automation" to schedule proactive runs.</p>
          </div>
        ) : (
          tasks.map((t) => (
            <div
              key={t.id}
              style={{
                backgroundColor: 'var(--bg-secondary)',
                border: '1px solid var(--border-color)',
                borderRadius: '8px',
                padding: '16px',
                display: 'flex',
                alignItems: 'center',
                justifyContent: 'space-between',
              }}
            >
              <div>
                <div style={{ display: 'flex', alignItems: 'center', gap: '10px' }}>
                  <span style={{ fontWeight: 600, fontSize: '15px' }}>{t.title}</span>
                  <span className={`badge ${t.enabled ? 'active' : ''}`}>
                    {t.schedule_type === 'cron' ? `Cron: ${t.cron_expr}` : `Every ${t.interval_secs}s`}
                  </span>
                </div>
                <div
                  style={{
                    color: 'var(--text-secondary)',
                    fontSize: '13px',
                    marginTop: '6px',
                    maxWidth: '600px',
                    whiteSpace: 'nowrap',
                    overflow: 'hidden',
                    textOverflow: 'ellipsis',
                  }}
                >
                  Prompt: {t.prompt}
                </div>
              </div>

              <div style={{ display: 'flex', alignItems: 'center', gap: '12px' }}>
                <button
                  className="action-btn"
                  title={t.enabled ? 'Disable' : 'Enable'}
                  onClick={() => onToggleTask(t.id, !t.enabled)}
                >
                  {t.enabled ? (
                    <ToggleRight size={24} color="var(--accent-color)" />
                  ) : (
                    <ToggleLeft size={24} />
                  )}
                </button>
                <button
                  className="action-btn"
                  title="Delete"
                  onClick={() => onDeleteTask(t.id)}
                >
                  <Trash2 size={16} />
                </button>
              </div>
            </div>
          ))
        )}
      </div>

      {showCreateModal && (
        <div className="modal-backdrop">
          <form className="modal-card" onSubmit={handleSubmit}>
            <div className="modal-header">
              <span style={{ fontWeight: 600 }}>Create Scheduled Task</span>
            </div>
            <div className="modal-body">
              <div className="form-group">
                <label className="form-label">Task Title</label>
                <input
                  type="text"
                  className="form-input"
                  required
                  placeholder="e.g. Daily GitHub Issue Triage"
                  value={title}
                  onChange={(e) => setTitle(e.target.value)}
                />
              </div>

              <div className="form-group">
                <label className="form-label">Prompt / Objective</label>
                <textarea
                  className="form-input"
                  rows={3}
                  required
                  placeholder="e.g. Check newly opened issues on repository and summarize in notes."
                  value={prompt}
                  onChange={(e) => setPrompt(e.target.value)}
                />
              </div>

              <div className="form-group">
                <label className="form-label">Trigger Cadence</label>
                <div style={{ display: 'flex', gap: '16px', marginBottom: '8px' }}>
                  <label style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                    <input
                      type="radio"
                      name="sched_type"
                      checked={scheduleType === 'cron'}
                      onChange={() => setScheduleType('cron')}
                    />
                    <span>Cron Expression</span>
                  </label>
                  <label style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
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
                className="secondary-btn"
                onClick={() => setShowCreateModal(false)}
              >
                Cancel
              </button>
              <button type="submit" className="primary-btn">
                Schedule
              </button>
            </div>
          </form>
        </div>
      )}
    </div>
  );
};
