import { useState, useEffect, useRef } from 'react';
import type { ThinkingBlock as ThinkingBlockType } from '@/lib/types';

interface Props {
  blocks: ThinkingBlockType[];
  /** When true, thinking is still streaming — stay expanded. */
  streaming?: boolean;
}

export default function ThinkingBlockView({ blocks, streaming }: Props) {
  const [expanded, setExpanded] = useState(true);
  const userToggledRef = useRef(false);

  // Auto-collapse after thinking finishes (unless user toggled)
  useEffect(() => {
    if (streaming) {
      setExpanded(true);
      userToggledRef.current = false;
      return;
    }
    if (!userToggledRef.current && blocks?.length) {
      const t = setTimeout(() => setExpanded(false), 2500);
      return () => clearTimeout(t);
    }
  }, [streaming, blocks?.length]);

  if (!blocks?.length) return null;

  const total = blocks.map((b) => b.content).join('\n');
  const preview = total.length > 80 ? total.slice(0, 80) + '…' : total;

  const handleToggle = () => {
    userToggledRef.current = true;
    setExpanded((v) => !v);
  };

  return (
    <div className="mb-2 rounded-md overflow-hidden border border-border/50">
      <button
        className="w-full flex items-center gap-2 px-3 py-1.5 bg-think text-xs text-gray-400 hover:text-gray-300 transition-colors"
        onClick={handleToggle}
      >
        <svg className={`w-3 h-3 transition-transform ${expanded ? 'rotate-90' : ''}`}
          viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <polyline points="9 18 15 12 9 6" />
        </svg>
        <span className="text-gray-500">思考过程</span>
        <span className="text-gray-600">{blocks.length > 1 ? `(${blocks.length})` : ''}</span>
        {!expanded && (
          <span className="text-gray-600 truncate flex-1 text-left ml-1 italic">{preview}</span>
        )}
        {streaming && <span className="text-amber-400/70 animate-pulse ml-auto">…</span>}
      </button>
      {expanded && (
        <div className="bg-think/70 px-3 py-2 max-h-48 overflow-y-auto">
          <p className="text-xs text-gray-400 whitespace-pre-wrap leading-relaxed italic">{total}</p>
        </div>
      )}
    </div>
  );
}
