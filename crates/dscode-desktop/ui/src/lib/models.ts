/**
 * Shared model catalog + provider inference — keeps Settings default model,
 * per-channel model dropdowns, and InputBox picker in sync.
 *
 * Rules:
 * - Global pickers → **enabled channels only** + **enabled_models** whitelist
 * - Scan catalog (model_list) is for settings multi-select only
 * - Never inject hardcoded KNOWN_MODELS into pickers (labels only)
 * - Model → channel binding prefers the channel it was scanned under (not name prefix)
 */
import type { AppConfig, ModelDef, ProviderConfig } from './types';
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
 * Whitelist that appears in global pickers.
 * - `enabled_models == null/undefined` → legacy: all of model_list (or scanned)
 * - `enabled_models == []` → nothing (user cleared)
 * - otherwise explicit list
 */
export function effectiveEnabledModels(
  prov: ProviderConfig | undefined,
  scannedFallback: string[] = [],
): string[] {
  if (!prov) return [];
  const catalog =
    scannedFallback.length > 0
      ? scannedFallback
      : (prov.model_list || []).map((s) => s.trim()).filter(Boolean);

  if (prov.enabled_models === undefined || prov.enabled_models === null) {
    return catalog;
  }
  return prov.enabled_models.map((s) => String(s).trim()).filter(Boolean);
}

/**
 * Full scan catalog options (settings multi-select). Not filtered by whitelist.
 */
export function catalogModelOptionsForProvider(
  provider: string,
  fetched: string[] = [],
  persistedList?: string[],
): ModelOption[] {
  const known = knownModelsFor(provider);
  const out: ModelOption[] = [];
  const seen = new Set<string>();
  const push = (id: string) => {
    const tid = (id || '').trim();
    if (!tid || seen.has(tid)) return;
    seen.add(tid);
    out.push({ id: tid, label: labelFor(tid, known), provider });
  };
  const scanned =
    fetched.length > 0 ? fetched : (persistedList || []).filter(Boolean);
  for (const id of scanned) push(id);
  return out;
}

/**
 * Models for one channel that appear in **global** pickers (curated whitelist).
 * - With curated/scan: only enabled_models (or full catalog if not curated).
 * - Without scan: only the currently saved channel model (if any).
 */
export function modelOptionsForProvider(
  provider: string,
  fetched: string[] = [],
  savedModel?: string,
  persistedList?: string[],
  enabledModels?: string[] | null,
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

  if (
    scanned.length > 0 ||
    (enabledModels !== undefined && enabledModels !== null)
  ) {
    const curated = effectiveEnabledModels(
      {
        api_key: '',
        base_url: '',
        enabled: true,
        model: '',
        use_proxy: false,
        model_list: scanned,
        enabled_models: enabledModels,
      },
      scanned,
    );
    for (const id of curated) {
      // If we have a catalog, only show curated ids that still exist there
      if (scanned.length > 0 && !scanned.includes(id)) continue;
      push(id);
    }
    return out;
  }

  // Not scanned yet: keep current selection only (no phantom catalog).
  if (savedModel?.trim()) push(savedModel.trim());
  return out;
}

/**
 * Global selectable models: enabled channels × curated whitelist only.
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
      config.active_provider === p
        ? config.default_model || config.providers[p]?.model
        : config.providers[p]?.model,
      config.providers[p]?.model_list,
      config.providers[p]?.enabled_models,
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
 * Merge scan results into enabled_models whitelist.
 * - First curation (null/undefined): select all scanned.
 * - Re-scan: keep previous ∩ scanned; new ids stay unchecked.
 */
export function mergeEnabledAfterScan(
  previous: string[] | null | undefined,
  scanned: string[],
): string[] {
  const clean = scanned.map((s) => s.trim()).filter(Boolean);
  if (previous === undefined || previous === null) {
    return [...clean];
  }
  const set = new Set(clean);
  return previous.map((s) => s.trim()).filter((s) => s && set.has(s));
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
