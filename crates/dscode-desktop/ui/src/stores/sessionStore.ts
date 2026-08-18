import { create } from 'zustand';
import * as tauri from '@/lib/tauri';
import type { Session } from '@/lib/types';

export interface SessionStore {
  sessions: Session[];
  loading: boolean;
  error: string | null;
  loadSessions: () => Promise<void>;
  createSession: (title: string, workspace: string) => Promise<Session | null>;
  deleteSession: (id: string) => Promise<void>;
  getLastSession: () => Promise<Session | null>;
  updateWorkspace: (sessionId: string, workspace: string) => Promise<void>;
  updateTitle: (sessionId: string, title: string) => Promise<void>;
  updateModel: (sessionId: string, model: string) => Promise<void>;
  applyTitleLocal: (sessionId: string, title: string) => void;
  /** Bump updated_at + re-sort so the session moves into "今天" after activity. */
  touchSession: (sessionId: string, at?: number) => void;
}

function sortByUpdatedDesc(sessions: Session[]): Session[] {
  return [...sessions].sort((a, b) => (b.updated_at || 0) - (a.updated_at || 0));
}

export const useSessionStore = create<SessionStore>((set, get) => ({
  sessions: [],
  loading: false,
  error: null,

  loadSessions: async () => {
    set({ loading: true, error: null });
    try {
      const sessions = await tauri.listSessions();
      set({ sessions, loading: false });
    } catch (err: unknown) {
      set({ error: String(err), loading: false });
    }
  },

  createSession: async (title, workspace) => {
    try {
      const session = await tauri.createSession(title, workspace);
      set((s) => ({ sessions: [session, ...s.sessions] }));
      return session;
    } catch (err: unknown) {
      set({ error: String(err) });
      return null;
    }
  },

  getLastSession: async () => {
    try { return await tauri.getLastSession(); } catch { return null; }
  },

  updateWorkspace: async (sessionId, workspace) => {
    try {
      await tauri.updateSessionWorkspace(sessionId, workspace);
      const now = Math.floor(Date.now() / 1000);
      set((s) => ({
        sessions: sortByUpdatedDesc(
          s.sessions.map((ss) =>
            ss.id === sessionId ? { ...ss, workspace, updated_at: now } : ss,
          ),
        ),
      }));
    } catch (err: unknown) {
      set({ error: String(err) });
    }
  },

  updateTitle: async (sessionId, title) => {
    const trimmed = title.trim();
    if (!trimmed) return;
    try {
      await tauri.updateSessionTitle(sessionId, trimmed);
      get().applyTitleLocal(sessionId, trimmed);
    } catch (err: unknown) {
      set({ error: String(err) });
    }
  },

  updateModel: async (sessionId, model) => {
    const mid = (model || '').trim();
    if (!mid) return;
    try {
      await tauri.updateSessionModel(sessionId, mid);
      const now = Math.floor(Date.now() / 1000);
      set((s) => ({
        sessions: sortByUpdatedDesc(
          s.sessions.map((ss) =>
            ss.id === sessionId ? { ...ss, model: mid, updated_at: now } : ss,
          ),
        ),
      }));
    } catch (err: unknown) {
      set({ error: String(err) });
    }
  },

  applyTitleLocal: (sessionId, title) => {
    const now = Math.floor(Date.now() / 1000);
    set((s) => ({
      sessions: sortByUpdatedDesc(
        s.sessions.map((ss) =>
          ss.id === sessionId ? { ...ss, title, updated_at: now } : ss,
        ),
      ),
    }));
  },

  touchSession: (sessionId, at) => {
    const now = at ?? Math.floor(Date.now() / 1000);
    set((s) => {
      const idx = s.sessions.findIndex((ss) => ss.id === sessionId);
      if (idx < 0) return s;
      // Already newest with same/newer timestamp — skip churn
      if (idx === 0 && (s.sessions[0].updated_at || 0) >= now) return s;
      return {
        sessions: sortByUpdatedDesc(
          s.sessions.map((ss) =>
            ss.id === sessionId ? { ...ss, updated_at: now } : ss,
          ),
        ),
      };
    });
  },

  deleteSession: async (id) => {
    try {
      await tauri.deleteSession(id);
      set((s) => ({ sessions: s.sessions.filter((x) => x.id !== id) }));
    } catch { }
  },
}));
