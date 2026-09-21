import { create } from 'zustand';
import * as tauri from '@/lib/tauri';
import type { AppConfig, ProviderConfig } from '@/lib/types';
import { isProxyConfigured } from '@/lib/types';
import {
  effectiveEnabledModels,
  mergeEnabledAfterScan,
  resolveProviderForModel,
  resolveValidDefaultModel,
} from '@/lib/models';

let _saveTimer: ReturnType<typeof setTimeout> | null = null;
/** Latest config waiting to be written; overwritten by newer edits (coalescing). */
let _pendingConfig: AppConfig | null = null;
/** Callers awaiting the write of `_pendingConfig` (resolved when it lands). */
let _pendingResolvers: Array<{ resolve: () => void; reject: (e: any) => void }> = [];
/** Config writes currently in flight — drives `loading`, never gates writes. */
let _inflightSaves = 0;

function defaultAppConfig(): AppConfig {
  return {
    default_model: '',
    active_provider: 'deepseek',
    providers: {
      deepseek: {
        api_key: '',
        base_url: 'https://api.deepseek.com/v1',
        enabled: true,
        model: '',
        use_proxy: false,
        model_list: [],
        enabled_models: null,
      },
      openai: {
        api_key: '',
        base_url: 'https://api.openai.com/v1',
        enabled: false,
        model: '',
        use_proxy: false,
        model_list: [],
        enabled_models: null,
      },
      anthropic: {
        api_key: '',
        base_url: 'https://api.anthropic.com',
        enabled: false,
        model: '',
        use_proxy: false,
        model_list: [],
        enabled_models: null,
      },
      ollama: {
        api_key: '',
        base_url: 'http://127.0.0.1:11434',
        enabled: false,
        model: '',
        use_proxy: false,
        model_list: [],
        enabled_models: null,
      },
    },
    reasoning_effort: 'max',
    max_tokens: 0,
    temperature: 0,
    retention_days: 30,
    context_window_tokens: 1_000_000,
    context_compress_threshold: 0.8,
    proxy: { url: '', global: false, web_use_proxy: false },
    mcp_use_proxy: false,
    skills_use_proxy: false,
    absolute_trust: false,
    image_enabled: true,
    image_model: 'gpt-image-1',
    image_size: '1024x1024',
    image_provider: '',
  };
}

function parseEnabledModels(raw: any): string[] | null {
  // Field missing → null (legacy: treat as all model_list)
  if (raw === undefined || raw === null) return null;
  if (!Array.isArray(raw)) return null;
  return (raw as string[]).map((s) => String(s).trim()).filter(Boolean);
}

function ensureProvider(
  raw: any,
  fallback: ProviderConfig,
): ProviderConfig {
  const list = Array.isArray(raw?.model_list)
    ? (raw.model_list as string[]).map((s) => String(s).trim()).filter(Boolean)
    : fallback.model_list || [];
  // Only treat as curated when the key is present on the raw provider object
  const hasKey =
    raw != null && Object.prototype.hasOwnProperty.call(raw, 'enabled_models');
  const enabled_models = hasKey
    ? parseEnabledModels(raw.enabled_models)
    : fallback.enabled_models ?? null;
  return {
    api_key: raw?.api_key ?? fallback.api_key,
    base_url: raw?.base_url ?? fallback.base_url,
    enabled: !!raw?.enabled,
    model: (raw?.model || '').trim() || fallback.model || '',
    use_proxy: !!raw?.use_proxy,
    model_list: list,
    // hasKey + null array field → empty list Some([]); missing key → null
    enabled_models: hasKey
      ? Array.isArray(raw.enabled_models)
        ? (raw.enabled_models as string[]).map((s) => String(s).trim()).filter(Boolean)
        : []
      : enabled_models,
  };
}

/** Clamp proxy flags: no valid URL → all off; global → force on conceptually (saved flags may stay). */
function normalizeProxyFlags(cfg: AppConfig): AppConfig {
  const configured = isProxyConfigured(cfg.proxy?.url);
  if (!configured) {
    const providers = { ...cfg.providers };
    for (const k of Object.keys(providers)) {
      providers[k] = { ...providers[k], use_proxy: false };
    }
    return {
      ...cfg,
      proxy: { url: (cfg.proxy?.url || '').trim(), global: false, web_use_proxy: false },
      providers,
      mcp_use_proxy: false,
      skills_use_proxy: false,
    };
  }
  return {
    ...cfg,
    proxy: {
      url: cfg.proxy.url.trim(),
      global: !!cfg.proxy.global,
      web_use_proxy: !!cfg.proxy.global || !!cfg.proxy.web_use_proxy,
    },
  };
}

