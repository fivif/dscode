import { invoke as tauriInvoke, convertFileSrc } from '@tauri-apps/api/core';
import { listen as tauriListen } from '@tauri-apps/api/event';
import type { StreamEvent, Session, AppConfig } from './types';

/**
 * Are we inside the Tauri shell (vs. the browser/web shell serving the same UI)?
 *
 * `__TAURI_INTERNALS__` is Tauri 2's own IPC bridge and is injected into every
 * webview it owns; `window.__TAURI__` only exists when `app.withGlobalTauri` is
 * enabled, which `tauri.conf.json` does not set — probing it here would always
 * be false and silently send every call down the HTTP branch. Using the same
 * constant as `invoke`/`listen` also keeps the two transports from disagreeing
 * within one runtime.
 */
const IS_TAURI = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

/** Unified invoke: Tauri IPC in the desktop shell, HTTP `/api/invoke` in the browser. */
export async function invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  if (IS_TAURI) {
    return tauriInvoke<T>(command, args);
  }
  // `/api/invoke` sits behind the same bearer middleware as everything else
  // under `/api`, so without this header every command in the web shell — send,
  // list sessions, read config — comes back 401. `fetch` can set headers, so it
  // uses the header form rather than the query parameter (which `EventSource`
  // and `<img>` are stuck with).
  const token = webToken();
  const res = await fetch('/api/invoke', {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
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

/**
 * The `?token=` from the page URL, or `null` outside the browser.
 *
 * The web shell prints its per-startup token and the user opens the app with
 * `?token=<t>`. Every `/api/*` route is behind the bearer middleware
 * (`dscode-web/src/auth.rs`), and neither an `<img>` nor an `EventSource` can
 * set an `Authorization` header — the query parameter is the only channel both
 * of them have. Exporting it keeps the two call sites from drifting apart.
 */
export function webToken(): string | null {
  if (typeof window === 'undefined') return null;
  return new URLSearchParams(window.location.search).get('token');
}

function ensureEventSource(): EventSource {
  if (_es) return _es;
  // The URL is fixed at construction — `EventSource` has no way to change it —
  // so the token has to be read here rather than lazily like `imageSrc` does.
  // Without it this connection 401s, and `EventSource` fails **silently**: the
  // page still loads, but no stream events, title updates or task
  // notifications ever arrive, which reads as "the app is dead".
  const token = webToken();
  _es = new EventSource(
    token ? `/api/events?token=${encodeURIComponent(token)}` : '/api/events',
  );
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

// ── Assets ──

/**
 * Turn an absolute path on disk into a URL the webview can load in an `<img>`.
 *
 * Only used for images this app generated itself (`do_image_generate` writes
 * them under `~/.dscode/images/` and reports them as `dscode-image:<path>`) —
 * never for a URL that came from the model or a fetched page, which must stay
 * behind the click-to-load gate in `StreamingRenderer`.
 *
 * - Desktop: the Tauri asset protocol. `convertFileSrc` yields
 *   `http://asset.localhost/<encoded path>` on Windows, `asset://localhost/...`
 *   elsewhere; the protocol is off by default and scoped to the images
 *   directory in `tauri.conf.json` (`app.security.assetProtocol`). A path
 *   outside that scope is refused by the webview, not by this function.
 * - Web shell: `/api/image`, implemented in `dscode-web`. That router sits
 *   behind the bearer-token middleware (`dscode-web/src/auth.rs`), and an
 *   `<img>` cannot set an `Authorization` header, so forward the `?token=` the
 *   page was opened with — the same query transport the server already accepts
 *   for `EventSource`. Without it every image would 401 and render as broken.
 *
 * Returns `null` when there is no usable path (callers show a failure state).
 */
export function imageSrc(path: string): string | null {
  if (!path) return null;

  if (IS_TAURI) {
    try {
      return convertFileSrc(path);
    } catch {
      // Older/partial `__TAURI_INTERNALS__` without `convertFileSrc`: degrade to
      // a visible failure instead of throwing out of the whole markdown render.
      return null;
    }
  }

  const query = `path=${encodeURIComponent(path)}`;
  const token = webToken();
  return token ? `/api/image?${query}&token=${encodeURIComponent(token)}` : `/api/image?${query}`;
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
  let disposed = false;
  let stop: (() => void) | null = null;
  void listen<any>('stream-event', (event) => {
    if (disposed) return;
    const payload = event.payload;
    const sid = payload?.session_id;
    const ev = payload?.event;
    if (!sid || !ev) return;
    callback(sid as string, ev as StreamEvent);
  }).then((fn) => {
    // Unmounting before `listen` resolves used to leave the listener registered
    // forever (StrictMode's double-invoke then stacked two live listeners and
    // every token was applied twice). Honour the unsubscribe immediately.
    if (disposed) fn();
    else stop = fn;
  });
  return () => {
    disposed = true;
    stop?.();
    stop = null;
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
