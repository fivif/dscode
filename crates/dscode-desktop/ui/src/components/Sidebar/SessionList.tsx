import { useMemo, useState } from 'react';
import SessionItem from './SessionItem';
import type { Session } from '@/lib/types';
import { groupSessions, type SessionGroup } from '@/lib/types';
import { IconChevronRight16, IconMessage16 } from '@/components/icons';

interface Props {
  sessions: Session[];
  activeId: string | null;
  onSelect: (id: string) => void;
  onDelete: (id: string) => void;
  onRename: (id: string, title: string) => void;
}

const LABELS: Record<SessionGroup, string> = {
  Today: '今天',
  Yesterday: '昨天',
  'This Week': '本周',
  'This Month': '本月',
  Older: '更早',
};

export default function SessionList({ sessions, activeId, onSelect, onDelete, onRename }: Props) {
  const groups = useMemo(() => groupSessions(sessions), [sessions]);
  const order: SessionGroup[] = ['Today', 'Yesterday', 'This Week', 'This Month', 'Older'];

  // Only 'Older' starts collapsed
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set(['Older']));

  const toggle = (g: string) => {
    setCollapsed((prev) => {
      const next = new Set(prev);
      if (next.has(g)) next.delete(g); else next.add(g);
      return next;
    });
  };

  if (sessions.length === 0) {
    return (
      <div className="flex-1 flex flex-col items-center justify-center gap-2 px-4 text-center">
        <IconMessage16 size={28} className="text-faint shrink-0" />
        <p className="text-[13px] text-muted">暂无对话</p>
        <p className="text-[11px] text-faint">点击上方「新对话」开始</p>
      </div>
    );
  }

  return (
    <div className="flex-1 overflow-y-auto py-1">
      {order.map((group) => {
        const items = groups[group];
        if (items.length === 0) return null;
        const isCollapsed = collapsed.has(group);
        return (
          <div key={group}>
            <button
              className="w-full flex items-center gap-1.5 px-2.5 pt-2 pb-1 cursor-pointer text-left text-[11px] font-medium text-muted hover:text-secondary transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
              onClick={() => toggle(group)}
            >
              <IconChevronRight16
                size={10}
                className={`transition-transform shrink-0 ${isCollapsed ? '' : 'rotate-90'}`}
              />
              <span>{LABELS[group]}</span>
              <span className="text-faint ml-auto text-[11px] tabular-nums">{items.length}</span>
            </button>
            {!isCollapsed && items.map((s) => (
              <SessionItem
                key={s.id}
                session={s}
                isActive={s.id === activeId}
                onSelect={onSelect}
                onDelete={onDelete}
                onRename={onRename}
              />
            ))}
          </div>
        );
      })}
    </div>
  );
}
