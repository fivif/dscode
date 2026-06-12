import { create } from 'zustand';
import * as tauri from '@/lib/tauri';
import type { AppConfig, ProviderConfig } from '@/lib/types';

function defaultAppConfig(): AppConfig {
  return {
    default_model: 'deepseek-v4-pro',
    active_provider: 'deepseek',
    providers: {
      deepseek: { api_key: '', base_url: 'https://api.deepseek.com/v1', enabled: true, model: 'deepseek-v4-pro' },
      openai: { api_key: '', base_url: 'https://api.openai.com/v1', enabled: false, model: 'gpt-4o' },
      anthropic: { api_key: '', base_url: 'https://api.anthropic.com', enabled: false, model: 'claude-sonnet-4-20250514' },
    },
    reasoning_effort: 'max', max_tokens: 0, temperature: 0,
    retention_days: 30, context_window_tokens: 1000000, context_compress_threshold: 0.8,
  };
}

export interface ConfigStore {
  config: AppConfig; loading: boolean; error: string | null;
  loadConfig: () => Promise<void>;
  saveConfig: (c: AppConfig) => Promise<void>;
  updateConfig: (p: Partial<AppConfig>) => Promise<void>;
  updateProvider: (provider: string, p: Partial<ProviderConfig>) => Promise<void>;
}

export const useConfigStore = create<ConfigStore>((set, get) => ({
  config: defaultAppConfig(), loading: false, error: null,

  loadConfig: async () => {
    set({ loading: true });
    try {
      const r = await tauri.getConfig();
      // Find first enabled provider
      let active = 'deepseek';
      if (r.providers.openai?.enabled) active = 'openai';
      else if (r.providers.anthropic?.enabled) active = 'anthropic';
      else if (r.providers.deepseek?.enabled) active = 'deepseek';

      const mapped: AppConfig = {
        default_model: r.default_model,
        active_provider: active,
        providers: {
          deepseek: { ...r.providers.deepseek, model: r.default_model.startsWith('deepseek') ? r.default_model : 'deepseek-v4-pro' },
          openai: { ...r.providers.openai, model: r.default_model.startsWith('gpt-') || r.default_model.startsWith('openai') ? r.default_model : 'gpt-4o' },
          anthropic: { ...r.providers.anthropic, model: r.default_model.startsWith('claude-') || r.default_model.startsWith('anthropic') ? r.default_model : 'claude-sonnet-4-20250514' },
        },
        reasoning_effort: (r as any).generation?.reasoning_effort || 'max',
        max_tokens: (r as any).generation?.max_tokens || 0,
        temperature: (r as any).generation?.temperature || 0,
        retention_days: (r as any).session?.retention_days || 30,
        context_window_tokens: (r as any).context?.window_tokens || 1000000,
        context_compress_threshold: (r as any).context?.compress_threshold || 0.8,
      };
      set({ config: mapped, loading: false });
    } catch { set({ loading: false }); }
  },

  saveConfig: async (c) => {
    await tauri.updateConfig({
      default_model: c.default_model,
      router_model: c.default_model,
      providers: {
        deepseek: { api_key: c.providers.deepseek.api_key, base_url: c.providers.deepseek.base_url, enabled: c.providers.deepseek.enabled },
        openai: { api_key: c.providers.openai.api_key, base_url: c.providers.openai.base_url, enabled: c.providers.openai.enabled },
        anthropic: { api_key: c.providers.anthropic.api_key, base_url: c.providers.anthropic.base_url, enabled: c.providers.anthropic.enabled },
        ollama: { api_key: '', base_url: '', enabled: false },
      },
      session: { retention_days: c.retention_days },
      safety: { allow_write_outside_project: false, blocked_commands: [], tool_timeout_secs: 120 },
      generation: { reasoning_effort: c.reasoning_effort, max_tokens: c.max_tokens, temperature: c.temperature },
      context: { window_tokens: c.context_window_tokens, compress_threshold: c.context_compress_threshold },
    } as any);
  },

  updateConfig: async (p) => {
    const old = get().config; const merged = { ...old, ...p };
    set({ config: merged });
    try { await get().saveConfig(merged); } catch (e: any) { set({ config: old, error: e?.message }); }
  },

  updateProvider: async (provider, p) => {
    const old = get().config;
    const providers = { ...old.providers, [provider]: { ...old.providers[provider], ...p } };
    // If enabling this provider, set it as active
    const active = p.enabled ? provider : old.active_provider;
    const merged = { ...old, providers, active_provider: active };
    set({ config: merged });
    try { await get().saveConfig(merged); } catch (e: any) { set({ config: old, error: e?.message }); }
  },
}));
