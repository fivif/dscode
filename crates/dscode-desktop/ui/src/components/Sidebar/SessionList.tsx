import { useMemo, useState } from 'react';
import SessionItem from './SessionItem';
import type { Session } from '@/lib/types';
import { groupSessions, type SessionGroup } from '@/lib/types';

interface Props {
  sessions: Session[];
  activeId: string | null;
  onSelect: (id: string) => void;
  onDelete: (id: string) => void;
}

const LABELS: Record<SessionGroup, string> = {
  Today: '今天',
  Yesterday: '昨天',
  'This Week': '本周',
  'This Month': '本月',
  Older: '更早',
};

export default function SessionList({ sessions, activeId, onSelect, onDelete }: Props) {
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
      <div className="flex-1 flex items-center justify-center">
        <p className="text-gray-500 text-xs">暂无对话</p>
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
              className="group-header w-full flex items-center gap-1.5 cursor-pointer hover:text-gray-300 transition-colors text-left"
              onClick={() => toggle(group)}
            >
              <svg
                width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor"
                strokeWidth="2.5" strokeLinecap="round"
                className={`transition-transform shrink-0 ${isCollapsed ? '' : 'rotate-90'}`}
              >
                <polyline points="9 18 15 12 9 6" />
              </svg>
              <span>{LABELS[group]}</span>
              <span className="text-gray-600 ml-auto text-[10px]">{items.length}</span>
            </button>
            {!isCollapsed && items.map((s) => (
              <SessionItem
                key={s.id}
                session={s}
                isActive={s.id === activeId}
                onSelect={onSelect}
                onDelete={onDelete}
              />
            ))}
          </div>
        );
      })}
    </div>
  );
}
