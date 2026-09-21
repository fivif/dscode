import { useState, useEffect } from 'react';
import { invoke } from '@/lib/tauri';
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

  return (
    <div
      className={
        compact
          ? 'mt-2 mb-1 panel overflow-hidden'
          : 'border-t border-border bg-main/80 px-4 py-2'
      }
    >
      <div className="flex items-center gap-2 px-3 py-1.5 border-b border-divider text-[11px]">
        <span className="text-primary font-medium tracking-wide">{title}</span>
        <span className="text-muted">
          {agents.length} {unit}
          {agents.length === 1 ? '' : 's'}
        </span>
        <span className="text-faint">·</span>
        <span className="text-muted normal-case">
          {running > 0 && <span className="text-secondary">{running} running</span>}
          {running > 0 && (done > 0 || failed > 0) && ' · '}
          {done > 0 && <span className="text-success">{done} done</span>}
          {failed > 0 && (
            <span className="text-danger">
              {done > 0 || running > 0 ? ' · ' : ''}
              {failed} failed
            </span>
          )}
        </span>
      </div>

      <div className="max-h-64 overflow-y-auto divide-y divide-divider">
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
      ? 'bg-muted animate-pulse'
      : agent.status === 'done'
        ? 'bg-success'
        : 'bg-danger';

  const statusChip =
    agent.status === 'running'
      ? 'text-muted border-border'
      : agent.status === 'done'
        ? 'text-success border-success/40 bg-success/10'
        : 'text-danger border-danger/40 bg-danger/10';

  const statusLabel = agent.status === 'running' ? 'running' : agent.status === 'done' ? 'done' : 'err';

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
        className="w-full flex items-start gap-2 px-3 py-1.5 text-left hover:bg-hover transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
        onClick={() => setExpanded((v) => !v)}
      >
        <span aria-hidden="true" className={`mt-1.5 w-1.5 h-1.5 rounded-full shrink-0 ${statusDot}`} />
        <div className="min-w-0 flex-1">
          <div className="flex items-center gap-2">
            <span className="text-[13px] font-mono text-primary shrink-0">{displayId}</span>
            <span className={`text-[11px] px-1.5 py-0.5 rounded-full border shrink-0 ${statusChip}`}>
              {statusLabel}
            </span>
            {agent.task && !expanded && (
              <span className="text-[13px] text-secondary truncate" title={agent.task}>
                {agent.task}
              </span>
            )}
          </div>
          {expanded && agent.task && (
            <div className="text-[13px] text-secondary mt-0.5 leading-snug">{agent.task}</div>
          )}
        </div>
        {agent.status === 'running' && (
          <span className="flex gap-1 shrink-0 mt-0.5">
            <button
              type="button"
              className="text-[11px] text-accent hover:bg-accent/10 px-1.5 py-0.5 rounded-control border border-accent/40 transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
              onClick={onNudge}
              title="Send mid-run instruction"
            >
              nudge
            </button>
            <button
              type="button"
              className="text-[11px] text-danger hover:bg-danger/10 px-1.5 py-0.5 rounded-control border border-danger/40 transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
              onClick={onStop}
              title="Stop this sub-agent"
            >
              stop
            </button>
          </span>
        )}
        <span className="text-[11px] text-muted shrink-0 mt-0.5 opacity-0 group-hover:opacity-100 transition-opacity duration-150">
          {expanded ? '收起' : '展开'}
        </span>
      </button>
      {expanded && (
        <div className="px-3 pb-2 pl-6">
          <div className="text-[11px] text-secondary whitespace-pre-wrap font-mono leading-relaxed max-h-32 overflow-y-auto rounded-control bg-input px-2 py-1.5 border border-border">
            {agent.output ||
              (agent.status === 'running' ? '执行中…' : '(无输出)')}
          </div>
        </div>
      )}
    </div>
  );
}
