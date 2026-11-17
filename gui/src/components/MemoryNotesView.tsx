import { useEffect, useState } from "react";
import { Search } from "lucide-react";
import { api } from "../api";

export function MemoryNotesView() {
  const [query, setQuery] = useState("");
  const [memories, setMemories] = useState<Array<Record<string, unknown>>>([]);
  const [notes, setNotes] = useState<Array<Record<string, unknown>>>([]);
  const [selectedNote, setSelectedNote] = useState<Record<string, unknown> | null>(null);

  const load = async (q: string) => {
    const [m, n] = await Promise.all([api.searchMemory(q), api.listNotes()]);
    setMemories(m);
    setNotes(n);
  };

  useEffect(() => {
    void load("");
  }, []);

  return (
    <div className="page-scroll">
      <div className="page-header">
        <div>
          <h1>Memory &amp; Notes</h1>
          <div style={{ fontSize: 13, color: "var(--text-muted)" }}>
            Inspect persistent memories and Markdown notes stored by Luna.
          </div>
        </div>
        <div style={{ display: "flex", gap: 8 }}>
          <input
            className="text-input"
            placeholder="Search memories..."
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter") void load(query);
            }}
          />
          <button className="btn-primary" onClick={() => void load(query)}>
            <Search size={14} />
            <span>Search</span>
          </button>
        </div>
      </div>

      <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 18 }}>
        <div>
          <h3 style={{ marginTop: 0 }}>Memories ({memories.length})</h3>
          <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            {memories.map((m, i) => (
              <div key={i} className="surface-card">
                <div style={{ fontSize: 12, color: "var(--accent)", fontWeight: 600 }}>
                  {String(m.key ?? m.id ?? "memory")}
                </div>
                <div style={{ fontSize: 13 }}>{String(m.content ?? "")}</div>
              </div>
            ))}
          </div>
        </div>

        <div>
          <h3 style={{ marginTop: 0 }}>Notes ({notes.length})</h3>
          <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
            {notes.map((n, i) => (
              <button
                key={i}
                className="surface-card"
                style={{ textAlign: "left" }}
                onClick={async () => {
                  const full = await api.readNote(String(n.id ?? n.title ?? ""));
                  setSelectedNote(full);
                }}
              >
                <div style={{ fontWeight: 600 }}>{String(n.title ?? n.id)}</div>
                <div style={{ fontSize: 12, color: "var(--text-muted)" }}>
                  {String(n.summary ?? "")}
                </div>
              </button>
            ))}
          </div>
          {selectedNote && (
            <div className="surface-card" style={{ marginTop: 14 }}>
              <strong>{String(selectedNote.title ?? "Note")}</strong>
              <pre className="mono-pre">{String(selectedNote.content ?? "")}</pre>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
