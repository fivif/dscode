import { useState } from 'react';
import type { ThinkingBlock as ThinkingBlockType } from '@/lib/types';

interface Props { blocks: ThinkingBlockType[]; }

export default function ThinkingBlockView({ blocks }: Props) {
  const [expanded, setExpanded] = useState(true); // Default: visible
  if (!blocks?.length) return null;

  const total = blocks.map((b) => b.content).join('\n');

  return (
    <div className="mb-2 rounded-md overflow-hidden border border-border/50">
      <button
        className="w-full flex items-center gap-2 px-3 py-1.5 bg-think text-xs text-gray-400 hover:text-gray-300 transition-colors"
        onClick={() => setExpanded(!expanded)}
      >
        <svg className={`w-3 h-3 transition-transform ${expanded ? 'rotate-90' : ''}`}
          viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <polyline points="9 18 15 12 9 6" />
        </svg>
        <span className="text-gray-500">思考过程</span>
        <span className="text-gray-600">{blocks.length > 1 ? `(${blocks.length})` : ''}</span>
      </button>
      {expanded && (
        <div className="bg-think/70 px-3 py-2">
          <p className="text-xs text-gray-400 whitespace-pre-wrap leading-relaxed italic">{total}</p>
        </div>
      )}
    </div>
  );
}
