import { useCallback, useEffect, useRef, useState } from 'react';
import type { Session } from '@/lib/types';
import { useChatStore } from '@/stores/chatStore';
import { IconClose16, IconPencil16 } from '@/components/icons';

interface Props {
  session: Session;
  isActive: boolean;
  onSelect: (id: string) => void;
  onDelete: (id: string) => void;
  onRename: (id: string, title: string) => void;
}

export default function SessionItem({ session, isActive, onSelect, onDelete, onRename }: Props) {
  const isStreaming = useChatStore((s) => s.isSessionStreaming(session.id));
  /** This session has a blocking Confirm-level prompt waiting (auto-denied on timeout). */
  const needsPermission = useChatStore((s) =>
    s.pendingPermissions.some((p) => p.session_id === session.id),
  );
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
      // The X sits right next to the rename pencil in a hover-revealed cluster;
      // deletion is irreversible (messages are dropped from the DB), so confirm
      // like every other destructive action in the app does.
      const name = session.title || '新对话';
      if (!confirm(`确定删除会话「${name}」？该会话的全部消息将被永久删除。`)) return;
      onDelete(session.id);
    },
    [session.id, session.title, onDelete],
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
        className={`row ${isActive ? 'active' : ''}`}
        onClick={(e) => e.stopPropagation()}
      >
        <input
          ref={inputRef}
          className="flex-1 min-w-0 bg-transparent text-[13px] text-primary border-b border-accent/60 py-0.5 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
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
      className={`row justify-between focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50 ${isActive ? 'active' : ''}`}
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
              className="w-1.5 h-1.5 rounded-full bg-success animate-pulse shrink-0"
              title="生成中（可切换到其他会话并行工作）"
            />
          )}
          {needsPermission && (
            <span
              className="w-1.5 h-1.5 rounded-full bg-warning shrink-0"
              title="该会话有一条危险命令等待确认（超时自动拒绝）"
            />
          )}
          <div className="truncate leading-snug">{session.title || '新对话'}</div>
        </div>
        {wsName && !session.title.includes(wsName) && (
          <div className="truncate text-[11px] text-muted leading-tight mt-0.5">{wsName}</div>
        )}
      </div>
      {hover && (
        <div className="flex items-center shrink-0 gap-0.5">
          <button
            className="icon-btn w-5 h-5"
            onClick={(e) => { e.stopPropagation(); setEditing(true); }}
            title="重命名"
          >
            <IconPencil16 size={11} />
          </button>
          <button
            className="icon-btn w-5 h-5 hover:text-danger"
            onClick={handleDelete}
            title="删除会话"
          >
            <IconClose16 size={12} />
          </button>
        </div>
      )}
    </div>
  );
}
