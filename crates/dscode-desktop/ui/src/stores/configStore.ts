import { create } from 'zustand';
import * as tauri from '@/lib/tauri';
import type { AppConfig, ProviderConfig } from '@/lib/types';
import { isProxyConfigured } from '@/lib/types';
import {
  resolveProviderForModel,
  resolveValidDefaultModel,
} from '@/lib/models';

let _saveTimer: ReturnType<typeof setTimeout> | null = null;
let _pendingConfig: AppConfig | null = null;
let _pendingResolvers: Array<{ resolve: () => void; reject: (e: any) => void }> = [];

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
      },
      openai: {
        api_key: '',
        base_url: 'https://api.openai.com/v1',
        enabled: false,
        model: '',
        use_proxy: false,
        model_list: [],
      },
      anthropic: {
        api_key: '',
        base_url: 'https://api.anthropic.com',
        enabled: false,
        model: '',
        use_proxy: false,
        model_list: [],
      },
      ollama: {
        api_key: '',
        base_url: 'http://127.0.0.1:11434',
        enabled: false,
        model: '',
        use_proxy: false,
        model_list: [],
      },
    },
    reasoning_effort: 'max',
    max_tokens: 0,
    temperature: 0,
    retention_days: 30,
    context_window_tokens: 1_000_000,
    context_compress_threshold: 0.8,
    proxy: { url: '', global: false },
    mcp_use_proxy: false,
    skills_use_proxy: false,
    absolute_trust: false,
  };
}

