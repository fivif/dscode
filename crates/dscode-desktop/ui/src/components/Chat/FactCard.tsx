import { useState, useEffect, useRef } from 'react';
import type { FactRecord } from '@/lib/types';
import { IconChevronRight16 } from '@/components/icons';

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
    <div className="mb-2 panel overflow-hidden">
      <button
        className="w-full flex items-center gap-2 px-3 py-1.5 text-[11px] uppercase tracking-wide text-muted hover:text-secondary transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
        onClick={handleToggle}
      >
        <IconChevronRight16
          size={12}
          className={`transition-transform duration-150 ${expanded ? 'rotate-90' : ''}`}
        />
        <span>{"\u{1F9E0} 记忆"}</span>
        <span className="text-faint">({facts.length})</span>
      </button>
      {expanded && (
        <div className="px-3 pb-2 border-t border-divider">
          <div className="space-y-1.5">
            {facts.map((fact) => (
              <div key={fact.id} className="flex items-center gap-1.5 py-1 text-[13px] font-mono">
                <span className="text-secondary whitespace-nowrap">{fact.subject}</span>
                <span className="text-faint mx-0.5">—</span>
                <span className="text-muted whitespace-nowrap">{fact.predicate}</span>
                <span className="text-faint mx-0.5">—</span>
                <span className="text-primary truncate">{fact.object}</span>
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
