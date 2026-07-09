import { useEffect, useRef } from 'react';
import { onAnyStreamEvent } from '@/lib/tauri';
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

  const isStreaming = useChatStore((s) => s.isStreaming);
  return { isStreaming };
}
