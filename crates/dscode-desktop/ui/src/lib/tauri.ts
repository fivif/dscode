import { invoke as tauriInvoke } from '@tauri-apps/api/core';
import { listen as tauriListen } from '@tauri-apps/api/event';
import type { StreamEvent, Session, AppConfig } from './types';

const IS_TAURI = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

/** Unified invoke: Tauri IPC in the desktop shell, HTTP `/api/invoke` in the browser. */
export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (IS_TAURI) {
    return tauriInvoke<T>(command, args);
  }
  const res = await fetch('/api/invoke', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ command, args: args ?? {} }),
  });
  if (!res.ok) {
    const text = await res.text();
    throw new Error(`invoke(${command}) failed: ${text}`);
  }
  return (await res.json()) as T;
}

// ── SSE event bridge (browser only) ──
let _es: EventSource | null = null;
const sseListeners = new Map<string, Set<(payload: unknown) => void>>();

function emitLocal(event: string, payload: unknown) {
  const set = sseListeners.get(event);
  if (!set) return;
  for (const cb of set) cb({ event, payload });
}

function ensureEventSource(): EventSource {
  if (_es) return _es;
  _es = new EventSource('/api/events');
  _es.addEventListener('server-event', (e) => {
    let data: unknown;
    try {
      data = JSON.parse((e as MessageEvent).data);
    } catch {
      return;
    }
    const d = data as Record<string, unknown>;
    if (d.Stream) emitLocal('stream-event', d.Stream);
    else if (d.SessionTitleUpdated) emitLocal('session-title-updated', d.SessionTitleUpdated);
    else if (d.TaskNotification) emitLocal('task-notification', d.TaskNotification);
  });
  return _es;
}

/** Unified listen: Tauri event in desktop, SSE in the browser. */
export async function listen<T>(
  event: string,
  callback: (e: { event: string; payload: T }) => void,
): Promise<() => void> {
  if (IS_TAURI) {
    return tauriListen(event, callback as never);
  }
  ensureEventSource();
  let set = sseListeners.get(event);
  if (!set) {
    set = new Set();
    sseListeners.set(event, set);
  }
  const cb = callback as unknown as (payload: unknown) => void;
  set.add(cb);
  return () => {
    set?.delete(cb);
  };
}

// ── Chat ──
export async function sendMessage(
  sessionId: string,
  message: string,
  teamsMode: boolean,
  attachments?: string[],
): Promise<void> {
  await invoke('send_message', {
    sessionId,
    message,
    teamsMode,
    attachments: attachments && attachments.length ? attachments : null,
  });
}

/** Stage bytes from paste/drag into session uploads; returns absolute path. */
export async function stageUpload(
  sessionId: string,
  name: string,
  base64Data: string,
): Promise<string> {
  return invoke('stage_upload', { sessionId, name, base64Data });
}

export async function approvePermission(requestId: string): Promise<void> {
  await invoke('approve_permission', { requestId });
}

export async function denyPermission(requestId: string): Promise<void> {
  await invoke('deny_permission', { requestId });
}

export async function abort(sessionId: string): Promise<void> {
  await invoke('abort', { sessionId });
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

export async function updateSessionTitle(sessionId: string, title: string): Promise<void> {
  return invoke('update_session_title', { sessionId, title });
}

/** Bind a model to a session (does not change global default_model). */
export async function updateSessionModel(sessionId: string, model: string): Promise<void> {
  return invoke('update_session_model', { sessionId, model });
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

export interface GlobalPromptInfo {
  global_prompt: string;
  replace_system_prompt: boolean;
  default_prompt: string;
  effective_prompt: string;
}

export async function getGlobalPrompt(): Promise<GlobalPromptInfo> {
  return invoke('get_global_prompt');
}

export async function setGlobalPrompt(
  globalPrompt: string,
  replaceSystemPrompt: boolean,
): Promise<GlobalPromptInfo> {
  return invoke('set_global_prompt', {
    globalPrompt,
    replaceSystemPrompt,
  });
}

export async function fetchModels(providerKey: string): Promise<string[]> {
  return invoke('fetch_models', { providerKey });
}

// ── Events ──
/** Listen to all session streams (multi-session concurrent runs). */
export function onAnyStreamEvent(
  callback: (sessionId: string, event: StreamEvent) => void
): () => void {
  const unlisten = listen<any>('stream-event', (event) => {
    const payload = event.payload;
    const sid = payload?.session_id;
    const ev = payload?.event;
    if (!sid || !ev) return;
    callback(sid as string, ev as StreamEvent);
  });
  return () => {
    unlisten.then((fn) => fn());
  };
}

/** @deprecated prefer onAnyStreamEvent for multi-session */
export function onStreamEvent(
  sessionId: string,
  callback: (event: StreamEvent) => void
): () => void {
  return onAnyStreamEvent((sid, ev) => {
    if (sid === sessionId) callback(ev);
  });
}

export async function listTools(): Promise<{ name: string; description: string }[]> {
  return invoke('list_tools');
}

export interface McpServerInfo {
  name: string;
  command: string;
  args: string[];
  connected: boolean;
  tool_count: number;
}

export interface McpReloadResult {
  registered: number;
  status: string[];
}

export async function listMcpServers(): Promise<McpServerInfo[]> {
  return invoke('list_mcp_servers');
}

export async function addMcpServer(
  name: string,
  command: string,
  args: string,
): Promise<McpReloadResult> {
  return invoke('add_mcp_server', { name, command, args });
}

export async function updateMcpServer(
  originalName: string,
  name: string,
  command: string,
  args: string,
): Promise<McpReloadResult> {
  return invoke('update_mcp_server', { originalName, name, command, args });
}

export async function removeMcpServer(name: string): Promise<McpReloadResult> {
  return invoke('remove_mcp_server', { name });
}

export async function reloadMcp(): Promise<McpReloadResult> {
  return invoke('reload_mcp');
}

export interface SkillResourceInfo {
  relative_path: string;
  absolute_path: string;
  kind: string;
  size_bytes: number;
  executable: boolean;
}

export interface SkillInfo {
  name: string;
  description: string;
  triggers: string[];
  hidden: boolean;
  body: string;
  root: string;
  resources: SkillResourceInfo[];
}

export async function listSkills(): Promise<SkillInfo[]> {
  return invoke('list_skills');
}

export async function saveSkill(
  name: string,
  description: string,
  body: string,
  triggers?: string,
  files?: { path: string; content: string }[],
): Promise<string> {
  return invoke('save_skill', {
    name,
    description,
    body,
    triggers: triggers || null,
    files: files || null,
  });
}

export async function writeSkillFile(
  skillName: string,
  relativePath: string,
  content: string,
): Promise<string> {
  return invoke('write_skill_file', { skillName, relativePath, content });
}

export async function skillsDir(): Promise<string> {
  return invoke('skills_dir');
}

/** Install from skills.sh / GitHub: owner/repo or owner/repo/skill */
export async function installSkillPackage(spec: string): Promise<string> {
  // Tauri arg name must match Rust `package`; avoid ES reserved binding name.
  return invoke('install_skill_package', { package: spec });
}

export async function deleteSkill(name: string, root?: string): Promise<string> {
  return invoke('delete_skill', { name, root: root || null });
}
