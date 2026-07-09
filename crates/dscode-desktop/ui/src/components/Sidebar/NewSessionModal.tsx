import { useState, useEffect } from 'react';
import { useSessionStore } from '@/stores/sessionStore';

interface Props {
  onClose: () => void;
  onCreated: (sessionId: string) => void;
}

export default function NewSessionModal({ onClose, onCreated }: Props) {
  const { sessions, createSession, getLastSession } = useSessionStore();
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
      // Provisional title from workspace folder; auto-renamed on first message
      const folder = workspace.split(/[/\\]/).filter(Boolean).pop();
      const title = folder ? folder : '新对话';
      const session = await createSession(title, workspace);
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
    <div className="fixed inset-0 z-50 bg-black/50 flex items-center justify-center" onClick={onClose}>
      <div className="bg-card border border-border rounded-xl w-full max-w-sm p-6 shadow-2xl" onClick={(e) => e.stopPropagation()}>
        <h3 className="text-base font-medium text-gray-200 mb-5">新建对话</h3>

        {/* Error message */}
        {error && (
          <div className="mb-3 p-2 bg-red-900/15 border border-red-900/30 rounded text-red-400 text-xs">{error}</div>
        )}

        {/* Inherit last workspace */}
        {lastWorkspace && (
          <button
            className="w-full text-left p-4 rounded-lg border border-border hover:bg-gray-700/50 transition-colors mb-3"
            onClick={() => handleCreate(lastWorkspace)}
            disabled={loading}
          >
            <div className="text-sm text-gray-200">沿用上次工作区</div>
            <div className="text-xs text-gray-500 mt-1 truncate">{lastWorkspace}</div>
          </button>
        )}

        {/* Browse for folder */}
        <button
          className="w-full text-left p-4 rounded-lg border border-border hover:bg-gray-700/50 transition-colors mb-3"
          onClick={handleBrowse}
          disabled={loading}
        >
          <div className="text-sm text-gray-200">浏览选择文件夹</div>
          <div className="text-xs text-gray-500 mt-1">选择项目根目录作为工作区</div>
        </button>

        {showInput && (
          <div className="mt-3">
            <input
              className="w-full bg-input border border-border rounded-lg px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-gray-500"
              placeholder="/path/to/project"
              value={customPath}
              onChange={(e) => setCustomPath(e.target.value)}
              onKeyDown={(e) => { if (e.key === 'Enter' && customPath.trim()) handleCreate(customPath.trim()); }}
              autoFocus
            />
          </div>
        )}

        <button className="mt-1 text-sm text-gray-400 hover:text-gray-200 transition-colors w-full text-center py-2" onClick={onClose}>
          取消
        </button>
      </div>
    </div>
  );
}
