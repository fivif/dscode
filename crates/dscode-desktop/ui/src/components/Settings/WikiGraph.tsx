import { useEffect, useRef, useState, useMemo } from 'react';
import * as d3 from 'd3';
import type { WikiGraph } from '@/lib/types';

// ── color palettes ──
const NODE_COLORS: Record<string, string> = {
  concept: '#a78bfa',
  fact: '#34d399',
  pattern: '#fbbf24',
  decision: '#60a5fa',
  rule: '#f472b6',
};

const COMMUNITY_COLORS = [
  '#ef4444', '#3b82f6', '#10b981', '#f59e0b',
  '#8b5cf6', '#ec4899', '#06b6d4', '#f97316',
  '#84cc16', '#14b8a6', '#6366f1', '#e11d48',
];

const TYPE_LABELS: Record<string, string> = {
  concept: 'Concept',
  fact: 'Fact',
  pattern: 'Pattern',
  decision: 'Decision',
  rule: 'Rule',
};

const BG = '#0a0a0f';
const GRAPH_H = 480;

// ── label propagation community detection ──
function detectCommunities(
  nodeIds: string[],
  edges: Array<{ source: string; target: string }>,
  iterations = 8,
): Map<string, number> {
  const adj = new Map<string, Set<string>>();
  for (const id of nodeIds) adj.set(id, new Set());
  for (const e of edges) {
    adj.get(e.source)?.add(e.target);
    adj.get(e.target)?.add(e.source);
  }

  const labels = new Map<string, number>();
  nodeIds.forEach((id, i) => labels.set(id, i));

  for (let iter = 0; iter < iterations; iter++) {
    const order = [...nodeIds].sort(() => Math.random() - 0.5);
    for (const id of order) {
      const neighbors = adj.get(id);
      if (!neighbors || neighbors.size === 0) continue;
      const counts = new Map<number, number>();
      for (const nb of neighbors) {
        const l = labels.get(nb)!;
        counts.set(l, (counts.get(l) || 0) + 1);
      }
      let bestLabel = labels.get(id)!;
      let bestCount = 0;
      for (const [l, c] of counts) {
        if (c > bestCount) { bestCount = c; bestLabel = l; }
      }
      labels.set(id, bestLabel);
    }
  }

  // compact labels to 0..k
  const seen = new Map<number, number>();
  let next = 0;
  const compacted = new Map<string, number>();
  for (const id of nodeIds) {
    const raw = labels.get(id)!;
    if (!seen.has(raw)) { seen.set(raw, next++); }
    compacted.set(id, seen.get(raw)!);
  }
  return compacted;
}

