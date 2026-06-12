import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { StreamEvent, Session, AppConfig, WikiNode, WikiGraph } from './types';

// ── Chat ──
export async function sendMessage(sessionId: string, message: string): Promise<void> {
  await invoke('send_message', { sessionId, message });
}

export async function abort(): Promise<void> {
  await invoke('abort');
}

// ── Sessions ──
export async function listSessions(): Promise<Session[]> {
  return invoke('list_sessions');
}

export async function getSession(id: string): Promise<Session> {
  return invoke('get_session', { id });
}

export async function createSession(title: string, workspace: string): Promise<Session> {
  return invoke('create_session', { title, workspace });
}

export async function getLastSession(): Promise<Session | null> {
  return invoke('get_last_session');
}

export async function updateSessionWorkspace(sessionId: string, workspace: string): Promise<void> {
  return invoke('update_session_workspace', { sessionId, workspace });
}

export async function deleteSession(id: string): Promise<void> {
  await invoke('delete_session', { id });
}

// ── Config ──
export async function getConfig(): Promise<AppConfig> {
  return invoke('get_config');
}

export async function updateConfig(config: AppConfig): Promise<void> {
  await invoke('update_config', { config });
}

// ── Wiki ──
export async function wikiSearch(query: string): Promise<WikiNode[]> {
  return invoke('wiki_search', { query });
}

export async function wikiGraph(): Promise<WikiGraph> {
  return invoke('wiki_graph');
}

export async function fetchModels(providerKey: string): Promise<string[]> {
  return invoke('fetch_models', { providerKey });
}

// ── Events ──
export function onStreamEvent(
  sessionId: string,
  callback: (event: StreamEvent) => void
): () => void {
  const unlisten = listen<StreamEvent & { session_id?: string }>('stream-event', (event) => {
    if (!event.payload.session_id || event.payload.session_id !== sessionId) return;
    callback(event.payload);
  });
  return () => {
    unlisten.then((fn) => fn());
  };
}

export async function listTools(): Promise<{ name: string; description: string }[]> {
  return invoke('list_tools');
}

export async function listSkills(): Promise<{ name: string; description: string; triggers: string[]; hidden: boolean; body: string }[]> {
  return invoke('list_skills');
}

export async function saveSkill(name: string, description: string, body: string): Promise<void> {
  return invoke('save_skill', { name, description, body });
}

export async function deleteSkill(name: string): Promise<void> {
  return invoke('delete_skill', { name });
}
