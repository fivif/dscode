import { useEffect, useRef } from 'react';
import * as d3 from 'd3';
import type { WikiGraph } from '@/lib/types';

const NODE_COLORS: Record<string, string> = {
  concept: '#a78bfa', fact: '#34d399', pattern: '#fbbf24',
  decision: '#60a5fa', rule: '#f472b6',
};

const BG = '#0a0a0f';
const H = 420;

export default function WikiGraphView({ graph, highlightId }: { graph: WikiGraph; highlightId?: string }) {
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!graph?.nodes?.length) return;
    const el = containerRef.current;
    if (!el) return;
    const W = el.clientWidth || 700;
    el.innerHTML = '';

    // Filter: if highlightId, show only that node + depth-1 neighbors
    let displayNodes: any[];
    let displayEdges: any[];
    if (highlightId) {
      const neighborIds = new Set<string>();
      neighborIds.add(highlightId);
      for (const e of graph.edges || []) {
        if (e.source === highlightId) neighborIds.add(e.target);
        if (e.target === highlightId) neighborIds.add(e.source);
      }
      displayNodes = (graph.nodes || []).filter(n => neighborIds.has(n.id));
      displayEdges = (graph.edges || []).filter(e => neighborIds.has(e.source) && neighborIds.has(e.target));
    } else {
      // Global: top 50 by edge count
      const deg = new Map<string, number>();
      for (const e of graph.edges || []) {
        deg.set(e.source, (deg.get(e.source)||0)+1);
        deg.set(e.target, (deg.get(e.target)||0)+1);
      }
      const sorted = [...(graph.nodes||[])].sort((a,b) => (deg.get(b.id)||0)-(deg.get(a.id)||0));
      const topN = sorted.slice(0, 50);
      const ids = new Set(topN.map(n => n.id));
      displayNodes = topN;
      displayEdges = (graph.edges || []).filter(e => ids.has(e.source) && ids.has(e.target));
    }

    if (!displayNodes.length) return;

    const nodes: any[] = displayNodes.map((n: any) => ({
      id: n.id, title: n.title||n.id, _deg: 1, _type: n.node_type||'fact',
      x: W/2 + (Math.random()-0.5)*W*0.3, y: H/2 + (Math.random()-0.5)*H*0.3,
    }));
    const nodeMap = new Map(nodes.map(n => [n.id, n]));
    const edges = displayEdges.filter((e: any) => nodeMap.has(e.source) && nodeMap.has(e.target));

    const svg = d3.select(el).append('svg').attr('width', W).attr('height', H).style('background', BG);
    const g = svg.append('g');

    // Quartz's exact force parameters
    const sim = d3.forceSimulation(nodes)
      .force('link', d3.forceLink(edges).id((d: any)=>d.id).distance(30))
      .force('charge', d3.forceManyBody().strength(-50*0.5))
      .force('center', d3.forceCenter(W/2, H/2).strength(0.3))
      .force('collide', d3.forceCollide(8))
      .alphaDecay(0.02)
      .alpha(0.5);

    const link = g.selectAll('line').data(edges).join('line')
      .attr('stroke', '#374151').attr('stroke-width', 0.5);

    const node = g.selectAll('circle').data(nodes).join('circle')
      .attr('r', 6)
      .attr('fill', (d: any) => NODE_COLORS[d._type]||'#888')
      .attr('stroke', '#1f2937').attr('stroke-width', 1)
      .style('cursor', 'pointer');
    node.append('title').text((d: any) => d.title);

    const label = g.selectAll('text').data(nodes).join('text')
      .text((d: any) => { const t=d.title||''; return t.length>12?t.slice(0,10)+'…':t; })
      .attr('dy', 14).attr('text-anchor', 'middle')
      .attr('font-size', 8).attr('fill', '#6b7280').style('pointer-events', 'none');

    node.call(d3.drag<any, any>()
      .on('start', function(ev,d:any) { if(!ev.active) sim.alphaTarget(0.3).restart(); d.fx=d.x; d.fy=d.y; })
      .on('drag', function(ev,d:any) { d.fx=ev.x; d.fy=ev.y; })
      .on('end', function(ev,d:any) { if(!ev.active) sim.alphaTarget(0); d.fx=null; d.fy=null; }) as any);

    const zoom = d3.zoom<SVGSVGElement, unknown>()
      .scaleExtent([0.2,5])
      .on('zoom', ev => { g.attr('transform', ev.transform.toString()); });
    svg.call(zoom);

    sim.on('tick', () => {
      link.attr('x1', (d:any)=>d.source.x).attr('y1',(d:any)=>d.source.y)
        .attr('x2',(d:any)=>d.target.x).attr('y2',(d:any)=>d.target.y);
      node.attr('cx',(d:any)=>d.x).attr('cy',(d:any)=>d.y);
      label.attr('x',(d:any)=>d.x).attr('y',(d:any)=>d.y);
    });

    return () => sim.stop();
  }, [graph, highlightId]);

  if (!graph?.nodes?.length) return <p className="text-gray-500 text-sm text-center py-8">图谱为空</p>;

  return (
    <div>
      <div ref={containerRef} className="rounded-lg border border-border overflow-hidden" style={{ height: H, background: BG }} />
      <div className="flex items-center gap-3 mt-2 flex-wrap">
        {Object.entries(NODE_COLORS).map(([t,c]) => (
          <span key={t} className="flex items-center gap-1 text-[10px] text-gray-500"><span className="w-2 h-2 rounded-full" style={{background:c}}/>{t}</span>
        ))}
        <span className="text-[10px] text-gray-600 ml-auto">滚轮缩放 · 拖拽移动</span>
      </div>
    </div>
  );
}
