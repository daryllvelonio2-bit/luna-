import React, { useEffect, useMemo, useRef, useState } from 'react';
import type { KnowledgeEdge, KnowledgeNode } from '../types';
import { ZoomIn, ZoomOut, Maximize2, RotateCcw, Tag, Eye, EyeOff } from 'lucide-react';

interface NeuronGraphProps {
  nodes: KnowledgeNode[];
  edges: KnowledgeEdge[];
  selectedNodeId: string | null;
  onSelectNode: (nodeId: string | null) => void;
  height?: number;
}

interface PositionedNode {
  x: number;
  y: number;
  radius: number;
}

function nodeColor(node: KnowledgeNode): string {
  if (node.kind === 'hub') return '#818cf8'; // luminous indigo hub
  if (node.kind === 'note') return '#f59e0b'; // warm amber note
  if (node.kind === 'tag' || node.kind === 'concept') return '#2dd4bf'; // teal synapse bridge
  switch (node.category) {
    case 'workflow_rule':
      return '#a78bfa'; // violet workflow rule
    case 'user_profile':
      return '#34d399'; // emerald user profile
    case 'preference':
      return '#fbbf24'; // gold preference
    default:
      return '#60a5fa'; // sky blue fact
  }
}

function nodeRadius(node: KnowledgeNode): number {
  if (node.kind === 'hub') return Math.min(24, 15 + Math.sqrt(Math.max(1, node.degree)) * 1.9);
  if (node.kind === 'note') return Math.min(16, 11 + Math.sqrt(Math.max(1, node.degree)) * 1.1);
  if (node.kind === 'memory') return Math.min(13, 8.5 + Math.sqrt(Math.max(1, node.degree)) * 0.9);
  return 7.5;
}

