// ── Domain types for DS Code desktop app ──

// ── Session ──
export interface Session {
  id: string;
  title: string;
  workspace: string;
  created_at: number;
  updated_at: number;
  messages: Message[];
}

// ── Message ──
export interface Message {
  id: string;
  session_id?: string;
  role: 'user' | 'assistant';
  content: string;
  created_at: number; // Unix seconds
  // Transient UI state — not stored on backend
  tool_calls?: ToolCallRecord[];
  thinking_blocks?: ThinkingBlock[];
  /** Only valid while streaming; null when complete */
  stream_state?: StreamState | null;
}

export interface StreamState {
  text: string;
  thinking: ThinkingBlock[];
  tool_calls: ToolCallRecord[];
}

export interface ThinkingBlock {
  step: number;
  content: string;
}

export interface ToolCallRecord {
  id: string;
  name: string;
  description: string;
  status: 'running' | 'success' | 'error';
  result: string;
}

// ── Stream events (emitted by Tauri backend) ──
export type StreamEvent =
  | { type: 'thinking'; content: string; step: number }
  | { type: 'token'; content: string }
  | { type: 'tool_start'; id: string; name: string; description: string }
  | { type: 'tool_progress'; id: string; chunk: string }
  | { type: 'tool_end'; id: string; status: 'success' | 'error'; result: string }
  | { type: 'fact'; id: string; subject: string; predicate: string; object: string }
  | { type: 'error'; content: string }
  | { type: 'complete'; usage?: { input_tokens: number; output_tokens: number } };

// ── Config (matches Rust Config struct) ──
export interface RustProviderConfig {
  api_key: string;
  base_url: string;
  enabled: boolean;
}

export interface Config {
  default_model: string;
  router_model: string;
  providers: {
    deepseek: RustProviderConfig;
    openai: RustProviderConfig;
    anthropic: RustProviderConfig;
    ollama: RustProviderConfig;
  };
  session: { retention_days: number };
  safety: { allow_write_outside_project: boolean; blocked_commands: string[]; tool_timeout_secs: number };
  generation: { reasoning_effort: string; max_tokens: number; temperature: number };
}

// ── App-level config for settings UI ──
export interface ProviderConfig {
  api_key: string;
  base_url: string;
  enabled: boolean;
  model: string;
}

export interface AppConfig {
  default_model: string;
  active_provider: string;
  providers: Record<string, ProviderConfig>;
  reasoning_effort: string;
  max_tokens: number;
  temperature: number;
  retention_days: number;
  context_window_tokens: number;
  context_compress_threshold: number;
}

// ── Wiki types ──
export interface WikiNode {
  id: string;
  title: string;
  content: string;
  links: string[];
}

export interface WikiGraph {
  nodes: Array<{ id: string; title: string; content: string; node_type: string; tags: string[]; session_id?: string; links?: string[] }>;
  edges: Array<{ source: string; target: string; weight?: number }>;
}

// ── Model definitions ──
export interface ModelDef {
  provider: string;
  id: string;
  display: string;
}

export const KNOWN_MODELS: ModelDef[] = [
  { provider: 'deepseek', id: 'deepseek-v4-pro', display: 'DeepSeek V4 Pro' },
  { provider: 'deepseek', id: 'deepseek-v4-flash', display: 'DeepSeek V4 Flash' },
  { provider: 'deepseek', id: 'deepseek-chat', display: 'DeepSeek Chat' },
  { provider: 'deepseek', id: 'deepseek-reasoner', display: 'DeepSeek Reasoner' },
  { provider: 'anthropic', id: 'claude-sonnet-4-20250514', display: 'Claude Sonnet 4' },
  { provider: 'anthropic', id: 'claude-haiku-4-5', display: 'Claude Haiku 4.5' },
];

// ── Helper for grouping sessions ──
export type SessionGroup = 'Today' | 'Yesterday' | 'This Week' | 'This Month' | 'Older';

export function groupSessions(sessions: Session[]): Record<SessionGroup, Session[]> {
  const now = new Date();
  const startOfDay = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const startOfYesterday = new Date(startOfDay.getTime() - 86400000);
  const dayOfWeek = now.getDay();
  const startOfWeek = new Date(startOfDay.getTime() - (dayOfWeek === 0 ? 6 : dayOfWeek - 1) * 86400000);
  const startOfMonth = new Date(now.getFullYear(), now.getMonth(), 1);

  const groups: Record<SessionGroup, Session[]> = {
    Today: [],
    Yesterday: [],
    'This Week': [],
    'This Month': [],
    Older: [],
  };

  for (const s of sessions) {
    // updated_at is Unix seconds, convert to ms for JS Date
    const d = new Date(s.updated_at * 1000);
    if (d >= startOfDay) groups['Today'].push(s);
    else if (d >= startOfYesterday) groups['Yesterday'].push(s);
    else if (d >= startOfWeek) groups['This Week'].push(s);
    else if (d >= startOfMonth) groups['This Month'].push(s);
    else groups['Older'].push(s);
  }

  return groups;
}

// Runtime helper: generate unique ids
let _idCounter = 0;
export function genId(): string {
  _idCounter++;
  return `${Date.now()}-${_idCounter}`;
}
