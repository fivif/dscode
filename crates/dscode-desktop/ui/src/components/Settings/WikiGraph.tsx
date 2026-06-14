import { useEffect, useRef, useState } from 'react';
import * as d3 from 'd3';
import type { WikiGraph } from '@/lib/types';

const MAX_NODES = 80;
const NODE_COLORS: Record<string, string> = {
  concept: '#a78bfa', fact: '#34d399', pattern: '#fbbf24',
  decision: '#60a5fa', rule: '#f472b6',
};

export default function WikiGraphView({ graph }: { graph: WikiGraph }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const [stats, setStats] = useState({ nodes: 0, edges: 0 });
  const gRef = useRef<any>(null);

  useEffect(() => {
    if (!graph?.nodes?.length) return;
    const el = containerRef.current;
    if (!el) return;

    const W = el.clientWidth || 700;
    const H = 420;

    // Compute degree and cap nodes
    const deg = new Map<string, number>();
    for (const e of graph.edges || []) {
      deg.set(e.source, (deg.get(e.source) || 0) + 1);
      deg.set(e.target, (deg.get(e.target) || 0) + 1);
    }

    let rawNodes = (graph.nodes || []).map((n: any) => ({
      id: n.id || n.title, title: n.title || n.id || '?',
      _deg: deg.get(n.id || n.title) || 0,
      _type: n.node_type || 'fact',
    }));

    // Sort by degree, take top N
    rawNodes.sort((a, b) => b._deg - a._deg);
    const capped = rawNodes.slice(0, MAX_NODES);

    const nodeMap = new Map(capped.map(n => [n.id, n]));
    const edges = (graph.edges || [])
      .filter(e => nodeMap.has(e.source) && nodeMap.has(e.target))
      .map(e => ({ source: e.source, target: e.target }));

    setStats({ nodes: capped.length, edges: edges.length });

    // Clear
    el.innerHTML = '';

    const svg = d3.select(el).append('svg')
      .attr('width', W).attr('height', H)
      .style('background', '#0a0a0f');

    const g = svg.append('g');

    const zoom = d3.zoom<SVGSVGElement, unknown>()
      .scaleExtent([0.15, 4])
      .on('zoom', (ev) => { g.attr('transform', ev.transform.toString()); });
    svg.call(zoom);

    const rScale = (d: number) => Math.max(3, Math.min(16, 4 + d * 1.5));

    const sim = d3.forceSimulation(capped as any)
      .force('link', d3.forceLink(edges).id((d: any) => d.id).distance(60))
      .force('charge', d3.forceManyBody().strength(-120))
      .force('center', d3.forceCenter(W / 2, H / 2))
      .force('collide', d3.forceCollide((d: any) => rScale(d._deg) + 5))
      .alphaDecay(0.03)
      .alpha(0.5);

    // Edges
    const link = g.append('g').selectAll('line').data(edges).join('line')
      .attr('stroke', '#2a2d35').attr('stroke-width', 0.6);

    // Nodes
    const node = g.append('g').selectAll('circle').data(capped).join('circle')
      .attr('r', (d: any) => rScale(d._deg))
      .attr('fill', (d: any) => NODE_COLORS[d._type] || '#6b7280')
      .attr('stroke', '#1f2937').attr('stroke-width', 1)
      .style('cursor', 'pointer');

    // Labels (hidden when >40 nodes)
    const hideLabels = capped.length > 40;
    const label = g.append('g').selectAll('text').data(capped).join('text')
      .text((d: any) => (d.title || '').length > 12 ? d.title.slice(0, 10) + '…' : (d.title || ''))
      .attr('dy', (d: any) => rScale(d._deg) + 10)
      .attr('text-anchor', 'middle')
      .attr('font-size', 8).attr('font-family', 'system-ui, sans-serif')
      .attr('fill', '#6b7280')
      .style('pointer-events', 'none')
      .style('display', hideLabels ? 'none' : null);

    // Hover highlight
    node.on('mouseenter', function (_: any, d: any) {
      const r = rScale(d._deg);
      d3.select(this).transition().duration(100)
        .attr('r', r + 3).attr('stroke', '#fff').attr('stroke-width', 2);
      label.filter((n: any) => n.id === d.id)
        .attr('fill', '#e5e7eb').attr('font-size', 10).style('display', null);
    });
    node.on('mouseleave', function (_: any, d: any) {
      d3.select(this).transition().duration(100)
        .attr('r', rScale(d._deg)).attr('stroke', '#1f2937').attr('stroke-width', 1);
      label.filter((n: any) => n.id === d.id)
        .attr('fill', '#6b7280').attr('font-size', 8);
      if (hideLabels) label.style('display', 'none');
    });

    // Drag
    const drag = d3.drag<any, any>()
      .on('start', (ev, d) => { if (!ev.active) sim.alphaTarget(0.05).restart(); d.fx = d.x; d.fy = d.y; })
      .on('drag', (ev, d) => { d.fx = ev.x; d.fy = ev.y; })
      .on('end', (ev, d) => { if (!ev.active) sim.alphaTarget(0); d.fx = null; d.fy = null; });
    (node as any).call(drag);

    // Tick — update positions, stop after layout settles
    let tickCount = 0;
    sim.on('tick', () => {
      link.attr('x1', (d: any) => d.source.x).attr('y1', (d: any) => d.source.y)
        .attr('x2', (d: any) => d.target.x).attr('y2', (d: any) => d.target.y);
      node.attr('cx', (d: any) => d.x).attr('cy', (d: any) => d.y);
      label.attr('x', (d: any) => d.x).attr('y', (d: any) => d.y);
      tickCount++;
      if (tickCount > 180) sim.stop();
    });

    gRef.current = { svg, sim, zoom };

    return () => { sim.stop(); };
  }, [graph]);

  if (!graph?.nodes?.length) {
    return <p className="text-gray-500 text-sm text-center py-8">图谱为空，摄入会话数据后自动生成</p>;
  }

  return (
    <div>
      <div ref={containerRef} className="rounded-lg border border-border overflow-hidden" style={{ height: 420 }} />
      <div className="flex items-center gap-2 mt-2">
        <button className="text-[10px] px-2 py-0.5 rounded bg-gray-700 text-gray-400 hover:text-gray-200"
          onClick={() => { const g = gRef.current; if (g) g.svg.transition().duration(300).call(g.zoom.scaleBy, 1.4); }}>＋</button>
        <button className="text-[10px] px-2 py-0.5 rounded bg-gray-700 text-gray-400 hover:text-gray-200"
          onClick={() => { const g = gRef.current; if (g) g.svg.transition().duration(300).call(g.zoom.scaleBy, 0.7); }}>−</button>
        <button className="text-[10px] px-2 py-0.5 rounded bg-gray-700 text-gray-400 hover:text-gray-200"
          onClick={() => { const g = gRef.current; if (g) g.svg.transition().duration(400).call(g.zoom.transform, d3.zoomIdentity); }}>⊡</button>
        <span className="text-[10px] text-gray-600 ml-auto">
          {stats.nodes}/{graph.nodes.length} 节点 · {stats.edges} 边
        </span>
      </div>
      <div className="flex items-center gap-3 mt-2 flex-wrap">
        {Object.entries(NODE_COLORS).map(([type, color]) => (
          <span key={type} className="flex items-center gap-1 text-[10px] text-gray-500">
            <span className="w-2 h-2 rounded-full" style={{ backgroundColor: color }} />{type}
          </span>
        ))}
      </div>
    </div>
  );
}
