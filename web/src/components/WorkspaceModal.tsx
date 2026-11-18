import React, { useState, useEffect, useCallback } from 'react';
import { api } from '../api';
import type { WorkspaceBrowseResult } from '../types';
import { Folder, ArrowUp, Home, Check, X } from 'lucide-react';

interface WorkspaceModalProps {
  isOpen: boolean;
  onClose: () => void;
  currentWorkspace: string;
  launchWorkspaceRoot?: string;
  onSelectWorkspace: (path: string, scope: 'session' | 'global', reset?: boolean) => Promise<void>;
}

const RECENT_STORAGE_KEY = 'luna_recent_workspaces';

function loadRecent(): string[] {
  try {
    const raw = localStorage.getItem(RECENT_STORAGE_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.slice(0, 6) : [];
  } catch {
    return [];
  }
}

function saveRecent(path: string) {
  try {
    const prev = loadRecent().filter((p) => p !== path);
    localStorage.setItem(RECENT_STORAGE_KEY, JSON.stringify([path, ...prev].slice(0, 6)));
  } catch {
    // ignore
  }
}

export const WorkspaceModal: React.FC<WorkspaceModalProps> = ({
  isOpen,
  onClose,
  currentWorkspace,
  launchWorkspaceRoot,
  onSelectWorkspace,
}) => {
  const [browse, setBrowse] = useState<WorkspaceBrowseResult | null>(null);
  const [pathInput, setPathInput] = useState(currentWorkspace);
  const [scope, setScope] = useState<'session' | 'global'>('session');
  const [loading, setLoading] = useState(false);
  const [saving, setSaving] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  const [recent, setRecent] = useState<string[]>([]);

  const loadDir = useCallback(async (target?: string) => {
    setLoading(true);
    setErrorMsg(null);
    try {
      const res = await api.browseWorkspace(target);
      setBrowse(res);
      setPathInput(res.current);
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (!isOpen) return;
    setRecent(loadRecent());
    loadDir(currentWorkspace || undefined);
  }, [isOpen, currentWorkspace, loadDir]);

  useEffect(() => {
    if (!isOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [isOpen, onClose]);

  if (!isOpen) return null;

  const confirmPath = async (targetPath: string, reset = false) => {
    const clean = targetPath.trim();
    if (!clean && !reset) return;
    setSaving(true);
    setErrorMsg(null);
    try {
      await onSelectWorkspace(clean, scope, reset);
      if (clean) saveRecent(clean);
      onClose();
    } catch (e) {
      setErrorMsg(String(e));
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal-card modal-card-wide"
        role="dialog"
        aria-modal="true"
        aria-label="Select workspace"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal-header">
          <div className="modal-title-row">
            <Folder size={16} className="text-accent" />
            <span>Select workspace</span>
          </div>
          <button type="button" className="icon-btn" onClick={onClose} aria-label="Close">
            <X size={16} />
          </button>
        </div>

        <div className="modal-body">
          {errorMsg && <div className="inline-banner inline-banner-err">{errorMsg}</div>}

          <div className="inline-row">
            <input
              type="text"
              className="form-input flex-1 mono-input"
              value={pathInput}
              placeholder="/path/to/project"
              onChange={(e) => setPathInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') {
                  e.preventDefault();
                  loadDir(pathInput);
                }
              }}
            />
            <button
              type="button"
              className="btn btn-secondary"
              onClick={() => loadDir(pathInput)}
              disabled={loading}
            >
              Open
            </button>
          </div>

          <div className="workspace-nav-bar">
            {browse?.parent && (
              <button
                type="button"
                className="picker-action-chip"
                onClick={() => loadDir(browse.parent!)}
              >
                <ArrowUp size={12} />
                <span>Parent</span>
              </button>
            )}
            {browse?.home && (
              <button
                type="button"
                className="picker-action-chip"
                onClick={() => loadDir(browse.home!)}
              >
                <Home size={12} />
                <span>Home</span>
              </button>
            )}
            <span className="workspace-current-path" title={browse?.current || currentWorkspace}>
              {browse?.current || currentWorkspace}
            </span>
          </div>

          {recent.length > 0 && (
            <div className="workspace-recent-row">
              <span className="form-label">Recent</span>
              <div className="workspace-recent-chips">
                {recent.map((r) => (
                  <button
                    key={r}
                    type="button"
                    className={`picker-action-chip ${r === currentWorkspace ? 'active' : ''}`}
                    onClick={() => confirmPath(r)}
                    title={r}
                  >
                    {r.split('/').filter(Boolean).pop() || r}
                  </button>
                ))}
              </div>
            </div>
          )}

          <div className="workspace-dir-list">
            {loading ? (
              <div className="picker-empty">Loading folders…</div>
            ) : !browse || browse.entries.length === 0 ? (
              <div className="picker-empty">No subfolders in this directory.</div>
            ) : (
              browse.entries.map((entry) => (
                <button
                  key={entry.path}
                  type="button"
                  className="workspace-dir-row"
                  onClick={() => loadDir(entry.path)}
                >
                  <Folder size={14} className="workspace-dir-icon" />
                  <span className="workspace-dir-name">{entry.name}</span>
                </button>
              ))
            )}
          </div>
        </div>

        <div className="modal-footer modal-footer-between">
          <div className="inline-row">
            <div className="segmented-toggle" role="group" aria-label="Workspace scope">
              <button
                type="button"
                className={`segmented-btn ${scope === 'session' ? 'active' : ''}`}
                onClick={() => setScope('session')}
              >
                This session
              </button>
              <button
                type="button"
                className={`segmented-btn ${scope === 'global' ? 'active' : ''}`}
                onClick={() => setScope('global')}
              >
                Global default
              </button>
            </div>
            {launchWorkspaceRoot && currentWorkspace !== launchWorkspaceRoot && (
              <button
                type="button"
                className="btn btn-secondary btn-xs"
                disabled={saving}
                onClick={() => confirmPath(launchWorkspaceRoot, true)}
              >
                Reset
              </button>
            )}
          </div>
          <div className="inline-row">
            <button type="button" className="btn btn-secondary" onClick={onClose}>
              Cancel
            </button>
            <button
              type="button"
              className="btn btn-primary"
              disabled={saving || !pathInput.trim()}
              onClick={() => confirmPath(browse?.current || pathInput)}
            >
              <Check size={14} />
              <span>{saving ? 'Switching…' : 'Use this folder'}</span>
            </button>
          </div>
        </div>
      </div>
    </div>
  );
};
