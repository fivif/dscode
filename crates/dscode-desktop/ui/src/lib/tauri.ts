import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { StreamEvent, Session, AppConfig } from './types';

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
