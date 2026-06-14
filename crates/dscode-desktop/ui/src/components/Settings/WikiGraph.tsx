import { useEffect, useRef, useState } from 'react';
import type { WikiGraph } from '@/lib/types';

interface Props { graph: WikiGraph; onSelectNode?: (node: any) => void; }

const NODE_COLORS: Record<string, string> = {
  concept: '#a78bfa',
  fact: '#34d399',
  pattern: '#fbbf24',
  decision: '#60a5fa',
  rule: '#f472b6',
};

export default function WikiGraphView({ graph, onSelectNode }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const [selected, setSelected] = useState<any>(null);
  const nodesRef = useRef<any[]>([]);
  const animRef = useRef<number>(0);
  const dragRef = useRef<{ node: any; ox: number; oy: number } | null>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas || !graph?.nodes?.length) return;
    const ctx = canvas.getContext('2d');
    if (!ctx) return;

    const dpr = window.devicePixelRatio || 1;
    const rect = canvas.getBoundingClientRect();
    canvas.width = rect.width * dpr;
    canvas.height = rect.height * dpr;
    ctx.scale(dpr, dpr);

    const W = rect.width;
    const H = rect.height;
    const cx = W / 2;
    const cy = H / 2;

    // Build edge map
    const edgeMap = new Map<string, Set<string>>();
    for (const e of graph.edges || []) {
      if (!edgeMap.has(e.source)) edgeMap.set(e.source, new Set());
      if (!edgeMap.has(e.target)) edgeMap.set(e.target, new Set());
      edgeMap.get(e.source)!.add(e.target);
      edgeMap.get(e.target)!.add(e.source);
    }

    // Build node objects
    const nodes = (graph.nodes || []).map((n: any, i: number) => ({
      ...n,
      x: cx + (Math.random() - 0.5) * W * 0.4,
      y: cy + (Math.random() - 0.5) * H * 0.4,
      vx: 0, vy: 0,
    }));
    nodesRef.current = nodes;

    const nodeMap = new Map<string, any>();
    for (const n of nodes) nodeMap.set(n.id || n.title, n);

    // Force-directed simulation
    function simulate() {
      // Forces
      for (const n of nodes) {
        let fx = 0, fy = 0;
        // Center gravity
        fx += (cx - n.x) * 0.001;
        fy += (cy - n.y) * 0.001;

        // Repulsion between all nodes
        for (const m of nodes) {
          if (m === n) continue;
          const dx = n.x - m.x, dy = n.y - m.y;
          const dist = Math.sqrt(dx * dx + dy * dy) + 1;
          const force = 200 / (dist * dist);
          fx += (dx / dist) * force;
          fy += (dy / dist) * force;
        }

        // Spring attraction along edges
        const neighbors = edgeMap.get(n.id || n.title);
        if (neighbors) {
          for (const neighborId of neighbors) {
            const m = nodeMap.get(neighborId);
            if (!m) continue;
            const dx = m.x - n.x, dy = m.y - n.y;
            const dist = Math.sqrt(dx * dx + dy * dy);
            const force = (dist - 80) * 0.02;
            fx += (dx / (dist + 1)) * force;
            fy += (dy / (dist + 1)) * force;
          }
        }

        // Apply velocity with damping
        n.vx = (n.vx + fx) * 0.5;
        n.vy = (n.vy + fy) * 0.5;
        n.x += n.vx;
        n.y += n.vy;
        // Bounds
        n.x = Math.max(40, Math.min(W - 40, n.x));
        n.y = Math.max(40, Math.min(H - 40, n.y));
      }
    }

    function draw() {
      if (!ctx || !canvas) return;
      const W = canvas.width / dpr;
      const H = canvas.height / dpr;

      simulate();
      ctx.clearRect(0, 0, W, H);

      // Draw edges
      ctx.strokeStyle = '#374151';
      ctx.lineWidth = 0.5;
      for (const e of graph.edges || []) {
        const s = nodeMap.get(e.source);
        const t = nodeMap.get(e.target);
        if (!s || !t) continue;
        ctx.beginPath();
        ctx.moveTo(s.x, s.y);
        ctx.lineTo(t.x, t.y);
        ctx.stroke();
      }

      // Draw nodes
      for (const n of nodes) {
        const color = NODE_COLORS[n.node_type] || '#6b7280';
        const isSelected = selected && (selected.id === n.id || selected.title === n.title);
        const r = isSelected ? 12 : 8;

        // Halo
        if (isSelected) {
          ctx.beginPath();
          ctx.arc(n.x, n.y, r + 4, 0, Math.PI * 2);
          ctx.fillStyle = color + '30';
          ctx.fill();
        }

        // Node circle
        ctx.beginPath();
        ctx.arc(n.x, n.y, r, 0, Math.PI * 2);
        ctx.fillStyle = color;
        ctx.fill();
        ctx.strokeStyle = '#111827';
        ctx.lineWidth = 1.5;
        ctx.stroke();

        // Label
        const label = n.title?.length > 12 ? n.title.slice(0, 12) + '...' : (n.title || '');
        if (label) {
          ctx.font = '10px Inter, system-ui, sans-serif';
          ctx.fillStyle = '#9ca3af';
          ctx.textAlign = 'center';
          ctx.fillText(label, n.x, n.y + r + 12);
        }
      }

      animRef.current = requestAnimationFrame(draw);
    }

    // Handle click
    function onClick(e: MouseEvent) {
      const rect = canvas!.getBoundingClientRect();
      const mx = e.clientX - rect.left;
      const my = e.clientY - rect.top;
      for (const n of nodes) {
        const dx = n.x - mx, dy = n.y - my;
        if (Math.sqrt(dx * dx + dy * dy) < 14) {
          setSelected(n);
          onSelectNode?.(n);
          return;
        }
      }
      setSelected(null);
    }

    // Handle drag
    function onDown(e: MouseEvent) {
      const rect = canvas!.getBoundingClientRect();
      const mx = e.clientX - rect.left;
      const my = e.clientY - rect.top;
      for (const n of nodes) {
        const dx = n.x - mx, dy = n.y - my;
        if (Math.sqrt(dx * dx + dy * dy) < 14) {
          dragRef.current = { node: n, ox: n.x - mx, oy: n.y - my };
          return;
        }
      }
    }
    function onMove(e: MouseEvent) {
      if (!dragRef.current) return;
      const rect = canvas!.getBoundingClientRect();
      dragRef.current.node.x = e.clientX - rect.left + dragRef.current.ox;
      dragRef.current.node.y = e.clientY - rect.top + dragRef.current.oy;
    }
    function onUp() { dragRef.current = null; }

    canvas.addEventListener('click', onClick);
    canvas.addEventListener('mousedown', onDown);
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);

    animRef.current = requestAnimationFrame(draw);

    return () => {
      cancelAnimationFrame(animRef.current);
      canvas.removeEventListener('click', onClick);
      canvas.removeEventListener('mousedown', onDown);
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
  }, [graph, selected, onSelectNode]);

  if (!graph?.nodes?.length) {
    return <p className="text-gray-500 text-sm text-center py-8">图谱为空，摄入会话数据后自动生成</p>;
  }

  return (
    <div className="relative">
      <canvas ref={canvasRef} className="w-full rounded-lg border border-border bg-[#0a0a0f]" style={{ height: 400 }} />
      <div className="flex items-center gap-3 mt-2 flex-wrap">
        {Object.entries(NODE_COLORS).map(([type, color]) => (
          <span key={type} className="flex items-center gap-1 text-[10px] text-gray-500">
            <span className="w-2 h-2 rounded-full" style={{ backgroundColor: color }} />
            {type}
          </span>
        ))}
      </div>
      {selected && (
        <div className="mt-2 p-3 bg-card border border-border rounded-lg">
          <div className="flex items-center gap-2 mb-1">
            <span className="text-[10px] px-1.5 py-0.5 rounded" style={{ backgroundColor: NODE_COLORS[selected.node_type] + '30', color: NODE_COLORS[selected.node_type] }}>
              {selected.node_type || 'fact'}
            </span>
            <h3 className="text-sm font-medium text-gray-200">{selected.title}</h3>
          </div>
          <p className="text-xs text-gray-400 leading-relaxed">{selected.content}</p>
          {selected.tags?.length > 0 && (
            <div className="flex gap-1 mt-2">
              {selected.tags.map((t: string, j: number) => (
                <span key={j} className="text-[10px] px-1.5 py-0.5 rounded bg-gray-800 text-gray-500">{t}</span>
              ))}
            </div>
          )}
        </div>
      )}
    </div>
  );
}
