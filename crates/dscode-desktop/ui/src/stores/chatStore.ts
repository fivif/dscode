import { create } from 'zustand';
import { sendMessage as tauriSendMessage, abort as tauriAbort, getSession } from '@/lib/tauri';
import type { Message, StreamEvent, ToolCallRecord, ThinkingBlock } from '@/lib/types';
import { genId } from '@/lib/types';

interface ActiveStream { sessionId: string; msgId: string; text: string; thinking: ThinkingBlock[]; toolCalls: ToolCallRecord[]; }

export interface ChatStore {
  messages: Message[]; activeSessionId: string | null;
  isStreaming: boolean; streamError: string | null;
  _stream: ActiveStream | null;
  setActiveSession: (id: string) => void;
  loadSessionMessages: (id: string) => Promise<void>;
  setMessages: (m: Message[]) => void; clearMessages: () => void;
  startStream: (s: string, u: Message) => void;
  handleStreamEvent: (e: StreamEvent) => void;
  endStream: (err?: string) => void;
  sendMessage: (c: string) => Promise<void>;
  abortStream: () => Promise<void>;
}

export const useChatStore = create<ChatStore>((set, get) => {
  function patchStreamingMsg(msgId: string, patch: any) {
    set((s) => ({
      messages: s.messages.map((m) => (m.id === msgId ? { ...m, ...patch } : m)),
    }));
  }

  return {
    messages: [], activeSessionId: null, isStreaming: false, streamError: null, _stream: null,

    setActiveSession(id) { set({ activeSessionId: id, messages: [], streamError: null }); },
    async loadSessionMessages(id: string) {
      try {
        const session = await getSession(id);
        const msgs = (session?.messages || []).map((m: any) => {
          // Strip tool-prefix from old-format messages
          if (m.content && m.tool_calls?.length > 0 && m.content.startsWith('🔧')) {
            m.content = '';
          }
          if (m.reasoning_content && (!m.content || m.content === '')) {
            return { ...m, thinking_blocks: [{ step: m.role === 'assistant' ? 0 : 0, content: m.reasoning_content }] };
          }
          if (m.role === 'tool' && m.tool_call_id) {
            return { ...m, role: 'assistant' as const, content: '', tool_calls: [{ id: m.tool_call_id, name: 'tool', description: '', status: 'success' as const, result: m.content || '' }] };
          }
          return m;
        });
        set({ messages: msgs });
      } catch { set({ messages: [] }); }
    },
    setMessages(messages) { set({ messages }); },
    clearMessages() { set({ messages: [], streamError: null }); },

    startStream(sid, userMsg) {
      const id = genId();
      const asst: Message = { id, session_id: sid, role: 'assistant', content: '', created_at: Math.floor(Date.now() / 1000), thinking_blocks: [], tool_calls: [], stream_state: { text: '', thinking: [], tool_calls: [] } };
      set({ isStreaming: true, streamError: null, messages: [...get().messages, userMsg, asst], _stream: { sessionId: sid, msgId: id, text: '', thinking: [], toolCalls: [] } });
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
          set((s) => ({
            messages: s.messages.map((m) => ({
              ...m, tool_calls: (m.tool_calls || []).map((t) =>
                t.id === event.id ? { ...t, ...(event.type === 'tool_progress' ? { result: (t.result || '') + (event as any).chunk } : {}), ...updates } : t
              ),
            })),
          }));
          break;
        }

        case 'error': set({ streamError: event.content }); break;
        case 'fact': break;

        case 'complete': {
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

    endStream(error) { set({ isStreaming: false, _stream: null, streamError: error || null }); },

    async sendMessage(content) {
      const sid = get().activeSessionId; if (!sid || get().isStreaming) return;
      const userMsg: Message = { id: genId(), session_id: sid, role: 'user', content, created_at: Math.floor(Date.now() / 1000) };
      get().startStream(sid, userMsg);
      try { await tauriSendMessage(sid, content); } catch (e: any) { get().endStream(String(e)); }
    },

    async abortStream() { try { await tauriAbort(); } catch {}; get().endStream(); },
  };
});
