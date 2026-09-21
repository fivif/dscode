import { useState } from 'react';
import { useConfigStore } from '@/stores/configStore';
import * as tauri from '@/lib/tauri';
import {
  catalogModelOptionsForProvider,
  effectiveEnabledModels,
  inferProvider,
  modelOptionsForProvider,
} from '@/lib/models';
import type { FieldContext } from './fieldSchema';
import { FieldList, FieldRow } from './fieldSchema';
import { CHANNEL_FIELDS, PROVIDER_KEYS, PROVIDER_LABELS } from './settingsFields';

/**
 * 渠道 — per-channel tab strip, the four simple channel fields (descriptors in
 * `settingsFields.tsx`), then the scanned-model checklist, which is the one
 * widget too specific to describe declaratively.
 */
export default function ChannelSection({ ctx }: { ctx: FieldContext }) {
  const { applyFetchedModels, clearFetchedModels, setEnabledModels } = useConfigStore();
  const [activeTab, setActiveTab] = useState<string>(() =>
    inferProvider(ctx.config.default_model),
  );
  // `config` loads asynchronously: on a fresh install the tab is derived from an
  // empty default_model ('' → no tab), which left the whole Provider form
  // (API key, base_url, 启用) unreachable until the user happened to click a tab.
  const tab = activeTab || inferProvider(ctx.config.default_model) || 'deepseek';
  const [fetchingProvider, setFetchingProvider] = useState<string | null>(null);
  const [fetchMsg, setFetchMsg] = useState<string | null>(null);
  const [modelFilter, setModelFilter] = useState('');

  // Do NOT auto-switch channel tab when default model changes — that made
  // "获取列表" results disappear when user was inspecting another channel.

  const handleFetchModels = async (provider: string) => {
    setFetchingProvider(provider);
    setFetchMsg(null);
    try {
      const models = await tauri.fetchModels(provider);
      await applyFetchedModels(provider, models);
      setFetchMsg(`已扫描 ${models.length} 个模型；请勾选要上架到总列表的项（首次默认全选）`);
    } catch (e: any) {
      setFetchMsg(String(e));
    }
    setFetchingProvider(null);
  };

  /** Curated options for channel preferred-model dropdown */
  const getCuratedOptions = (provider: string) =>
    modelOptionsForProvider(
      provider,
      ctx.fetchedModels[provider] || [],
      ctx.config.providers[provider]?.model,
      ctx.config.providers[provider]?.model_list,
      ctx.config.providers[provider]?.enabled_models,
    );

  const getCatalogOptions = (provider: string) =>
    catalogModelOptionsForProvider(
      provider,
      ctx.fetchedModels[provider] || [],
      ctx.config.providers[provider]?.model_list,
    );

  return (
    <>
      <div className="flex items-center gap-1 mb-5 border-b border-divider">
        {PROVIDER_KEYS.map((k) => (
          <button
            key={k}
            className={`px-4 py-2 text-[13px] transition-colors border-b-2 -mb-px rounded-t-control focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50 ${
              tab === k
                ? 'border-accent text-primary'
                : 'border-transparent text-secondary hover:text-primary'
            }`}
            onClick={() => {
              setActiveTab(k);
              setModelFilter('');
              setFetchMsg(null);
            }}
          >
            {PROVIDER_LABELS[k]}
          </button>
        ))}
      </div>

      {PROVIDER_KEYS.map((k) => {
        if (tab !== k) return null;
        const prov = ctx.config.providers[k];
        return (
          <div key={k} className="space-y-5">
            <FieldList fields={CHANNEL_FIELDS} ctx={{ ...ctx, providerKey: k }} />

            <FieldRow
              label="上架模型"
              action={
                <button
                  className="btn-ghost px-1.5 py-1 text-[11px] -mr-1.5 disabled:opacity-40"
                  onClick={() => handleFetchModels(k)}
                  disabled={fetchingProvider === k}
                >
                  {fetchingProvider === k
                    ? '获取中...'
                    : (ctx.fetchedModels[k]?.length || prov.model_list?.length)
                      ? `重新扫描`
                      : '获取列表'}
                </button>
              }
            >
              {(() => {
                const catalog = getCatalogOptions(k);
                const curated = getCuratedOptions(k);
                const enabledSet = new Set(
                  effectiveEnabledModels(prov, ctx.fetchedModels[k] || prov.model_list || []),
                );
                const preferred =
                  ctx.config.active_provider === k ? ctx.config.default_model : prov.model;
                const selectValue = curated.some((m) => m.id === preferred)
                  ? preferred
                  : curated[0]?.id || '';
                const filter = modelFilter.trim().toLowerCase();
                const filtered = filter
                  ? catalog.filter((m) => m.id.toLowerCase().includes(filter))
                  : catalog;
                const listedN = catalog.length;
                const enabledN = enabledSet.size;

                const toggleOne = (id: string, on: boolean) => {
                  const next = new Set(enabledSet);
                  if (on) next.add(id);
                  else next.delete(id);
                  void setEnabledModels(k, Array.from(next));
                };
                const selectAll = () =>
                  void setEnabledModels(
                    k,
                    catalog.map((m) => m.id),
                  );
                const clearAll = () => void setEnabledModels(k, []);

                return (
                  <>
                    {/* Preferred model among curated only */}
                    <select
                      className="field disabled:opacity-40"
                      disabled={!prov.enabled || curated.length === 0}
                      value={selectValue}
                      onChange={(e) => {
                        const id = e.target.value;
                        ctx.setProvider(k, { model: id });
                        if (prov.enabled) ctx.setDefaultModel(id, k);
                      }}
                    >
                      {curated.length === 0 ? (
                        <option value="">暂无上架模型 — 扫描并勾选</option>
                      ) : (
                        curated.map((m) => (
                          <option key={m.id} value={m.id}>
                            {m.label}
                          </option>
                        ))
                      )}
                    </select>
                    <p className="mt-1 text-[11px] text-muted">
                      上表为渠道偏好/设为默认；下方勾选决定哪些进入总列表与输入框
                    </p>

                    {listedN > 0 && (
                      <div className="mt-2 rounded-card border border-border bg-input overflow-hidden">
                        <div className="px-2.5 py-1.5 text-[11px] text-muted border-b border-divider space-y-1.5">
                          <div className="flex items-center justify-between gap-2">
                            <span>
                              已扫 {listedN} · 已上架 {enabledN}
                            </span>
                            <div className="flex items-center gap-1 shrink-0">
                              <button
                                type="button"
                                className="btn-ghost px-2 py-0.5 text-[11px] disabled:opacity-40"
                                onClick={selectAll}
                                disabled={!prov.enabled}
                              >
                                全选
                              </button>
                              <button
                                type="button"
                                className="btn-ghost px-2 py-0.5 text-[11px] disabled:opacity-40"
                                onClick={clearAll}
                                disabled={!prov.enabled}
                              >
                                清空
                              </button>
                              <button
                                type="button"
                                className="btn-ghost px-2 py-0.5 text-[11px] text-muted"
                                onClick={() => clearFetchedModels(k)}
                              >
                                清除扫描
                              </button>
                            </div>
                          </div>
                          {listedN > 8 && (
                            <input
                              type="search"
                              className="field py-1 text-[11px]"
                              placeholder="过滤模型 id…"
                              value={tab === k ? modelFilter : ''}
                              onChange={(e) => setModelFilter(e.target.value)}
                            />
                          )}
                        </div>
                        <div className="max-h-52 overflow-y-auto">
                          {filtered.map((m) => {
                            const checked = enabledSet.has(m.id);
                            return (
                              <label
                                key={m.id}
                                className={`flex items-center gap-2 px-2.5 py-1.5 text-[11px] font-mono border-b border-divider last:border-0 cursor-pointer transition-colors ${
                                  checked
                                    ? 'bg-accent-soft text-primary'
                                    : 'text-secondary hover:bg-hover hover:text-primary'
                                } ${!prov.enabled ? 'opacity-40 pointer-events-none' : ''}`}
                              >
                                <input
                                  type="checkbox"
                                  className="w-3.5 h-3.5 rounded accent-accent shrink-0 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
                                  checked={checked}
                                  disabled={!prov.enabled}
                                  onChange={(e) => toggleOne(m.id, e.target.checked)}
                                />
                                <span className="truncate flex-1" title={m.id}>
                                  {m.id}
                                </span>
                                {checked && selectValue === m.id && (
                                  <span className="text-[11px] text-success shrink-0">默认</span>
                                )}
                              </label>
                            );
                          })}
                          {filtered.length === 0 && (
                            <div className="px-2.5 py-3 text-[11px] text-muted">无匹配</div>
                          )}
                        </div>
                      </div>
                    )}

                    {!prov.enabled && (
                      <p className="mt-1.5 text-[11px] text-muted">
                        渠道未启用 — 启用后上架模型才会出现在默认/输入框
                      </p>
                    )}
                    {prov.enabled && ctx.config.active_provider === k && (
                      <p className="mt-1.5 text-[11px] text-success">
                        正在作为默认/输入框模型使用
                      </p>
                    )}
                    {prov.enabled && listedN === 0 && (
                      <p className="mt-1.5 text-[11px] text-muted">
                        点击「获取列表」扫描接口模型，再勾选上架到总列表
                      </p>
                    )}
                    {fetchMsg && tab === k && (
                      <p
                        className={`mt-1.5 text-[13px] ${
                          fetchMsg.startsWith('已扫描') ? 'text-success' : 'text-danger'
                        }`}
                      >
                        {fetchMsg}
                      </p>
                    )}
                  </>
                );
              })()}
            </FieldRow>
          </div>
        );
      })}
    </>
  );
}
