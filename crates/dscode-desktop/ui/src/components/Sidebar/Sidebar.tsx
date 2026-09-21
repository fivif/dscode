import { useCallback, useEffect, useState } from 'react';
import SessionList from './SessionList';
import NewSessionModal from './NewSessionModal';
import GlobalPromptModal from './GlobalPromptModal';
import { useSessionStore } from '@/stores/sessionStore';
import { useChatStore } from '@/stores/chatStore';
import {
  IconChevronLeft16,
  IconChevronRight16,
  IconPencil16,
  IconPlus16,
  IconSearch16,
  IconServer16,
  IconSettings16,
  IconSparkles16,
} from '@/components/icons';

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
    let disposed = false;
    let unlisten: (() => void) | undefined;
    (async () => {
      try {
        const { listen } = await import('@/lib/tauri');
        const stop = await listen<{ session_id: string; title: string }>('session-title-updated', (e) => {
          if (disposed) return;
          const { session_id, title } = e.payload || {};
          if (session_id && title) applyTitleLocal(session_id, title);
        });
        // Unmounting before `listen` resolves used to leave the listener live —
        // and every remount (collapse/expand) stacked another one.
        if (disposed) stop();
        else unlisten = stop;
      } catch { /* web/dev without tauri */ }
    })();
    return () => {
      disposed = true;
      unlisten?.();
      unlisten = undefined;
    };
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
          <button className="icon-btn" onClick={onToggleCollapse} title="展开">
            <IconChevronRight16 />
          </button>
          <button className="icon-btn" onClick={() => setShowModal(true)} title="新对话">
            <IconPlus16 />
          </button>
          <div className="flex-1" />
          <button className="icon-btn" onClick={() => setShowPromptModal(true)} title="全局提示词">
            <IconPencil16 />
          </button>
          <button className="icon-btn" onClick={onOpenSettings} title="设置">
            <IconSettings16 />
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
            className="btn btn-secondary flex-1"
            onClick={() => setShowModal(true)}
            disabled={loading}
          >
            新对话
          </button>
          <button className="icon-btn" title="搜索">
            <IconSearch16 size={15} />
          </button>
          <button className="icon-btn" onClick={onToggleCollapse} title="收起侧栏">
            <IconChevronLeft16 size={15} />
          </button>
        </div>

        {loading && sessions.length === 0 ? (
          <div className="flex-1 flex items-center justify-center"><span className="text-muted text-[13px]">加载中...</span></div>
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
            <button className="icon-btn" onClick={onOpenMcp} title="MCP 工具">
              <IconServer16 />
            </button>
            <button className="icon-btn" onClick={onOpenSkills} title="Skills">
              <IconSparkles16 />
            </button>
            <button
              className="icon-btn"
              onClick={() => setShowPromptModal(true)}
              title="全局提示词"
            >
              <IconPencil16 />
            </button>
          </div>
          <button className="icon-btn" onClick={onOpenSettings} title="设置">
            <IconSettings16 size={17} />
          </button>
        </div>
      </aside>
    </>
  );
}
