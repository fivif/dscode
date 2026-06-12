import { useEffect, useRef } from 'react';
import { onStreamEvent } from '@/lib/tauri';
import { useChatStore } from '@/stores/chatStore';
import type { StreamEvent } from '@/lib/types';

/**
 * Listens to Tauri stream events and dispatches them into chatStore.
 * Starts listening when activeSessionId is set; cleans up on unmount
 * or when the session changes.
 */
export function useStreamEvents() {
  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const isStreaming = useChatStore((s) => s.isStreaming);
  const handleStreamEvent = useChatStore((s) => s.handleStreamEvent);
  const unlistenRef = useRef<(() => void) | null>(null);

  useEffect(() => {
    // Tear down previous listener
    if (unlistenRef.current) {
      unlistenRef.current();
      unlistenRef.current = null;
    }

    if (!activeSessionId) return;

    // Subscribe to stream events
    const unlisten = onStreamEvent(activeSessionId, (event: StreamEvent) => {
      handleStreamEvent(event);
    });
    unlistenRef.current = unlisten;

    return () => {
      if (unlistenRef.current) {
        unlistenRef.current();
        unlistenRef.current = null;
      }
    };
  }, [activeSessionId, handleStreamEvent]);

  return { isStreaming };
}
