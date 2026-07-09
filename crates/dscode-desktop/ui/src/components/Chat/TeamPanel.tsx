import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { TeamAgent } from '@/lib/types';
import { useChatStore } from '@/stores/chatStore';

export type AgentPanelKind = 'teams' | 'auto' | 'auto_teams';

interface Props {
  agents: TeamAgent[];
  /** teams | auto MAGI | auto+teams parallel MAGI */
  kind?: AgentPanelKind;
  compact?: boolean;
}

export default function TeamPanel({ agents, kind = 'teams', compact = true }: Props) {
  if (!agents.length) return null;

  const running = agents.filter((a) => a.status === 'running').length;
  const done = agents.filter((a) => a.status === 'done').length;
  const failed = agents.filter((a) => a.status === 'error').length;

  const isAuto = kind === 'auto';
  const isHybrid = kind === 'auto_teams';
  const title = isHybrid ? 'Auto · Teams' : isAuto ? 'Auto' : 'Teams';
  const unit = isAuto || isHybrid ? 'subtask' : 'agent';

  // Hybrid: purple-tinted (teams) + auto; pure auto: neutral; pure teams: purple
  const shell = isHybrid
    ? 'border-purple-500/25 bg-purple-500/[0.05]'
    : isAuto
      ? 'border-white/[0.1] bg-white/[0.025]'
      : 'border-purple-500/20 bg-purple-500/[0.04]';
  const headBorder = isAuto && !isHybrid ? 'border-white/[0.06]' : 'border-purple-500/15';
  const titleCls = isHybrid
    ? 'text-purple-200/90'
    : isAuto
      ? 'text-gray-300'
      : 'text-purple-300/90';

  return (
    <div
      className={
        compact
          ? `mt-2 mb-1 rounded-lg border ${shell} overflow-hidden`
          : 'border-t border-border bg-main/80 px-4 py-2'
      }
    >
      <div className={`flex items-center gap-2 px-3 py-1.5 border-b ${headBorder} text-[11px]`}>
        <span className={`${titleCls} font-medium tracking-wide`}>{title}</span>
        <span className="text-gray-500">
          {agents.length} {unit}
          {agents.length === 1 ? '' : 's'}
        </span>
        <span className="text-gray-600">·</span>
        <span className="text-gray-500 normal-case">
          {running > 0 && <span className="text-gray-300">{running} running</span>}
          {running > 0 && (done > 0 || failed > 0) && ' · '}
          {done > 0 && <span className="text-emerald-400/80">{done} done</span>}
          {failed > 0 && (
            <span className="text-red-400/80">
              {done > 0 || running > 0 ? ' · ' : ''}
              {failed} failed
            </span>
          )}
        </span>
      </div>

      <div className="max-h-64 overflow-y-auto divide-y divide-white/[0.04]">
        {agents.map((a) => (
          <AgentRow key={a.id} agent={a} />
        ))}
      </div>
    </div>
  );
}

function AgentRow({ agent }: { agent: TeamAgent }) {
  const [expanded, setExpanded] = useState(false);
  const sessionId = useChatStore((s) => s.activeSessionId);

  useEffect(() => {
    if (agent.status === 'running') setExpanded(true);
  }, [agent.status]);

  const statusDot =
    agent.status === 'running'
      ? 'bg-gray-300 animate-pulse'
      : agent.status === 'done'
        ? 'bg-emerald-400'
        : 'bg-red-400';

  const displayId = agent.id.includes('#') ? agent.id.replace('#', ' ·') : agent.id;

  const onStop = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (!sessionId) return;
    try {
      await invoke('stop_team_agent', { sessionId, agentId: agent.id });
    } catch {
      /* ignore */
    }
  };

  const onNudge = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (!sessionId) return;
    const text =
      typeof window !== 'undefined'
        ? window.prompt('Nudge this sub-agent (extra instruction):', '')
        : null;
    if (!text?.trim()) return;
    try {
      await invoke('nudge_team_agent', {
        sessionId,
        agentId: agent.id,
        message: text.trim(),
      });
    } catch {
      /* ignore */
    }
  };

  return (
    <div className="group">
      <button
        type="button"
        className="w-full flex items-start gap-2 px-3 py-1.5 text-left hover:bg-white/[0.03] transition-colors"
        onClick={() => setExpanded((v) => !v)}
      >
        <span className={`mt-1.5 w-1.5 h-1.5 rounded-full shrink-0 ${statusDot}`} />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="text-[11px] font-mono text-gray-300 shrink-0">{displayId}</span>
            <span className="text-[10px] text-gray-600 shrink-0">
              {agent.status === 'running' ? '…' : agent.status === 'done' ? 'done' : 'err'}
            </span>
            {agent.task && !expanded && (
              <span className="text-[11px] text-gray-500 truncate" title={agent.task}>
                {agent.task}
              </span>
            )}
          </div>
          {expanded && agent.task && (
            <div className="text-[11px] text-gray-400 mt-0.5 leading-snug">{agent.task}</div>
          )}
        </div>
        {agent.status === 'running' && (
          <span className="flex gap-1 shrink-0 mt-0.5">
            <button
              type="button"
              className="text-[10px] text-sky-400/90 hover:text-sky-300 px-1.5 py-0.5 rounded border border-sky-500/30"
              onClick={onNudge}
              title="Send mid-run instruction"
            >
              nudge
            </button>
            <button
              type="button"
              className="text-[10px] text-red-400/90 hover:text-red-300 px-1.5 py-0.5 rounded border border-red-500/30"
              onClick={onStop}
              title="Stop this sub-agent"
            >
              stop
            </button>
          </span>
        )}
        <span className="text-[10px] text-gray-600 shrink-0 mt-0.5 opacity-0 group-hover:opacity-100">
          {expanded ? '收起' : '展开'}
        </span>
      </button>
      {expanded && (
        <div className="px-3 pb-2 pl-6">
          <div className="text-[10px] text-gray-500 whitespace-pre-wrap font-mono leading-relaxed max-h-32 overflow-y-auto rounded bg-black/25 px-2 py-1.5 border border-white/[0.05]">
            {agent.output ||
              (agent.status === 'running' ? '执行中…' : '(无输出)')}
          </div>
        </div>
      )}
    </div>
  );
}
