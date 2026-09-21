import { useChatStore } from '@/stores/chatStore';
import { useSessionStore } from '@/stores/sessionStore';
import * as tauri from '@/lib/tauri';
import { IconAlert16 } from '@/components/icons';

/**
 * Pending dangerous-command confirmations.
 *
 * Shown for EVERY session, not just the visible one: a request from a session
 * the user switched away from (or raised while they were on the settings page)
 * is still blocking, and the backend denies it when its 120 s timer expires.
 */
export default function PermissionBanner() {
  const pending = useChatStore((s) => s.pendingPermissions);
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const removePermission = useChatStore((s) => s.removePermission);
  const sessions = useSessionStore((s) => s.sessions);

  if (pending.length === 0) return null;

  const titleOf = (sessionId: string) => {
    const s = sessions.find((x) => x.id === sessionId);
    return s?.title || sessionId.slice(0, 8);
  };

  return (
    <div className="border-t-2 border-warning bg-warning/10 px-3 py-2 space-y-2 max-h-48 overflow-y-auto">
      {pending.map((p) => (
        <div
          key={p.id}
          /* Coloured state edge, so it takes `shadow-lift` rather than
             `shadow-card` — the card hairline is white and would seam across
             the warning border. */
          className="rounded-card border border-warning/40 border-l-2 border-l-warning bg-card px-3 py-2 text-[13px] space-y-2 shadow-lift"
        >
          <div className="flex items-start gap-2">
            <IconAlert16 size={16} className="text-warning shrink-0 mt-0.5" />
            <div className="min-w-0 flex-1">
              <div className="text-[15px] font-semibold text-primary">
                需要确认危险命令
                <span className="ml-2 text-[11px] font-normal text-muted">
                  会话：{titleOf(p.session_id)}
                  {p.session_id !== activeSessionId ? '（非当前会话）' : ''}
                </span>
              </div>
              <div className="text-secondary mt-0.5">{p.reason}</div>
              <pre className="mt-1.5 text-[11px] font-mono text-primary bg-main border border-border rounded-control px-2 py-1.5 overflow-x-auto whitespace-pre-wrap break-all">
                {p.command}
              </pre>
              <div className="text-[11px] text-muted mt-1">
                {p.timeout_secs || 120}s 内未操作将自动拒绝
              </div>
            </div>
          </div>
          <div className="flex justify-end gap-2">
            <button
              type="button"
              className="btn btn-secondary"
              onClick={async () => {
                try {
                  await tauri.denyPermission(p.id);
                } catch {
                  /* expired */
                }
                removePermission(p.id);
              }}
            >
              拒绝
            </button>
            <button
              type="button"
              className="btn btn-danger"
              onClick={async () => {
                try {
                  await tauri.approvePermission(p.id);
                } catch {
                  /* expired */
                }
                removePermission(p.id);
              }}
            >
              允许执行
            </button>
          </div>
        </div>
      ))}
    </div>
  );
}
