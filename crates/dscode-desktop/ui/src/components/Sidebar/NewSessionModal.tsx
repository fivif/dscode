import { useState, useEffect } from 'react';
import { useSessionStore } from '@/stores/sessionStore';

interface Props {
  onClose: () => void;
  onCreated: (sessionId: string) => void;
}

export default function NewSessionModal({ onClose, onCreated }: Props) {
  const { createSession, getLastSession } = useSessionStore();
  const [lastWorkspace, setLastWorkspace] = useState('');
  const [showInput, setShowInput] = useState(false);
  const [customPath, setCustomPath] = useState('');
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');

  useEffect(() => {
    getLastSession().then((s) => {
      if (s?.workspace) setLastWorkspace(s.workspace);
    });
  }, []);

  const handleCreate = async (workspace: string) => {
    setLoading(true);
    setError('');
    try {
      // Naming is the backend's job: an empty title makes it store a
      // workspace-derived placeholder, which stays renameable by the first
      // message (and by the LLM namer that runs after it).
      const session = await createSession('', workspace);
      if (session) {
        onCreated(session.id);
        onClose();
      } else {
        setError('创建会话失败，请重试');
      }
    } catch (e: any) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  const handleBrowse = async () => {
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({ directory: true, title: '选择工作目录' });
      if (selected && typeof selected === 'string') {
        handleCreate(selected);
      }
    } catch {
      setShowInput(true);
    }
  };

  return (
    <div className="fixed inset-0 z-50 bg-black/50 backdrop-blur-sm flex items-center justify-center" onClick={onClose}>
      <div className="menu w-full max-w-sm p-6 shadow-modal" onClick={(e) => e.stopPropagation()}>
        <h3 className="text-[15px] font-semibold text-primary mb-5">新建对话</h3>

        {/* Error message */}
        {error && (
          <div className="mb-3 p-2 bg-danger/10 border border-danger/40 rounded-control text-danger text-[13px]">{error}</div>
        )}

        {/* Inherit last workspace */}
        {lastWorkspace && (
          <button
            className="w-full text-left p-4 rounded-card border border-border hover:bg-hover transition-colors mb-3 disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
            onClick={() => handleCreate(lastWorkspace)}
            disabled={loading}
          >
            <div className="text-[13px] text-primary">沿用上次工作区</div>
            <div className="text-[11px] text-muted mt-1 truncate">{lastWorkspace}</div>
          </button>
        )}

        {/* Browse for folder */}
        <button
          className="w-full text-left p-4 rounded-card border border-border hover:bg-hover transition-colors mb-3 disabled:opacity-40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
          onClick={handleBrowse}
          disabled={loading}
        >
          <div className="text-[13px] text-primary">浏览选择文件夹</div>
          <div className="text-[11px] text-muted mt-1">选择项目根目录作为工作区</div>
        </button>

        {showInput && (
          <div className="mt-3">
            <input
              className="field font-mono"
              placeholder="/path/to/project"
              value={customPath}
              onChange={(e) => setCustomPath(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Enter' && customPath.trim()) handleCreate(customPath.trim()); }}
              autoFocus
            />
          </div>
        )}

        <button className="btn btn-ghost w-full mt-1" onClick={onClose}>
          取消
        </button>
      </div>
    </div>
  );
}
