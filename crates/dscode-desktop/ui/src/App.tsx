import { useState, useEffect, useRef } from 'react';
import Sidebar from '@/components/Sidebar/Sidebar';
import ChatArea from '@/components/Chat/ChatArea';
import InputBox from '@/components/Chat/InputBox';
import SettingsPage from '@/components/Settings/SettingsPage';
import McpPage from '@/components/Settings/McpPage';
import SkillsPage from '@/components/Settings/SkillsPage';
import PermissionBanner from '@/components/Chat/PermissionBanner';
import { useStreamEvents } from '@/hooks/useStreamEvents';
import { useConfigStore } from '@/stores/configStore';
import { useChatStore } from '@/stores/chatStore';
import { useSessionStore } from '@/stores/sessionStore';

type Page = 'chat' | 'settings' | 'mcp' | 'skills';

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
    let cancelled = false;
    getLastSession().then((s) => {
      if (cancelled || !s?.id) return;
      // The IPC can land after the user already picked a session (or created a
      // new chat) — never yank them into a different conversation, or the next
      // message goes somewhere they did not choose.
      if (useChatStore.getState().activeSessionId) return;
      setActiveSession(s.id);
      loadSessionMessages(s.id);
    });
    return () => {
      cancelled = true;
    };
  }, [getLastSession, setActiveSession, loadSessionMessages]);

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
    <div className="flex h-full w-full app-ground text-primary">
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
            // A 1px hairline centred in the 6px hit strip: visible enough to say
            // "this pane resizes", quiet enough not to read as a divider.
            <div
              role="separator"
              aria-orientation="vertical"
              className="group w-1.5 shrink-0 cursor-col-resize flex justify-center"
              onMouseDown={() => { dragging.current = true; }}
            >
              <div className="h-full w-px bg-border transition-[width,background-color] duration-150 ease-ios group-hover:w-[2px] group-hover:bg-accent/50 group-active:bg-accent" />
            </div>
          )}
        </>
      )}

      <main className="flex-1 flex flex-col min-w-0">
        {page === 'chat' && (
          <>
            <ChatArea />
            <PermissionBanner />
            <InputBox />
          </>
        )}
        {page === 'settings' && <SettingsPage onBack={() => setPage('chat')} />}
        {page === 'mcp' && <McpPage onBack={() => setPage('chat')} />}
        {page === 'skills' && <SkillsPage onBack={() => setPage('chat')} />}
        {/* A pending approval blocks the agent and is auto-denied on timeout —
            keep it visible (and answerable) outside the chat page too. */}
        {page !== 'chat' && <PermissionBanner />}
      </main>
    </div>
  );
}
