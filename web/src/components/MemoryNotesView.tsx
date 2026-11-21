import React, { useEffect, useMemo, useState } from 'react';
import type {
  KnowledgeEdge,
  KnowledgeGraphResponse,
  KnowledgeNode,
  MemoryFact,
  NoteEntry,
} from '../types';
import { NeuronGraph } from './NeuronGraph';
import { MarkdownContent } from './MarkdownContent';
import {
  Search,
  Database,
  FileText,
  PanelLeft,
  Network,
  LayoutList,
  Maximize2,
  RefreshCw,
  X,
  ArrowUpRight,
  GitBranch,
  Sparkles,
  Tag,
  Clock,
  FolderOpen,
} from 'lucide-react';

interface MemoryNotesViewProps {
  sidebarOpen: boolean;
  onToggleSidebar: () => void;
  onSearchMemories: (q: string) => Promise<MemoryFact[]>;
  onSearchNotes: (q: string) => Promise<NoteEntry[]>;
  onGetNote: (noteId: string) => Promise<NoteEntry>;
  onGetKnowledgeGraph: (q?: string) => Promise<KnowledgeGraphResponse>;
}

type ViewMode = 'network' | 'directory' | 'expanded';
type CategoryFilter = 'all' | 'workflow_rule' | 'fact' | 'user_profile' | 'notes';

function humanizeSubject(raw?: string | null): string {
  if (!raw) return 'Memory';
  const parts = raw
    .replace(/^project\./, '')
    .replace(/^user\./, '')
    .split('.')
    .filter(Boolean)
    .map((p) =>
      p
        .replace(/[_-]+/g, ' ')
        .replace(/\b\w/g, (c) => c.toUpperCase())
    );
  return parts.join(' › ') || raw;
}

function formatCategoryLabel(cat?: string): string {
  switch (cat) {
    case 'workflow_rule':
      return 'Workflow rule';
    case 'user_profile':
      return 'Profile';
    case 'preference':
      return 'Preference';
    case 'note':
      return 'Note';
    case 'hub':
      return 'Topic hub';
    case 'tag':
      return 'Tag';
    case 'concept':
      return 'Concept';
    default:
      return 'Fact';
  }
}

function formatRelativeDate(iso?: string | null): string {
  if (!iso) return '';
  const dt = new Date(iso);
  if (Number.isNaN(dt.getTime())) return '';
  const diffMs = Date.now() - dt.getTime();
  const mins = Math.floor(diffMs / 60_000);
  if (mins < 1) return 'Just now';
  if (mins < 60) return `${mins}m ago`;
  const hrs = Math.floor(mins / 60);
  if (hrs < 24) return `${hrs}h ago`;
  const days = Math.floor(hrs / 24);
  if (days < 30) return `${days}d ago`;
  return dt.toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}

function memoryPrimaryText(m: MemoryFact): string {
  if (m.object && m.object.trim().length > 0) {
    return m.object.trim();
  }
  const raw = (m.text || '').trim();
  if (m.subject && raw.startsWith(m.subject)) {
    const rest = raw.slice(m.subject.length).trim();
    if (m.predicate && rest.startsWith(m.predicate)) {
      return rest.slice(m.predicate.length).trim() || raw;
    }
    return rest || raw;
  }
  return raw;
}

