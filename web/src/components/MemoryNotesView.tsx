import React, { useState, useEffect } from 'react';
import type { MemoryFact, NoteEntry } from '../types';
import { Search, Database, FileText } from 'lucide-react';

interface MemoryNotesViewProps {
  onSearchMemories: (q: string) => Promise<MemoryFact[]>;
  onSearchNotes: (q: string) => Promise<NoteEntry[]>;
}

export const MemoryNotesView: React.FC<MemoryNotesViewProps> = ({
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
    <div style={{ flex: 1, padding: '24px', overflowY: 'auto' }}>
      <div style={{ marginBottom: '20px' }}>
        <h2 style={{ fontSize: '18px', fontWeight: 600 }}>Memory & Persistent Notes</h2>
        <p style={{ color: 'var(--text-secondary)', fontSize: '13px', marginTop: '4px' }}>
          Real-time access to the SQLite memory facts and workspace notes graph.
        </p>
      </div>

      <div style={{ display: 'flex', gap: '8px', marginBottom: '24px' }}>
        <div style={{ position: 'relative', flex: 1 }}>
          <input
            type="text"
            className="form-input"
            style={{ width: '100%', paddingLeft: '32px' }}
            placeholder="Search memories and notes by keyword..."
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={handleKeyDown}
          />
          <Search
            size={16}
            style={{
              position: 'absolute',
              left: '10px',
              top: '50%',
              transform: 'translateY(-50%)',
              color: 'var(--text-muted)',
            }}
          />
        </div>
        <button className="primary-btn" onClick={() => performSearch(query)} disabled={loading}>
          {loading ? 'Searching...' : 'Search'}
        </button>
      </div>

      <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '20px' }}>
        <div>
          <div style={{ display: 'flex', alignItems: 'center', gap: '8px', marginBottom: '12px' }}>
            <Database size={16} color="var(--accent-color)" />
            <h3 style={{ fontSize: '14px', fontWeight: 600 }}>Memory Facts ({memories.length})</h3>
          </div>
          <div style={{ display: 'flex', flexDirection: 'column', gap: '8px' }}>
            {memories.length === 0 ? (
              <div
                style={{
                  padding: '20px',
                  textAlign: 'center',
                  backgroundColor: 'var(--bg-secondary)',
                  borderRadius: '6px',
                  color: 'var(--text-muted)',
                  fontSize: '13px',
                }}
              >
                No matching memory facts.
              </div>
            ) : (
              memories.map((m, i) => (
                <div
                  key={i}
                  style={{
                    backgroundColor: 'var(--bg-secondary)',
                    border: '1px solid var(--border-color)',
                    borderRadius: '6px',
                    padding: '12px',
                    fontSize: '13px',
                  }}
                >
                  <div style={{ color: 'var(--accent-color)', fontSize: '11px', marginBottom: '4px' }}>
                    {m.category || 'fact'} {m.predicate ? `• ${m.predicate}` : ''}
                  </div>
                  <div>{m.text}</div>
                </div>
              ))
            )}
          </div>
        </div>

        <div>
          <div style={{ display: 'flex', alignItems: 'center', gap: '8px', marginBottom: '12px' }}>
            <FileText size={16} color="var(--warning-color)" />
            <h3 style={{ fontSize: '14px', fontWeight: 600 }}>Workspace Notes ({notes.length})</h3>
          </div>
          <div style={{ display: 'flex', flexDirection: 'column', gap: '8px' }}>
            {notes.length === 0 ? (
              <div
                style={{
                  padding: '20px',
                  textAlign: 'center',
                  backgroundColor: 'var(--bg-secondary)',
                  borderRadius: '6px',
                  color: 'var(--text-muted)',
                  fontSize: '13px',
                }}
              >
                No matching notes found.
              </div>
            ) : (
              notes.map((n, i) => (
                <div
                  key={i}
                  style={{
                    backgroundColor: 'var(--bg-secondary)',
                    border: '1px solid var(--border-color)',
                    borderRadius: '6px',
                    padding: '12px',
                    fontSize: '13px',
                  }}
                >
                  <div style={{ fontWeight: 600, marginBottom: '4px' }}>{n.title}</div>
                  {n.body && (
                    <div
                      style={{
                        color: 'var(--text-secondary)',
                        fontSize: '12px',
                        whiteSpace: 'pre-wrap',
                        maxHeight: '120px',
                        overflowY: 'auto',
                      }}
                    >
                      {n.body}
                    </div>
                  )}
                </div>
              ))
            )}
          </div>
        </div>
      </div>
    </div>
  );
};
