/**
 * Shared model catalog + provider inference — keeps Settings default model,
 * per-channel model dropdowns, and InputBox picker in sync.
 *
 * Rules:
 * - Global pickers → **enabled channels only**
 * - After API fetch (or persisted model_list) → show **only** real scanned ids
 * - Never inject hardcoded KNOWN_MODELS into pickers (labels only)
 * - Model → channel binding prefers the channel it was scanned under (not name prefix)
 */
import type { AppConfig, ModelDef } from './types';
import { KNOWN_MODELS } from './types';

export type ModelOption = { id: string; label: string; provider: string };

/** Infer provider key from a model id (routing fallback when channel unknown). */
export function inferProvider(modelId: string): string {
  const m = (modelId || '').toLowerCase();
  if (m.startsWith('deepseek')) return 'deepseek';
  if (m.startsWith('claude') || m.startsWith('anthropic')) return 'anthropic';
  if (
    m.startsWith('gpt-') ||
    m.startsWith('o1') ||
    m.startsWith('o3') ||
    m.startsWith('o4') ||
    m.startsWith('chatgpt') ||
    m.startsWith('openai')
  ) {
    return 'openai';
  }
  if (m.startsWith('ollama/') || m.startsWith('llama') || m.startsWith('qwen')) return 'ollama';
  return '';
}

export function knownModelsFor(provider: string): ModelOption[] {
  return KNOWN_MODELS.filter((m) => m.provider === provider).map((m) => ({
    id: m.id,
    label: m.display,
    provider: m.provider,
  }));
}

export function enabledProviderKeys(config: AppConfig): string[] {
  return Object.entries(config.providers || {})
    .filter(([, p]) => p?.enabled)
    .map(([k]) => k);
}

function labelFor(id: string, known: ModelOption[]): string {
  return known.find((x) => x.id === id)?.label || id;
}

/** Real scanned ids for a channel: in-memory fetch wins, else persisted model_list. */
export function scannedModelsFor(
  provider: string,
  config: AppConfig,
  fetchedByProvider: Record<string, string[]> = {},
): string[] {
  const live = fetchedByProvider[provider];
  if (live && live.length > 0) {
    return live.map((s) => s.trim()).filter(Boolean);
  }
  const saved = config.providers[provider]?.model_list || [];
  return saved.map((s) => s.trim()).filter(Boolean);
}

/**
 * Models for one channel (Settings channel tab + global picker slice).
 * - With scan results: **only** those ids (no hardcoded catalog).
 * - Without scan: only the currently saved channel model (if any), so we never
 *   show ghost gpt-4o / o3-mini entries for custom OpenAI-compatible endpoints.
 */
export function modelOptionsForProvider(
  provider: string,
  fetched: string[] = [],
  savedModel?: string,
  persistedList?: string[],
): ModelOption[] {
  const known = knownModelsFor(provider);
  const out: ModelOption[] = [];
  const seen = new Set<string>();

  const push = (id: string) => {
    const tid = (id || '').trim();
    if (!tid || seen.has(tid)) return;
    seen.add(tid);
    out.push({
      id: tid,
      label: labelFor(tid, known),
      provider,
    });
  };

  const scanned =
    fetched.length > 0
      ? fetched
      : (persistedList || []).filter(Boolean);

  if (scanned.length > 0) {
    for (const id of scanned) push(id);
    return out;
  }

  // Not scanned yet: keep current selection only (no phantom catalog).
  if (savedModel?.trim()) push(savedModel.trim());
  return out;
}

/**
 * Global selectable models: enabled providers only, real lists only.
 */
export function availableModels(
  config: AppConfig,
  fetchedByProvider: Record<string, string[]> = {},
): ModelOption[] {
  const enabled = enabledProviderKeys(config);
  if (enabled.length === 0) return [];

  const list = enabled.flatMap((p) =>
    modelOptionsForProvider(
      p,
      fetchedByProvider[p] || [],
      // Prefer default when this channel owns it; else channel saved model
      config.active_provider === p
        ? config.default_model || config.providers[p]?.model
        : config.providers[p]?.model,
      config.providers[p]?.model_list,
    ),
  );

  const seen = new Set<string>();
  return list.filter((m) => {
    const key = `${m.provider}::${m.id}`;
    if (seen.has(key)) return false;
    if (!enabled.includes(m.provider)) return false;
    seen.add(key);
    return true;
  });
}

/**
 * Resolve which provider channel a model id belongs to for routing/UI.
 * Prefer: explicit override → available list match → name inference → active.
 */
export function resolveProviderForModel(
  config: AppConfig,
  modelId: string,
  fetchedByProvider: Record<string, string[]> = {},
  providerOverride?: string,
): string {
  if (providerOverride && config.providers[providerOverride]) {
    return providerOverride;
  }
  const opts = availableModels(config, fetchedByProvider);
  const hits = opts.filter((m) => m.id === modelId);
  if (hits.length === 1) return hits[0].provider;
  if (hits.length > 1) {
    const activeHit = hits.find((m) => m.provider === config.active_provider);
    if (activeHit) return activeHit.provider;
    return hits[0].provider;
  }
  // Model not in lists yet: if only one enabled channel, use it (custom gateway)
  const enabled = enabledProviderKeys(config);
  if (enabled.length === 1) return enabled[0];
  return inferProvider(modelId) || config.active_provider || enabled[0] || 'deepseek';
}

export function resolveValidDefaultModel(
  config: AppConfig,
  fetchedByProvider: Record<string, string[]> = {},
): string | null {
  const opts = availableModels(config, fetchedByProvider);
  if (opts.length === 0) return null;
  if (opts.some((m) => m.id === config.default_model)) {
    return config.default_model;
  }
  const active = config.active_provider;
  const fromActive = opts.find((m) => m.provider === active);
  if (fromActive) return fromActive.id;
  return opts[0].id;
}

export function toModelDef(opt: ModelOption): ModelDef {
  return { id: opt.id, display: opt.label, provider: opt.provider };
}

export function modelDisplayName(modelId: string, options?: ModelOption[]): string {
  const fromOpts = options?.find((m) => m.id === modelId);
  if (fromOpts) return fromOpts.label;
  const known = KNOWN_MODELS.find((m) => m.id === modelId);
  return known?.display || modelId;
}
