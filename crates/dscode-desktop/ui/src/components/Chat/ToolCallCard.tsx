import { useState, useEffect } from 'react';
import type { ToolCallRecord } from '@/lib/types';

interface Props { tool: ToolCallRecord; }

const COLORS: Record<ToolCallRecord['status'], string> = {
  running: 'border-amber-500/60',
  success: 'border-emerald-600/50',
  error: 'border-red-500/50',
};

const ICONS: Record<ToolCallRecord['status'], string> = {
  running: '●',
  success: '✓',
  error: '✗',
};

const ICON_COLORS: Record<ToolCallRecord['status'], string> = {
  running: 'text-amber-400',
  success: 'text-emerald-400',
  error: 'text-red-400',
};

export default function ToolCallCard({ tool }: Props) {
  const [expanded, setExpanded] = useState(false);

  useEffect(() => {
    if (tool.status === 'running') setExpanded(true);
    else {
      const t = setTimeout(() => setExpanded(false), 4000);
      return () => clearTimeout(t);
    }
  }, [tool.status]);

  return (
    <div className={`mb-1.5 ml-1 border ${COLORS[tool.status]} bg-card/60 rounded-md overflow-hidden transition-colors`}>
      <button
        className="w-full flex items-center gap-2 px-2.5 py-1.5 text-left hover:bg-white/[0.03] transition-colors"
        onClick={() => setExpanded(!expanded)}
      >
        <span className={`text-[10px] font-mono ${ICON_COLORS[tool.status]} ${tool.status === 'running' ? 'animate-pulse' : ''}`}>
          {ICONS[tool.status]}
        </span>
        <span className="text-[11px] text-gray-300 font-mono truncate flex-1">{tool.name}</span>
        {tool.status === 'running' && (
          <span className="text-[10px] text-amber-400/70 animate-pulse">running</span>
        )}
        <svg className={`w-3 h-3 text-gray-600 transition-transform shrink-0 ${expanded ? 'rotate-90' : ''}`}
          viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
          <polyline points="9 18 15 12 9 6" />
        </svg>
      </button>
      {expanded && (
        <div className="px-2.5 pb-2 pt-0.5 border-t border-border/30">
          {tool.result && (
            <pre className="text-[11px] text-gray-400 bg-black/20 rounded p-2 max-h-44 overflow-y-auto whitespace-pre-wrap font-mono leading-relaxed">
              {tool.result}
            </pre>
          )}
        </div>
      )}
    </div>
  );
}
