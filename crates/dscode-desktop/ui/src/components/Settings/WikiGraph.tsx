import { useRef, useState, useCallback } from 'react';
import ForceGraph2D from 'react-force-graph-2d';
import type { WikiGraph } from '@/lib/types';

const NODE_COLORS: Record<string, string> = {
  concept: '#a78bfa',
  fact: '#34d399',
  pattern: '#fbbf24',
  decision: '#60a5fa',
  rule: '#f472b6',
};

export default function WikiGraphView({ graph }: { graph: WikiGraph }) {
  const fgRef = useRef<any>(null);
  const [selected, setSelected] = useState<any>(null);

  const data = {
    nodes: (graph.nodes || []).map((n: any) => ({
      id: n.id || n.title,
      name: n.title || n.id || '?',
      val: 2,
      color: NODE_COLORS[n.node_type] || '#6b7280',
      _data: n,
    })),
    links: (graph.edges || [])
      .filter((e: any) => (graph.nodes || []).some((n: any) => (n.id || n.title) === e.source) && (graph.nodes || []).some((n: any) => (n.id || n.title) === e.target))
      .map((e: any) => ({ source: e.source, target: e.target })),
  };

  const handleClick = useCallback((node: any) => {
    setSelected(node?._data || null);
  }, []);

  if (!data.nodes.length) {
    return <p className="text-gray-500 text-sm text-center py-8">图谱为空，摄入会话数据后自动生成</p>;
  }

  return (
    <div>
      <div className="rounded-lg border border-border overflow-hidden bg-[#0a0a0f]" style={{ height: 420 }}>
        <ForceGraph2D
          ref={fgRef}
          graphData={data}
          width={800}
          height={420}
          nodeLabel={(n: any) => n.name}
          nodeColor={(n: any) => n.color}
          nodeRelSize={5}
          linkColor={() => '#374151'}
          linkWidth={0.5}
          linkDirectionalArrowLength={0}
          backgroundColor="#0a0a0f"
          onNodeClick={handleClick}
          cooldownTicks={100}
          d3AlphaDecay={0.02}
          d3VelocityDecay={0.3}
          enableNodeDrag={true}
          enableZoomInteraction={true}
          minZoom={0.3}
          maxZoom={3}
        />
      </div>

      <div className="flex items-center gap-3 mt-2 flex-wrap">
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
