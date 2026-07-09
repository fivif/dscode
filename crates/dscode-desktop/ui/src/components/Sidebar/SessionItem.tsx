import { useCallback, useEffect, useRef, useState } from 'react';
import type { Session } from '@/lib/types';
import { useChatStore } from '@/stores/chatStore';

interface Props {
  session: Session;
  isActive: boolean;
  onSelect: (id: string) => void;
  onDelete: (id: string) => void;
  onRename: (id: string, title: string) => void;
}

export default function SessionItem({ session, isActive, onSelect, onDelete, onRename }: Props) {
  const isStreaming = useChatStore((s) => s.isSessionStreaming(session.id));
  const [hover, setHover] = useState(false);
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(session.title);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!editing) setDraft(session.title);
  }, [session.title, editing]);

  useEffect(() => {
    if (editing) {
      inputRef.current?.focus();
      inputRef.current?.select();
    }
  }, [editing]);

  const handleDelete = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      e.preventDefault();
      onDelete(session.id);
    },
    [session.id, onDelete],
  );

  const commit = useCallback(() => {
    const t = draft.trim();
    setEditing(false);
    if (t && t !== session.title) {
      onRename(session.id, t);
    } else {
      setDraft(session.title);
    }
  }, [draft, session.id, session.title, onRename]);

  const cancel = useCallback(() => {
    setDraft(session.title);
    setEditing(false);
  }, [session.title]);

  if (editing) {
    return (
      <div
        className={`sidebar-item ${isActive ? 'active' : ''} flex items-center gap-1`}
        onClick={(e) => e.stopPropagation()}
      >
        <input
          ref={inputRef}
          className="flex-1 min-w-0 bg-transparent text-sm text-gray-100 outline-none border-b border-blue-500/60 py-0.5"
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onBlur={commit}
          onKeyDown={(e) => {
            if (e.key === 'Enter') { e.preventDefault(); commit(); }
            if (e.key === 'Escape') { e.preventDefault(); cancel(); }
          }}
          maxLength={80}
        />
      </div>
    );
  }

  // Secondary hint: workspace folder
  const wsName = session.workspace
    ? session.workspace.split(/[/\\]/).filter(Boolean).pop()
    : '';

  return (
    <div
      className={`sidebar-item ${isActive ? 'active' : ''} flex items-center justify-between group`}
      onClick={() => onSelect(session.id)}
      onDoubleClick={(e) => {
        e.stopPropagation();
        setEditing(true);
      }}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      title={session.workspace ? `${session.title}\n${session.workspace}\n双击重命名` : `${session.title}\n双击重命名`}
    >
      <div className="flex-1 min-w-0 pr-1">
        <div className="flex items-center gap-1.5 min-w-0">
          {isStreaming && (
            <span
              className="w-1.5 h-1.5 rounded-full bg-emerald-400 animate-pulse shrink-0"
              title="生成中（可切换到其他会话并行工作）"
            />
          )}
          <div className="truncate text-sm leading-snug">{session.title || '新对话'}</div>
        </div>
        {wsName && !session.title.includes(wsName) && (
          <div className="truncate text-[10px] text-gray-600 leading-tight mt-0.5">{wsName}</div>
        )}
      </div>
      {hover && (
        <div className="flex items-center shrink-0 gap-0.5">
          <button
            className="w-5 h-5 flex items-center justify-center rounded text-gray-500 hover:text-gray-200 hover:bg-gray-700"
            onClick={(e) => { e.stopPropagation(); setEditing(true); }}
            title="重命名"
          >
            <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M12 20h9" /><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z" />
            </svg>
          </button>
          <button
            className="w-5 h-5 flex items-center justify-center rounded text-gray-500 hover:text-red-400 hover:bg-gray-700"
            onClick={handleDelete}
            title="删除会话"
          >
            <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
              <path d="M3 3.5L9 9.5M9 3.5L3 9.5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
            </svg>
          </button>
        </div>
      )}
    </div>
  );
}