export default function WikiGraphView({ graph, highlightId }: { graph: WikiGraph; highlightId?: string }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const zoomRef = useRef<d3.ZoomBehavior<SVGSVGElement, unknown> | null>(null);
  const simRef = useRef<d3.Simulation<any, any> | null>(null);
  const [communityMode, setCommunityMode] = useState(false);

  // ── prepare display data ──
  const { displayNodes, displayEdges, communities } = useMemo(() => {
    if (!graph?.nodes?.length) return { displayNodes: [], displayEdges: [], communities: new Map<string, number>() };

    let nodes: any[];
    let edges: any[];

    if (highlightId) {
      // neighborhood view
      const neighborIds = new Set<string>();
      neighborIds.add(highlightId);
      for (const e of graph.edges || []) {
        if (e.source === highlightId) neighborIds.add(e.target);
        if (e.target === highlightId) neighborIds.add(e.source);
      }
      nodes = (graph.nodes || []).filter((n: any) => neighborIds.has(n.id));
      edges = (graph.edges || []).filter(
        (e: any) => neighborIds.has(e.source) && neighborIds.has(e.target),
      );
    } else {
      // global: top nodes by degree
      const deg = new Map<string, number>();
      for (const e of graph.edges || []) {
        deg.set(e.source, (deg.get(e.source) || 0) + 1);
        deg.set(e.target, (deg.get(e.target) || 0) + 1);
      }
      const sorted = [...(graph.nodes || [])].sort(
        (a: any, b: any) => (deg.get(b.id) || 0) - (deg.get(a.id) || 0),
      );
      const topN = sorted.slice(0, 80);
      const ids = new Set(topN.map((n: any) => n.id));
      nodes = topN;
      edges = (graph.edges || []).filter(
        (e: any) => ids.has(e.source) && ids.has(e.target),
      );
    }

    const nodeIds = nodes.map((n: any) => n.id);
    const comms = detectCommunities(nodeIds, edges);

    return { displayNodes: nodes, displayEdges: edges, communities: comms };
  }, [graph, highlightId]);

  // ── D3 render ──
  useEffect(() => {
    if (!displayNodes.length) return;
    const el = containerRef.current;
    if (!el) return;
    const W = el.clientWidth || 700;
    const H = GRAPH_H;
    el.innerHTML = '';

    // compute degree for sizing
    const degree = new Map<string, number>();
    for (const e of displayEdges) {
      degree.set(e.source, (degree.get(e.source) || 0) + 1);
      degree.set(e.target, (degree.get(e.target) || 0) + 1);
    }
    const maxDeg = Math.max(1, ...degree.values());

    // map to D3 simulation nodes
    const simNodes: any[] = displayNodes.map((n: any) => {
      const d = degree.get(n.id) || 0;
      return {
        id: n.id,
        title: n.title || n.id,
        _type: n.node_type || 'fact',
        _deg: d,
        _r: 4 + (d / maxDeg) * 10,
        _community: communities.get(n.id) ?? 0,
        x: W / 2 + (Math.random() - 0.5) * W * 0.3,
        y: H / 2 + (Math.random() - 0.5) * H * 0.3,
      };
    });

    const nodeMap = new Map(simNodes.map((n) => [n.id, n]));

    // build edge weight map for stroke-width
    const edgeWeights = new Map<string, number>();
    for (const e of displayEdges) {
      edgeWeights.set(`${e.source}::${e.target}`, e.weight || 1);
      edgeWeights.set(`${e.target}::${e.source}`, e.weight || 1);
    }
    const maxW = Math.max(0.5, ...edgeWeights.values());

    const simEdges = displayEdges
      .filter((e: any) => nodeMap.has(e.source) && nodeMap.has(e.target))
      .map((e: any) => ({
        source: e.source,
        target: e.target,
        _weight: e.weight || 1,
      }));

    // ── SVG setup ──
    const svg = d3
      .select(el)
      .append('svg')
      .attr('width', W)
      .attr('height', H)
      .style('background', BG)
      .style('border-radius', '8px');

    // gradient def for edges
    const defs = svg.append('defs');

    const g = svg.append('g');

    // ── simulation ──
    const sim = d3
      .forceSimulation(simNodes)
      .force(
        'link',
        d3
          .forceLink(simEdges)
          .id((d: any) => d.id)
          .distance(40)
          .strength((l: any) => 0.15 * (l._weight / maxW)),
      )
      .force('charge', d3.forceManyBody().strength(-60))
      .force('center', d3.forceCenter(W / 2, H / 2).strength(0.2))
      .force('collide', d3.forceCollide((d: any) => d._r + 2))
      .alphaDecay(0.015)
      .alpha(0.6);

    simRef.current = sim;

    // ── edges ──
    const link = g
      .selectAll('line')
      .data(simEdges)
      .join('line')
      .attr('stroke', '#374151')
      .attr('stroke-width', (d: any) => {
        const w = d._weight || 1;
        return 0.3 + (w / maxW) * 3;
      })
      .attr('stroke-opacity', 0.5);

    // ── nodes ──
    const node = g
      .selectAll('circle')
      .data(simNodes)
      .join('circle')
      .attr('r', (d: any) => d._r)
      .attr('fill', (d: any) => NODE_COLORS[d._type] || '#888')
      .attr('stroke', '#1f2937')
      .attr('stroke-width', 1.5)
      .style('cursor', 'pointer')
      .style('transition', 'fill 0.3s ease');

    node.append('title').text((d: any) => `${d.title}\nType: ${d._type}\nDegree: ${d._deg}`);

    // ── labels ──
    const label = g
      .selectAll('text')
      .data(simNodes)
      .join('text')
      .text((d: any) => {
        const t = d.title || '';
        return t.length > 14 ? t.slice(0, 12) + '…' : t;
      })
      .attr('dy', (d: any) => d._r + 12)
      .attr('text-anchor', 'middle')
      .attr('font-size', 8)
      .attr('fill', '#6b7280')
      .style('pointer-events', 'none')
      .style('user-select', 'none');

    // ── drag ──
    node.call(
      d3
        .drag<any, any>()
        .on('start', function (ev, d: any) {
          if (!ev.active) sim.alphaTarget(0.3).restart();
          d.fx = d.x;
          d.fy = d.y;
        })
        .on('drag', function (ev, d: any) {
          d.fx = ev.x;
          d.fy = ev.y;
        })
        .on('end', function (ev, d: any) {
          if (!ev.active) sim.alphaTarget(0);
          d.fx = null;
          d.fy = null;
        }) as any,
    );

    // ── zoom ──
    const zoom = d3
      .zoom<SVGSVGElement, unknown>()
      .scaleExtent([0.15, 6])
      .on('zoom', (ev) => {
        g.attr('transform', ev.transform.toString());
      });
    svg.call(zoom);
    zoomRef.current = zoom;

    // ── tick ──
    sim.on('tick', () => {
      link
        .attr('x1', (d: any) => d.source.x)
        .attr('y1', (d: any) => d.source.y)
        .attr('x2', (d: any) => d.target.x)
        .attr('y2', (d: any) => d.target.y);

      node.attr('cx', (d: any) => d.x).attr('cy', (d: any) => d.y);
      label.attr('x', (d: any) => d.x).attr('y', (d: any) => d.y);
    });

    return () => {
      sim.stop();
    };
  }, [displayNodes, displayEdges, communities]);

  // ── update node colors when community mode toggles ──
  useEffect(() => {
    const svg = d3.select(containerRef.current).select('svg');
    if (svg.empty()) return;
    svg
      .selectAll('circle')
      .transition()
      .duration(400)
      .attr('fill', (d: any) => {
        if (communityMode) {
          const idx = (d._community ?? 0) % COMMUNITY_COLORS.length;
          return COMMUNITY_COLORS[idx];
        }
        return NODE_COLORS[d._type] || '#888';
      });
  }, [communityMode, displayNodes]);

  // ── toolbar handlers ──
  const zoomIn = () => {
    const svgEl = containerRef.current?.querySelector('svg');
    if (!svgEl || !zoomRef.current) return;
    d3.select(svgEl).transition().duration(250).call(zoomRef.current.scaleBy, 1.4);
  };

  const zoomOut = () => {
    const svgEl = containerRef.current?.querySelector('svg');
    if (!svgEl || !zoomRef.current) return;
    d3.select(svgEl).transition().duration(250).call(zoomRef.current.scaleBy, 0.7);
  };

  const fitToView = () => {
    const svgEl = containerRef.current?.querySelector('svg');
    if (!svgEl || !zoomRef.current) return;
    const gEl = svgEl.querySelector('g');
    if (!gEl) return;
    const bbox = gEl.getBBox();
    if (bbox.width === 0 || bbox.height === 0) return;
    const W = containerRef.current!.clientWidth || 700;
    const H = GRAPH_H;
    const pad = 40;
    const scale = Math.min((W - pad * 2) / bbox.width, (H - pad * 2) / bbox.height, 2);
    const tx = W / 2 - (bbox.x + bbox.width / 2) * scale;
    const ty = H / 2 - (bbox.y + bbox.height / 2) * scale;
    d3
      .select(svgEl)
      .transition()
      .duration(500)
      .call(zoomRef.current.transform, d3.zoomIdentity.translate(tx, ty).scale(scale));
  };

  // ── community color mapping for legend ──
  const communityCount = useMemo(() => {
    const set = new Set<number>();
    for (const n of displayNodes) {
      set.add(communities.get(n.id) ?? 0);
    }
    return set.size;
  }, [displayNodes, communities]);

  if (!graph?.nodes?.length) {
    return <p className="text-gray-500 text-sm text-center py-8">Graph is empty</p>;
  }

  return (
    <div>
      {/* ── stats row ── */}
      <div className="flex items-center gap-4 mb-2 text-[11px] text-gray-500">
        <span>
          Nodes <strong className="text-gray-300">{displayNodes.length}</strong>
        </span>
        <span>
          Edges <strong className="text-gray-300">{displayEdges.length}</strong>
        </span>
        {highlightId && (
          <span className="text-blue-400">
            &middot; Neighborhood view
          </span>
        )}
      </div>

      {/* ── graph canvas ── */}
      <div
        ref={containerRef}
        className="rounded-lg border border-border overflow-hidden"
        style={{ height: GRAPH_H, background: BG }}
      />

      {/* ── toolbar ── */}
      <div className="flex items-center gap-1 mt-2">
        <button
          onClick={zoomIn}
          className="w-7 h-7 flex items-center justify-center rounded bg-gray-800 hover:bg-gray-700 text-gray-400 hover:text-gray-200 text-sm transition-colors"
          title="Zoom in"
        >
          +
        </button>
        <button
          onClick={zoomOut}
          className="w-7 h-7 flex items-center justify-center rounded bg-gray-800 hover:bg-gray-700 text-gray-400 hover:text-gray-200 text-sm transition-colors"
          title="Zoom out"
        >
          &minus;
        </button>
        <button
          onClick={fitToView}
          className="w-7 h-7 flex items-center justify-center rounded bg-gray-800 hover:bg-gray-700 text-gray-400 hover:text-gray-200 text-sm transition-colors"
          title="Fit to view"
        >
          <svg className="w-3.5 h-3.5" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
            <path strokeLinecap="round" strokeLinejoin="round" d="M4 8V4m0 0h4M4 4l5 5m11-1V4m0 0h-4m4 0l-5 5M4 16v4m0 0h4m-4 0l5-5m11 5l-5-5m5 5v-4m0 4h-4" />
          </svg>
        </button>
        <span className="text-[10px] text-gray-600 ml-auto">
          Scroll to zoom &middot; Drag to pan
        </span>
      </div>

      {/* ── legend ── */}
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 mt-2 text-[10px]">
        {/* type legend */}
        <span className="text-gray-600 mr-1">Types:</span>
        {Object.entries(NODE_COLORS).map(([t, c]) => (
          <span key={t} className="flex items-center gap-1 text-gray-500">
            <span className="w-2.5 h-2.5 rounded-full shrink-0" style={{ background: c }} />
            {TYPE_LABELS[t] || t}
          </span>
        ))}

        {/* community toggle */}
        <span className="text-gray-700 mx-1">|</span>
        <label className="flex items-center gap-1.5 text-gray-500 cursor-pointer select-none">
          <input
            type="checkbox"
            checked={communityMode}
            onChange={(e) => setCommunityMode(e.target.checked)}
            className="w-3 h-3 rounded accent-purple-600 cursor-pointer"
          />
          Communities ({communityCount})
        </label>

        {communityMode && (
          <span className="flex items-center gap-1 ml-1">
            {Array.from({ length: Math.min(communityCount, 6) }, (_, i) => (
              <span
                key={i}
                className="w-2.5 h-2.5 rounded-full shrink-0"
                style={{ background: COMMUNITY_COLORS[i % COMMUNITY_COLORS.length] }}
              />
            ))}
            {communityCount > 6 && <span className="text-gray-600">+{communityCount - 6}</span>}
          </span>
        )}
      </div>
    </div>
  );
}