export const MemoryNotesView: React.FC<MemoryNotesViewProps> = ({
  sidebarOpen,
  onToggleSidebar,
  onSearchMemories,
  onSearchNotes,
  onGetNote,
  onGetKnowledgeGraph,
}) => {
  const [query, setQuery] = useState('');
  const [viewMode, setViewMode] = useState<ViewMode>('network');
  const [categoryFilter, setCategoryFilter] = useState<CategoryFilter>('all');
  const [selectedClusterId, setSelectedClusterId] = useState<number | null>(null);

  const [memories, setMemories] = useState<MemoryFact[]>([]);
  const [notes, setNotes] = useState<NoteEntry[]>([]);
  const [graph, setGraph] = useState<KnowledgeGraphResponse>({
    nodes: [],
    edges: [],
    clusters: [],
    stats: {
      total_memories: 0,
      total_notes: 0,
      total_hubs: 0,
      total_tags: 0,
      total_concepts: 0,
      total_nodes: 0,
      total_edges: 0,
    },
  });
  const [loading, setLoading] = useState(false);

  const [selectedNodeId, setSelectedNodeId] = useState<string | null>(null);
  const [fullNote, setFullNote] = useState<NoteEntry | null>(null);
  const [loadingNote, setLoadingNote] = useState(false);

  const performLoad = async (term: string) => {
    setLoading(true);
    try {
      const g = await onGetKnowledgeGraph(term);
      setGraph(g);
      if (g.memories && g.notes) {
        setMemories(g.memories);
        setNotes(g.notes);
      } else {
        const [m, n] = await Promise.all([onSearchMemories(term), onSearchNotes(term)]);
        setMemories(m);
        setNotes(n);
      }
    } catch (err) {
      console.error(err);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    performLoad('');
  }, []);

  const nodeById = useMemo(
    () => new Map<string, KnowledgeNode>(graph.nodes.map((n) => [n.id, n])),
    [graph.nodes]
  );

  // Filter graph nodes & edges by category or cluster
  const { filteredNodes, filteredEdges } = useMemo(() => {
    const allowed = new Set<string>();
    for (const n of graph.nodes) {
      if (selectedClusterId !== null && n.cluster !== selectedClusterId && n.kind !== 'tag' && n.kind !== 'concept') {
        continue;
      }
      if (categoryFilter === 'all') {
        allowed.add(n.id);
      } else if (categoryFilter === 'notes') {
        if (n.kind === 'note' || n.kind === 'hub' || n.kind === 'tag' || n.kind === 'concept') {
          allowed.add(n.id);
        }
      } else {
        if (
          (n.kind === 'memory' && n.category === categoryFilter) ||
          n.kind === 'hub' ||
          n.kind === 'tag' ||
          n.kind === 'concept'
        ) {
          allowed.add(n.id);
        }
      }
    }

    const edges = graph.edges.filter((e) => allowed.has(e.source) && allowed.has(e.target));
    // Keep hubs/tags/concepts only if they still have at least 1 edge under the active filter
    const connectedIds = new Set<string>();
    for (const e of edges) {
      connectedIds.add(e.source);
      connectedIds.add(e.target);
    }
    const nodes = graph.nodes.filter((n) => {
      if (!allowed.has(n.id)) return false;
      if (categoryFilter !== 'all' || selectedClusterId !== null) {
        if (n.kind === 'hub' || n.kind === 'tag' || n.kind === 'concept') {
          return connectedIds.has(n.id);
        }
      }
      return true;
    });

    return { filteredNodes: nodes, filteredEdges: edges };
  }, [graph, categoryFilter, selectedClusterId]);

  const connectionCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const e of graph.edges) {
      counts.set(e.source, (counts.get(e.source) || 0) + 1);
      counts.set(e.target, (counts.get(e.target) || 0) + 1);
    }
    return counts;
  }, [graph.edges]);

  const selectedNode = useMemo(
    () => (selectedNodeId ? nodeById.get(selectedNodeId) || null : null),
    [selectedNodeId, nodeById]
  );

  // Fetch full Markdown note body when a note node is selected
  useEffect(() => {
    if (!selectedNode || selectedNode.kind !== 'note' || !selectedNode.ref_id) {
      setFullNote(null);
      return;
    }
    const existing = notes.find((n) => n.id === selectedNode.ref_id);
    if (existing?.body && existing.body.length > 0) {
      setFullNote(existing);
    }
    setLoadingNote(true);
    onGetNote(selectedNode.ref_id)
      .then((fetched) => setFullNote(fetched))
      .catch(() => {})
      .finally(() => setLoadingNote(false));
  }, [selectedNode?.id]);

  // Connected neighbor entries for the selected node
  const selectedConnections = useMemo(() => {
    if (!selectedNodeId) return [];
    const list: Array<{ node: KnowledgeNode; edge: KnowledgeEdge }> = [];
    for (const edge of graph.edges) {
      if (edge.source === selectedNodeId) {
        const other = nodeById.get(edge.target);
        if (other) list.push({ node: other, edge });
      } else if (edge.target === selectedNodeId) {
        const other = nodeById.get(edge.source);
        if (other) list.push({ node: other, edge });
      }
    }
    return list;
  }, [selectedNodeId, graph.edges, nodeById]);

  const filteredMemories = useMemo(() => {
    return memories.filter((m) => {
      if (categoryFilter === 'notes') return false;
      if (categoryFilter !== 'all' && m.category !== categoryFilter) return false;
      if (selectedClusterId !== null && m.id) {
        const n = nodeById.get(`mem:${m.id}`);
        if (n && n.cluster !== selectedClusterId) return false;
      }
      return true;
    });
  }, [memories, categoryFilter, selectedClusterId, nodeById]);

  const filteredNotes = useMemo(() => {
    return notes.filter((n) => {
      if (categoryFilter !== 'all' && categoryFilter !== 'notes') return false;
      if (selectedClusterId !== null && n.id) {
        const node = nodeById.get(`note:${n.id}`);
        if (node && node.cluster !== selectedClusterId) return false;
      }
      return true;
    });
  }, [notes, categoryFilter, selectedClusterId, nodeById]);

  const selectedMemoryRecord = useMemo(() => {
    if (!selectedNode || selectedNode.kind !== 'memory' || !selectedNode.ref_id) return null;
    return memories.find((m) => m.id === selectedNode.ref_id) || null;
  }, [selectedNode, memories]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') {
      performLoad(query);
    }
  };

  return (
    <div className="page-view know-view">
      <header className="chat-header know-header">
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
          <div className="know-title-wrap">
            <span className="chat-header-title">Memory & Notes</span>
            <span className="know-header-stats tabular-nums">
              {graph.stats.total_memories} memories · {graph.stats.total_notes} notes ·{' '}
              {graph.stats.total_edges} connections
            </span>
          </div>
        </div>

        <div className="know-header-controls">
          <div className="know-view-switcher" role="tablist" aria-label="Knowledge view mode">
            <button
              type="button"
              role="tab"
              aria-selected={viewMode === 'network'}
              className={`know-switch-btn ${viewMode === 'network' ? 'active' : ''}`}
              onClick={() => setViewMode('network')}
            >
              <Network size={13} />
              <span>Network</span>
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={viewMode === 'directory'}
              className={`know-switch-btn ${viewMode === 'directory' ? 'active' : ''}`}
              onClick={() => setViewMode('directory')}
            >
              <LayoutList size={13} />
              <span>Directory</span>
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={viewMode === 'expanded'}
              className={`know-switch-btn ${viewMode === 'expanded' ? 'active' : ''}`}
              onClick={() => setViewMode('expanded')}
            >
              <Maximize2 size={13} />
              <span>Full Map</span>
            </button>
          </div>

          <button
            type="button"
            className="icon-btn"
            onClick={() => performLoad(query)}
            disabled={loading}
            title="Refresh memories, notes, and connections"
            aria-label="Refresh"
          >
            <RefreshCw size={14} className={loading ? 'spin' : ''} />
          </button>
        </div>
      </header>

      <div className="page-scroll know-scroll">
        <div className="know-container">
          {/* Intro + Search + Filters */}
          <div className="know-toolbar">
            <div className="know-search-row">
              <div className="search-input-wrap">
                <Search size={15} className="search-input-icon" />
                <input
                  type="text"
                  className="form-input search-input"
                  placeholder="Search memories, notes, topics, or #tags..."
                  value={query}
                  onChange={(e) => setQuery(e.target.value)}
                  onKeyDown={handleKeyDown}
                />
                {query && (
                  <button
                    type="button"
                    className="know-search-clear"
                    onClick={() => {
                      setQuery('');
                      performLoad('');
                    }}
                    aria-label="Clear search"
                  >
                    <X size={13} />
                  </button>
                )}
              </div>
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => performLoad(query)}
                disabled={loading}
              >
                {loading ? 'Searching…' : 'Search'}
              </button>
            </div>

            <div className="know-filter-strip">
              <div className="know-filter-pills">
                {(
                  [
                    ['all', 'All'],
                    ['workflow_rule', 'Workflows'],
                    ['fact', 'Facts'],
                    ['user_profile', 'Profiles'],
                    ['notes', 'Notes'],
                  ] as Array<[CategoryFilter, string]>
                ).map(([key, label]) => (
                  <button
                    key={key}
                    type="button"
                    className={`know-filter-pill ${categoryFilter === key ? 'active' : ''}`}
                    onClick={() => setCategoryFilter(key)}
                  >
                    {label}
                  </button>
                ))}
              </div>

              {graph.clusters.length > 0 && (
                <div className="know-cluster-pills">
                  <button
                    type="button"
                    className={`know-cluster-pill ${selectedClusterId === null ? 'active' : ''}`}
                    onClick={() => setSelectedClusterId(null)}
                  >
                    All topics
                  </button>
                  {graph.clusters.slice(0, 7).map((c) => (
                    <button
                      key={c.id}
                      type="button"
                      className={`know-cluster-pill ${selectedClusterId === c.id ? 'active' : ''}`}
                      onClick={() =>
                        setSelectedClusterId((prev) => (prev === c.id ? null : c.id))
                      }
                    >
                      <span>{c.label}</span>
                      <span className="know-cluster-count tabular-nums">{c.node_count}</span>
                    </button>
                  ))}
                </div>
              )}
            </div>
          </div>

          {/* Neuron Graph + Synaptic Inspector */}
          {viewMode !== 'directory' && (
            <div className={`know-stage ${viewMode === 'expanded' && !selectedNode ? 'know-stage-full' : ''}`}>
              <div className="know-graph-pane">
                <NeuronGraph
                  nodes={filteredNodes}
                  edges={filteredEdges}
                  selectedNodeId={selectedNodeId}
                  onSelectNode={setSelectedNodeId}
                  height={viewMode === 'expanded' ? 620 : 480}
                />
              </div>

              <aside className="know-inspector-pane">
                {!selectedNode ? (
                  <div className="know-inspector-empty">
                    <div className="know-inspector-hero-icon">
                      <Sparkles size={18} />
                    </div>
                    <h3 className="know-inspector-title">Neural Knowledge Map</h3>
                    <p className="know-inspector-copy">
                      Memories, workflow rules, and workspace notes are linked through shared
                      projects, tags, and recurring concepts. Click any neuron to trace its
                      connections or read full notes.
                    </p>

                    <div className="know-hubs-quick">
                      <div className="know-inspector-section-label">Active topic hubs</div>
                      <div className="know-hub-chip-list">
                        {graph.clusters.map((c) => {
                          const hubNode = graph.nodes.find(
                            (n) => n.kind === 'hub' && n.cluster === c.id
                          );
                          return (
                            <button
                              key={c.id}
                              type="button"
                              className="know-hub-jump"
                              onClick={() => {
                                if (hubNode) setSelectedNodeId(hubNode.id);
                              }}
                            >
                              <span>{c.label}</span>
                              <span className="tabular-nums text-muted">{c.node_count}</span>
                            </button>
                          );
                        })}
                      </div>
                    </div>
                  </div>
                ) : (
                  <div className="know-inspector-content">
                    <div className="know-inspector-head">
                      <div className="know-inspector-badges">
                        <span className={`know-kind-badge kind-${selectedNode.category}`}>
                          {formatCategoryLabel(selectedNode.category)}
                        </span>
                        {selectedNode.meta.project && (
                          <span className="know-project-badge">
                            <FolderOpen size={11} />
                            <span>{selectedNode.meta.project}</span>
                          </span>
                        )}
                      </div>
                      <button
                        type="button"
                        className="icon-btn"
                        onClick={() => setSelectedNodeId(null)}
                        title="Close inspector"
                        aria-label="Close inspector"
                      >
                        <X size={14} />
                      </button>
                    </div>

                    <h2 className="know-inspector-heading">{selectedNode.label}</h2>
                    <div className="know-inspector-submeta">
                      {selectedNode.updated_at && (
                        <span className="know-meta-chip">
                          <Clock size={11} />
                          <span>Updated {formatRelativeDate(selectedNode.updated_at)}</span>
                        </span>
                      )}
                      <span className="know-meta-chip">
                        <GitBranch size={11} />
                        <span>{selectedConnections.length} synapses</span>
                      </span>
                      {selectedNode.meta.confidence !== undefined &&
                        selectedNode.meta.confidence !== null && (
                          <span className="know-meta-chip">
                            {Math.round(selectedNode.meta.confidence * 100)}% confidence
                          </span>
                        )}
                    </div>

                    {selectedNode.tags.length > 0 && (
                      <div className="know-tags-row">
                        {selectedNode.tags.map((t) => (
                          <span key={t} className="know-tag-chip">
                            <Tag size={10} />
                            <span>{t}</span>
                          </span>
                        ))}
                      </div>
                    )}

                    {/* Body / Details */}
                    <div className="know-inspector-body-box">
                      {selectedNode.kind === 'note' ? (
                        loadingNote && !fullNote ? (
                          <div className="text-muted" style={{ fontSize: 13 }}>
                            Loading note contents…
                          </div>
                        ) : (
                          <div className="know-note-markdown">
                            <MarkdownContent
                              content={fullNote?.body || selectedNode.preview || 'Empty note.'}
                            />
                          </div>
                        )
                      ) : selectedNode.kind === 'memory' ? (
                        <div className="know-memory-detail">
                          <p className="know-memory-value">
                            {selectedMemoryRecord
                              ? memoryPrimaryText(selectedMemoryRecord)
                              : selectedNode.preview}
                          </p>
                          <div className="know-kv-grid">
                            {selectedNode.meta.subject && (
                              <div className="know-kv-row">
                                <span className="know-kv-k">Subject</span>
                                <span className="know-kv-v mono-text">
                                  {selectedNode.meta.subject}
                                </span>
                              </div>
                            )}
                            {selectedNode.meta.predicate && (
                              <div className="know-kv-row">
                                <span className="know-kv-k">Predicate</span>
                                <span className="know-kv-v mono-text">
                                  {selectedNode.meta.predicate}
                                </span>
                              </div>
                            )}
                            {selectedNode.meta.path && (
                              <div className="know-kv-row">
                                <span className="know-kv-k">Path</span>
                                <span className="know-kv-v mono-text">
                                  {selectedNode.meta.path}
                                </span>
                              </div>
                            )}
                          </div>
                        </div>
                      ) : (
                        <p className="know-memory-value">{selectedNode.preview}</p>
                      )}
                    </div>

                    {/* Connected Synapses */}
                    <div className="know-connections-section">
                      <div className="know-inspector-section-label">
                        Connected neurons ({selectedConnections.length})
                      </div>
                      {selectedConnections.length === 0 ? (
                        <div className="text-muted" style={{ fontSize: 12.5 }}>
                          No direct connections found.
                        </div>
                      ) : (
                        <div className="know-connections-list">
                          {selectedConnections.map(({ node, edge }) => (
                            <button
                              key={`${edge.id}-${node.id}`}
                              type="button"
                              className="know-conn-row"
                              onClick={() => setSelectedNodeId(node.id)}
                            >
                              <div className="know-conn-left">
                                <span className={`know-conn-dot kind-${node.category}`} />
                                <div className="know-conn-text">
                                  <span className="know-conn-title">{node.label}</span>
                                  <span className="know-conn-reason">
                                    {edge.reason} · {formatCategoryLabel(node.category)}
                                  </span>
                                </div>
                              </div>
                              <ArrowUpRight size={13} className="know-conn-arrow" />
                            </button>
                          ))}
                        </div>
                      )}
                    </div>
                  </div>
                )}
              </aside>
            </div>
          )}

          {/* Structured Human-Readable Directory */}
          {viewMode !== 'expanded' && (
            <div className="two-col-grid know-directory-grid">
              <section className="section-col">
                <div className="section-heading">
                  <Database size={14} className="text-accent" />
                  <span>Memories</span>
                  <span className="section-count tabular-nums">{filteredMemories.length}</span>
                </div>

                {filteredMemories.length === 0 ? (
                  <div className="empty-inline">No matching memory facts.</div>
                ) : (
                  <div className="know-card-list">
                    {filteredMemories.map((m, i) => {
                      const nid = m.id ? `mem:${m.id}` : '';
                      const isSelected = Boolean(nid && selectedNodeId === nid);
                      const links = nid ? connectionCounts.get(nid) || 0 : 0;
                      const cat = m.category || 'fact';
                      return (
                        <button
                          key={m.id || i}
                          type="button"
                          className={`know-item-card ${isSelected ? 'selected' : ''}`}
                          onClick={() => {
                            if (nid) setSelectedNodeId(nid);
                          }}
                        >
                          <div className="know-item-top">
                            <span className={`know-kind-badge kind-${cat}`}>
                              {formatCategoryLabel(cat)}
                            </span>
                            <span className="know-item-subject">
                              {humanizeSubject(m.subject)}
                            </span>
                            {links > 0 && (
                              <span className="know-link-pill tabular-nums">
                                <GitBranch size={10} />
                                <span>{links}</span>
                              </span>
                            )}
                          </div>
                          <div className="know-item-body">{memoryPrimaryText(m)}</div>
                          <div className="know-item-footer">
                            {m.predicate && (
                              <span className="know-item-meta">{m.predicate}</span>
                            )}
                            {m.path && (
                              <span className="know-item-path" title={m.path}>
                                {m.path}
                              </span>
                            )}
                            {m.updated_at && (
                              <span className="know-item-time">
                                {formatRelativeDate(m.updated_at)}
                              </span>
                            )}
                          </div>
                        </button>
                      );
                    })}
                  </div>
                )}
              </section>

              <section className="section-col">
                <div className="section-heading">
                  <FileText size={14} className="text-warning" />
                  <span>Workspace notes</span>
                  <span className="section-count tabular-nums">{filteredNotes.length}</span>
                </div>

                {filteredNotes.length === 0 ? (
                  <div className="empty-inline">No matching workspace notes.</div>
                ) : (
                  <div className="know-card-list">
                    {filteredNotes.map((n, i) => {
                      const nid = n.id ? `note:${n.id}` : '';
                      const isSelected = Boolean(nid && selectedNodeId === nid);
                      const links = nid ? connectionCounts.get(nid) || 0 : 0;
                      return (
                        <button
                          key={n.id || i}
                          type="button"
                          className={`know-item-card ${isSelected ? 'selected' : ''}`}
                          onClick={() => {
                            if (nid) setSelectedNodeId(nid);
                            if (viewMode === 'directory') setViewMode('network');
                          }}
                        >
                          <div className="know-item-top">
                            <span className="know-kind-badge kind-note">Note</span>
                            {n.project && (
                              <span className="know-project-badge">
                                <FolderOpen size={10} />
                                <span>{n.project}</span>
                              </span>
                            )}
                            {links > 0 && (
                              <span className="know-link-pill tabular-nums">
                                <GitBranch size={10} />
                                <span>{links}</span>
                              </span>
                            )}
                          </div>
                          <div className="know-item-title">{n.title}</div>
                          {(n.summary_pointer || n.body) && (
                            <div className="know-item-preview">
                              {n.summary_pointer || n.body}
                            </div>
                          )}
                          <div className="know-item-footer">
                            {(n.tags || []).slice(0, 4).map((tag) => (
                              <span key={tag} className="know-tag-chip">
                                #{tag}
                              </span>
                            ))}
                            {n.updated_at && (
                              <span className="know-item-time">
                                {formatRelativeDate(n.updated_at)}
                              </span>
                            )}
                          </div>
                        </button>
                      );
                    })}
                  </div>
                )}
              </section>
            </div>
          )}
        </div>
      </div>
    </div>
  );
};
