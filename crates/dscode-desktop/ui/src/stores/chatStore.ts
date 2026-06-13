import { create } from 'zustand';
import { sendMessage as tauriSendMessage, abort as tauriAbort, getSession } from '@/lib/tauri';
import type { Message, StreamEvent, ToolCallRecord, ThinkingBlock } from '@/lib/types';
import { genId } from '@/lib/types';

interface ActiveStream { sessionId: string; msgId: string; text: string; thinking: ThinkingBlock[]; toolCalls: ToolCallRecord[]; }

export interface ChatStore {
  messages: Message[]; activeSessionId: string | null;
  isStreaming: boolean; streamError: string | null;
  _stream: ActiveStream | null;
  setActiveSession: (id: string | null) => void;
  loadSessionMessages: (id: string) => Promise<void>;
  setMessages: (m: Message[]) => void; clearMessages: () => void;
  startStream: (s: string, u: Message) => void;
  handleStreamEvent: (e: StreamEvent) => void;
  endStream: (err?: string) => void;
  sendMessage: (c: string) => Promise<void>;
  abortStream: () => Promise<void>;
}

let _streamTimeoutId: ReturnType<typeof setTimeout> | null = null;

export const useChatStore = create<ChatStore>((set, get) => {
  return {
    messages: [], activeSessionId: null, isStreaming: false, streamError: null, _stream: null,

    setActiveSession(id) {
      if (_streamTimeoutId) { clearTimeout(_streamTimeoutId); _streamTimeoutId = null; }
      set({ activeSessionId: id, messages: [], isStreaming: false, streamError: null, _stream: null });
    },
    async loadSessionMessages(id: string) {
      try {
        const session = await getSession(id);
        // Each DB row becomes one renderable item (thinking, text, or tool card).
        // Tool results are attached to their preceding tool_calls message.
        let prevWithToolCalls: any = null;
        const msgs: any[] = [];
        for (const m of (session?.messages || [])) {
          if (m.role === 'tool') {
            // Attach tool result to the preceding tool_calls message
            if (prevWithToolCalls?.tool_calls) {
              for (const tc of prevWithToolCalls.tool_calls) {
                if (tc.id === m.tool_call_id) { tc.result = m.content || ''; break; }
              }
            }
            continue;
          }
          // reasoning_content → thinking block
          if (m.reasoning_content) {
            msgs.push({ ...m, thinking_blocks: [{ step: 0, content: m.reasoning_content }] });
            prevWithToolCalls = m.tool_calls?.length > 0 ? m : null;
            continue;
          }
          // tool_calls → tool card message with proper names
          if (m.tool_calls?.length > 0) {
            const namedTC = m.tool_calls.map((tc: any) => ({
              id: tc.id, name: tc.function?.name || tc.name || 'tool',
              description: tc.description || '', status: 'success' as const, result: tc.result || ''
            }));
            msgs.push({ ...m, tool_calls: namedTC });
            prevWithToolCalls = { ...m, tool_calls: namedTC };
            continue;
          }
          // regular message
          msgs.push(m);
          prevWithToolCalls = null;
        }
        set({ messages: msgs });
      } catch { set({ messages: [] }); }
    },
    setMessages(messages) { set({ messages }); },
    clearMessages() { set({ messages: [], streamError: null }); },

    startStream(sid, userMsg) {
      if (get().isStreaming) return;
      if (_streamTimeoutId) { clearTimeout(_streamTimeoutId); _streamTimeoutId = null; }
      const id = genId();
      const asst: Message = { id, session_id: sid, role: 'assistant', content: '', created_at: Math.floor(Date.now() / 1000), thinking_blocks: [], tool_calls: [], stream_state: { text: '', thinking: [], tool_calls: [] } };
      set({ isStreaming: true, streamError: null, messages: [...get().messages, userMsg, asst], _stream: { sessionId: sid, msgId: id, text: '', thinking: [], toolCalls: [] } });
      _streamTimeoutId = setTimeout(() => {
        const state = get();
        if (state.isStreaming) {
          state.endStream('Stream timed out - no complete event received after 10 minutes');
        }
      }, 600000);
    },

    /** Flush current streaming text into a completed message, start fresh stream msg */
    _flushAndNewStreamMsg(logText: string) {
      set((s) => {
        const st = s._stream;
        if (!st) return s;
        // Mark current streaming msg as complete (no stream_state)
        const newId = genId();
        const completedMsg = {
          id: st.msgId, session_id: st.sessionId, role: 'assistant' as const,
          content: logText || st.text, thinking_blocks: st.thinking, tool_calls: st.toolCalls,
          created_at: Math.floor(Date.now() / 1000), stream_state: undefined,
        };
        // Create new empty streaming message
        const newStreamMsg: Message = {
          id: newId, session_id: st.sessionId, role: 'assistant', content: '',
          created_at: Math.floor(Date.now() / 1000), thinking_blocks: [], tool_calls: [],
          stream_state: { text: '', thinking: [], tool_calls: [] },
        };
        return {
          _stream: { ...st, msgId: newId, text: '', thinking: [], toolCalls: [] },
          messages: s.messages.map((m) => m.id === st.msgId ? completedMsg : m).concat(newStreamMsg),
        };
      });
    },

    handleStreamEvent(event) {
      const stream = get()._stream;
      if (!stream) return;
      const { msgId } = stream;

      switch (event.type) {
        case 'thinking': {
          set((s) => {
            const st = s._stream;
            if (!st) return s;
            const idx = st.thinking.findIndex((t) => t.step === event.step);
            const thinking = idx >= 0
              ? st.thinking.map((t, i) => i === idx ? { step: t.step, content: t.content + event.content } : t)
              : [...st.thinking, { step: event.step, content: event.content }];
            return {
              _stream: { ...st, thinking },
              messages: s.messages.map((m) => m.id === st.msgId ? { ...m, thinking_blocks: thinking } : m),
            };
          });
          break;
        }

        case 'token': {
          set((s) => {
            const st = s._stream;
            if (!st) return s;
            const text = st.text + event.content;
            return {
              _stream: { ...st, text },
              messages: s.messages.map((m) => m.id === st.msgId ? { ...m, content: text, thinking_blocks: st.thinking, tool_calls: st.toolCalls, stream_state: { text, thinking: st.thinking, tool_calls: st.toolCalls } } : m),
            };
          });
          break;
        }

        case 'tool_start': {
          // Flush accumulated text into a completed message, start fresh for tool card
          const stream = get()._stream;
          if (stream && stream.text.trim()) {
            get()._flushAndNewStreamMsg(stream.text);
          }
          const tc: ToolCallRecord = { id: event.id, name: event.name, description: event.description, status: 'running', result: '' };
          set((s) => {
            const st = s._stream;
            if (!st) return s;
            return {
              _stream: { ...st, toolCalls: [...st.toolCalls, tc] },
              messages: s.messages.map((m) => m.id === st.msgId ? { ...m, tool_calls: [...(m.tool_calls || []), tc] } : m),
            };
          });
          break;
        }

        case 'tool_progress':
        case 'tool_end': {
          const updates: any = event.type === 'tool_end' ? { status: event.status, result: event.result } : {};
          set((s) => {
            const st = s._stream;
            if (!st) return s;
            const updatedToolCalls = st.toolCalls.map((t) =>
              t.id === event.id ? { ...t, ...(event.type === 'tool_progress' ? { result: (t.result || '') + (event as any).chunk } : {}), ...updates } : t
            );
            return {
              _stream: { ...st, toolCalls: updatedToolCalls },
              messages: s.messages.map((m) => m.id === st.msgId ? { ...m, tool_calls: updatedToolCalls } : m),
            };
          });
          break;
        }

        case 'error': {
          if (_streamTimeoutId) { clearTimeout(_streamTimeoutId); _streamTimeoutId = null; }
          set({ streamError: event.content, isStreaming: false, _stream: null });
          break;
        }
        case 'fact': break;

        case 'complete': {
          if (_streamTimeoutId) { clearTimeout(_streamTimeoutId); _streamTimeoutId = null; }
          set((s) => {
            const st = s._stream;
            if (!st) return { isStreaming: false, _stream: null };
            return {
              isStreaming: false, _stream: null,
              messages: s.messages.map((m) => m.id === st.msgId ? { ...m, content: st.text, thinking_blocks: st.thinking, tool_calls: st.toolCalls, stream_state: undefined } : m),
            };
          });
          break;
        }
      }
    },

    endStream(error) {
      if (_streamTimeoutId) { clearTimeout(_streamTimeoutId); _streamTimeoutId = null; }
      set((s) => {
        const st = s._stream;
        if (!st) return { isStreaming: false, _stream: null, streamError: error || null };
        return {
          isStreaming: false, _stream: null, streamError: error || null,
          messages: s.messages.map((m) => m.id === st.msgId ? { ...m, content: st.text, thinking_blocks: st.thinking, tool_calls: st.toolCalls, stream_state: undefined } : m),
        };
      });
    },

    async sendMessage(content) {
      const sid = get().activeSessionId; if (!sid || get().isStreaming) return;
      const userMsg: Message = { id: genId(), session_id: sid, role: 'user', content, created_at: Math.floor(Date.now() / 1000) };
      get().startStream(sid, userMsg);
      try { await tauriSendMessage(sid, content); } catch (e: any) { get().endStream(String(e)); }
    },

    async abortStream() { try { await tauriAbort(); } catch {}; get().endStream(); },
  };
});