/**
 * Keep default_model, active_provider, and providers[p].model consistent.
 * Source of truth for the active chat model is always `default_model`.
 * `providerOverride` must be used when the model came from a scanned channel list
 * (custom OpenAI gateways often return ids that would mis-infer to deepseek/etc.).
 */
function withSyncedModel(
  cfg: AppConfig,
  modelId: string,
  providerOverride?: string,
  fetchedByProvider: Record<string, string[]> = {},
): AppConfig {
  const tid = (modelId || '').trim();
  if (!tid) return cfg;

  const provider = resolveProviderForModel(
    cfg,
    tid,
    fetchedByProvider,
    providerOverride,
  );
  const providers = { ...cfg.providers };
  if (providers[provider] && !providers[provider].enabled) {
    return cfg;
  }
  if (providers[provider]) {
    providers[provider] = {
      ...providers[provider],
      model: tid,
    };
  }
  return {
    ...cfg,
    default_model: tid,
    active_provider: provider,
    providers,
  };
}

/** After a real /models scan: persist catalog + merge whitelist, clamp default. */
function applyScanToConfig(
  cfg: AppConfig,
  provider: string,
  models: string[],
): AppConfig {
  const clean = models.map((s) => s.trim()).filter(Boolean);
  const providers = { ...cfg.providers };
  const prev = providers[provider];
  if (!prev) return cfg;

  const enabled_models = mergeEnabledAfterScan(prev.enabled_models, clean);
  let channelModel = (prev.model || '').trim();
  if (enabled_models.length > 0) {
    if (!enabled_models.includes(channelModel)) {
      channelModel = enabled_models[0];
    }
  } else {
    channelModel = '';
  }
  providers[provider] = {
    ...prev,
    model_list: clean,
    enabled_models,
    model: channelModel,
  };

  let next: AppConfig = { ...cfg, providers };
  const fetched = { [provider]: clean };

  // Keep default inside curated whitelist when this channel owns it
  const ownsDefault =
    next.active_provider === provider ||
    resolveProviderForModel(next, next.default_model, fetched) === provider;

  if (ownsDefault && prev.enabled) {
    if (enabled_models.length === 0) {
      if (next.active_provider === provider) {
        next = { ...next, default_model: '' };
      }
    } else if (!enabled_models.includes(next.default_model)) {
      next = withSyncedModel(next, channelModel || enabled_models[0], provider, fetched);
    } else {
      next = withSyncedModel(next, next.default_model, provider, fetched);
    }
  }

  const allFetched: Record<string, string[]> = {};
  for (const [k, p] of Object.entries(next.providers)) {
    if (p.model_list?.length) allFetched[k] = p.model_list;
  }
  allFetched[provider] = clean;
  const clamped = resolveValidDefaultModel(next, allFetched);
  if (clamped && clamped !== next.default_model) {
    next = withSyncedModel(next, clamped, undefined, allFetched);
  }
  return next;
}

/** Apply explicit whitelist for a channel and clamp default if needed. */
function applyEnabledModelsToConfig(
  cfg: AppConfig,
  provider: string,
  enabledModels: string[],
): AppConfig {
  const providers = { ...cfg.providers };
  const prev = providers[provider];
  if (!prev) return cfg;
  const clean = enabledModels.map((s) => s.trim()).filter(Boolean);
  let channelModel = (prev.model || '').trim();
  if (clean.length > 0) {
    if (!clean.includes(channelModel)) channelModel = clean[0];
  } else {
    channelModel = '';
  }
  providers[provider] = {
    ...prev,
    enabled_models: clean,
    model: channelModel,
  };
  let next: AppConfig = { ...cfg, providers };
  const allFetched: Record<string, string[]> = {};
  for (const [k, p] of Object.entries(next.providers)) {
    if (p.model_list?.length) allFetched[k] = p.model_list;
  }
  const ownsDefault =
    next.active_provider === provider ||
    resolveProviderForModel(next, next.default_model, allFetched) === provider;
  if (ownsDefault && prev.enabled) {
    if (clean.length === 0) {
      next = { ...next, default_model: '' };
    } else if (!clean.includes(next.default_model)) {
      next = withSyncedModel(next, channelModel || clean[0], provider, allFetched);
    }
  }
  const clamped = resolveValidDefaultModel(next, allFetched);
  if (clamped && clamped !== next.default_model) {
    next = withSyncedModel(next, clamped, undefined, allFetched);
  }
  return next;
}

