import { useEffect, useRef, useState } from 'react';
import * as d3 from 'd3';
import type { WikiGraph } from '@/lib/types';

const NODE_COLORS: Record<string, string> = {
  concept: '#a78bfa', fact: '#34d399', pattern: '#fbbf24',
  decision: '#60a5fa', rule: '#f472b6',
};

export default function WikiGraphView({ graph }: { graph: WikiGraph }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const hoverRef = useRef<any>(null);
  const simRef = useRef<d3.Simulation<any, any> | null>(null);
  const [selected, setSelected] = useState<any>(null);

  useEffect(() => {
    if (!graph?.nodes?.length) return;
    const canvas = canvasRef.current;
    if (!canvas) return;
    const container = canvas.parentElement;
    if (!container) return;

    const W = container.clientWidth || 700;
    const H = 420;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = W * dpr;
    canvas.height = H * dpr;
    canvas.style.width = W + 'px';
    canvas.style.height = H + 'px';
    const ctx = canvas.getContext('2d')!;
    ctx.scale(dpr, dpr);

    // Compute degree for sizing
    const deg = new Map<string, number>();
    for (const e of graph.edges || []) {
      deg.set(e.source, (deg.get(e.source) || 0) + 1);
      deg.set(e.target, (deg.get(e.target) || 0) + 1);
    }

    const nodes: any[] = (graph.nodes || []).map((n: any) => ({
      id: n.id || n.title, title: n.title || '?',
      _deg: deg.get(n.id || n.title) || 0,
      _type: n.node_type || 'fact',
      _data: n,
    }));

    const nodeMap = new Map(nodes.map(n => [n.id, n]));
    const edges = (graph.edges || [])
      .filter((e: any) => nodeMap.has(e.source) && nodeMap.has(e.target))
      .map((e: any) => ({ source: e.source, target: e.target }));

    const rScale = (d: number) => Math.max(3, Math.min(20, 4 + d * 1.5));

    // D3 force simulation for physics only — Canvas for rendering
    const sim = d3.forceSimulation(nodes)
      .force('link', d3.forceLink(edges).id((d: any) => d.id).distance(50))
      .force('charge', d3.forceManyBody().strength(-150))
      .force('center', d3.forceCenter(W / 2, H / 2))
      .force('collide', d3.forceCollide((d: any) => rScale(d._deg) + 5))
      .alphaDecay(0.02)
      .alpha(0.4);

    // Throttled render — only draw every 3 ticks
    let tickCount = 0;
    let throttleCount = 0;
    let stopped = false;

    function draw() {
      ctx.clearRect(0, 0, W, H);
      ctx.fillStyle = '#0a0a0f';
      ctx.fillRect(0, 0, W, H);

      // Edges
      ctx.strokeStyle = '#2a2d35';
      ctx.lineWidth = 0.5;
      ctx.beginPath();
      for (const e of edges) {
        const s = nodeMap.get(e.source), t = nodeMap.get(e.target);
        if (!s || s.x == null || t.x == null) continue;
        ctx.moveTo(s.x, s.y);
        ctx.lineTo(t.x, t.y);
      }
      ctx.stroke();

      // Nodes
      for (const n of nodes) {
        if (n.x == null) continue;
        const r = rScale(n._deg);
        const color = NODE_COLORS[n._type] || '#6b7280';
        const isHover = hoverRef.current === n;
        const rr = isHover ? r + 4 : r;

        ctx.beginPath();
        ctx.arc(n.x, n.y, rr, 0, Math.PI * 2);
        ctx.fillStyle = color;
        ctx.fill();
        ctx.strokeStyle = isHover ? '#fff' : '#1f2937';
        ctx.lineWidth = isHover ? 2 : 1;
        ctx.stroke();
      }

      // Labels (only on hover or when stopped with fewer nodes)
      if (stopped || hoverRef.current) {
        for (const n of nodes) {
          if (n.x == null) continue;
          const isHover = hoverRef.current === n;
          if (!isHover && !stopped) continue;
          if (!isHover && nodes.length > 60) continue;
          const r = rScale(n._deg);
          const label = n.title.length > 14 ? n.title.slice(0, 12) + '…' : n.title;
          ctx.font = `${isHover ? 11 : 8}px system-ui, sans-serif`;
          ctx.fillStyle = isHover ? '#e5e7eb' : '#6b7280';
          ctx.textAlign = 'center';
          ctx.fillText(label, n.x, n.y + r + 10);
        }
      }
    }

    sim.on('tick', () => {
      throttleCount++;
      if (throttleCount % 3 !== 0) return; // Draw every 3rd tick
      tickCount = throttleCount;
      draw();
      if (tickCount > 150) { sim.stop(); stopped = true; draw(); }
    });
    sim.on('end', () => { stopped = true; draw(); });

    simRef.current = sim;

    // Mouse interaction on canvas
    canvas.onmousemove = (e) => {
      const r = canvas.getBoundingClientRect();
      const mx = e.clientX - r.left, my = e.clientY - r.top;
      let found: any = null;
      for (const n of nodes) {
        if (n.x == null) continue;
        const rr = rScale(n._deg) + 6;
        if ((n.x - mx) ** 2 + (n.y - my) ** 2 < rr * rr) { found = n; break; }
      }
      hoverRef.current = found;
      canvas.style.cursor = found ? 'pointer' : 'default';
      draw();
    };

    canvas.onclick = (e) => {
      if (!hoverRef.current) return;
      setSelected(hoverRef.current._data);
    };

    // Drag via d3
    const drag = d3.drag<any, any>()
      .subject(() => hoverRef.current || undefined)
      .on('start', (ev, d) => { if (!ev.active) sim.alphaTarget(0.05).restart(); d.fx = d.x; d.fy = d.y; })
      .on('drag', (ev, d) => { d.fx = ev.x; d.fy = ev.y; })
      .on('end', (ev, d) => { if (!ev.active) sim.alphaTarget(0); d.fx = null; d.fy = null; });
    d3.select(canvas).call(drag as any);

    // Zoom with mouse wheel
    let scale = 1, tx = 0, ty = 0;
    canvas.onwheel = (e) => {
      e.preventDefault();
      const r = canvas.getBoundingClientRect();
      const mx = e.clientX - r.left, my = e.clientY - r.top;
      const factor = e.deltaY < 0 ? 1.15 : 0.85;
      scale *= factor;
      scale = Math.max(0.2, Math.min(5, scale));
      tx = mx - (mx - tx) * factor;
      ty = my - (my - ty) * factor;
      ctx.setTransform(dpr * scale, 0, 0, dpr * scale, tx * dpr, ty * dpr);
      draw();
    };

    draw();
    return () => { sim.stop(); };
  }, [graph]);

  if (!graph?.nodes?.length) {
    return <p className="text-gray-500 text-sm text-center py-8">图谱为空，摄入会话数据后自动生成</p>;
  }

  return (
    <div>
      <div className="rounded-lg border border-border overflow-hidden bg-[#0a0a0f]" style={{ height: 420 }}>
        <canvas ref={canvasRef} style={{ width: '100%', height: 420 }} />
      </div>
      <div className="flex items-center gap-2 mt-2">
        <span className="text-[10px] text-gray-600">{graph.nodes.length} 节点 · {graph.edges?.length || 0} 边</span>
        <span className="text-[10px] text-gray-500 ml-auto">滚轮缩放 · 拖拽移动</span>
      </div>
      <div className="flex items-center gap-3 mt-1 flex-wrap">
        {Object.entries(NODE_COLORS).map(([type, color]) => (
          <span key={type} className="flex items-center gap-1 text-[10px] text-gray-500">
            <span className="w-2 h-2 rounded-full" style={{ backgroundColor: color }} />{type}
          </span>
        ))}
      </div>
      {selected && (
        <div className="mt-3 p-3 bg-card border border-border rounded-lg">
          <div className="flex items-center gap-2 mb-1">
            <span className="text-[10px] px-1.5 py-0.5 rounded" style={{ backgroundColor: (NODE_COLORS[selected.node_type] || '#6b7280') + '30', color: NODE_COLORS[selected.node_type] || '#6b7280' }}>
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
