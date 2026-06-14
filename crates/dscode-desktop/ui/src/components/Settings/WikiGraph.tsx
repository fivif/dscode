import { useEffect, useRef, useState } from 'react';
import type { WikiGraph } from '@/lib/types';

type GraphNode = { id: string; title: string; content: string; node_type: string; tags: string[]; x: number; y: number; vx: number; vy: number; };
type GraphEdge = { source: string; target: string };

const COLORS: Record<string, { fill: string; text: string }> = {
  concept:  { fill: '#a78bfa', text: '#c4b5fd' },
  fact:     { fill: '#34d399', text: '#6ee7b7' },
  pattern:  { fill: '#fbbf24', text: '#fcd34d' },
  decision: { fill: '#60a5fa', text: '#93c5fd' },
  rule:     { fill: '#f472b6', text: '#f9a8d4' },
};

export default function WikiGraphView({ graph }: { graph: WikiGraph }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);
  const nodesRef = useRef<GraphNode[]>([]);
  const edgesRef = useRef<GraphEdge[]>([]);
  const animRef = useRef(0);
  const hoverRef = useRef<GraphNode | null>(null);
  const dragRef = useRef<{ node: GraphNode; ox: number; oy: number } | null>(null);
  const [selected, setSelected] = useState<GraphNode | null>(null);
  const [size, setSize] = useState({ w: 800, h: 400 });

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const obs = new ResizeObserver(([e]) => {
      setSize({ w: e.contentRect.width, h: Math.max(400, e.contentRect.height || 400) });
    });
    obs.observe(container);
    return () => obs.disconnect();
  }, []);

  useEffect(() => {
    if (!graph?.nodes?.length) return;
    const W = size.w, H = size.h, cx = W / 2, cy = H / 2;

    // Build nodes
    const nodes: GraphNode[] = (graph.nodes || []).map((n: any) => ({
      id: n.id || n.title,
      title: n.title || n.id || '?',
      content: n.content || '',
      node_type: n.node_type || 'fact',
      tags: n.tags || [],
      x: cx + (Math.random() - 0.5) * Math.min(W, H) * 0.5,
      y: cy + (Math.random() - 0.5) * Math.min(W, H) * 0.5,
      vx: 0, vy: 0,
    }));
    const nodeMap = new Map(nodes.map(n => [n.id, n]));
    const edges: GraphEdge[] = (graph.edges || [])
      .filter(e => nodeMap.has(e.source) && nodeMap.has(e.target));
    nodesRef.current = nodes;
    edgesRef.current = edges;

    const canvas = canvasRef.current;
    if (!canvas) return;
    const dpr = window.devicePixelRatio || 1;
    canvas.width = W * dpr;
    canvas.height = H * dpr;
    canvas.style.width = W + 'px';
    canvas.style.height = H + 'px';
    const ctx = canvas.getContext('2d')!;
    ctx.scale(dpr, dpr);

    let frame = 0;
    function physics() {
      for (const n of nodes) {
        let fx = (cx - n.x) * 0.0008;
        let fy = (cy - n.y) * 0.0008;
        for (const m of nodes) {
          if (m === n) continue;
          const dx = n.x - m.x, dy = n.y - m.y;
          const dist = Math.max(1, Math.sqrt(dx * dx + dy * dy));
          const force = 1500 / (dist * dist);
          fx += (dx / dist) * force;
          fy += (dy / dist) * force;
        }
        for (const e of edges) {
          const other = e.source === n.id ? nodeMap.get(e.target) : e.target === n.id ? nodeMap.get(e.source) : null;
          if (!other) continue;
          const dx = other.x - n.x, dy = other.y - n.y;
          const dist = Math.max(1, Math.sqrt(dx * dx + dy * dy));
          const force = (dist - 90) * 0.012;
          fx += (dx / dist) * force;
          fy += (dy / dist) * force;
        }
        n.vx = (n.vx + fx) * 0.55;
        n.vy = (n.vy + fy) * 0.55;
        n.x += n.vx;
        n.y += n.vy;
        n.x = Math.max(30, Math.min(W - 30, n.x));
        n.y = Math.max(30, Math.min(H - 30, n.y));
      }
    }

    function draw() {
      ctx.clearRect(0, 0, W, H);
      // BG
      ctx.fillStyle = '#0a0a0f';
      ctx.fillRect(0, 0, W, H);

      physics();
      // Only run full physics for first 200 frames then slow down
      if (frame > 200 && frame % 3 !== 0) { frame++; return frame_loop(); }
      frame++;

      // Edges
      ctx.strokeStyle = '#1f2937';
      ctx.lineWidth = 0.6;
      for (const e of edges) {
        const s = nodeMap.get(e.source), t = nodeMap.get(e.target);
        if (!s || !t) continue;
        ctx.beginPath(); ctx.moveTo(s.x, s.y); ctx.lineTo(t.x, t.y); ctx.stroke();
      }

      // Nodes
      for (const n of nodes) {
        const c = COLORS[n.node_type] || COLORS.fact;
        const isHover = hoverRef.current === n;
        const isSel = selected?.id === n.id;
        const r = isHover || isSel ? 9 : 6;

        // Glow
        if (isHover || isSel) {
          ctx.beginPath(); ctx.arc(n.x, n.y, r + 5, 0, Math.PI * 2);
          ctx.fillStyle = c.fill + '20'; ctx.fill();
        }
        // Circle
        ctx.beginPath(); ctx.arc(n.x, n.y, r, 0, Math.PI * 2);
        ctx.fillStyle = c.fill; ctx.fill();
        // Label
        if (isHover || isSel || frame > 180) {
          const label = n.title.length > 14 ? n.title.slice(0, 14) + '…' : n.title;
          ctx.font = '11px Inter, system-ui, sans-serif';
          ctx.fillStyle = isHover || isSel ? c.text : '#6b7280';
          ctx.textAlign = 'center';
          ctx.fillText(label, n.x, n.y + r + 12);
        }
      }

      animRef.current = requestAnimationFrame(draw);
    }
    function frame_loop() { animRef.current = requestAnimationFrame(draw); }

    function hitTest(mx: number, my: number) {
      for (const n of nodes) {
        if (Math.hypot(n.x - mx, n.y - my) < 12) return n;
      }
      return null;
    }

    canvas.onmousemove = (e) => {
      const r = canvas.getBoundingClientRect();
      const n = hitTest(e.clientX - r.left, e.clientY - r.top);
      canvas.style.cursor = n ? 'pointer' : 'default';
      hoverRef.current = n;
    };
    canvas.onmousedown = (e) => {
      const r = canvas.getBoundingClientRect();
      const n = hitTest(e.clientX - r.left, e.clientY - r.top);
      if (n) dragRef.current = { node: n, ox: n.x - (e.clientX - r.left), oy: n.y - (e.clientY - r.top) };
    };
    canvas.onclick = (e) => {
      const r = canvas.getBoundingClientRect();
      const n = hitTest(e.clientX - r.left, e.clientY - r.top);
      setSelected(n);
    };
    window.onmousemove = (e) => {
      if (!dragRef.current) return;
      const r = canvas.getBoundingClientRect();
      dragRef.current.node.x = e.clientX - r.left + dragRef.current.ox;
      dragRef.current.node.y = e.clientY - r.top + dragRef.current.oy;
    };
    window.onmouseup = () => { dragRef.current = null; };

    animRef.current = requestAnimationFrame(draw);
    return () => cancelAnimationFrame(animRef.current);
  }, [graph, size, selected]);

  if (!graph?.nodes?.length) return <p className="text-gray-500 text-sm text-center py-8">图谱为空，摄入会话数据后自动生成</p>;

  return (
    <div ref={containerRef} className="w-full" style={{ minHeight: 400 }}>
      <canvas ref={canvasRef} className="w-full rounded-lg border border-border" style={{ minHeight: 400 }} />
      <div className="flex items-center gap-3 mt-2 flex-wrap">
        {Object.entries(COLORS).map(([type, c]) => (
          <span key={type} className="flex items-center gap-1 text-[10px] text-gray-500">
            <span className="w-2 h-2 rounded-full" style={{ backgroundColor: c.fill }} />{type}
          </span>
        ))}
      </div>
      {selected && (
        <div className="mt-3 p-3 bg-card border border-border rounded-lg">
          <div className="flex items-center gap-2 mb-1">
            <span className="text-[10px] px-1.5 py-0.5 rounded" style={{ backgroundColor: (COLORS[selected.node_type] || COLORS.fact).fill + '25', color: (COLORS[selected.node_type] || COLORS.fact).text }}>
              {selected.node_type}
            </span>
            <h3 className="text-sm font-medium text-gray-200">{selected.title}</h3>
          </div>
          <p className="text-xs text-gray-400 leading-relaxed">{selected.content}</p>
        </div>
      )}
    </div>
  );
}
