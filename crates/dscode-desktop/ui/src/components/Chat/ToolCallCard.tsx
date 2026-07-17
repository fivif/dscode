import { useState, useEffect, useRef, memo, useMemo } from 'react';
import type { ToolCallRecord } from '@/lib/types';
import { IconCheck, IconDot, IconX } from '@/components/icons';

interface Props { tool: ToolCallRecord; }

const COLORS: Record<ToolCallRecord['status'], string> = {
  running: 'border-amber-500/60',
  success: 'border-emerald-600/50',
  error: 'border-red-500/50',
};

const ICON_COLORS: Record<ToolCallRecord['status'], string> = {
  running: 'text-amber-400',
  success: 'text-emerald-400',
  error: 'text-red-400',
};

function StatusIcon({ status }: { status: ToolCallRecord['status'] }) {
  const cls = `${ICON_COLORS[status]} ${status === 'running' ? 'animate-pulse' : ''} shrink-0`;
  if (status === 'running') return <IconDot className={cls} size={10} />;
  if (status === 'success') return <IconCheck className={cls} size={12} />;
  return <IconX className={cls} size={12} />;
}

function lastProgressLine(text: string): string {
  const lines = text
    .split('\n')
    .map((l) => l.trim())
    .filter(Boolean);
  if (lines.length === 0) return '';
  // Prefer last checkmark / progress line for multi-lane search UX
  for (let i = lines.length - 1; i >= 0; i--) {
    const l = lines[i];
    if (
      l.startsWith('✓') ||
      l.startsWith('✗') ||
      l.startsWith('●') ||
      l.startsWith('⟳') ||
      l.startsWith('▸') ||
      l.includes('…')
    ) {
      return l.length > 72 ? l.slice(0, 72) + '…' : l;
    }
  }
  const last = lines[lines.length - 1];
  return last.length > 72 ? last.slice(0, 72) + '…' : last;
}

function ToolCallCardInner({ tool }: Props) {
  // Default collapsed while running — expanding huge streaming logs freezes the UI.
  const isSearch = tool.name === 'do_web_search';
  const [expanded, setExpanded] = useState(false);
  const userToggledRef = useRef(false);

  useEffect(() => {
    if (userToggledRef.current) return;
    if (isSearch && tool.status === 'running') {
      setExpanded(true);
    }
  }, [isSearch, tool.status, tool.result]);

  const handleToggle = () => {
    userToggledRef.current = true;
    setExpanded(!expanded);
  };

  // Cap DOM text while streaming (still keep full string in store for tool_end).
  const displayResult = useMemo(() => {
    if (!tool.result) return '';
    if (tool.status === 'running' && tool.result.length > 4000) {
      return '…\n' + tool.result.slice(-4000);
    }
    if (tool.result.length > 30_000) {
      return tool.result.slice(0, 12_000) + '\n…[truncated for display]…\n' + tool.result.slice(-8_000);
    }
    return tool.result;
  }, [tool.result, tool.status]);

  const liveHint = useMemo(() => {
    if (tool.status !== 'running' || !tool.result) return '';
    return lastProgressLine(tool.result);
  }, [tool.status, tool.result]);

  return (
    <div className={`mb-1.5 ml-1 border ${COLORS[tool.status]} bg-card/60 rounded-md overflow-hidden transition-colors`}>
      <button
        className="w-full flex items-center gap-2 px-2.5 py-1.5 text-left hover:bg-white/[0.03] transition-colors"
        onClick={handleToggle}
      >
        <StatusIcon status={tool.status} />
        <span className="text-[11px] text-gray-300 font-mono truncate shrink-0 max-w-[9rem]">{tool.name}</span>
        {tool.status === 'running' && liveHint ? (
          <span className="text-[10px] text-amber-400/80 truncate flex-1 font-mono animate-pulse">
            {liveHint}
          </span>
        ) : tool.status === 'running' ? (
          <span className="text-[10px] text-amber-400/70 animate-pulse flex-1">running</span>
        ) : (
          <span className="flex-1" />
        )}
        <svg className={`w-3 h-3 text-gray-600 transition-transform shrink-0 ${expanded ? 'rotate-90' : ''}`}
          viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
          <polyline points="9 18 15 12 9 6" />
        </svg>
      </button>
      {expanded && (
        <div className="px-2.5 pb-2 pt-0.5 border-t border-border/30">
          {displayResult ? (
            <pre className="text-[11px] text-gray-400 bg-black/20 rounded p-2 max-h-44 overflow-y-auto whitespace-pre-wrap font-mono leading-relaxed">
              {displayResult}
            </pre>
          ) : tool.status === 'running' ? (
            <div className="text-[10px] text-gray-600 px-1 py-1.5 animate-pulse">
              准备并发检索…
            </div>
          ) : null}
        </div>
      )}
    </div>
  );
}

export default memo(ToolCallCardInner);