/**
 * Build the **full** `Config` payload Rust expects.
 *
 * `update_config` deserializes a whole `Config` and `Config::save()` rewrites the
 * entire TOML, while every field carries `#[serde(default)]` — so any key this
 * payload omits is silently reset to its default on disk (`config.toml` is shared
 * with the CLI). That used to wipe `agent.git_bash_path` / `agent.read_before_edit`
 * / `agent.memory_*`, `context.max_agent_iterations` and the whole `[teams]`
 * section on every settings save.
 *
 * So: read the current config, spread it, and override only the fields this UI
 * actually owns. Sections the settings page never touches (`agent`, `teams`) are
 * passed through byte-for-byte.
 *
 * Throws when the current config cannot be read — the caller must abort the save
 * rather than write a payload with missing sections.
 */
async function buildSavePayload(
  cfg: AppConfig,
): Promise<{ payload: Record<string, unknown>; synced: AppConfig }> {
  let synced = cfg.default_model
    ? withSyncedModel(cfg, cfg.default_model, cfg.active_provider, {
        ...Object.fromEntries(
          Object.entries(cfg.providers).map(([k, p]) => [k, p.model_list || []]),
        ),
      })
    : cfg;
  synced = normalizeProxyFlags(synced);

  let cur: any;
  try {
    cur = await tauri.getConfig();
  } catch (e: any) {
    throw new Error(
      `读取当前配置失败，已取消本次保存以免覆盖 config.toml：${e?.message || String(e)}`,
    );
  }

  /**
   * Pack one channel for the wire. Fields this UI does not model are copied from
   * the current on-disk channel instead of being dropped — e.g. `api_format`
   * (`"responses"` switches a channel to the OpenAI Responses wire format) is
   * only settable outside the settings UI and used to be reset on every save.
   */
  const packProv = (key: string) => {
    const p = synced.providers[key];
    const base: Record<string, unknown> = {
      api_key: p?.api_key || '',
      base_url: p?.base_url || '',
      enabled: !!p?.enabled,
      use_proxy: !!p?.use_proxy && isProxyConfigured(synced.proxy.url),
      model: p?.model || '',
      model_list: Array.isArray(p?.model_list) ? p.model_list : [],
    };
    // Only write enabled_models when curated (array, including empty).
    // Omit when null so legacy "all model_list" round-trips as missing key…
    // but Rust Option needs Some for empty; we always send array once user curated.
    if (p?.enabled_models !== undefined && p?.enabled_models !== null) {
      base.enabled_models = Array.isArray(p.enabled_models) ? p.enabled_models : [];
    }
    const disk = cur?.providers?.[key];
    if (disk && typeof disk === 'object') {
      for (const [k, v] of Object.entries(disk)) {
        if (!(k in base)) base[k] = v;
      }
    }
    return base;
  };

  const payload: Record<string, unknown> = {
    // Pass through every section we do not own (agent, teams, safety extras,
    // extensions extras, …) so nothing is reset to its serde default.
    ...(cur || {}),
    default_model: synced.default_model,
    router_model: synced.default_model,
    active_provider: synced.active_provider || 'deepseek',
    providers: {
      deepseek: packProv('deepseek'),
      openai: packProv('openai'),
      anthropic: packProv('anthropic'),
      ollama: packProv('ollama'),
    },
    session: { ...(cur?.session || {}), retention_days: synced.retention_days },
    safety: { ...(cur?.safety || {}), absolute_trust: !!synced.absolute_trust },
    generation: {
      ...(cur?.generation || {}),
      reasoning_effort: synced.reasoning_effort,
      max_tokens: synced.max_tokens,
      temperature: synced.temperature,
      proxy_url: '', // legacy cleared; use top-level proxy
      image_enabled: synced.image_enabled,
      image_model: synced.image_model,
      image_size: synced.image_size,
      image_provider: synced.image_provider,
    },
    context: {
      ...(cur?.context || {}),
      window_tokens: synced.context_window_tokens,
      compress_threshold: synced.context_compress_threshold,
    },
    extensions: {
      ...(cur?.extensions || {}),
      mcp_use_proxy:
        isProxyConfigured(synced.proxy.url) &&
        (synced.proxy.global || synced.mcp_use_proxy),
      skills_use_proxy:
        isProxyConfigured(synced.proxy.url) &&
        (synced.proxy.global || synced.skills_use_proxy),
    },
    proxy: {
      url: synced.proxy.url.trim(),
      global: isProxyConfigured(synced.proxy.url) && !!synced.proxy.global,
      web_use_proxy:
        isProxyConfigured(synced.proxy.url) &&
        (!!synced.proxy.global || !!synced.proxy.web_use_proxy),
    },
  };

  return { payload, synced };
}

