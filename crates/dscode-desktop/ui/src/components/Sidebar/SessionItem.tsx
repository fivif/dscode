import { useCallback, useState } from 'react';
import type { Session } from '@/lib/types';

interface Props {
  session: Session;
  isActive: boolean;
  onSelect: (id: string) => void;
  onDelete: (id: string) => void;
}

export default function SessionItem({ session, isActive, onSelect, onDelete }: Props) {
  const [hover, setHover] = useState(false);

  const handleDelete = useCallback(
    (e: React.MouseEvent) => {
      e.stopPropagation();
      e.preventDefault();
      onDelete(session.id);
    },
    [session.id, onDelete],
  );

  return (
    <div
      className={`sidebar-item ${isActive ? 'active' : ''} flex items-center justify-between group`}
      onClick={() => onSelect(session.id)}
      onMouseEnter={() => setHover(true)}
      onMouseLeave={() => setHover(false)}
      title={session.title}
    >
      <span className="truncate flex-1">{session.title || 'Untitled'}</span>
      {hover && (
        <button
          className="ml-1 w-5 h-5 flex items-center justify-center rounded text-gray-500 hover:text-red-400 hover:bg-gray-700 shrink-0"
          onClick={handleDelete}
          title="Delete session"
        >
          <svg width="12" height="12" viewBox="0 0 12 12" fill="none">
            <path
              d="M3 3.5L9 9.5M9 3.5L3 9.5"
              stroke="currentColor"
              strokeWidth="1.5"
              strokeLinecap="round"
            />
          </svg>
        </button>
      )}
    </div>
  );
}
