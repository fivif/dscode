import { useState, useEffect, useRef } from 'react';
import Sidebar from '@/components/Sidebar/Sidebar';
import ChatArea from '@/components/Chat/ChatArea';
import InputBox from '@/components/Chat/InputBox';
import SettingsPage from '@/components/Settings/SettingsPage';
import McpPage from '@/components/Settings/McpPage';
import SkillsPage from '@/components/Settings/SkillsPage';
import { useStreamEvents } from '@/hooks/useStreamEvents';
import { useConfigStore } from '@/stores/configStore';
import { useChatStore } from '@/stores/chatStore';
import { useSessionStore } from '@/stores/sessionStore';

type Page = 'chat' | 'settings' | 'mcp' | 'skills' | 'wiki';

import WikiPage from '@/components/Settings/WikiPage';

export default function App() {
  const [page, setPage] = useState<Page>('chat');
  const [sidebarWidth, setSidebarWidth] = useState(260);
  const [sidebarCollapsed, setSidebarCollapsed] = useState(false);
  const dragging = useRef(false);
  const loadConfig = useConfigStore((s) => s.loadConfig);
  const setActiveSession = useChatStore((s) => s.setActiveSession);
  const loadSessionMessages = useChatStore((s) => s.loadSessionMessages);
  const getLastSession = useSessionStore((s) => s.getLastSession);

  useEffect(() => { loadConfig(); }, [loadConfig]);
  useStreamEvents();

  // Auto-select last session on startup
  useEffect(() => {
    getLastSession().then((s) => {
      if (s?.id) {
        setActiveSession(s.id);
        loadSessionMessages(s.id);
      }
    });
  }, []);

  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      if (!dragging.current || sidebarCollapsed) return;
      setSidebarWidth(Math.max(180, Math.min(420, e.clientX)));
    };
    const onUp = () => { dragging.current = false; };
    window.addEventListener('mousemove', onMove);
    window.addEventListener('mouseup', onUp);
    return () => {
      window.removeEventListener('mousemove', onMove);
      window.removeEventListener('mouseup', onUp);
    };
  }, [sidebarCollapsed]);

  const toggleCollapse = () => setSidebarCollapsed((v) => !v);

  return (
    <div className="flex h-full w-full bg-main text-gray-100">
      {page === 'chat' && (
        <>
          <Sidebar
            onOpenSettings={() => setPage('settings')}
            onOpenMcp={() => setPage('mcp')}
            onOpenSkills={() => setPage('skills')}
            width={sidebarWidth}
            collapsed={sidebarCollapsed}
            onToggleCollapse={toggleCollapse}
          />
          {!sidebarCollapsed && (
            <div
              className="w-1.5 cursor-col-resize bg-transparent hover:bg-gray-600 active:bg-gray-500 transition-colors shrink-0"
              onMouseDown={() => { dragging.current = true; }}
            />
          )}
        </>
      )}

      <main className="flex-1 flex flex-col min-w-0">
        {page === 'chat' && (
          <>
            <ChatArea />
            <InputBox onOpenWiki={() => setPage('wiki')} />
          </>
        )}
        {page === 'settings' && <SettingsPage onBack={() => setPage('chat')} />}
        {page === 'mcp' && <McpPage onBack={() => setPage('chat')} />}
        {page === 'skills' && <SkillsPage onBack={() => setPage('chat')} />}
        {page === 'wiki' && <WikiPage onBack={() => setPage('chat')} />}
      </main>
    </div>
  );
}
