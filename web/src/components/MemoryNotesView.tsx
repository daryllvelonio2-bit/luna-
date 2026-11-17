import React, { useState, useEffect } from 'react';
import type { MemoryFact, NoteEntry } from '../types';
import { Search, Database, FileText, PanelLeft } from 'lucide-react';

interface MemoryNotesViewProps {
  sidebarOpen: boolean;
  onToggleSidebar: () => void;
  onSearchMemories: (q: string) => Promise<MemoryFact[]>;
  onSearchNotes: (q: string) => Promise<NoteEntry[]>;
}

export const MemoryNotesView: React.FC<MemoryNotesViewProps> = ({
  sidebarOpen,
  onToggleSidebar,
  onSearchMemories,
  onSearchNotes,
}) => {
  const [query, setQuery] = useState('');
  const [memories, setMemories] = useState<MemoryFact[]>([]);
  const [notes, setNotes] = useState<NoteEntry[]>([]);
  const [loading, setLoading] = useState(false);

  const performSearch = async (term: string) => {
    setLoading(true);
    try {
      const [m, n] = await Promise.all([onSearchMemories(term), onSearchNotes(term)]);
      setMemories(m);
      setNotes(n);
    } catch (err) {
      console.error(err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    performSearch('');
  }, []);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') {
      performSearch(query);
    }
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
          <span className="chat-header-title">Memory & Notes</span>
        </div>
      </header>

      <div className="page-scroll">
        <div className="page-container">
          <div className="page-intro">
            <h1 className="page-title">Memory & persistent notes</h1>
            <p className="page-subtitle">
              Stored preferences, workflow rules, and workspace reference notes.
            </p>
          </div>

          <div className="search-bar-row">
            <div className="search-input-wrap">
              <Search size={15} className="search-input-icon" />
              <input
                type="text"
                className="form-input search-input"
                placeholder="Search memories and notes…"
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={handleKeyDown}
              />
            </div>
            <button
              type="button"
              className="btn btn-primary"
              onClick={() => performSearch(query)}
              disabled={loading}
            >
              {loading ? 'Searching…' : 'Search'}
            </button>
          </div>

          <div className="two-col-grid">
            <section className="section-col">
              <div className="section-heading">
                <Database size={14} className="text-accent" />
                <span>Memories</span>
                <span className="section-count tabular-nums">{memories.length}</span>
              </div>

              {memories.length === 0 ? (
                <div className="empty-inline">No matching memory facts.</div>
              ) : (
                <div className="surface-list">
                  {memories.map((m, i) => (
                    <div key={i} className="surface-row surface-row-stack">
                      <div className="surface-row-kicker">
                        {m.category || 'fact'}
                        {m.predicate ? ` · ${m.predicate}` : ''}
                      </div>
                      <div className="surface-row-body">{m.text}</div>
                    </div>
                  ))}
                </div>
              )}
            </section>

            <section className="section-col">
              <div className="section-heading">
                <FileText size={14} className="text-warning" />
                <span>Workspace notes</span>
                <span className="section-count tabular-nums">{notes.length}</span>
              </div>

              {notes.length === 0 ? (
                <div className="empty-inline">No matching notes found.</div>
              ) : (
                <div className="surface-list">
                  {notes.map((n, i) => (
                    <div key={i} className="surface-row surface-row-stack">
                      <div className="surface-row-title">{n.title}</div>
                      {n.body && <div className="note-preview">{n.body}</div>}
                    </div>
                  ))}
                </div>
              )}
            </section>
          </div>
        </div>
      </div>
    </div>
  );
};
