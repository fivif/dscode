import { useState, useEffect, useRef } from 'react';
import type { FactRecord } from '@/lib/types';

interface Props { facts: FactRecord[]; }

export default function FactCard({ facts }: Props) {
  const [expanded, setExpanded] = useState(true);
  const userToggledRef = useRef(false);

  useEffect(() => {
    const t = setTimeout(() => { if (!userToggledRef.current) setExpanded(false); }, 4000);
    return () => clearTimeout(t);
  }, [facts.length]);

  const handleToggle = () => {
    userToggledRef.current = true;
    setExpanded(prev => !prev);
  };

  if (!facts?.length) return null;

  return (
    <div className="mb-2 rounded-md overflow-hidden border border-border/50 bg-card/60">
      <button
        className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-gray-400 hover:text-gray-300 transition-colors"
        onClick={handleToggle}
      >
        <svg className={`w-3 h-3 transition-transform ${expanded ? 'rotate-90' : ''}`}
          viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <polyline points="9 18 15 12 9 6" />
        </svg>
        <span className="text-gray-500">{"\u{1F9E0} 记忆"}</span>
        <span className="text-gray-600">({facts.length})</span>
      </button>
      {expanded && (
        <div className="px-3 pb-2 border-t border-border/30">
          <div className="space-y-1.5">
            {facts.map((fact) => (
              <div key={fact.id} className="flex items-center gap-1.5 py-1 text-[11px] font-mono text-gray-400">
                <span className="text-purple-400/80 whitespace-nowrap">{fact.subject}</span>
                <span className="text-gray-600 mx-0.5">—</span>
                <span className="text-purple-300/60 whitespace-nowrap">{fact.predicate}</span>
                <span className="text-gray-600 mx-0.5">—</span>
                <span className="text-purple-400/80 truncate">{fact.object}</span>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
