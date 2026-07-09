import { useChatStore } from '@/stores/chatStore';
import * as tauri from '@/lib/tauri';

/** Pending dangerous-command confirmations for the active session. */
export default function PermissionBanner() {
  const pending = useChatStore((s) => s.pendingPermissions);
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const removePermission = useChatStore((s) => s.removePermission);

  const mine = pending.filter((p) => p.session_id === activeSessionId);
  if (mine.length === 0) return null;

  return (
    <div className="border-t border-amber-500/40 bg-amber-500/10 px-3 py-2 space-y-2 max-h-48 overflow-y-auto">
      {mine.map((p) => (
        <div
          key={p.id}
          className="rounded-lg border border-amber-500/30 bg-card/90 px-3 py-2 text-xs space-y-2"
        >
          <div className="flex items-start gap-2">
            <svg
              width="16"
              height="16"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="1.8"
              className="text-amber-400 shrink-0 mt-0.5"
              aria-hidden
            >
              <path d="M10.29 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.71 3.86a2 2 0 0 0-3.42 0z" />
              <line x1="12" y1="9" x2="12" y2="13" />
              <line x1="12" y1="17" x2="12.01" y2="17" />
            </svg>
            <div className="min-w-0 flex-1">
              <div className="text-amber-200 font-medium">需要确认危险命令</div>
              <div className="text-gray-400 mt-0.5">{p.reason}</div>
              <pre className="mt-1.5 text-[11px] font-mono text-gray-200 bg-black/30 rounded px-2 py-1.5 overflow-x-auto whitespace-pre-wrap break-all">
                {p.command}
              </pre>
              <div className="text-[10px] text-gray-600 mt-1">
                {p.timeout_secs || 120}s 内未操作将自动拒绝
              </div>
            </div>
          </div>
          <div className="flex justify-end gap-2">
            <button
              type="button"
              className="px-3 py-1 rounded-md text-gray-400 hover:text-gray-200 border border-border"
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
              className="px-3 py-1 rounded-md text-white bg-amber-600 hover:bg-amber-500"
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
