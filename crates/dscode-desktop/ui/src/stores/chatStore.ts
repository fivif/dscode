import { create } from 'zustand';
import { sendMessage as tauriSendMessage, abort as tauriAbort, getSession } from '@/lib/tauri';
import type { Message, StreamEvent, ToolCallRecord, ThinkingBlock, FactRecord, TeamAgent, PlanChoice } from '@/lib/types';
import { genId } from '@/lib/types';

interface ActiveStream { sessionId: string; msgId: string; text: string; thinking: ThinkingBlock[]; toolCalls: ToolCallRecord[]; facts: FactRecord[]; }

/** Snapshot of one session's chat UI (enables concurrent multi-session runs). */
export interface SessionBuffer {
  messages: Message[];
  isStreaming: boolean;
  streamError: string | null;
  _stream: ActiveStream | null;
  _teamHostMsgId: string | null;
  teamAgents: TeamAgent[];
}

function emptyBuffer(): SessionBuffer {
  return {
    messages: [],
    isStreaming: false,
    streamError: null,
    _stream: null,
    _teamHostMsgId: null,
    teamAgents: [],
  };
}

// Batch high-frequency stream updates (~30–60fps) to avoid main-thread jank.
// Without this, every bash line / team token does a full messages[] map + React tree walk.
let _flushScheduled = false;
let _pendingText = '';
/** tool_call_id → pending progress chunks */
const _pendingToolChunks: Record<string, string> = {};
/** agent_id → pending output chunks */
const _pendingTeamChunks: Record<string, string> = {};
let _rafId: number | null = null;

function patchMessageById(messages: Message[], msgId: string, patch: Partial<Message>): Message[] {
  // Single-pass update; avoid cloning unchanged messages when possible.
  let found = false;
  const next = messages.map((m) => {
    if (m.id !== msgId) return m;
    found = true;
    return { ...m, ...patch };
  });
  return found ? next : messages;
}

function flushStreamBatch(set: (fn: (s: ChatStore) => Partial<ChatStore>) => void, get: () => ChatStore) {
  _flushScheduled = false;
  _rafId = null;
  const text = _pendingText;
  _pendingText = '';
  const toolKeys = Object.keys(_pendingToolChunks);
  const teamKeys = Object.keys(_pendingTeamChunks);
  const toolChunks: Record<string, string> = {};
  for (const k of toolKeys) {
    toolChunks[k] = _pendingToolChunks[k];
    delete _pendingToolChunks[k];
  }
  const teamChunks: Record<string, string> = {};
  for (const k of teamKeys) {
    teamChunks[k] = _pendingTeamChunks[k];
    delete _pendingTeamChunks[k];
  }
  if (!text && toolKeys.length === 0 && teamKeys.length === 0) return;

  set((s) => {
    const st = s._stream;
    let messages = s.messages;
    let stream = st;
    let teamHostId = s._teamHostMsgId;

    if (text && st) {
      const newText = st.text + text;
      const toolCalls = st.toolCalls;
      messages = patchMessageById(messages, st.msgId, {
        content: newText,
        thinking_blocks: st.thinking,
        tool_calls: toolCalls,
        fact_cards: st.facts,
        stream_state: {
          text: newText,
          thinking: st.thinking,
          tool_calls: toolCalls,
          fact_cards: st.facts,
        },
      });
      stream = { ...st, text: newText };
    }

    if (toolKeys.length > 0 && stream) {
      let toolCalls = stream.toolCalls;
      for (const id of toolKeys) {
        const chunk = toolChunks[id] || '';
        toolCalls = toolCalls.map((t) =>
          t.id === id
            ? { ...t, result: ((t.result || '') + chunk).slice(-80_000) }
            : t,
        );
      }
      messages = patchMessageById(messages, stream.msgId, { tool_calls: toolCalls });
      stream = { ...stream, toolCalls };
    }

    if (teamKeys.length > 0) {
      const hostId = stream?.msgId || teamHostId;
      if (hostId) {
        let agents: TeamAgent[] = [];
        const host = messages.find((m) => m.id === hostId);
        agents = [...(host?.team_agents || [])];
        for (const aid of teamKeys) {
          const chunk = teamChunks[aid] || '';
          agents = upsertTeamAgent(agents, aid, { output: chunk }, 'output');
        }
        messages = patchMessageById(messages, hostId, { team_agents: agents });
        teamHostId = hostId;
      }
    }

    const sid = stream?.sessionId || s.activeSessionId;
    if (!sid) {
      return { messages, _stream: stream, _teamHostMsgId: teamHostId };
    }
    return {
      messages,
      _stream: stream,
      _teamHostMsgId: teamHostId,
      sessionBuffers: {
        ...s.sessionBuffers,
        [sid]: {
          ...(s.sessionBuffers[sid] || emptyBuffer()),
          messages,
          isStreaming: true,
          _stream: stream,
          _teamHostMsgId: teamHostId,
          teamAgents: s.teamAgents,
          streamError: s.streamError,
        },
      },
    };
  });
}