function ensureProvider(
  raw: any,
  fallback: ProviderConfig,
): ProviderConfig {
  const list = Array.isArray(raw?.model_list)
    ? (raw.model_list as string[]).map((s) => String(s).trim()).filter(Boolean)
    : fallback.model_list || [];
  return {
    api_key: raw?.api_key ?? fallback.api_key,
    base_url: raw?.base_url ?? fallback.base_url,
    enabled: !!raw?.enabled,
    model: (raw?.model || '').trim() || fallback.model || '',
    use_proxy: !!raw?.use_proxy,
    model_list: list,
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
      proxy: { url: (cfg.proxy?.url || '').trim(), global: false },
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

/** After a real /models scan: persist list, clamp channel + default model. */
function applyScanToConfig(
  cfg: AppConfig,
  provider: string,
  models: string[],
): AppConfig {
  const clean = models.map((s) => s.trim()).filter(Boolean);
  const providers = { ...cfg.providers };
  const prev = providers[provider];
  if (!prev) return cfg;

  let channelModel = (prev.model || '').trim();
  if (clean.length > 0 && !clean.includes(channelModel)) {
    channelModel = clean[0];
  }
  providers[provider] = {
    ...prev,
    model_list: clean,
    model: channelModel,
  };

  let next: AppConfig = { ...cfg, providers };
  const fetched = { [provider]: clean };

  // If this channel is active (or owns default), keep default inside real list
  const ownsDefault =
    next.active_provider === provider ||
    resolveProviderForModel(next, next.default_model, fetched) === provider;

  if (ownsDefault && prev.enabled) {
    if (clean.length === 0) {
      // scanned empty — clear default if it pointed here
      if (next.active_provider === provider) {
        next = { ...next, default_model: '' };
      }
    } else if (!clean.includes(next.default_model)) {
      next = withSyncedModel(next, channelModel || clean[0], provider, fetched);
    } else {
      next = withSyncedModel(next, next.default_model, provider, fetched);
    }
  }

  // Global clamp across all enabled
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
    await get().saveConfig(nextCfg);
  },

  clearFetchedModels: async (provider) => {
    const cfg = get().config;
    const providers = { ...cfg.providers };
    if (providers[provider]) {
      providers[provider] = { ...providers[provider], model_list: [] };
    }
    const next = { ...cfg, providers };
    set((s) => {
      const fetched = { ...s.fetchedModels };
      delete fetched[provider];
      return { config: next, fetchedModels: fetched };
    });
    await get().saveConfig(next);
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
        },
        mcp_use_proxy: !!(r as any).extensions?.mcp_use_proxy,
        skills_use_proxy: !!(r as any).extensions?.skills_use_proxy,
        absolute_trust: !!(r as any).safety?.absolute_trust,
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

  saveConfig: async (c) => {
    _pendingConfig = c;
    const oldResolvers = _pendingResolvers;
    _pendingResolvers = [];
    for (const r of oldResolvers) r.resolve();

    if (_saveTimer) clearTimeout(_saveTimer);
    _saveTimer = setTimeout(async () => {
      _saveTimer = null;
      const cfg = _pendingConfig!;
      _pendingConfig = null;
      try {
        let synced = cfg.default_model
          ? withSyncedModel(cfg, cfg.default_model, cfg.active_provider, {
              ...Object.fromEntries(
                Object.entries(cfg.providers).map(([k, p]) => [k, p.model_list || []]),
              ),
            })
          : cfg;
        synced = normalizeProxyFlags(synced);
        const packProv = (p?: ProviderConfig) => ({
          api_key: p?.api_key || '',
          base_url: p?.base_url || '',
          enabled: !!p?.enabled,
          use_proxy: !!p?.use_proxy && isProxyConfigured(synced.proxy.url),
          model: p?.model || '',
          model_list: Array.isArray(p?.model_list) ? p!.model_list : [],
        });
        // Preserve mcp_servers / skills_dirs / agent / safety extras already on disk
        let existingExt: any = {};
        let existingAgent: any = { global_prompt: '', replace_system_prompt: false };
        let existingSafety: any = {};
        try {
          const cur = await tauri.getConfig();
          existingExt = (cur as any).extensions || {};
          existingSafety = (cur as any).safety || {};
          if ((cur as any).agent) {
            existingAgent = {
              global_prompt: (cur as any).agent.global_prompt || '',
              replace_system_prompt: !!(cur as any).agent.replace_system_prompt,
            };
          }
        } catch {
          /* ignore */
        }
        await tauri.updateConfig({
          default_model: synced.default_model,
          router_model: synced.default_model,
          active_provider: synced.active_provider || 'deepseek',
          providers: {
            deepseek: packProv(synced.providers.deepseek),
            openai: packProv(synced.providers.openai),
            anthropic: packProv(synced.providers.anthropic),
            ollama: packProv(synced.providers.ollama),
          },
          session: { retention_days: synced.retention_days },
          safety: {
            allow_write_outside_project: !!existingSafety.allow_write_outside_project,
            blocked_commands: existingSafety.blocked_commands || [],
            tool_timeout_secs: existingSafety.tool_timeout_secs || 120,
            absolute_trust: !!synced.absolute_trust,
            permission_timeout_secs: existingSafety.permission_timeout_secs || 120,
          },
          generation: {
            reasoning_effort: synced.reasoning_effort,
            max_tokens: synced.max_tokens,
            temperature: synced.temperature,
            proxy_url: '', // legacy cleared; use top-level proxy
          },
          context: {
            window_tokens: synced.context_window_tokens,
            compress_threshold: synced.context_compress_threshold,
          },
          extensions: {
            mcp_servers: existingExt.mcp_servers || [],
            skills_dirs: existingExt.skills_dirs || [],
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
          },
          agent: existingAgent,
        } as any);
        // Keep store aligned if save path re-synced
        set({ config: synced });
        const resolvers = _pendingResolvers;
        _pendingResolvers = [];
        for (const r of resolvers) r.resolve();
      } catch (e: any) {
        const resolvers = _pendingResolvers;
        _pendingResolvers = [];
        for (const r of resolvers) r.reject(e);
      }
    }, 300);

    return new Promise<void>((resolve, reject) => {
      _pendingResolvers.push({ resolve, reject });
    });
  },

  updateConfig: async (p) => {
    if (get().loading) return;
    set({ loading: true });
    try {
      let merged = { ...get().config, ...p };
      if (p.proxy) {
        merged.proxy = { ...get().config.proxy, ...p.proxy };
      }
      if (p.default_model) {
        merged = withSyncedModel(merged, p.default_model);
      }
      merged = normalizeProxyFlags(merged);
      set({ config: merged });
      await get().saveConfig(merged);
    } catch (e: any) {
      console.error('updateConfig failed:', e);
    } finally {
      set({ loading: false });
    }
  },

  setDefaultModel: async (modelId, providerHint) => {
    if (get().loading) return;
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
    set({ loading: true });
    try {
      const merged = withSyncedModel(cfg, modelId, p, fetched);
      set({ config: merged });
      await get().saveConfig(merged);
    } catch (e: any) {
      console.error('setDefaultModel failed:', e);
    } finally {
      set({ loading: false });
    }
  },

  updateProvider: async (provider, p) => {
    if (get().loading) return;
    set({ loading: true });
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
        if (scanned.length === 0 || scanned.includes(p.model)) {
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
    } finally {
      set({ loading: false });
    }
  },
}));
