import { useCallback, useEffect, useState } from 'react';
import SessionList from './SessionList';
import NewSessionModal from './NewSessionModal';
import GlobalPromptModal from './GlobalPromptModal';
import { useSessionStore } from '@/stores/sessionStore';
import { useChatStore } from '@/stores/chatStore';

interface Props {
  onOpenSettings: () => void;
  onOpenMcp: () => void;
  onOpenSkills: () => void;
  width: number;
  collapsed: boolean;
  onToggleCollapse: () => void;
}

export default function Sidebar({ onOpenSettings, onOpenMcp, onOpenSkills, width, collapsed, onToggleCollapse }: Props) {
  const { sessions, loading, loadSessions, deleteSession, updateTitle, applyTitleLocal } = useSessionStore();
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const setActiveSession = useChatStore((s) => s.setActiveSession);
  const loadSessionMessages = useChatStore((s) => s.loadSessionMessages);
  const [showModal, setShowModal] = useState(false);
  const [showPromptModal, setShowPromptModal] = useState(false);

  useEffect(() => { loadSessions(); }, [loadSessions]);

  // Live-update titles when backend auto-names from first message
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    (async () => {
      try {
        const { listen } = await import('@tauri-apps/api/event');
        unlisten = await listen<{ session_id: string; title: string }>('session-title-updated', (e) => {
          const { session_id, title } = e.payload || {};
          if (session_id && title) applyTitleLocal(session_id, title);
        });
      } catch { /* web/dev without tauri */ }
    })();
    return () => { unlisten?.(); };
  }, [applyTitleLocal]);

  const handleSelect = useCallback((id: string) => {
    if (id === activeSessionId) return;
    setActiveSession(id);
    loadSessionMessages(id);
  }, [setActiveSession, loadSessionMessages, activeSessionId]);

  const handleDelete = useCallback(async (id: string) => {
    await deleteSession(id);
    if (activeSessionId === id) { setActiveSession(null); }
  }, [deleteSession, activeSessionId, setActiveSession]);

  const handleRename = useCallback((id: string, title: string) => {
    updateTitle(id, title);
  }, [updateTitle]);

  const handleSessionCreated = useCallback((sessionId: string) => {
    setActiveSession(sessionId);
    loadSessionMessages(sessionId);
  }, [setActiveSession, loadSessionMessages]);

  if (collapsed) {
    return (
      <>
        {showModal && <NewSessionModal onClose={() => setShowModal(false)} onCreated={handleSessionCreated} />}
        {showPromptModal && <GlobalPromptModal onClose={() => setShowPromptModal(false)} />}
        <aside className="bg-sidebar flex flex-col h-full border-r border-border shrink-0 items-center py-3 gap-3" style={{ width: 48 }}>
          <button className="p-1.5 rounded-md text-gray-500 hover:text-gray-300 hover:bg-card transition-colors" onClick={onToggleCollapse} title="展开">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><polyline points="9 18 15 12 9 6" /></svg>
          </button>
          <button className="p-1.5 rounded-md text-gray-500 hover:text-gray-300 hover:bg-card transition-colors" onClick={() => setShowModal(true)} title="新对话">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><line x1="12" y1="5" x2="12" y2="19" /><line x1="5" y1="12" x2="19" y2="12" /></svg>
          </button>
          <div className="flex-1" />
          <button className="p-1.5 rounded-md text-gray-500 hover:text-gray-300 hover:bg-card transition-colors" onClick={() => setShowPromptModal(true)} title="全局提示词">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
              <path d="M12 20h9" />
              <path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z" />
            </svg>
          </button>
          <button className="p-1.5 rounded-md text-gray-500 hover:text-gray-300 hover:bg-card transition-colors" onClick={onOpenSettings} title="设置">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8"><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" /></svg>
          </button>
        </aside>
      </>
    );
  }

  return (
    <>
      {showModal && <NewSessionModal onClose={() => setShowModal(false)} onCreated={handleSessionCreated} />}
      {showPromptModal && <GlobalPromptModal onClose={() => setShowPromptModal(false)} />}
      <aside className="bg-sidebar flex flex-col h-full border-r border-border shrink-0" style={{ width }}>
        <div className="p-2.5 flex items-center gap-1.5">
          <button
            className="flex-1 py-2 text-xs text-gray-300 bg-card border border-border rounded-lg hover:bg-gray-700 transition-colors text-center"
            onClick={() => setShowModal(true)}
            disabled={loading}
          >
            新对话
          </button>
          <button className="p-1.5 rounded-md text-gray-500 hover:text-gray-300 hover:bg-card transition-colors" title="搜索">
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><circle cx="11" cy="11" r="8" /><line x1="21" y1="21" x2="16.65" y2="16.65" /></svg>
          </button>
          <button className="p-1.5 rounded-md text-gray-500 hover:text-gray-300 hover:bg-card transition-colors" onClick={onToggleCollapse} title="收起侧栏">
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round"><polyline points="15 18 9 12 15 6" /></svg>
          </button>
        </div>

        {loading && sessions.length === 0 ? (
          <div className="flex-1 flex items-center justify-center"><span className="text-gray-500 text-xs">加载中...</span></div>
        ) : (
          <SessionList
            sessions={sessions}
            activeId={activeSessionId}
            onSelect={handleSelect}
            onDelete={handleDelete}
            onRename={handleRename}
          />
        )}

        <div className="p-2 border-t border-border flex justify-between">
          <div className="flex gap-1">
            <button className="p-1.5 rounded-md text-gray-500 hover:text-gray-300 hover:bg-card transition-colors" onClick={onOpenMcp} title="MCP 工具">
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                <rect x="2" y="2" width="20" height="8" rx="2" /><rect x="2" y="14" width="20" height="8" rx="2" /><circle cx="8" cy="6" r="1" fill="currentColor" /><circle cx="8" cy="18" r="1" fill="currentColor" />
              </svg>
            </button>
            <button className="p-1.5 rounded-md text-gray-500 hover:text-gray-300 hover:bg-card transition-colors" onClick={onOpenSkills} title="Skills">
              {/* Sparkles — Agent Skills / capabilities */}
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                <path d="M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0z" />
                <path d="M20 3v4" />
                <path d="M22 5h-4" />
                <path d="M4 17v2" />
                <path d="M5 18H3" />
              </svg>
            </button>
            <button
              className="p-1.5 rounded-md text-gray-500 hover:text-gray-300 hover:bg-card transition-colors"
              onClick={() => setShowPromptModal(true)}
              title="全局提示词"
            >
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                <path d="M12 20h9" />
                <path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z" />
              </svg>
            </button>
          </div>
          <button className="p-1.5 rounded-md text-gray-500 hover:text-gray-300 hover:bg-card transition-colors" onClick={onOpenSettings} title="设置">
            <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round"><circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" /></svg>
          </button>
        </div>
      </aside>
    </>
  );
}