function scheduleFlush(set: any, get: any) {
  if (_flushScheduled) return;
  _flushScheduled = true;
  // rAF ≈ 1 frame; fall back to 32ms if rAF unavailable (tests / SSR)
  if (typeof requestAnimationFrame === 'function') {
    _rafId = requestAnimationFrame(() => flushStreamBatch(set, get));
  } else {
    setTimeout(() => flushStreamBatch(set, get), 32);
  }
}

function drainPending() {
  _flushScheduled = false;
  if (_rafId != null && typeof cancelAnimationFrame === 'function') {
    cancelAnimationFrame(_rafId);
    _rafId = null;
  }
  _pendingText = '';
  for (const k of Object.keys(_pendingToolChunks)) delete _pendingToolChunks[k];
  for (const k of Object.keys(_pendingTeamChunks)) delete _pendingTeamChunks[k];
}

function snapshotActive(s: {
  messages: Message[];
  isStreaming: boolean;
  streamError: string | null;
  _stream: ActiveStream | null;
  _teamHostMsgId: string | null;
  teamAgents: TeamAgent[];
}): SessionBuffer {
  return {
    messages: s.messages,
    isStreaming: s.isStreaming,
    streamError: s.streamError,
    _stream: s._stream,
    _teamHostMsgId: s._teamHostMsgId,
    teamAgents: s.teamAgents,
  };
}

/** Find message index that should own live Teams events (streaming msg, else last assistant). */
function teamHostIndex(messages: Message[], streamMsgId?: string | null): number {
  if (streamMsgId) {
    const i = messages.findIndex((m) => m.id === streamMsgId);
    if (i >= 0) return i;
  }
  for (let i = messages.length - 1; i >= 0; i--) {
    if (messages[i].role === 'assistant') return i;
  }
  return -1;
}

/**
 * Upsert a team agent into a list.
 * Same agent_id while running → update that row.
 * Same agent_id after done/error → new wave instance (`agent-1#2`).
 */
function upsertTeamAgent(
  agents: TeamAgent[],
  agentId: string,
  patch: Partial<TeamAgent> & { task?: string },
  mode: 'start' | 'output' | 'end',
): TeamAgent[] {
  // Prefer last matching open (running) slot for this base id
  const base = agentId.split('#')[0];
  let idx = -1;
  for (let i = agents.length - 1; i >= 0; i--) {
    const idBase = agents[i].id.split('#')[0];
    if (idBase === base && agents[i].status === 'running') {
      idx = i;
      break;
    }
  }
  if (mode === 'start') {
    if (idx >= 0) {
      // Re-start while still marked running — overwrite
      const next = [...agents];
      next[idx] = {
        ...next[idx],
        task: patch.task ?? next[idx].task,
        status: 'running',
        output: '',
      };
      return next;
    }
    // New instance: if base id already used, suffix #n
    const used = agents.filter((a) => a.id === base || a.id.startsWith(base + '#')).length;
    const id = used === 0 ? base : `${base}#${used + 1}`;
    return [
      ...agents,
      {
        id,
        task: patch.task || '',
        status: 'running',
        output: '',
      },
    ];
  }
  if (idx < 0) {
    // Fallback: last matching base id
    for (let i = agents.length - 1; i >= 0; i--) {
      if (agents[i].id.split('#')[0] === base) {
        idx = i;
        break;
      }
    }
  }
  if (idx < 0) {
    return [
      ...agents,
      {
        id: base,
        task: patch.task || '',
        status: (patch.status as TeamAgent['status']) || 'running',
        output: patch.output || '',
      },
    ];
  }
  const next = [...agents];
  const cur = next[idx];
  if (mode === 'output') {
    next[idx] = { ...cur, output: cur.output + (patch.output || '') };
  } else {
    next[idx] = {
      ...cur,
      status: (patch.status as TeamAgent['status']) || cur.status,
      output: cur.output || patch.output || '',
    };
  }
  return next;
}

type AgentPanelKind = 'teams' | 'auto' | 'auto_teams';

function panelKindFromAgentId(agentId: string): AgentPanelKind {
  // at-N = /auto + Teams parallel MAGI
  // subtask-N = sequential /auto
  // agent-N = plain Teams dispatcher
  if (agentId.startsWith('at-') || agentId.startsWith('auto-teams')) return 'auto_teams';
  if (agentId.startsWith('subtask-') || agentId.startsWith('magi-')) return 'auto';
  return 'teams';
}

function withTeamAgentsOnHost(
  messages: Message[],
  streamMsgId: string | null | undefined,
  mutator: (agents: TeamAgent[]) => TeamAgent[],
  kind?: AgentPanelKind,
): { messages: Message[]; teamAgents: TeamAgent[] } {
  const idx = teamHostIndex(messages, streamMsgId);
  if (idx < 0) {
    const teamAgents = mutator([]);
    return { messages, teamAgents };
  }
  const host = messages[idx];
  const team_agents = mutator(host.team_agents || []);
  const agent_panel_kind = kind || host.agent_panel_kind || 'teams';
  const nextMsgs = messages.map((m, i) =>
    i === idx ? { ...m, team_agents, agent_panel_kind } : m,
  );
  return { messages: nextMsgs, teamAgents: team_agents };
}

