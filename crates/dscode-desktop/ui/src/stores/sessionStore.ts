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
      set((s) => ({
        sessions: s.sessions.map((ss) =>
          ss.id === sessionId ? { ...ss, workspace } : ss
        ),
      }));
    } catch (err: unknown) {
      set({ error: String(err) });
    }
  },

  deleteSession: async (id) => {
    try {
      await tauri.deleteSession(id);
      set((s) => ({ sessions: s.sessions.filter((x) => x.id !== id) }));
    } catch { }
  },
}));
