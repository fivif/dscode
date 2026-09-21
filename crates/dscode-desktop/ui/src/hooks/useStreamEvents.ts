import { useEffect, useRef } from 'react';
import { listen, onAnyStreamEvent } from '@/lib/tauri';
import { useChatStore } from '@/stores/chatStore';
import type { StreamEvent } from '@/lib/types';

/**
 * Listens to Tauri stream events for **all** sessions so background chats
 * keep updating while the user views another session.
 */
export function useStreamEvents() {
  const handleStreamEvent = useChatStore((s) => s.handleStreamEvent);
  const unlistenRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    if (unlistenRef.current) {
      unlistenRef.current();
      unlistenRef.current = null;
    }

    const unlisten = onAnyStreamEvent((sessionId: string, event: StreamEvent) => {
      handleStreamEvent(sessionId, event);
    });
    unlistenRef.current = unlisten;

    return () => {
      if (unlistenRef.current) {
        unlistenRef.current();
        unlistenRef.current = null;
      }
    };
  }, [handleStreamEvent]);

  // The shell's event bridge can fall behind the bus and drop events; it tells
  // us so we can say that the visible transcript may be incomplete.
  useEffect(() => {
    let disposed = false;
    let stop: (() => void) | undefined;
    void listen<{ skipped: number }>('event-bridge-lagged', (e) => {
      if (disposed) return;
      useChatStore.getState().markBridgeLagged(Number(e.payload?.skipped) || 0);
    }).then((fn) => {
      if (disposed) fn();
      else stop = fn;
    });
    return () => {
      disposed = true;
      stop?.();
    };
  }, []);

  const isStreaming = useChatStore((s) => s.isStreaming);
  return { isStreaming };
}