const TEAMS_MODE_STORAGE = 'dscode.teamsModeBySession';

function loadTeamsModeMap(): Record<string, boolean> {
  try {
    const raw = localStorage.getItem(TEAMS_MODE_STORAGE);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    return parsed && typeof parsed === 'object' ? parsed : {};
  } catch {
    return {};
  }
}

function saveTeamsModeMap(map: Record<string, boolean>) {
  try {
    localStorage.setItem(TEAMS_MODE_STORAGE, JSON.stringify(map));
  } catch {
    /* ignore quota */
  }
}

export interface ChatStore {
  messages: Message[]; activeSessionId: string | null;
  isStreaming: boolean; streamError: string | null;
  _stream: ActiveStream | null;
  /** Last assistant msg of current turn — Teams often emit after stream complete */
  _teamHostMsgId: string | null;
  /** Cached UI state per session (background streams keep updating) */
  sessionBuffers: Record<string, SessionBuffer>;
  /** Current session's Teams toggle (derived from per-session map) */
  teamsMode: boolean;
  /** Persist Teams on/off per session id */
  teamsModeBySession: Record<string, boolean>;
  toggleTeams: () => void;
  /** Mirror of latest turn's team agents (for status); primary data lives on messages */
  teamAgents: TeamAgent[]; updateTeamAgent: (id: string, patch: Partial<TeamAgent>) => void;
  setActiveSession: (id: string | null) => void;
  loadSessionMessages: (id: string) => Promise<void>;
  setMessages: (m: Message[]) => void; clearMessages: () => void;
  startStream: (s: string, u: Message) => void;
  /** Internal: split stream message when tools appear mid-turn */
  _flushAndNewStreamMsg: (logText: string) => void;
  handleStreamEvent: (sessionId: string, e: StreamEvent) => void;
  endStream: (err?: string, sessionId?: string) => void;
  sendMessage: (c: string, attachmentPaths?: string[]) => Promise<void>;
  abortStream: () => Promise<void>;
  /** Whether a session (any) is currently generating */
  isSessionStreaming: (id: string) => boolean;
  /** Mark /plan choice card as answered after user picks option/custom */
  markPlanAnswered: (messageId: string, selected: string) => void;
  /** Pending Safe-mode permission prompts */
  pendingPermissions: import('@/lib/types').PermissionRequest[];
  removePermission: (id: string) => void;
}

let _streamTimeouts: Record<string, ReturnType<typeof setTimeout>> = {};
/** Idle timeout between events (not absolute wall clock). /auto can run long with heartbeats. */
const STREAM_IDLE_MS = 15 * 60 * 1000;

function armStreamIdleTimeout(sessionId: string, get: () => ChatStore) {
  if (_streamTimeouts[sessionId]) clearTimeout(_streamTimeouts[sessionId]);
  _streamTimeouts[sessionId] = setTimeout(() => {
    const state = get();
    const streaming =
      state.activeSessionId === sessionId
        ? state.isStreaming
        : !!state.sessionBuffers[sessionId]?.isStreaming;
    if (streaming) {
      state.endStream(
        'Stream idle timeout: no events for 15 minutes (API may be stuck — try again or check network)',
        sessionId,
      );
    }
  }, STREAM_IDLE_MS);
}

function clearStreamIdleTimeout(sessionId?: string | null) {
  if (sessionId) {
    if (_streamTimeouts[sessionId]) {
      clearTimeout(_streamTimeouts[sessionId]);
      delete _streamTimeouts[sessionId];
    }
    return;
  }
  for (const id of Object.keys(_streamTimeouts)) {
    clearTimeout(_streamTimeouts[id]);
  }
  _streamTimeouts = {};
}

