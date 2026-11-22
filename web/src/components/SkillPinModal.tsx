import React, { useState, useEffect, useCallback, useMemo } from 'react';
import { api } from '../api';
import type { SkillSummary, PinnedSkillsResponse } from '../types';
import { Pin, Check, X, Search, Globe, MessageSquare, Trash2, Sparkles } from 'lucide-react';

interface SkillPinModalProps {
  isOpen: boolean;
  onClose: () => void;
  sessionId?: string | null;
  skills: SkillSummary[];
  onPinnedChanged?: (res: PinnedSkillsResponse) => void;
}

export const SkillPinModal: React.FC<SkillPinModalProps> = ({
  isOpen,
  onClose,
  sessionId,
  skills,
  onPinnedChanged,
}) => {
  const [pinnedData, setPinnedData] = useState<PinnedSkillsResponse>({
    global: [],
    session: [],
    all: [],
    items: [],
  });
  const [selectedSkills, setSelectedSkills] = useState<string[]>([]);
  const [scope, setScope] = useState<'session' | 'global'>(sessionId ? 'session' : 'global');
  const [search, setSearch] = useState('');
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);

  const loadPinned = useCallback(async () => {
    setLoading(true);
    setErrorMsg(null);
    try {
      const res = await api.getPinnedSkills(sessionId || undefined);
      setPinnedData(res);
      onPinnedChanged?.(res);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setLoading(false);
    }
  }, [sessionId, onPinnedChanged]);

  useEffect(() => {
    if (!isOpen) return;
    loadPinned();
    setSelectedSkills([]);
    setSearch('');
    setScope(sessionId ? 'session' : 'global');
  }, [isOpen, sessionId, loadPinned]);

  useEffect(() => {
    if (!isOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [isOpen, onClose]);

  const filteredSkills = useMemo(() => {
    const q = search.trim().toLowerCase();
    if (!q) return skills;
    return skills.filter(
      (s) => s.name.toLowerCase().includes(q) || (s.description && s.description.toLowerCase().includes(q))
    );
  }, [skills, search]);

  const toggleSkill = (name: string) => {
    setSelectedSkills((prev) =>
      prev.includes(name) ? prev.filter((s) => s !== name) : [...prev, name]
    );
  };

  const handlePin = async () => {
    if (selectedSkills.length === 0) return;
    setSaving(true);
    setErrorMsg(null);
    try {
      const res = await api.pinSkills(selectedSkills, scope, sessionId || undefined);
      setPinnedData(res);
      setSelectedSkills([]);
      onPinnedChanged?.(res);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setSaving(false);
    }
  };

  const handleUnpin = async (skillName: string, itemScope: 'global' | 'session') => {
    setSaving(true);
    setErrorMsg(null);
    try {
      const res = await api.unpinSkill(skillName, itemScope, sessionId || undefined);
      setPinnedData(res);
      onPinnedChanged?.(res);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setSaving(false);
    }
  };

  if (!isOpen) return null;

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal-card modal-card-wide"
        role="dialog"
        aria-modal="true"
        aria-label="Pin Skills"
        onClick={(e) => e.stopPropagation()}
        style={{ maxWidth: '640px' }}
      >
        <div className="modal-header">
          <div className="modal-title-row">
            <Pin size={16} className="text-accent" />
            <span>Pin Skills</span>
          </div>
          <button type="button" className="icon-btn" onClick={onClose} aria-label="Close">
            <X size={16} />
          </button>
        </div>

        <div className="modal-body" style={{ display: 'flex', flexDirection: 'column', gap: '16px' }}>
          <p style={{ margin: 0, fontSize: '0.86rem', color: 'var(--text-muted)' }}>
            Pinned skills have their full instructions injected directly into every turn's system prompt, keeping them active without requiring slash invocations or on-demand skill calls.
          </p>

          {errorMsg && <div className="inline-banner inline-banner-err">{errorMsg}</div>}

          {/* Currently Pinned Skills */}
          {pinnedData.items.length > 0 && (
            <div style={{ display: 'flex', flexDirection: 'column', gap: '8px' }}>
              <div style={{ fontSize: '0.78rem', fontWeight: 600, color: 'var(--text-muted)', textTransform: 'uppercase', letterSpacing: '0.05em' }}>
                Currently Pinned ({pinnedData.items.length})
              </div>
              <div style={{ display: 'flex', flexWrap: 'wrap', gap: '8px' }}>
                {pinnedData.items.map((item) => (
                  <div
                    key={`${item.scope}-${item.name}`}
                    style={{
                      display: 'inline-flex',
                      alignItems: 'center',
                      gap: '6px',
                      padding: '4px 10px',
                      borderRadius: '16px',
                      fontSize: '0.82rem',
                      background: 'var(--bg-elevated)',
                      border: '1px solid var(--border)',
                    }}
                  >
                    <Pin size={12} className="text-accent" />
                    <span style={{ fontWeight: 600 }}>{item.name}</span>
                    <span
                      style={{
                        fontSize: '0.72rem',
                        padding: '1px 6px',
                        borderRadius: '10px',
                        background: item.scope === 'global' ? 'rgba(234, 179, 8, 0.15)' : 'rgba(56, 189, 248, 0.15)',
                        color: item.scope === 'global' ? '#eab308' : '#38bdf8',
                        textTransform: 'uppercase',
                        fontWeight: 600,
                      }}
                    >
                      {item.scope}
                    </span>
                    <button
                      type="button"
                      onClick={() => handleUnpin(item.name, item.scope as 'global' | 'session')}
                      disabled={saving}
                      title={`Unpin ${item.name}`}
                      style={{
                        background: 'none',
                        border: 'none',
                        cursor: 'pointer',
                        padding: '2px',
                        display: 'flex',
                        alignItems: 'center',
                        color: 'var(--text-muted)',
                      }}
                    >
                      <Trash2 size={12} />
                    </button>
                  </div>
                ))}
              </div>
            </div>
          )}

          {/* Scope selection */}
          <div style={{ display: 'flex', flexDirection: 'column', gap: '8px' }}>
            <div style={{ fontSize: '0.78rem', fontWeight: 600, color: 'var(--text-muted)', textTransform: 'uppercase', letterSpacing: '0.05em' }}>
              Pin Scope
            </div>
            <div style={{ display: 'flex', gap: '12px' }}>
              <label
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: '8px',
                  padding: '8px 12px',
                  borderRadius: '6px',
                  border: `1px solid ${scope === 'session' ? 'var(--accent)' : 'var(--border)'}`,
                  background: scope === 'session' ? 'rgba(56, 189, 248, 0.08)' : 'var(--bg-elevated)',
                  cursor: sessionId ? 'pointer' : 'not-allowed',
                  opacity: sessionId ? 1 : 0.5,
                  flex: 1,
                }}
              >
                <input
                  type="radio"
                  name="pin_scope"
                  value="session"
                  checked={scope === 'session'}
                  disabled={!sessionId}
                  onChange={() => setScope('session')}
                />
                <MessageSquare size={14} />
                <div>
                  <div style={{ fontSize: '0.85rem', fontWeight: 600 }}>This Session Only</div>
                  <div style={{ fontSize: '0.74rem', color: 'var(--text-muted)' }}>Active only in current chat session</div>
                </div>
              </label>

              <label
                style={{
                  display: 'flex',
                  alignItems: 'center',
                  gap: '8px',
                  padding: '8px 12px',
                  borderRadius: '6px',
                  border: `1px solid ${scope === 'global' ? 'var(--accent)' : 'var(--border)'}`,
                  background: scope === 'global' ? 'rgba(56, 189, 248, 0.08)' : 'var(--bg-elevated)',
                  cursor: 'pointer',
                  flex: 1,
                }}
              >
                <input
                  type="radio"
                  name="pin_scope"
                  value="global"
                  checked={scope === 'global'}
                  onChange={() => setScope('global')}
                />
                <Globe size={14} />
                <div>
                  <div style={{ fontSize: '0.85rem', fontWeight: 600 }}>Globally</div>
                  <div style={{ fontSize: '0.74rem', color: 'var(--text-muted)' }}>Active in all new and existing sessions</div>
                </div>
              </label>
            </div>
          </div>

          {/* Skill picker */}
          <div style={{ display: 'flex', flexDirection: 'column', gap: '8px' }}>
            <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
              <div style={{ fontSize: '0.78rem', fontWeight: 600, color: 'var(--text-muted)', textTransform: 'uppercase', letterSpacing: '0.05em' }}>
                Select Skills ({selectedSkills.length} selected)
              </div>
              {filteredSkills.length > 5 && (
                <div style={{ position: 'relative', width: '200px' }}>
                  <Search size={13} style={{ position: 'absolute', left: '8px', top: '50%', transform: 'translateY(-50%)', color: 'var(--text-muted)' }} />
                  <input
                    type="text"
                    value={search}
                    onChange={(e) => setSearch(e.target.value)}
                    placeholder="Filter skills..."
                    className="form-input"
                    style={{ paddingLeft: '26px', fontSize: '0.78rem', height: '28px' }}
                  />
                </div>
              )}
            </div>

            <div
              style={{
                maxHeight: '220px',
                overflowY: 'auto',
                border: '1px solid var(--border)',
                borderRadius: '6px',
                padding: '4px',
                display: 'flex',
                flexDirection: 'column',
                gap: '2px',
              }}
            >
              {filteredSkills.length === 0 ? (
                <div style={{ padding: '16px', textAlign: 'center', color: 'var(--text-muted)', fontSize: '0.84rem' }}>
                  No skills found. Drop SKILL.md directories into ~/.luna/skills to install.
                </div>
              ) : (
                filteredSkills.map((s) => {
                  const isPinnedGlobal = pinnedData.global.some((g) => g.toLowerCase() === s.name.toLowerCase());
                  const isPinnedSession = pinnedData.session.some((sp) => sp.toLowerCase() === s.name.toLowerCase());
                  const isSelected = selectedSkills.includes(s.name);

                  return (
                    <div
                      key={s.name}
                      onClick={() => toggleSkill(s.name)}
                      style={{
                        display: 'flex',
                        alignItems: 'flex-start',
                        gap: '10px',
                        padding: '8px 10px',
                        borderRadius: '4px',
                        cursor: 'pointer',
                        background: isSelected ? 'var(--bg-elevated-hover, rgba(255,255,255,0.06))' : 'transparent',
                        border: isSelected ? '1px solid var(--border-accent, var(--accent))' : '1px solid transparent',
                      }}
                    >
                      <input
                        type="checkbox"
                        checked={isSelected}
                        onChange={() => {}} // handled by parent onClick
                        style={{ marginTop: '3px' }}
                      />
                      <div style={{ flex: 1, minWidth: 0 }}>
                        <div style={{ display: 'flex', alignItems: 'center', gap: '6px' }}>
                          <span style={{ fontWeight: 600, fontSize: '0.86rem' }}>{s.name}</span>
                          {isPinnedGlobal && (
                            <span style={{ fontSize: '0.7rem', color: '#eab308', background: 'rgba(234, 179, 8, 0.1)', padding: '1px 5px', borderRadius: '4px' }}>
                              Pinned Global
                            </span>
                          )}
                          {isPinnedSession && !isPinnedGlobal && (
                            <span style={{ fontSize: '0.7rem', color: '#38bdf8', background: 'rgba(56, 189, 248, 0.1)', padding: '1px 5px', borderRadius: '4px' }}>
                              Pinned Session
                            </span>
                          )}
                        </div>
                        {s.description && (
                          <div style={{ fontSize: '0.76rem', color: 'var(--text-muted)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', marginTop: '2px' }}>
                            {s.description}
                          </div>
                        )}
                      </div>
                    </div>
                  );
                })
              )}
            </div>
          </div>
        </div>

        <div className="modal-footer" style={{ display: 'flex', justifyContent: 'flex-end', gap: '8px' }}>
          <button type="button" className="btn btn-secondary" onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={selectedSkills.length === 0 || saving}
            onClick={handlePin}
          >
            <Pin size={14} />
            <span>Pin {selectedSkills.length > 0 ? `${selectedSkills.length} Skill(s)` : 'Skills'}</span>
          </button>
        </div>
      </div>
    </div>
  );
};
