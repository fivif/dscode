import { useEffect, useRef, useState, useCallback } from 'react';
import * as d3 from 'd3';
import type { WikiGraph } from '@/lib/types';

const NODE_COLORS: Record<string, string> = {
  concept: '#a78bfa',
  fact: '#34d399',
  pattern: '#fbbf24',
  decision: '#60a5fa',
  rule: '#f472b6',
};

const BG = '#0a0a0f';
const EDGE_COLOR = '#374151';
const GRAPH_HEIGHT = 420;

function rScale(deg: number): number {
  return Math.max(4, Math.min(16, 4 + deg * 1.2));
}

function computeDegrees(edges: Array<{ source: string; target: string }>): Map<string, number> {
  const deg = new Map<string, number>();
  for (const e of edges) {
    deg.set(e.source, (deg.get(e.source) || 0) + 1);
    deg.set(e.target, (deg.get(e.target) || 0) + 1);
  }
  return deg;
}

interface GraphState {
  svg: d3.Selection<SVGSVGElement, unknown, null, undefined>;
  zoom: d3.ZoomBehavior<SVGSVGElement, unknown>;
  simulation: d3.Simulation<any, any>;
  nodes: any[];
  width: number;
  height: number;
}

export default function WikiGraphView({ graph }: { graph: WikiGraph }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const stateRef = useRef<GraphState | null>(null);
  const [selected, setSelected] = useState<any>(null);

  // ── Fit graph to bounds ──
  const fitToBounds = useCallback(() => {
    const state = stateRef.current;
    if (!state) return;
    const { svg, zoom, simulation, width: cw, height: ch } = state;
    const ns = simulation.nodes();
    if (!ns.length) return;
    const xExt = d3.extent(ns, (d: any) => d.x) as [number, number] | [undefined, undefined];
    const yExt = d3.extent(ns, (d: any) => d.y) as [number, number] | [undefined, undefined];
    if (xExt[0] == null || yExt[0] == null) return;
    const dx = (xExt[1] as number) - (xExt[0] as number) || 1;
    const dy = (yExt[1] as number) - (yExt[0] as number) || 1;
    const cx = ((xExt[0] as number) + (xExt[1] as number)) / 2;
    const cy = ((yExt[0] as number) + (yExt[1] as number)) / 2;
    const pad = 40;
    const scale = 0.85 / Math.max(dx / (cw - pad * 2), dy / (ch - pad * 2), 0.2);
    const tx = cw / 2 - cx * scale;
    const ty = ch / 2 - cy * scale;
    svg.transition().duration(500).call(
      zoom.transform,
      d3.zoomIdentity.translate(tx, ty).scale(Math.min(scale, 2)),
    );
  }, []);

  const zoomIn = useCallback(() => {
    const state = stateRef.current;
    if (!state) return;
    state.svg.transition().duration(300).call(state.zoom.scaleBy, 1.5);
  }, []);

  const zoomOut = useCallback(() => {
    const state = stateRef.current;
    if (!state) return;
    state.svg.transition().duration(300).call(state.zoom.scaleBy, 0.7);
  }, []);

  // ── Main render effect ──
  useEffect(() => {
    const container = containerRef.current;
    if (!container || !graph?.nodes?.length) return;

    // Kill previous simulation and clear DOM
    if (stateRef.current) {
      stateRef.current.simulation.stop();
      stateRef.current = null;
    }
    container.innerHTML = '';

    const cw = container.clientWidth || 700;
    if (cw < 50) return;
    const ch = GRAPH_HEIGHT;

    // ── Prepare data (dedup by title, keep only nodes with edges) ──
    const deg = computeDegrees(graph.edges || []);
    const seen = new Set<string>();
    const nodes: any[] = [];
    for (const n of graph.nodes) {
      const key = n.title || n.id;
      if (seen.has(key)) continue;
      seen.add(key);
      const d = deg.get(n.id) || 0;
      // Only include nodes with at least 1 edge (connected) or top 10 by degree
      nodes.push({
        id: n.id,
        title: n.title || n.id,
        _deg: d,
        _type: n.node_type || 'fact',
        _data: n,
        x: cw / 2 + (Math.random() - 0.5) * cw * 0.3,
        y: ch / 2 + (Math.random() - 0.5) * ch * 0.3,
      });
    }
    // Sort by degree, keep top 100
    nodes.sort((a, b) => b._deg - a._deg);
    const displayNodes = nodes.slice(0, Math.min(100, nodes.length));

    const nodeMap = new Map<string, any>(displayNodes.map((n) => [n.id, n]));
    const edges = (graph.edges || [])
      .filter((e) => nodeMap.has(e.source) && nodeMap.has(e.target))
      .map((e) => ({ source: e.source, target: e.target }));

    // ── SVG container ──
    const svg = d3.select(container)
      .append('svg')
      .attr('viewBox', [0, 0, cw, ch])
      .attr('width', cw)
      .attr('height', ch)
      .style('display', 'block');

    // Background rect
    svg.append('rect')
      .attr('width', cw)
      .attr('height', ch)
      .attr('fill', BG);

    // ── Group (appended first, elements inside it later) ──
    const g = svg.append('g');

    // ── Simulation ──
    const simulation = d3.forceSimulation(displayNodes)
      .force('link', d3.forceLink(edges).id((d: any) => d.id).distance(55).strength(0.4))
      .force('charge', d3.forceManyBody().strength(-80))
      .force('center', d3.forceCenter(cw / 2, ch / 2))
      .force('collide', d3.forceCollide((d: any) => rScale(d._deg || 1) + 10).strength(0.6))
      .alphaDecay(0.008)
      .alpha(0.6);

    // ── Edges ──
    const link = g.append('g')
      .selectAll('line')
      .data(edges)
      .join('line')
      .attr('stroke', EDGE_COLOR)
      .attr('stroke-width', 0.5)
      .attr('stroke-opacity', 0.15);

    // ── Nodes ──
    const node = g.append('g')
      .selectAll('circle')
      .data(displayNodes)
      .join('circle')
      .attr('r', (d: any) => rScale(d._deg || 1))
      .attr('fill', (d: any) => NODE_COLORS[d._type] || '#6b7280')
      .attr('stroke', '#1a1b1e')
      .attr('stroke-width', 1.5)
      .style('cursor', 'pointer');

    // Tooltips (native SVG <title>)
    node.append('title').text((d: any) => d.title || d.id);

    // ── Labels ──
    const label = g.append('g')
      .selectAll('text')
      .data(displayNodes)
      .join('text')
      .text((d: any) => {
        const t = d.title || '';
        return t.length > 20 ? t.slice(0, 18) + '…' : t;
      })
      .attr('dx', 0)
      .attr('dy', 14)
      .attr('text-anchor', 'middle')
      .attr('font-size', 8)
      .attr('font-family', 'system-ui, -apple-system, sans-serif')
      .attr('fill', '#8b8d91')
      .style('pointer-events', 'none')
      .style('user-select', 'none');

    // ── Hover ──
    node.on('mouseenter', function (_, d: any) {
      d3.select(this)
        .transition().duration(120)
        .attr('r', rScale(d._deg || 1) + 3)
        .attr('stroke', '#fff')
        .attr('stroke-width', 2.5);
      label.filter((nd: any) => nd.id === d.id)
        .transition().duration(120)
        .attr('fill', '#e0e2e6')
        .attr('font-size', 11);
    });

    node.on('mouseleave', function (_, d: any) {
      d3.select(this)
        .transition().duration(120)
        .attr('r', rScale(d._deg || 1))
        .attr('stroke', '#1a1b1e')
        .attr('stroke-width', 1.5);
      label.filter((nd: any) => nd.id === d.id)
        .transition().duration(120)
        .attr('fill', '#8b8d91')
        .attr('font-size', 8);
    });

    // ── Click → select ──
    node.on('click', (_, d: any) => {
      setSelected(d._data);
    });

    // ── Drag (pause simulation while dragging for smooth control) ──
    const drag = d3.drag<any, any>()
      .on('start', (ev, d: any) => {
        simulation.stop();  // Freeze layout during drag
        d.fx = d.x;
        d.fy = d.y;
      })
      .on('drag', (ev, d: any) => {
        d.fx = ev.x;
        d.fy = ev.y;
      })
      .on('end', () => {
        simulation.alpha(0.02).restart();  // Minimal reheat after drag
      });
    node.call(drag);

    // ── Zoom behaviour (AFTER elements exist so handler can reference them) ──
    const zoom = d3.zoom<SVGSVGElement, unknown>()
      .scaleExtent([0.08, 8])
      .on('zoom', (ev) => {
        g.attr('transform', ev.transform);
        const k = ev.transform.k;
        (label.style as any)('display', k > 0.6 ? null : 'none');
        label.attr('font-size', (d: any) =>
          Math.max(6, Math.min(12, rScale(d._deg || 1) * k * 0.8)),
        );
        link.attr('stroke-opacity', Math.min(0.25, 0.06 + k * 0.10));
      });
    svg.call(zoom);

    // ── Tick (stop after 350 for smooth settling) ──
    let tickCount = 0;
    simulation.on('tick', () => {
      tickCount++;
      link.attr('x1', (d: any) => d.source.x).attr('y1', (d: any) => d.source.y)
        .attr('x2', (d: any) => d.target.x).attr('y2', (d: any) => d.target.y);
      node.attr('cx', (d: any) => d.x).attr('cy', (d: any) => d.y);
      label.attr('x', (d: any) => d.x).attr('y', (d: any) => d.y);
      if (tickCount > 350) simulation.stop();
    });

    // ── Fit once layout settles ──
    simulation.on('end', () => {
      requestAnimationFrame(fitToBounds);
    });

    stateRef.current = { svg, zoom, simulation, nodes: displayNodes, width: cw, height: ch };

    return () => {
      simulation.stop();
      stateRef.current = null;
      container.innerHTML = '';
    };
  }, [graph, fitToBounds]);

  // ── Empty state ──
  if (!graph?.nodes?.length) {
    return (
      <div
        className="flex flex-col items-center justify-center rounded-lg border border-border"
        style={{ height: GRAPH_HEIGHT, backgroundColor: BG }}
      >
        <p className="text-gray-500 text-sm">图谱为空，摄入会话数据后自动生成</p>
      </div>
    );
  }

  return (
    <div>
      {/* Graph + toolbar */}
      <div
        className="rounded-lg border border-border overflow-hidden relative"
        style={{ height: GRAPH_HEIGHT, backgroundColor: BG }}
      >
        <div ref={containerRef} style={{ width: '100%', height: '100%' }} />

        {/* Toolbar */}
        <div className="absolute top-2 right-2 flex gap-1 z-10">
          <button
            onClick={zoomIn}
            className="w-7 h-7 flex items-center justify-center rounded bg-[#1f2937] text-gray-300 hover:bg-[#374151] text-sm font-mono select-none"
            title="放大"
          >
            +
          </button>
          <button
            onClick={zoomOut}
            className="w-7 h-7 flex items-center justify-center rounded bg-[#1f2937] text-gray-300 hover:bg-[#374151] text-sm font-mono select-none"
            title="缩小"
          >
            &minus;
          </button>
          <button
            onClick={fitToBounds}
            className="w-7 h-7 flex items-center justify-center rounded bg-[#1f2937] text-gray-300 hover:bg-[#374151] text-sm select-none"
            title="适配视图"
          >
            &#x21A7;
          </button>
        </div>
      </div>

      {/* Stats + usage hint */}
      <div className="flex items-center gap-2 mt-2">
        <span className="text-[10px] text-gray-600">
          {graph.nodes.length} 节点 &middot; {graph.edges?.length || 0} 边
        </span>
        <span className="text-[10px] text-gray-500 ml-auto">
          滚轮缩放 &middot; 拖拽移动
        </span>
      </div>

      {/* Legend */}
      <div className="flex items-center gap-3 mt-1 flex-wrap">
        {Object.entries(NODE_COLORS).map(([type, color]) => (
          <span key={type} className="flex items-center gap-1 text-[10px] text-gray-500">
            <span className="w-2 h-2 rounded-full" style={{ backgroundColor: color }} />
            {type}
          </span>
        ))}
      </div>

      {/* Selected node detail card */}
      {selected && (
        <div className="mt-3 p-3 bg-card border border-border rounded-lg">
          <div className="flex items-center gap-2 mb-1">
            <span
              className="text-[10px] px-1.5 py-0.5 rounded"
              style={{
                backgroundColor: (NODE_COLORS[selected.node_type] || '#6b7280') + '30',
                color: NODE_COLORS[selected.node_type] || '#6b7280',
              }}
            >
              {selected.node_type || 'fact'}
            </span>
            <h3 className="text-sm font-medium text-gray-200">{selected.title}</h3>
          </div>
          <p className="text-xs text-gray-400 leading-relaxed">{selected.content}</p>
        </div>
      )}
    </div>
  );
}