export const useChatStore = create<ChatStore>((set, get) => {
  const teamsModeBySession = loadTeamsModeMap();
  return {
    messages: [], activeSessionId: null, isStreaming: false, streamError: null, _stream: null,
    _teamHostMsgId: null,
    sessionBuffers: {},
    teamsMode: false,
    teamsModeBySession,
    teamAgents: [],
    pendingPermissions: [],
    removePermission(id) {
      set((s) => ({
        pendingPermissions: s.pendingPermissions.filter((p) => p.id !== id),
      }));
    },
    isSessionStreaming(id) {
      const s = get();
      if (s.activeSessionId === id) return s.isStreaming;
      return !!s.sessionBuffers[id]?.isStreaming;
    },
    markPlanAnswered(messageId, selected) {
      set((s) => {
        const messages = s.messages.map((m) =>
          m.id === messageId && m.plan_choice
            ? {
                ...m,
                plan_choice: {
                  ...m.plan_choice,
                  answered: true,
                  selected,
                },
              }
            : m,
        );
        const sid = s.activeSessionId;
        const sessionBuffers = sid
          ? {
              ...s.sessionBuffers,
              [sid]: {
                ...(s.sessionBuffers[sid] || emptyBuffer()),
                messages,
              },
            }
          : s.sessionBuffers;
        return { messages, sessionBuffers };
      });
    },
    toggleTeams() {
      // Per-session UI toggle — do NOT inject /teams into chat.
      const sid = get().activeSessionId;
      if (!sid) return;
      const next = !get().teamsModeBySession[sid];
      const map = { ...get().teamsModeBySession, [sid]: next };
      saveTeamsModeMap(map);
      set({ teamsModeBySession: map, teamsMode: next });
    },
    updateTeamAgent(id, patch) {
      set((s) => {
        const hostId = s._stream?.msgId || s._teamHostMsgId;
        const { messages, teamAgents } = withTeamAgentsOnHost(s.messages, hostId, (agents) =>
          agents.map((a) => (a.id === id ? { ...a, ...patch } : a)),
        );
        return { messages, teamAgents };
      });
    },

    setActiveSession(id) {
      drainPending();
      const prev = get();
      const map = prev.teamsModeBySession;
      // Persist outgoing session (including in-flight stream) so it keeps running.
      let buffers = { ...prev.sessionBuffers };
      if (prev.activeSessionId) {
        buffers[prev.activeSessionId] = snapshotActive(prev);
      }

      if (!id) {
        set({
          activeSessionId: null,
          messages: [],
          isStreaming: false,
          streamError: null,
          _stream: null,
          _teamHostMsgId: null,
          teamAgents: [],
          sessionBuffers: buffers,
          teamsMode: false,
        });
        return;
      }

      const cached = buffers[id];
      if (cached) {
        set({
          activeSessionId: id,
          messages: cached.messages,
          isStreaming: cached.isStreaming,
          streamError: cached.streamError,
          _stream: cached._stream,
          _teamHostMsgId: cached._teamHostMsgId,
          teamAgents: cached.teamAgents,
          sessionBuffers: buffers,
          teamsMode: !!map[id],
        });
        return;
      }

      set({
        activeSessionId: id,
        messages: [],
        isStreaming: false,
        streamError: null,
        _stream: null,
        _teamHostMsgId: null,
        teamAgents: [],
        sessionBuffers: buffers,
        teamsMode: !!map[id],
      });
    },
    async loadSessionMessages(id: string) {
      // Prefer in-memory buffer (live stream or recent turn) over DB to avoid lag/clobber.
      const buf = get().sessionBuffers[id];
      if (buf && (buf.isStreaming || buf.messages.length > 0)) {
        if (get().activeSessionId === id) {
          set({
            messages: buf.messages,
            isStreaming: buf.isStreaming,
            streamError: buf.streamError,
            _stream: buf._stream,
            _teamHostMsgId: buf._teamHostMsgId,
            teamAgents: buf.teamAgents,
          });
        }
        return;
      }
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
          // fact message — attach to the preceding message's fact_cards
          if (m.role === 'fact' && m.subject && m.predicate && m.object) {
            const fact: FactRecord = { id: m.id, subject: m.subject, predicate: m.predicate, object: m.object };
            if (msgs.length > 0) {
              const last = msgs[msgs.length - 1];
              last.fact_cards = [...(last.fact_cards || []), fact];
            }
            continue;
          }
          // reasoning_content → thinking block
          if (m.reasoning_content) {
            msgs.push({ ...m, thinking_blocks: [{ step: 0, content: m.reasoning_content }] });
            prevWithToolCalls = (m.tool_calls && m.tool_calls.length > 0) ? m : null;
            continue;
          }
          // tool_calls → tool card message with proper names
          if (m.tool_calls && m.tool_calls.length > 0) {
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
        // Don't clobber a live stream that started while we were loading
        if (get().isSessionStreaming(id)) return;
        const nextBuf: SessionBuffer = {
          ...(get().sessionBuffers[id] || emptyBuffer()),
          messages: msgs,
          isStreaming: false,
          streamError: null,
          _stream: null,
        };
        if (get().activeSessionId === id) {
          set({
            messages: msgs,
            sessionBuffers: { ...get().sessionBuffers, [id]: nextBuf },
          });
        } else {
          set({ sessionBuffers: { ...get().sessionBuffers, [id]: nextBuf } });
        }
      } catch {
        if (get().activeSessionId === id) set({ messages: [] });
      }
    },
    setMessages(messages) { set({ messages }); },
    clearMessages() { set({ messages: [], streamError: null }); },

    startStream(sid, userMsg) {
      // Only block if THIS session is already streaming (other sessions may run in parallel).
      if (get().isSessionStreaming(sid)) return;
      clearStreamIdleTimeout(sid);
      const id = genId();
      const asst: Message = {
        id,
        session_id: sid,
        role: 'assistant',
        content: '',
        created_at: Math.floor(Date.now() / 1000),
        thinking_blocks: [],
        tool_calls: [],
        fact_cards: [],
        team_agents: [],
        stream_state: { text: '', thinking: [], tool_calls: [], fact_cards: [] },
      };
      const baseMsgs =
        get().activeSessionId === sid
          ? get().messages
          : get().sessionBuffers[sid]?.messages || [];
      const messages = [...baseMsgs, userMsg, asst];
      const stream: ActiveStream = {
        sessionId: sid,
        msgId: id,
        text: '',
        thinking: [],
        toolCalls: [],
        facts: [],
      };
      const buf: SessionBuffer = {
        messages,
        isStreaming: true,
        streamError: null,
        _stream: stream,
        _teamHostMsgId: id,
        teamAgents: [],
      };
      if (get().activeSessionId === sid) {
        set({
          isStreaming: true,
          streamError: null,
          teamAgents: [],
          _teamHostMsgId: id,
          messages,
          _stream: stream,
          sessionBuffers: { ...get().sessionBuffers, [sid]: buf },
        });
      } else {
        set({ sessionBuffers: { ...get().sessionBuffers, [sid]: buf } });
      }
      armStreamIdleTimeout(sid, get);
    },

    /** Flush current streaming text into a completed message, start fresh stream msg */
    _flushAndNewStreamMsg(logText: string) {
      set((s) => {
        const st = s._stream;
        if (!st) return s;
        const prev = s.messages.find((m) => m.id === st.msgId);
        // Mark current streaming msg as complete (no stream_state); keep team_agents
        const newId = genId();
        const completedMsg: Message = {
          id: st.msgId, session_id: st.sessionId, role: 'assistant',
          content: logText || st.text, thinking_blocks: st.thinking, tool_calls: st.toolCalls,
          fact_cards: st.facts,
          team_agents: prev?.team_agents,
          created_at: prev?.created_at ?? Math.floor(Date.now() / 1000), stream_state: undefined,
        };
        // Create new empty streaming message (Teams attach to host; prefer last asst after stream ends)
        const newStreamMsg: Message = {
          id: newId, session_id: st.sessionId, role: 'assistant', content: '',
          created_at: Math.floor(Date.now() / 1000), thinking_blocks: [], tool_calls: [],
          fact_cards: [], team_agents: [],
          stream_state: { text: '', thinking: [], tool_calls: [], fact_cards: [] },
        };
        const messages = s.messages.map((m) => m.id === st.msgId ? completedMsg : m).concat(newStreamMsg);
        const stream = { ...st, msgId: newId, text: '', thinking: [], toolCalls: [], facts: [] };
        return {
          _stream: stream,
          _teamHostMsgId: newId,
          messages,
          sessionBuffers: {
            ...s.sessionBuffers,
            [st.sessionId]: {
              messages,
              isStreaming: true,
              streamError: null,
              _stream: stream,
              _teamHostMsgId: newId,
              teamAgents: s.teamAgents,
            },
          },
        };
      });
    },

    handleStreamEvent(sessionId, event) {
      armStreamIdleTimeout(sessionId, get);

      // Background session: update buffer only (UI shows it when user switches back).
      if (sessionId !== get().activeSessionId) {
        applyBackgroundEvent(set, get, sessionId, event);
        return;
      }

      // Team / plan structured events may arrive after text stream pieces.
      if (
        event.type === 'team_agent_start' ||
        event.type === 'team_agent_output' ||
        event.type === 'team_agent_end' ||
        event.type === 'team_complete' ||
        event.type === 'plan_question'
      ) {
        // handled below
      } else {
        const stream = get()._stream;
        if (!stream || stream.sessionId !== sessionId) return;
      }

      // If current stream msg has tool_calls, new content starts a fresh msg
      const st0 = get()._stream;
      if (st0 && st0.toolCalls.length > 0 && (event.type === 'thinking' || event.type === 'token')) {
        get()._flushAndNewStreamMsg('');
      }

      switch (event.type) {
        case 'thinking': {
          set((s) => {
            const st = s._stream;
            if (!st) return s;
            const idx = st.thinking.findIndex((t) => t.step === event.step);
            const thinking = idx >= 0
              ? st.thinking.map((t, i) => i === idx ? { step: t.step, content: t.content + event.content } : t)
              : [...st.thinking, { step: event.step, content: event.content }];
            const messages = s.messages.map((m) => m.id === st.msgId ? { ...m, thinking_blocks: thinking } : m);
            return {
              _stream: { ...st, thinking },
              messages,
              sessionBuffers: { ...s.sessionBuffers, [sessionId]: { ...snapshotActive({ ...s, messages, _stream: { ...st, thinking } }), isStreaming: true } },
            };
          });
          break;
        }

        case 'token': {
          _pendingText += event.content;
          scheduleFlush(set, get);
          break;
        }

        case 'tool_start': {
          flushStreamBatch(set, get);
          const stream = get()._stream;
          if (stream && stream.text.trim()) {
            get()._flushAndNewStreamMsg(stream.text);
          }
          const tc: ToolCallRecord = { id: event.id, name: event.name, description: event.description, status: 'running', result: '' };
          set((s) => {
            const st = s._stream;
            if (!st) return s;
            const toolCalls = [...st.toolCalls, tc];
            const messages = patchMessageById(s.messages, st.msgId, {
              tool_calls: [...(s.messages.find((m) => m.id === st.msgId)?.tool_calls || []), tc],
            });
            return {
              _stream: { ...st, toolCalls },
              messages,
              sessionBuffers: { ...s.sessionBuffers, [sessionId]: { ...snapshotActive({ ...s, messages, _stream: { ...st, toolCalls } }), isStreaming: true } },
            };
          });
          break;
        }

        case 'tool_progress': {
          _pendingToolChunks[event.id] = (_pendingToolChunks[event.id] || '') + (event.chunk || '');
          scheduleFlush(set, get);
          break;
        }

        case 'tool_end': {
          flushStreamBatch(set, get);
          const updates = { status: event.status as ToolCallRecord['status'], result: event.result || '' };
          set((s) => {
            const st = s._stream;
            if (!st) return s;
            const updatedToolCalls = st.toolCalls.map((t) =>
              t.id === event.id ? { ...t, ...updates } : t
            );
            const messages = patchMessageById(s.messages, st.msgId, { tool_calls: updatedToolCalls });
            return {
              _stream: { ...st, toolCalls: updatedToolCalls },
              messages,
              sessionBuffers: { ...s.sessionBuffers, [sessionId]: { ...snapshotActive({ ...s, messages, _stream: { ...st, toolCalls: updatedToolCalls } }), isStreaming: true } },
            };
          });
          break;
        }

        case 'team_agent_start': {
          flushStreamBatch(set, get);
          set((s) => {
            const hostId = s._stream?.msgId || s._teamHostMsgId;
            const kind = panelKindFromAgentId(event.agent_id);
            const patch = withTeamAgentsOnHost(
              s.messages,
              hostId,
              (agents) =>
                upsertTeamAgent(agents, event.agent_id, { task: event.task }, 'start'),
              kind,
            );
            return {
              ...patch,
              sessionBuffers: {
                ...s.sessionBuffers,
                [sessionId]: {
                  ...snapshotActive({ ...s, ...patch }),
                  isStreaming: s.isStreaming,
                },
              },
            };
          });
          break;
        }
        case 'team_agent_output': {
          _pendingTeamChunks[event.agent_id] =
            (_pendingTeamChunks[event.agent_id] || '') + (event.content || '');
          scheduleFlush(set, get);
          break;
        }
        case 'team_agent_end': {
          flushStreamBatch(set, get);
          set((s) => {
            const hostId = s._stream?.msgId || s._teamHostMsgId;
            const kind = panelKindFromAgentId(event.agent_id);
            const patch = withTeamAgentsOnHost(
              s.messages,
              hostId,
              (agents) =>
                upsertTeamAgent(
                  agents,
                  event.agent_id,
                  {
                    status: event.success ? 'done' : 'error',
                    output: event.summary,
                  },
                  'end',
                ),
              kind,
            );
            return {
              ...patch,
              sessionBuffers: {
                ...s.sessionBuffers,
                [sessionId]: { ...snapshotActive({ ...s, ...patch }), isStreaming: s.isStreaming },
              },
            };
          });
          break;
        }
        case 'team_complete': { break; }

        case 'plan_question': {
          const plan_choice: PlanChoice = {
            phase: event.phase || '',
            question: event.question || '',
            recommended: event.recommended || '',
            options: event.options || [],
            remaining: event.remaining ?? 0,
            auto_notes: event.auto_notes || [],
            answered: false,
          };
          set((s) => {
            // Attach to last assistant message (current stream host or last asst)
            let idx = -1;
            if (s._stream) {
              idx = s.messages.findIndex((m) => m.id === s._stream!.msgId);
            }
            if (idx < 0) {
              for (let i = s.messages.length - 1; i >= 0; i--) {
                if (s.messages[i].role === 'assistant') {
                  idx = i;
                  break;
                }
              }
            }
            if (idx < 0) return s;
            const messages = s.messages.map((m, i) =>
              i === idx ? { ...m, plan_choice } : m,
            );
            return {
              messages,
              sessionBuffers: {
                ...s.sessionBuffers,
                [sessionId]: {
                  ...snapshotActive({ ...s, messages }),
                  isStreaming: s.isStreaming,
                },
              },
            };
          });
          break;
        }

        case 'permission_request': {
          const req = {
            id: event.id,
            tool_call_id: event.tool_call_id,
            command: event.command,
            reason: event.reason,
            timeout_secs: event.timeout_secs || 120,
            session_id: sessionId,
          };
          set((s) => ({
            pendingPermissions: [
              ...s.pendingPermissions.filter((p) => p.id !== req.id),
              req,
            ],
          }));
          // Auto-remove UI card after timeout (backend already denies)
          setTimeout(() => {
            get().removePermission(req.id);
          }, (req.timeout_secs + 2) * 1000);
          break;
        }

        case 'error': {
          drainPending();
          clearStreamIdleTimeout(sessionId);
          set((s) => {
            const messages = s.messages;
            const buf: SessionBuffer = {
              messages,
              isStreaming: false,
              streamError: event.content,
              _stream: null,
              _teamHostMsgId: s._teamHostMsgId,
              teamAgents: s.teamAgents,
            };
            return {
              streamError: event.content,
              isStreaming: false,
              _stream: null,
              sessionBuffers: { ...s.sessionBuffers, [sessionId]: buf },
            };
          });
          break;
        }
        case 'fact': {
          const fact: FactRecord = { id: event.id, subject: event.subject, predicate: event.predicate, object: event.object };
          set((s) => {
            const st = s._stream;
            if (!st) return s;
            const facts = [...st.facts, fact];
            const messages = s.messages.map((m) => m.id === st.msgId ? { ...m, fact_cards: [...(m.fact_cards || []), fact] } : m);
            return {
              _stream: { ...st, facts },
              messages,
              sessionBuffers: { ...s.sessionBuffers, [sessionId]: { ...snapshotActive({ ...s, messages, _stream: { ...st, facts } }), isStreaming: true } },
            };
          });
          break;
        }

        case 'complete': {
          flushStreamBatch(set, get);
          clearStreamIdleTimeout(sessionId);
          set((s) => {
            const st = s._stream;
            if (!st) {
              return {
                isStreaming: false,
                _stream: null,
                sessionBuffers: {
                  ...s.sessionBuffers,
                  [sessionId]: {
                    ...(s.sessionBuffers[sessionId] || emptyBuffer()),
                    isStreaming: false,
                    _stream: null,
                  },
                },
              };
            }
            const messages = s.messages.map((m) =>
              m.id === st.msgId
                ? {
                    ...m,
                    content: st.text,
                    thinking_blocks: st.thinking,
                    tool_calls: st.toolCalls,
                    fact_cards: st.facts,
                    stream_state: undefined,
                  }
                : m,
            );
            return {
              isStreaming: false,
              _stream: null,
              messages,
              sessionBuffers: {
                ...s.sessionBuffers,
                [sessionId]: {
                  messages,
                  isStreaming: false,
                  streamError: null,
                  _stream: null,
                  _teamHostMsgId: s._teamHostMsgId,
                  teamAgents: s.teamAgents,
                },
              },
            };
          });
          break;
        }
      }
    },

    endStream(error, sessionId) {
      const sid = sessionId || get().activeSessionId;
      if (sid) clearStreamIdleTimeout(sid);
      if (sid && sid === get().activeSessionId) drainPending();
      set((s) => {
        const target = sessionId || s.activeSessionId;
        if (!target) {
          return { isStreaming: false, _stream: null, streamError: error || null };
        }
        if (target === s.activeSessionId) {
          const st = s._stream;
          const messages = st
            ? s.messages.map((m) =>
                m.id === st.msgId
                  ? {
                      ...m,
                      content: st.text,
                      thinking_blocks: st.thinking,
                      tool_calls: st.toolCalls,
                      fact_cards: st.facts,
                      stream_state: undefined,
                    }
                  : m,
              )
            : s.messages;
          return {
            isStreaming: false,
            _stream: null,
            streamError: error || null,
            messages,
            sessionBuffers: {
              ...s.sessionBuffers,
              [target]: {
                messages,
                isStreaming: false,
                streamError: error || null,
                _stream: null,
                _teamHostMsgId: s._teamHostMsgId,
                teamAgents: s.teamAgents,
              },
            },
          };
        }
        // Background session abort/timeout
        const buf = s.sessionBuffers[target] || emptyBuffer();
        const st = buf._stream;
        const messages = st
          ? buf.messages.map((m) =>
              m.id === st.msgId
                ? { ...m, content: st.text, stream_state: undefined }
                : m,
            )
          : buf.messages;
        return {
          sessionBuffers: {
            ...s.sessionBuffers,
            [target]: {
              ...buf,
              messages,
              isStreaming: false,
              streamError: error || null,
              _stream: null,
            },
          },
        };
      });
    },

    async sendMessage(content, attachmentPaths) {
      const sid = get().activeSessionId;
      // Only this session must be free — other sessions may still be streaming.
      if (!sid || get().isSessionStreaming(sid)) return;
      const paths = (attachmentPaths || []).filter(Boolean);
      if (!content.trim() && paths.length === 0) return;
      const teams = get().teamsMode;
      // Display: keep user text; note attachments for bubble chips
      const display =
        content.trim() ||
        (paths.length
          ? `（附件 ${paths.length} 个）`
          : '');
      const userMsg: Message = {
        id: genId(),
        session_id: sid,
        role: 'user',
        content: display,
        created_at: Math.floor(Date.now() / 1000),
        attachments: paths.map((path, i) => ({
          id: `att-${i}-${Date.now()}`,
          path,
          name: path.split(/[/\\]/).pop() || path,
          size: 0,
          kind: 'binary' as const,
        })),
      };
      get().startStream(sid, userMsg);
      try {
        await tauriSendMessage(sid, content, teams, paths.length ? paths : undefined);
      } catch (e: any) {
        get().endStream(String(e), sid);
      }
    },

    async abortStream() {
      const sid = get().activeSessionId;
      if (!sid) return;
      try {
        await tauriAbort(sid);
      } catch {
        /* ignore */
      }
      get().endStream(undefined, sid);
    },
  };
});