export interface ConfigStore {
  config: AppConfig;
  loading: boolean;
  error: string | null;
  /** model ids fetched from each provider API (mirrors persisted model_list) */
  fetchedModels: Record<string, string[]>;
  /** Apply a successful /models scan: memory + disk + clamp default */
  applyFetchedModels: (provider: string, models: string[]) => Promise<void>;
  /** Clear scan cache for a channel (reverts picker to "not scanned") */
  clearFetchedModels: (provider: string) => Promise<void>;
  /** Background re-scan all enabled channels that have credentials */
  refreshEnabledModels: () => Promise<void>;
  loadConfig: () => Promise<void>;
  saveConfig: (c: AppConfig) => Promise<void>;
  updateConfig: (p: Partial<AppConfig>) => Promise<void>;
  updateProvider: (provider: string, p: Partial<ProviderConfig>) => Promise<void>;
  /** Select chat model everywhere (settings default + input picker) */
  setDefaultModel: (modelId: string, providerHint?: string) => Promise<void>;
  /** Set which scanned models appear in the global list for a channel */
  setEnabledModels: (provider: string, models: string[]) => Promise<void>;
}

export const useConfigStore = create<ConfigStore>((set, get) => ({
  config: defaultAppConfig(),
  loading: false,
  error: null,
  fetchedModels: {},

  applyFetchedModels: async (provider, models) => {
    const clean = models.map((s) => s.trim()).filter(Boolean);
    const nextCfg = applyScanToConfig(get().config, provider, clean);
    set((s) => ({
      config: nextCfg,
      fetchedModels: { ...s.fetchedModels, [provider]: clean },
    }));
    try {
      await get().saveConfig(nextCfg);
    } catch {
      /* failure is surfaced through the store `error` */
    }
  },

  clearFetchedModels: async (provider) => {
    const cfg = get().config;
    const providers = { ...cfg.providers };
    if (providers[provider]) {
      providers[provider] = {
        ...providers[provider],
        model_list: [],
        enabled_models: null,
      };
    }
    const next = { ...cfg, providers };
    set((s) => {
      const fetched = { ...s.fetchedModels };
      delete fetched[provider];
      return { config: next, fetchedModels: fetched };
    });
    try {
      await get().saveConfig(next);
    } catch {
      /* failure is surfaced through the store `error` */
    }
  },

  setEnabledModels: async (provider, models) => {
    const nextCfg = applyEnabledModelsToConfig(get().config, provider, models);
    set({ config: nextCfg });
    try {
      await get().saveConfig(nextCfg);
    } catch {
      /* failure is surfaced through the store `error` */
    }
  },

  refreshEnabledModels: async () => {
    const { config } = get();
    const jobs = Object.entries(config.providers)
      .filter(([, p]) => p?.enabled && (p.api_key?.trim() || p.base_url?.trim()))
      .map(async ([key, p]) => {
        // Ollama often has empty key; others need key for /models
        if (key !== 'ollama' && !p.api_key?.trim()) return;
        try {
          const models = await tauri.fetchModels(key);
          await get().applyFetchedModels(key, models);
        } catch (e) {
          console.warn(`refresh models for ${key} failed:`, e);
        }
      });
    await Promise.all(jobs);
  },

  loadConfig: async () => {
    set({ loading: true });
    try {
      const r = await tauri.getConfig();
      const base = defaultAppConfig();
      const providers: Record<string, ProviderConfig> = {
        deepseek: ensureProvider(r.providers?.deepseek, base.providers.deepseek),
        openai: ensureProvider(r.providers?.openai, base.providers.openai),
        anthropic: ensureProvider(r.providers?.anthropic, base.providers.anthropic),
        ollama: ensureProvider((r.providers as any)?.ollama, base.providers.ollama),
      };

      // Restore persisted scans into memory so pickers match disk immediately
      const fetchedModels: Record<string, string[]> = {};
      for (const [k, p] of Object.entries(providers)) {
        if (p.model_list && p.model_list.length > 0) {
          fetchedModels[k] = [...p.model_list];
        }
      }

      let default_model = (r.default_model || '').trim();
      const legacyProxy = (r as any).generation?.proxy_url || '';
      const proxyUrl = ((r as any).proxy?.url || legacyProxy || '').trim();

      // Prefer persisted active_provider; fall back to scan-aware resolution
      let active = ((r as any).active_provider || '').trim();
      if (!active || !providers[active]) {
        active = resolveProviderForModel(
          { ...base, providers, default_model, active_provider: 'deepseek' } as AppConfig,
          default_model,
          fetchedModels,
        );
      }

      let draft: AppConfig = {
        default_model,
        active_provider: active,
        providers,
        reasoning_effort: (r as any).generation?.reasoning_effort || 'max',
        max_tokens: (r as any).generation?.max_tokens || 0,
        temperature: (r as any).generation?.temperature ?? 0,
        retention_days: (r as any).session?.retention_days || 30,
        context_window_tokens: (r as any).context?.window_tokens || 1_000_000,
        context_compress_threshold: (r as any).context?.compress_threshold || 0.8,
        proxy: {
          url: proxyUrl,
          global: !!(r as any).proxy?.global && isProxyConfigured(proxyUrl),
          web_use_proxy:
            !!(r as any).proxy?.global || !!(r as any).proxy?.web_use_proxy,
        },
        mcp_use_proxy: !!(r as any).extensions?.mcp_use_proxy,
        skills_use_proxy: !!(r as any).extensions?.skills_use_proxy,
        absolute_trust: !!(r as any).safety?.absolute_trust,
        // `??` not `||`: `false` is a real value. A missing key means an older
        // config.toml, where the Rust side defaults the feature to on.
        image_enabled: (r as any).generation?.image_enabled ?? true,
        // Empty model/size is meaningful on the Rust side (it substitutes its
        // own default), so showing that default here is the same value.
        image_model: (r as any).generation?.image_model || 'gpt-image-1',
        image_size: (r as any).generation?.image_size || '1024x1024',
        // Empty provider means "follow active_provider" — must NOT be coerced.
        image_provider: ((r as any).generation?.image_provider || '').trim(),
      };
      draft = normalizeProxyFlags(draft);

      // Drop default if it is a ghost not present in any real scanned list
      // (unless no scans at all — then keep saved id until user scans)
      const hasAnyScan = Object.values(fetchedModels).some((a) => a.length > 0);
      if (hasAnyScan) {
        const valid = resolveValidDefaultModel(draft, fetchedModels);
        if (valid && valid !== draft.default_model) {
          draft = withSyncedModel(draft, valid, undefined, fetchedModels);
        } else if (valid) {
          draft = withSyncedModel(draft, valid, undefined, fetchedModels);
        } else if (!valid) {
          draft = { ...draft, default_model: '' };
        }
      } else if (default_model) {
        // No scans yet: still bind default to an enabled channel without inventing models
        const p = resolveProviderForModel(draft, default_model, {});
        if (draft.providers[p]?.enabled) {
          draft = withSyncedModel(draft, default_model, p, {});
        }
      }

      set({ config: draft, fetchedModels, loading: false, error: null });

      // Background re-scan so lists stay real (does not block UI)
      void get().refreshEnabledModels();
    } catch (e: any) {
      set({ loading: false, error: e?.message || String(e) });
    }
  },

  /**
   * Coalescing save: edits within the debounce window merge into one write and
   * the newest state always wins. Callers' promises settle only after *their*
   * (or a newer) payload has actually been persisted.
   */
  saveConfig: async (c) => {
    _pendingConfig = c;

    if (_saveTimer) clearTimeout(_saveTimer);
    _saveTimer = setTimeout(() => {
      _saveTimer = null;
      const cfg = _pendingConfig;
      _pendingConfig = null;
      const resolvers = _pendingResolvers;
      _pendingResolvers = [];
      if (!cfg) {
        for (const r of resolvers) r.resolve();
        return;
      }
      void (async () => {
        _inflightSaves += 1;
        set({ loading: true });
        try {
          const { payload, synced } = await buildSavePayload(cfg);
          await tauri.updateConfig(payload as any);
          // Only re-align the store when nothing newer was edited meanwhile;
          // otherwise this stale snapshot would clobber in-flight keystrokes.
          if (get().config === cfg) {
            set({ config: synced, error: null });
          } else {
            set({ error: null });
          }
          for (const r of resolvers) r.resolve();
        } catch (e: any) {
          const msg = e?.message || String(e);
          console.error('saveConfig failed:', e);
          // Surface it: the UI keeps showing the new value, so a silent failure
          // is indistinguishable from a successful save.
          set({ error: `配置保存失败（改动未写入磁盘）：${msg}` });
          for (const r of resolvers) r.reject(e);
        } finally {
          _inflightSaves = Math.max(0, _inflightSaves - 1);
          if (_inflightSaves === 0) set({ loading: false });
        }
      })();
    }, 300);

    return new Promise<void>((resolve, reject) => {
      _pendingResolvers.push({ resolve, reject });
    });
  },

  updateConfig: async (p) => {
    let merged = { ...get().config, ...p };
    if (p.proxy) {
      merged.proxy = { ...get().config.proxy, ...p.proxy };
    }
    if (p.default_model) {
      merged = withSyncedModel(merged, p.default_model);
    }
    merged = normalizeProxyFlags(merged);
    set({ config: merged });
    try {
      await get().saveConfig(merged);
    } catch (e: any) {
      console.error('updateConfig failed:', e);
    }
  },

  setDefaultModel: async (modelId, providerHint) => {
    const cfg = get().config;
    const fetched = get().fetchedModels;
    const p = resolveProviderForModel(cfg, modelId, fetched, providerHint);
    if (!cfg.providers[p]?.enabled) {
      console.warn('setDefaultModel blocked: provider disabled', p, modelId);
      return;
    }
    // Must be in that channel's real scanned list when a scan exists
    const scanned = fetched[p]?.length
      ? fetched[p]
      : cfg.providers[p]?.model_list || [];
    if (scanned.length > 0 && !scanned.includes(modelId)) {
      console.warn('setDefaultModel blocked: model not in scanned list', modelId, p);
      return;
    }
    try {
      const merged = withSyncedModel(cfg, modelId, p, fetched);
      set({ config: merged });
      await get().saveConfig(merged);
    } catch (e: any) {
      console.error('setDefaultModel failed:', e);
    }
  },

  updateProvider: async (provider, p) => {
    try {
      const current = get().config;
      const fetched = get().fetchedModels;
      const nextProv = { ...current.providers[provider], ...p };
      let providers = { ...current.providers, [provider]: nextProv };
      let merged: AppConfig = { ...current, providers };

      // Channel model pick → becomes global default when channel enabled
      if (p.model && nextProv.enabled) {
        const scanned =
          fetched[provider]?.length
            ? fetched[provider]
            : nextProv.model_list || [];
        const curated = effectiveEnabledModels(nextProv, scanned);
        const allowed =
          curated.length === 0
            ? scanned.length === 0 || scanned.includes(p.model)
            : curated.includes(p.model);
        if (allowed) {
          merged = withSyncedModel(merged, p.model, provider, fetched);
        }
      } else if (p.enabled === false) {
        const ownsDefault = current.active_provider === provider;
        if (ownsDefault) {
          const fallback = Object.entries(merged.providers).find(
            ([k, v]) => k !== provider && v.enabled,
          );
          if (fallback) {
            const fbList =
              fetched[fallback[0]]?.length
                ? fetched[fallback[0]]
                : fallback[1].model_list || [];
            const fbModel =
              (fallback[1].model && fbList.includes(fallback[1].model)
                ? fallback[1].model
                : fbList[0]) ||
              resolveValidDefaultModel(merged, fetched) ||
              '';
            if (fbModel) {
              merged = withSyncedModel(merged, fbModel, fallback[0], fetched);
            } else {
              merged = { ...merged, default_model: '', active_provider: fallback[0] };
            }
          }
        }
      } else if (p.enabled === true) {
        const valid = resolveValidDefaultModel(merged, fetched);
        if (!valid && nextProv.model) {
          merged = withSyncedModel(merged, nextProv.model, provider, fetched);
        }
      }

      const clamped = resolveValidDefaultModel(merged, get().fetchedModels);
      if (clamped && clamped !== merged.default_model) {
        merged = withSyncedModel(merged, clamped, undefined, get().fetchedModels);
      }

      set({ config: merged });
      await get().saveConfig(merged);
    } catch (e: any) {
      console.error('updateProvider failed:', e);
    }
  },
}));
