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

/** Pending or sent file attachment (local path after dialog / stage). */
export interface FileAttachment {
  id: string;
  /** Absolute path on disk (for send_message) */
  path: string;
  /** Display name */
  name: string;
  size: number;
  /** MIME if known */
  mime?: string;
  /** image | text | binary */
  kind: 'image' | 'text' | 'binary';
  /** Optional preview for images (object URL or convertFileSrc) */
  previewUrl?: string;
}

// ── Message ──
export interface Message {
  id: string;
  session_id?: string;
  role: 'user' | 'assistant' | 'tool' | 'system' | 'fact';
  content: string;
  created_at: number; // Unix seconds
  /** User-visible attachment list (UI); full paths also embedded in content for agent */
  attachments?: FileAttachment[];
  // Backend-native fields (DB serialization)
  tool_call_id?: string;
  reasoning_content?: string;
  name?: string;
  /** Fact fields (fact role) */
  subject?: string;
  predicate?: string;
  object?: string;
  // Transient UI state — not stored on backend
  tool_calls?: ToolCallRecord[];
  thinking_blocks?: ThinkingBlock[];
  fact_cards?: FactRecord[];
  /** Sub-agents attached to this assistant turn (Teams or /auto) */
  team_agents?: TeamAgent[];
  /** How to label the agent panel — /auto, Teams, or combined */
  agent_panel_kind?: 'teams' | 'auto' | 'auto_teams';
  /** /plan interview choices (buttons + custom input) */
  plan_choice?: PlanChoice;
  /** Only valid while streaming; null when complete */
  stream_state?: StreamState | null;
}

/** Structured /plan question for interactive choice UI */
export interface PlanChoice {
  phase: string;
  question: string;
  recommended: string;
  options: string[];
  remaining: number;
  auto_notes: string[];
  /** Disabled after user picks an answer */
  answered?: boolean;
  selected?: string;
}

export interface FactRecord {
  id: string;
  subject: string;
  predicate: string;
  object: string;
}

export interface StreamState {
  text: string;
  thinking: ThinkingBlock[];
  tool_calls: ToolCallRecord[];
  fact_cards: FactRecord[];
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

// ── Team agents ──
export interface TeamAgent {
  id: string;
  task: string;
  status: 'running' | 'done' | 'error';
  output: string;
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
  | { type: 'complete'; usage?: { input_tokens: number; output_tokens: number } }
  | { type: 'team_agent_start'; agent_id: string; task: string }
  | { type: 'team_agent_output'; agent_id: string; content: string }
  | { type: 'team_agent_end'; agent_id: string; success: boolean; summary: string }
  | { type: 'team_complete'; completed: number; failed: number }
  | {
      type: 'plan_question';
      phase: string;
      question: string;
      recommended: string;
      options: string[];
      remaining: number;
      auto_notes: string[];
    };

// ── Config (matches Rust Config struct) ──
export interface RustProviderConfig {
  api_key: string;
  base_url: string;
  enabled: boolean;
  use_proxy?: boolean;
}

export interface ProxyConfig {
  /** e.g. http://127.0.0.1:7890 — empty = not configured */
  url: string;
  /** Force proxy app-wide when url is valid */
  global: boolean;
}

export interface Config {
  default_model: string;
  router_model: string;
  active_provider?: string;
  providers: {
    deepseek: RustProviderConfig;
    openai: RustProviderConfig;
    anthropic: RustProviderConfig;
    ollama: RustProviderConfig;
  };
  session: { retention_days: number };
  safety: { allow_write_outside_project: boolean; blocked_commands: string[]; tool_timeout_secs: number };
  generation: { reasoning_effort: string; max_tokens: number; temperature: number; proxy_url?: string };
  context?: { window_tokens: number; compress_threshold: number };
  extensions?: {
    mcp_servers?: unknown[];
    skills_dirs?: string[];
    mcp_use_proxy?: boolean;
    skills_use_proxy?: boolean;
  };
  proxy?: ProxyConfig;
  agent?: {
    global_prompt?: string;
    replace_system_prompt?: boolean;
  };
}

// ── App-level config for settings UI ──
export interface ProviderConfig {
  api_key: string;
  base_url: string;
  enabled: boolean;
  model: string;
  /** Use proxy for this channel (requires valid proxy URL; forced when global) */
  use_proxy: boolean;
  /** Last successful /models scan (persisted). Empty = not scanned yet. */
  model_list?: string[];
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
  proxy: ProxyConfig;
  mcp_use_proxy: boolean;
  skills_use_proxy: boolean;
}

/** Valid non-empty proxy URL with supported scheme */
export function isProxyConfigured(url: string | undefined | null): boolean {
  const u = (url || '').trim().toLowerCase();
  if (!u) return false;
  return (
    u.startsWith('http://') ||
    u.startsWith('https://') ||
    u.startsWith('socks5://') ||
    u.startsWith('socks5h://') ||
    u.startsWith('socks4://')
  );
}

// ── Model definitions ──
export interface ModelDef {
  provider: string;
  id: string;
  display: string;
}

/** Display labels only — never injected into pickers as selectable ghost models. */
export const KNOWN_MODELS: ModelDef[] = [
  { provider: 'deepseek', id: 'deepseek-v4-pro', display: 'DeepSeek V4 Pro' },
  { provider: 'deepseek', id: 'deepseek-v4-flash', display: 'DeepSeek V4 Flash' },
  { provider: 'deepseek', id: 'deepseek-chat', display: 'DeepSeek Chat' },
  { provider: 'deepseek', id: 'deepseek-reasoner', display: 'DeepSeek Reasoner' },
  { provider: 'openai', id: 'gpt-4o', display: 'GPT-4o' },
  { provider: 'openai', id: 'gpt-4o-mini', display: 'GPT-4o Mini' },
  { provider: 'openai', id: 'gpt-4.1', display: 'GPT-4.1' },
  { provider: 'openai', id: 'o3-mini', display: 'o3-mini' },
  { provider: 'anthropic', id: 'claude-sonnet-4-20250514', display: 'Claude Sonnet 4' },
  { provider: 'anthropic', id: 'claude-opus-4-20250514', display: 'Claude Opus 4' },
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
