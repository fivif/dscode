import { useEffect, useRef } from 'react';
import * as d3 from 'd3';
import type { WikiGraph } from '@/lib/types';

const NODE_COLORS: Record<string, string> = {
  concept: '#a78bfa', fact: '#34d399', pattern: '#fbbf24',
  decision: '#60a5fa', rule: '#f472b6',
};

const STYLE = {
  bg: '#0a0a0f', edge: '#374151', nodeStroke: '#1f2937',
  label: '#9ca3af', labelHover: '#e5e7eb',
};

export default function WikiGraphView({ graph }: { graph: WikiGraph }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const gRef = useRef<any>(null);

  useEffect(() => {
    if (!graph?.nodes?.length) return;
    const el = containerRef.current;
    if (!el) return;
    const W = el.clientWidth;
    const H = 420;

    // Clear previous
    el.innerHTML = '';

    const svg = d3.select(el).append('svg')
      .attr('width', W).attr('height', H)
      .style('background', STYLE.bg);

    const g = svg.append('g');
    const zoom = d3.zoom<SVGSVGElement, unknown>()
      .scaleExtent([0.08, 6])
      .on('zoom', (ev) => { g.attr('transform', ev.transform.toString()); });
    svg.call(zoom);

    // Prepare data
    const degrees = new Map<string, number>();
    for (const e of graph.edges || []) {
      degrees.set(e.source, (degrees.get(e.source) || 0) + 1);
      degrees.set(e.target, (degrees.get(e.target) || 0) + 1);
    }

    const rScale = (deg: number) => Math.max(4, Math.min(22, 5 + deg * 2));

    const nodes = (graph.nodes || []).map((n: any) => ({
      id: n.id || n.title,
      title: n.title || n.id || '?',
      _deg: degrees.get(n.id || n.title) || 0,
      _type: n.node_type || 'fact',
      _data: n,
    }));

    const nodeMap = new Map(nodes.map(n => [n.id, n]));
    const edges = (graph.edges || [])
      .filter(e => nodeMap.has(e.source) && nodeMap.has(e.target))
      .map(e => ({ source: e.source, target: e.target }));

    // Simulation
    const sim = d3.forceSimulation(nodes as any)
      .force('link', d3.forceLink(edges).id((d: any) => d.id).distance(55))
      .force('charge', d3.forceManyBody().strength(-80))
      .force('x', d3.forceX(W / 2).strength(0.008))
      .force('y', d3.forceY(H / 2).strength(0.008))
      .force('collide', d3.forceCollide((d: any) => rScale(d._deg || 1) + 6))
      .alphaDecay(0.015)
      .alpha(0.3);

    // Edges
    const link = g.append('g').selectAll('line').data(edges).join('line')
      .attr('stroke', STYLE.edge).attr('stroke-width', 0.8).attr('stroke-opacity', 0.4);

    // Nodes
    const node = g.append('g').selectAll('circle').data(nodes).join('circle')
      .attr('r', (d: any) => rScale(d._deg || 1))
      .attr('fill', (d: any) => NODE_COLORS[d._type] || '#888')
      .attr('stroke', STYLE.nodeStroke).attr('stroke-width', 1.5)
      .style('cursor', 'pointer');

    node.append('title').text((d: any) => d.title);

    // Labels
    const label = g.append('g').selectAll('text').data(nodes).join('text')
      .text((d: any) => {
        const t = d.title || '';
        return t.length > 16 ? t.slice(0, 14) + '…' : t;
      })
      .attr('dy', (d: any) => rScale(d._deg || 1) + 13)
      .attr('text-anchor', 'middle')
      .attr('font-size', 9).attr('font-family', 'system-ui, sans-serif')
      .attr('fill', STYLE.label).style('pointer-events', 'none');

    // Hover
    node.on('mouseenter', function (_: any, d: any) {
      d3.select(this).transition().duration(150)
        .attr('r', rScale(d._deg || 1) + 3).attr('stroke', '#fff').attr('stroke-width', 2.5);
    });
    node.on('mouseleave', function (_: any, d: any) {
      d3.select(this).transition().duration(150)
        .attr('r', rScale(d._deg || 1)).attr('stroke', STYLE.nodeStroke).attr('stroke-width', 1.5);
    });

    // Drag
    const drag = d3.drag<any, any>()
      .on('start', (ev, d) => { if (!ev.active) sim.alphaTarget(0.08).restart(); d.fx = d.x; d.fy = d.y; })
      .on('drag', (ev, d) => { d.fx = ev.x; d.fy = ev.y; })
      .on('end', (ev, d) => { if (!ev.active) sim.alphaTarget(0); d.fx = null; d.fy = null; });
    (node as any).call(drag);

    // Tick
    sim.on('tick', () => {
      link.attr('x1', (d: any) => d.source.x).attr('y1', (d: any) => d.source.y)
        .attr('x2', (d: any) => d.target.x).attr('y2', (d: any) => d.target.y);
      node.attr('cx', (d: any) => d.x).attr('cy', (d: any) => d.y);
      label.attr('x', (d: any) => d.x).attr('y', (d: any) => d.y);
    });

    // Zoom on label visibility
    zoom.on('zoom', (ev) => {
      g.attr('transform', ev.transform.toString());
      label.style('display', ev.transform.k > 0.5 ? null : 'none');
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
          onClick={() => {
            const g = gRef.current;
            if (g) g.svg.transition().duration(400).call(g.zoom.transform, d3.zoomIdentity);
          }}>⊡</button>
        <span className="text-[10px] text-gray-600 ml-auto">{graph.nodes.length} 节点 · {graph.edges?.length || 0} 边</span>
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