/** Apply stream events to a non-visible session buffer (multi-session). */
function applyBackgroundEvent(
  set: (fn: (s: ChatStore) => Partial<ChatStore>) => void,
  get: () => ChatStore,
  sessionId: string,
  event: StreamEvent,
) {
  set((s) => {
    const buf = { ...(s.sessionBuffers[sessionId] || emptyBuffer()) };
    let st = buf._stream;

    const write = (next: SessionBuffer) => ({
      sessionBuffers: { ...s.sessionBuffers, [sessionId]: next },
    });

    switch (event.type) {
      case 'token': {
        if (!st) return s;
        const text = st.text + event.content;
        const messages = buf.messages.map((m) =>
          m.id === st!.msgId ? { ...m, content: text } : m,
        );
        return write({
          ...buf,
          messages,
          isStreaming: true,
          _stream: { ...st, text },
        });
      }
      case 'thinking': {
        if (!st) return s;
        const idx = st.thinking.findIndex((t) => t.step === event.step);
        const thinking =
          idx >= 0
            ? st.thinking.map((t, i) =>
                i === idx ? { step: t.step, content: t.content + event.content } : t,
              )
            : [...st.thinking, { step: event.step, content: event.content }];
        const messages = buf.messages.map((m) =>
          m.id === st!.msgId ? { ...m, thinking_blocks: thinking } : m,
        );
        return write({
          ...buf,
          messages,
          isStreaming: true,
          _stream: { ...st, thinking },
        });
      }
      case 'tool_start': {
        if (!st) return s;
        const tc: ToolCallRecord = {
          id: event.id,
          name: event.name,
          description: event.description,
          status: 'running',
          result: '',
        };
        const toolCalls = [...st.toolCalls, tc];
        const messages = buf.messages.map((m) =>
          m.id === st!.msgId
            ? { ...m, tool_calls: [...(m.tool_calls || []), tc] }
            : m,
        );
        return write({
          ...buf,
          messages,
          isStreaming: true,
          _stream: { ...st, toolCalls, text: st.text },
        });
      }
      case 'tool_end': {
        if (!st) return s;
        const toolCalls = st.toolCalls.map((t) =>
          t.id === event.id
            ? { ...t, status: event.status as any, result: event.result }
            : t,
        );
        const messages = buf.messages.map((m) =>
          m.id === st!.msgId ? { ...m, tool_calls: toolCalls } : m,
        );
        return write({
          ...buf,
          messages,
          isStreaming: true,
          _stream: { ...st, toolCalls },
        });
      }
      case 'team_agent_start':
      case 'team_agent_output':
      case 'team_agent_end': {
        const kind = panelKindFromAgentId(
          'agent_id' in event ? event.agent_id : '',
        );
        let agents = buf.messages.find((m) => m.id === (st?.msgId || buf._teamHostMsgId))
          ?.team_agents || buf.teamAgents;
        if (event.type === 'team_agent_start') {
          agents = upsertTeamAgent(agents, event.agent_id, { task: event.task }, 'start');
        } else if (event.type === 'team_agent_output') {
          agents = upsertTeamAgent(agents, event.agent_id, { output: event.content }, 'output');
        } else {
          agents = upsertTeamAgent(
            agents,
            event.agent_id,
            {
              status: event.success ? 'done' : 'error',
              output: event.summary,
            },
            'end',
          );
        }
        const hostId = st?.msgId || buf._teamHostMsgId;
        const messages = buf.messages.map((m) =>
          m.id === hostId ? { ...m, team_agents: agents, agent_panel_kind: kind } : m,
        );
        return write({
          ...buf,
          messages,
          teamAgents: agents,
        });
      }
      case 'plan_question': {
        const plan_choice: PlanChoice = {
          phase: event.phase || '',
          question: event.question || '',
          recommended: event.recommended || '',
          options: event.options || [],
          remaining: event.remaining ?? 0,
          auto_notes: event.auto_notes || [],
          answered: false,
        };
        let idx = -1;
        if (st) idx = buf.messages.findIndex((m) => m.id === st!.msgId);
        if (idx < 0) {
          for (let i = buf.messages.length - 1; i >= 0; i--) {
            if (buf.messages[i].role === 'assistant') {
              idx = i;
              break;
            }
          }
        }
        if (idx < 0) return s;
        const messages = buf.messages.map((m, i) =>
          i === idx ? { ...m, plan_choice } : m,
        );
        return write({ ...buf, messages });
      }
      case 'error': {
        return write({
          ...buf,
          isStreaming: false,
          streamError: event.content,
          _stream: null,
        });
      }
      case 'complete': {
        if (st) {
          const messages = buf.messages.map((m) =>
            m.id === st!.msgId
              ? {
                  ...m,
                  content: st!.text,
                  thinking_blocks: st!.thinking,
                  tool_calls: st!.toolCalls,
                  stream_state: undefined,
                }
              : m,
          );
          return write({
            ...buf,
            messages,
            isStreaming: false,
            streamError: null,
            _stream: null,
          });
        }
        return write({ ...buf, isStreaming: false, _stream: null });
      }
      default:
        return s;
    }
  });
}