function hashSeed(str: string): number {
  let h = 2166136261;
  for (let i = 0; i < str.length; i++) {
    h ^= str.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return (h >>> 0) / 4294967296;
}

function computeDeterministicLayout(
  nodes: KnowledgeNode[],
  edges: KnowledgeEdge[]
): Record<string, PositionedNode> {
  const count = nodes.length;
  if (count === 0) return {};

  const width = 1000;
  const height = 660;
  const cx = width / 2;
  const cy = height / 2;

  const hubs = nodes.filter((n) => n.kind === 'hub');
  const clusterRingPos: Record<number, { x: number; y: number }> = {};
  const hubCount = Math.max(1, hubs.length);
  const ringRx = Math.min(310, 145 + hubCount * 19);
  const ringRy = Math.min(205, 115 + hubCount * 12);

  hubs.forEach((h, idx) => {
    const angle = (idx / hubCount) * Math.PI * 2 - Math.PI / 2;
    clusterRingPos[h.cluster] = {
      x: cx + Math.cos(angle) * ringRx,
      y: cy + Math.sin(angle) * ringRy,
    };
  });

  const pos: Record<string, { x: number; y: number; vx: number; vy: number; r: number }> = {};
  nodes.forEach((n, idx) => {
    const anchor = clusterRingPos[n.cluster] || { x: cx, y: cy };
    const r = nodeRadius(n);
    if (n.kind === 'hub') {
      pos[n.id] = { x: anchor.x, y: anchor.y, vx: 0, vy: 0, r };
    } else {
      const a = hashSeed(n.id) * Math.PI * 2 + idx * 0.37;
      const dist = n.kind === 'tag' || n.kind === 'concept' ? 54 : 76 + hashSeed(n.label) * 44;
      pos[n.id] = {
        x: anchor.x + Math.cos(a) * dist,
        y: anchor.y + Math.sin(a) * dist,
        vx: 0,
        vy: 0,
        r,
      };
    }
  });

  const ids = nodes.map((n) => n.id);
  const byId = new Map(nodes.map((n) => [n.id, n]));
  const iterations = Math.min(180, Math.max(95, 220 - count));

  for (let iter = 0; iter < iterations; iter++) {
    const alpha = 1 - iter / iterations;
    // Repulsion between pairs
    for (let i = 0; i < ids.length; i++) {
      const idA = ids[i];
      const a = pos[idA];
      for (let j = i + 1; j < ids.length; j++) {
        const idB = ids[j];
        const b = pos[idB];
        let dx = b.x - a.x;
        let dy = b.y - a.y;
        let distSq = dx * dx + dy * dy;
        if (distSq < 1) {
          dx = 0.7;
          dy = 0.7;
          distSq = 1;
        }
        const dist = Math.sqrt(distSq);
        const kindA = byId.get(idA)?.kind;
        const kindB = byId.get(idB)?.kind;
        const isMajor =
          kindA === 'hub' || kindB === 'hub' || kindA === 'note' || kindB === 'note';
        const minGap = a.r + b.r + (isMajor ? 58 : 30);
        const repulse = (3200 / distSq) * (0.58 + alpha * 0.65);
        const overlapPush = dist < minGap ? (minGap - dist) * 0.36 : 0;
        const fx = (dx / dist) * (repulse + overlapPush);
        const fy = (dy / dist) * (repulse + overlapPush);
        a.vx -= fx;
        a.vy -= fy;
        b.vx += fx;
        b.vy += fy;
      }
    }

    // Spring attraction along edges
    for (const e of edges) {
      const a = pos[e.source];
      const b = pos[e.target];
      if (!a || !b) continue;
      const dx = b.x - a.x;
      const dy = b.y - a.y;
      const dist = Math.sqrt(dx * dx + dy * dy) || 1;
      const targetLen =
        e.relation === 'belongs_to'
          ? 106
          : e.relation === 'tagged' || e.relation === 'mentions'
          ? 146
          : 165;
      const strength = 0.045 * (0.5 + e.weight * 0.7);
      const force = (dist - targetLen) * strength;
      const fx = (dx / dist) * force;
      const fy = (dy / dist) * force;
      a.vx += fx;
      a.vy += fy;
      b.vx -= fx;
      b.vy -= fy;
    }

    // Mild cluster gravity & viewport centering
    for (const n of nodes) {
      const p = pos[n.id];
      const anchor = clusterRingPos[n.cluster];
      if (anchor) {
        const pull = n.kind === 'hub' ? 0.045 : 0.014;
        p.vx += (anchor.x - p.x) * pull;
        p.vy += (anchor.y - p.y) * pull;
      }
      p.vx += (cx - p.x) * 0.004;
      p.vy += (cy - p.y) * 0.004;

      p.vx *= 0.68;
      p.vy *= 0.68;
      p.x += p.vx;
      p.y += p.vy;
    }
  }

  const out: Record<string, PositionedNode> = {};
  for (const n of nodes) {
    const p = pos[n.id];
    out[n.id] = { x: p.x, y: p.y, radius: p.r };
  }
  return out;
}

function curvedPath(x1: number, y1: number, x2: number, y2: number, seed: number): string {
  const mx = (x1 + x2) / 2;
  const my = (y1 + y2) / 2;
  const dx = x2 - x1;
  const dy = y2 - y1;
  const len = Math.sqrt(dx * dx + dy * dy) || 1;
  const curve = ((seed - 0.5) * 2) * Math.min(34, len * 0.16);
  const nx = (-dy / len) * curve;
  const ny = (dx / len) * curve;
  return `M ${x1.toFixed(1)} ${y1.toFixed(1)} Q ${(mx + nx).toFixed(1)} ${(my + ny).toFixed(1)} ${x2.toFixed(1)} ${y2.toFixed(1)}`;
}

export const NeuronGraph: React.FC<NeuronGraphProps> = ({
  nodes,
  edges,
  selectedNodeId,
  onSelectNode,
  height = 540,
}) => {
  const containerRef = useRef<HTMLDivElement>(null);
  const [hoveredId, setHoveredId] = useState<string | null>(null);
  const [showBridges, setShowBridges] = useState(true);
  const [showAllLabels, setShowAllLabels] = useState(false);

  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [manualOverrides, setManualOverrides] = useState<Record<string, { x: number; y: number }>>({});

  const dragRef = useRef<
    | { mode: 'pan'; startX: number; startY: number; origX: number; origY: number }
    | { mode: 'node'; nodeId: string; startX: number; startY: number; origX: number; origY: number; moved: boolean }
    | null
  >(null);

  const visibleNodes = useMemo(() => {
    if (showBridges) return nodes;
    return nodes.filter((n) => n.kind !== 'tag' && n.kind !== 'concept');
  }, [nodes, showBridges]);

  const visibleNodeIds = useMemo(() => new Set(visibleNodes.map((n) => n.id)), [visibleNodes]);

  const visibleEdges = useMemo(
    () => edges.filter((e) => visibleNodeIds.has(e.source) && visibleNodeIds.has(e.target)),
    [edges, visibleNodeIds]
  );

  const basePositions = useMemo(
    () => computeDeterministicLayout(visibleNodes, visibleEdges),
    [visibleNodes, visibleEdges]
  );

  useEffect(() => {
    setManualOverrides({});
  }, [nodes]);

  const positions = useMemo(() => {
    const merged: Record<string, PositionedNode> = {};
    for (const [id, base] of Object.entries(basePositions)) {
      const ov = manualOverrides[id];
      merged[id] = ov ? { ...base, x: ov.x, y: ov.y } : base;
    }
    return merged;
  }, [basePositions, manualOverrides]);

  const activeFocusId = hoveredId || selectedNodeId;

  const neighborMap = useMemo(() => {
    const map = new Map<string, Set<string>>();
    for (const e of visibleEdges) {
      if (!map.has(e.source)) map.set(e.source, new Set());
      if (!map.has(e.target)) map.set(e.target, new Set());
      map.get(e.source)!.add(e.target);
      map.get(e.target)!.add(e.source);
    }
    return map;
  }, [visibleEdges]);

  const activeNeighbors = useMemo(() => {
    if (!activeFocusId) return null;
    return neighborMap.get(activeFocusId) || new Set<string>();
  }, [activeFocusId, neighborMap]);

  const fitToView = () => {
    setZoom(1);
    setPan({ x: 0, y: 0 });
  };

  const resetLayout = () => {
    setManualOverrides({});
    fitToView();
  };

  // Gently pan to selected node if selected from outside
  useEffect(() => {
    if (!selectedNodeId || !positions[selectedNodeId]) return;
    const p = positions[selectedNodeId];
    const targetPanX = (500 - p.x) * 0.42;
    const targetPanY = (330 - p.y) * 0.42;
    setPan({ x: targetPanX, y: targetPanY });
  }, [selectedNodeId]);

  const handleWheel = (e: React.WheelEvent) => {
    e.preventDefault();
    const delta = e.deltaY < 0 ? 1.1 : 0.91;
    setZoom((z) => Math.max(0.45, Math.min(2.6, Number((z * delta).toFixed(3)))));
  };

  const handleBgPointerDown = (e: React.PointerEvent<SVGSVGElement>) => {
    if ((e.target as HTMLElement).closest('[data-neuron-node="true"]')) return;
    dragRef.current = {
      mode: 'pan',
      startX: e.clientX,
      startY: e.clientY,
      origX: pan.x,
      origY: pan.y,
    };
  };

  const handleNodePointerDown = (e: React.PointerEvent, nodeId: string) => {
    e.stopPropagation();
    const p = positions[nodeId];
    if (!p) return;
    dragRef.current = {
      mode: 'node',
      nodeId,
      startX: e.clientX,
      startY: e.clientY,
      origX: p.x,
      origY: p.y,
      moved: false,
    };
  };

  const handlePointerMove = (e: React.PointerEvent<SVGSVGElement>) => {
    const drag = dragRef.current;
    if (!drag) return;
    const rect = containerRef.current?.getBoundingClientRect();
    const scaleX = rect ? 1000 / Math.max(1, rect.width) : 1;
    const scaleY = rect ? 660 / Math.max(1, rect.height) : 1;
    const dx = ((e.clientX - drag.startX) * scaleX) / zoom;
    const dy = ((e.clientY - drag.startY) * scaleY) / zoom;

    if (drag.mode === 'pan') {
      setPan({ x: drag.origX + dx, y: drag.origY + dy });
    } else if (drag.mode === 'node') {
      if (Math.abs(e.clientX - drag.startX) > 3 || Math.abs(e.clientY - drag.startY) > 3) {
        drag.moved = true;
      }
      setManualOverrides((prev) => ({
        ...prev,
        [drag.nodeId]: { x: drag.origX + dx, y: drag.origY + dy },
      }));
    }
  };

  const handlePointerUp = () => {
    const drag = dragRef.current;
    dragRef.current = null;
    if (drag && drag.mode === 'node' && !drag.moved) {
      onSelectNode(selectedNodeId === drag.nodeId ? null : drag.nodeId);
    }
  };

  const viewTransform = `translate(${500 + pan.x * zoom}, ${330 + pan.y * zoom}) scale(${zoom}) translate(-500, -330)`;

  return (
    <div ref={containerRef} className="neuron-canvas-card" style={{ height }}>
      {/* Top-left HUD controls */}
      <div className="neuron-hud-top-left">
        <button
          type="button"
          className={`neuron-hud-chip ${showBridges ? 'active' : ''}`}
          onClick={() => setShowBridges((v) => !v)}
          title="Show or hide shared tag and concept synapse nodes"
        >
          <Tag size={12} />
          <span>Tags & concepts</span>
        </button>
        <button
          type="button"
          className={`neuron-hud-chip ${showAllLabels ? 'active' : ''}`}
          onClick={() => setShowAllLabels((v) => !v)}
          title="Toggle labels for all neurons vs hubs, notes, and active connections"
        >
          {showAllLabels ? <Eye size={12} /> : <EyeOff size={12} />}
          <span>{showAllLabels ? 'All labels' : 'Clean labels'}</span>
        </button>
        {selectedNodeId && (
          <button
            type="button"
            className="neuron-hud-chip"
            onClick={() => onSelectNode(null)}
          >
            <span>Clear focus</span>
          </button>
        )}
      </div>

      {/* Top-right hint */}
      <div className="neuron-hud-top-right">
        <span>Drag nodes · Scroll to zoom · Click neuron to trace synapses</span>
      </div>

      {visibleNodes.length === 0 ? (
        <div className="neuron-empty-state">
          No memories or notes match the current filter.
        </div>
      ) : (
        <svg
          className="neuron-svg"
          viewBox="0 0 1000 660"
          preserveAspectRatio="xMidYMid meet"
          onWheel={handleWheel}
          onPointerDown={handleBgPointerDown}
          onPointerMove={handlePointerMove}
          onPointerUp={handlePointerUp}
          onPointerLeave={handlePointerUp}
          onClick={(e) => {
            if (e.target === e.currentTarget) onSelectNode(null);
          }}
        >
          <defs>
            <radialGradient id="neuron-bg-glow" cx="50%" cy="48%" r="55%">
              <stop offset="0%" stopColor="rgba(99, 102, 241, 0.14)" />
              <stop offset="55%" stopColor="rgba(56, 189, 248, 0.05)" />
              <stop offset="100%" stopColor="rgba(0, 0, 0, 0)" />
            </radialGradient>
            <filter id="neuron-glow" x="-60%" y="-60%" width="220%" height="220%">
              <feGaussianBlur stdDeviation="3.5" result="blur" />
              <feMerge>
                <feMergeNode in="blur" />
                <feMergeNode in="SourceGraphic" />
              </feMerge>
            </filter>
          </defs>

          <rect x="0" y="0" width="1000" height="660" fill="url(#neuron-bg-glow)" />

          <g transform={viewTransform}>
            {/* Synaptic Axons (Edges) */}
            <g className="neuron-edges-layer">
              {visibleEdges.map((edge, idx) => {
                const a = positions[edge.source];
                const b = positions[edge.target];
                if (!a || !b) return null;

                const isHighlighted =
                  activeFocusId !== null &&
                  (edge.source === activeFocusId || edge.target === activeFocusId);
                const isDimmed = activeFocusId !== null && !isHighlighted;
                const d = curvedPath(a.x, a.y, b.x, b.y, hashSeed(edge.id || String(idx)));
                const targetNode = visibleNodes.find((n) => n.id === edge.target);
                const strokeColor = isHighlighted
                  ? targetNode
                    ? nodeColor(targetNode)
                    : '#818cf8'
                  : edge.confidence === 'EXTRACTED'
                  ? 'rgba(148, 163, 184, 0.34)'
                  : 'rgba(148, 163, 184, 0.18)';

                return (
                  <g key={edge.id}>
                    <path
                      d={d}
                      fill="none"
                      stroke={strokeColor}
                      strokeWidth={isHighlighted ? 2.1 : 1.15}
                      strokeOpacity={isDimmed ? 0.1 : isHighlighted ? 0.92 : 0.72}
                      strokeDasharray={edge.confidence === 'INFERRED' && !isHighlighted ? '4 4' : undefined}
                    />
                    {isHighlighted && (
                      <path
                        d={d}
                        fill="none"
                        stroke="#ffffff"
                        strokeWidth={1.6}
                        strokeOpacity={0.75}
                        className="neuron-synapse-pulse"
                      />
                    )}
                  </g>
                );
              })}
            </g>

            {/* Neurons (Nodes) */}
            <g className="neuron-nodes-layer">
              {visibleNodes.map((node) => {
                const p = positions[node.id];
                if (!p) return null;
                const color = nodeColor(node);
                const isSelected = selectedNodeId === node.id;
                const isHovered = hoveredId === node.id;
                const isNeighbor = activeNeighbors?.has(node.id) ?? false;
                const isDimmed =
                  activeFocusId !== null &&
                  node.id !== activeFocusId &&
                  !isNeighbor;

                const showLabel =
                  showAllLabels ||
                  node.kind === 'hub' ||
                  node.kind === 'note' ||
                  isSelected ||
                  isHovered ||
                  isNeighbor;

                const shortLabel =
                  node.label.length > 26 ? `${node.label.slice(0, 25)}…` : node.label;

                return (
                  <g
                    key={node.id}
                    data-neuron-node="true"
                    transform={`translate(${p.x.toFixed(1)}, ${p.y.toFixed(1)})`}
                    style={{
                      cursor: 'pointer',
                      opacity: isDimmed ? 0.2 : 1,
                      transition: 'opacity 160ms ease',
                    }}
                    onPointerEnter={() => setHoveredId(node.id)}
                    onPointerLeave={() => setHoveredId((prev) => (prev === node.id ? null : prev))}
                    onPointerDown={(e) => handleNodePointerDown(e, node.id)}
                  >
                    {/* Outer neural halo */}
                    <circle
                      r={p.radius + (isSelected || isHovered ? 9 : node.kind === 'hub' ? 6 : 4)}
                      fill={color}
                      fillOpacity={isSelected ? 0.24 : isHovered ? 0.2 : node.kind === 'hub' ? 0.14 : 0.08}
                    />

                    {/* Selection ring */}
                    {(isSelected || isHovered) && (
                      <circle
                        r={p.radius + 4}
                        fill="none"
                        stroke="#ffffff"
                        strokeOpacity={isSelected ? 0.9 : 0.55}
                        strokeWidth={1.5}
                      />
                    )}

                    {/* Soma core */}
                    <circle
                      r={p.radius}
                      fill={node.kind === 'hub' ? '#131622' : color}
                      fillOpacity={node.kind === 'hub' ? 0.96 : 0.92}
                      stroke={color}
                      strokeWidth={node.kind === 'hub' ? 2.6 : 1.4}
                      filter={node.kind === 'hub' || isSelected ? 'url(#neuron-glow)' : undefined}
                    />

                    {node.kind === 'hub' && (
                      <circle r={Math.max(4, p.radius * 0.36)} fill={color} />
                    )}

                    {showLabel && (
                      <g transform={`translate(0, ${p.radius + 14})`} pointerEvents="none">
                        <rect
                          x={-(shortLabel.length * 3.3 + 8)}
                          y={-9}
                          width={shortLabel.length * 6.6 + 16}
                          height={18}
                          rx={6}
                          fill="rgba(12, 14, 20, 0.84)"
                          stroke={isSelected ? color : 'rgba(255, 255, 255, 0.12)'}
                          strokeWidth={0.8}
                        />
                        <text
                          textAnchor="middle"
                          dominantBaseline="central"
                          fill={isSelected ? '#ffffff' : 'rgba(236, 238, 242, 0.92)'}
                          fontSize={node.kind === 'hub' ? 11 : 10}
                          fontWeight={node.kind === 'hub' || isSelected ? 600 : 500}
                        >
                          {shortLabel}
                        </text>
                      </g>
                    )}
                  </g>
                );
              })}
            </g>
          </g>
        </svg>
      )}

      {/* Bottom-left Legend */}
      <div className="neuron-legend">
        <span className="neuron-legend-item">
          <i style={{ backgroundColor: '#818cf8' }} /> Topic Hub
        </span>
        <span className="neuron-legend-item">
          <i style={{ backgroundColor: '#f59e0b' }} /> Note
        </span>
        <span className="neuron-legend-item">
          <i style={{ backgroundColor: '#a78bfa' }} /> Workflow
        </span>
        <span className="neuron-legend-item">
          <i style={{ backgroundColor: '#60a5fa' }} /> Fact
        </span>
        <span className="neuron-legend-item">
          <i style={{ backgroundColor: '#34d399' }} /> Profile
        </span>
        <span className="neuron-legend-item">
          <i style={{ backgroundColor: '#2dd4bf' }} /> Tag / Concept
        </span>
      </div>

      {/* Bottom-right Zoom controls */}
      <div className="neuron-zoom-controls">
        <button
          type="button"
          className="neuron-zoom-btn"
          onClick={() => setZoom((z) => Math.max(0.45, Number((z - 0.15).toFixed(2))))}
          title="Zoom out"
          aria-label="Zoom out"
        >
          <ZoomOut size={13} />
        </button>
        <span className="neuron-zoom-pct tabular-nums">{Math.round(zoom * 100)}%</span>
        <button
          type="button"
          className="neuron-zoom-btn"
          onClick={() => setZoom((z) => Math.min(2.6, Number((z + 0.15).toFixed(2))))}
          title="Zoom in"
          aria-label="Zoom in"
        >
          <ZoomIn size={13} />
        </button>
        <button
          type="button"
          className="neuron-zoom-btn"
          onClick={fitToView}
          title="Fit view"
          aria-label="Fit view"
        >
          <Maximize2 size={13} />
        </button>
        <button
          type="button"
          className="neuron-zoom-btn"
          onClick={resetLayout}
          title="Reset neuron positions"
          aria-label="Reset layout"
        >
          <RotateCcw size={13} />
        </button>
      </div>
    </div>
  );
};
