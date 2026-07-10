import { useState, useRef, useCallback, useEffect, useMemo } from 'react';
import { useConfigStore } from '@/stores/configStore';
import * as tauri from '@/lib/tauri';
import type { AppConfig } from '@/lib/types';
import { isProxyConfigured } from '@/lib/types';
import {
  availableModels,
  inferProvider,
  modelOptionsForProvider,
} from '@/lib/models';

interface Props {
  onBack: () => void;
}

const REASONING_LEVELS = [
  { value: 'low', label: 'low' },
  { value: 'medium', label: 'medium' },
  { value: 'high', label: 'high' },
  { value: 'max', label: 'max' },
];

const PROVIDER_KEYS = ['deepseek', 'openai', 'anthropic'] as const;
const PROVIDER_LABELS: Record<string, string> = {
  deepseek: 'DeepSeek',
  openai: 'OpenAI',
  anthropic: 'Anthropic',
};

export default function SettingsPage({ onBack }: Props) {
  const {
    config,
    updateConfig,
    updateProvider,
    setDefaultModel,
    error,
    fetchedModels,
    applyFetchedModels,
    clearFetchedModels,
  } = useConfigStore();
  const [activeTab, setActiveTab] = useState<string>(() =>
    inferProvider(config.default_model),
  );
  const [fetchingProvider, setFetchingProvider] = useState<string | null>(null);
  const [fetchMsg, setFetchMsg] = useState<string | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout>>();
  const [localTemp, setLocalTemp] = useState(config.temperature);

  useEffect(() => {
    setLocalTemp(config.temperature);
  }, [config.temperature]);

  // Do NOT auto-switch channel tab when default model changes — that made
  // "获取列表" results disappear when user was inspecting another channel.

  const debouncedUpdate = useCallback((patch: Partial<AppConfig>) => {
    if (debounceRef.current) clearTimeout(debounceRef.current);
    debounceRef.current = setTimeout(() => {
      updateConfig(patch);
    }, 300);
  }, [updateConfig]);

  const handleFetchModels = async (provider: string) => {
    setFetchingProvider(provider);
    setFetchMsg(null);
    try {
      const models = await tauri.fetchModels(provider);
      await applyFetchedModels(provider, models);
      setFetchMsg(`已加载 ${models.length} 个真实模型，并同步默认/输入框`);
    } catch (e: any) {
      setFetchMsg(String(e));
    }
    setFetchingProvider(null);
  };

  const defaultModelOptions = useMemo(
    () => availableModels(config, fetchedModels),
    [config, fetchedModels],
  );

  const getModelOptions = (provider: string) =>
    modelOptionsForProvider(
      provider,
      fetchedModels[provider] || [],
      config.providers[provider]?.model,
      config.providers[provider]?.model_list,
    );

  return (
    <div className="flex-1 flex flex-col bg-main h-full">
      {/* 顶栏 */}
      <div className="flex items-center gap-3 px-6 py-3.5 border-b border-border shrink-0">
        <button className="text-gray-400 hover:text-gray-200 transition-colors" onClick={onBack}>
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <polyline points="15 18 9 12 15 6" />
          </svg>
        </button>
        <h2 className="text-base font-medium text-gray-200 tracking-wide">设置</h2>
      </div>

      {/* 错误信息 */}
      {error && (
        <div className="mx-8 mt-4 p-3 bg-red-900/15 border border-red-900/30 rounded-lg text-red-400 text-xs">{error}</div>
      )}

      <div className="flex-1 overflow-y-auto">
        <div className="max-w-xl mx-auto px-8 py-8 space-y-10">

          {/* ── 通用设置 ── */}
          <section>
            <h3 className="text-xs font-medium text-gray-500 uppercase tracking-widest mb-5">通用</h3>
            <div className="space-y-5">
              {/* 默认模型 — 与输入框模型选择、渠道模型同一数据源 */}
              <Row label="默认模型">
                <select
                  className="w-full bg-card border border-border rounded-lg px-3 py-2.5 text-sm text-gray-200 focus:outline-none focus:border-gray-500"
                  value={
                    defaultModelOptions.some((m) => m.id === config.default_model)
                      ? config.default_model
                      : defaultModelOptions[0]?.id || ''
                  }
                  onChange={(e) => {
                    const id = e.target.value;
                    const opt = defaultModelOptions.find((m) => m.id === id);
                    setDefaultModel(id, opt?.provider);
                  }}
                  disabled={defaultModelOptions.length === 0}
                >
                  {defaultModelOptions.length === 0 ? (
                    <option value="">请先启用渠道并「获取列表」</option>
                  ) : (
                    defaultModelOptions.map((m) => (
                      <option key={`${m.provider}:${m.id}`} value={m.id}>
                        {m.label} ({m.provider})
                      </option>
                    ))
                  )}
                </select>
                <p className="mt-1.5 text-[10px] text-gray-600">
                  仅已启用渠道 · 与输入框同一数据源 · 只显示「获取列表」扫到的真实模型
                  {config.active_provider
                    ? ` · 当前渠道：${PROVIDER_LABELS[config.active_provider] || config.active_provider}`
                    : ''}
                  {fetchedModels[config.active_provider]?.length
                    ? ` · 已扫描 ${fetchedModels[config.active_provider].length} 个`
                    : ' · 请在渠道页点击「获取列表」'}
                </p>
              </Row>

              {/* 推理深度 */}
              <Row label="推理深度">
                <div className="flex flex-col gap-1.5 items-end">
                  <div className="flex gap-1.5">
                    {REASONING_LEVELS.map((l) => (
                      <button
                        key={l.value}
                        type="button"
                        title="DeepSeek/OpenAI: reasoning_effort；Claude: extended thinking budget_tokens"
                        className={`px-4 py-2 rounded-md text-xs font-mono transition-colors ${
                          config.reasoning_effort === l.value
                            ? 'bg-gray-500/30 text-gray-100 ring-1 ring-gray-500/50'
                            : 'bg-card text-gray-500 hover:text-gray-300 border border-border'
                        }`}
                        onClick={() => updateConfig({ reasoning_effort: l.value })}
                      >
                        {l.label}
                      </button>
                    ))}
                  </div>
                  <p className="text-[10px] text-gray-600 max-w-xs text-right leading-snug">
                    DeepSeek/OpenAI 兼容：发 reasoning_effort（max→high）。Claude 原生：extended thinking
                    budget（low≈4k … max≈32k tokens）。调高更慢更贵，可能出现「思考过程」块。
                  </p>
                </div>
              </Row>

              {/* 最大 Token */}
              <Row label="回复长度限制">
                <label className="flex items-center gap-2 cursor-pointer">
                  <input type="checkbox" className="w-4 h-4 rounded accent-gray-500"
                    checked={config.max_tokens > 0}
                    onChange={(e) => updateConfig({ max_tokens: e.target.checked ? 8192 : 0 })}
                  />
                  <span className="text-xs text-gray-400">{config.max_tokens > 0 ? '已限制' : '不限制（默认）'}</span>
                </label>
                {config.max_tokens > 0 && (
                  <input
                    type="number" className="w-28 bg-card border border-border rounded-lg px-3 py-2.5 text-sm text-gray-200 focus:outline-none focus:border-gray-500 mt-2"
                    min={256} max={128000}
                    value={config.max_tokens}
                    onChange={(e) => {
                      const val = e.target.value;
                      const num = val === '' ? 0 : parseInt(val);
                      debouncedUpdate({ max_tokens: isNaN(num) ? 8192 : num });
                    }}
                  />
                )}
              </Row>

              {/* 温度 */}
              <Row label="温度">
                <div className="flex items-center gap-3">
                  <input type="range" min={0} max={2} step={0.1} value={localTemp}
                    onChange={(e) => {
                      const v = parseFloat(e.target.value);
                      setLocalTemp(v);
                      debouncedUpdate({ temperature: v });
                    }}
                    className="w-36 accent-gray-400" />
                  <span className="text-xs text-gray-500 font-mono w-6">{localTemp.toFixed(1)}</span>
                </div>
              </Row>

              {/* 上下文窗口 */}
              <Row label="上下文窗口">
                <select
                  className="w-full bg-card border border-border rounded-lg px-3 py-2.5 text-sm text-gray-200 focus:outline-none focus:border-gray-500"
                  value={config.context_window_tokens}
                  onChange={(e) => updateConfig({ context_window_tokens: parseInt(e.target.value) })}
                >
                  <option value={128000}>128K</option>
                  <option value={256000}>256K</option>
                  <option value={512000}>512K</option>
                  <option value={1000000}>1M (默认)</option>
                </select>
              </Row>

              {/* 压缩阈值 */}
              <Row label="压缩阈值">
                <div className="flex items-center gap-3">
                  <input type="range" min={0.6} max={0.95} step={0.05}
                    value={config.context_compress_threshold}
                    onChange={(e) => debouncedUpdate({ context_compress_threshold: parseFloat(e.target.value) })}
                    className="w-36 accent-gray-400" />
                  <span className="text-xs text-gray-500 font-mono w-10">{(config.context_compress_threshold * 100).toFixed(0)}%</span>
                </div>
              </Row>

              {/* 会话保留 */}
              <Row label="会话保留天数">
                <input
                  type="number" className="w-24 bg-card border border-border rounded-lg px-3 py-2.5 text-sm text-gray-200 focus:outline-none focus:border-gray-500"
                  min={1} max={365}
                  value={config.retention_days}
                  onChange={(e) => updateConfig({ retention_days: parseInt(e.target.value) || 30 })}
                />
              </Row>
            </div>
          </section>

          {/* ── 代理 ── */}
          <section>
            <h3 className="text-xs font-medium text-gray-500 uppercase tracking-widest mb-5">网络代理</h3>
            <div className="space-y-5">
              <Row label="代理地址">
                <input
                  type="text"
                  className="w-full bg-card border border-border rounded-lg px-3 py-2.5 text-sm text-gray-200 focus:outline-none focus:border-gray-500 font-mono"
                  placeholder="http://127.0.0.1:7890 或 socks5://127.0.0.1:1080"
                  value={config.proxy?.url || ''}
                  onChange={(e) =>
                    updateConfig({
                      proxy: { ...config.proxy, url: e.target.value },
                    })
                  }
                />
                <p className="mt-1.5 text-[10px] text-gray-600">
                  支持 http / https / socks5。留空表示未配置；未配置时各处代理开关不可用。
                  {isProxyConfigured(config.proxy?.url) ? (
                    <span className="text-emerald-500/80"> · 已识别有效代理</span>
                  ) : config.proxy?.url?.trim() ? (
                    <span className="text-amber-500/80"> · 地址无效</span>
                  ) : null}
                </p>
              </Row>

              <Row label="全局启用">
                <label
                  className={`flex items-center gap-2 ${
                    isProxyConfigured(config.proxy?.url) ? 'cursor-pointer' : 'opacity-40 cursor-not-allowed'
                  }`}
                >
                  <input
                    type="checkbox"
                    className="w-4 h-4 rounded accent-gray-500"
                    checked={!!config.proxy?.global && isProxyConfigured(config.proxy?.url)}
                    disabled={!isProxyConfigured(config.proxy?.url)}
                    onChange={(e) =>
                      updateConfig({
                        proxy: { ...config.proxy, global: e.target.checked },
                      })
                    }
                  />
                  <span className="text-xs text-gray-400">
                    强制全软件走代理（模型 / MCP / Skill 下载全部启用，各处不可单独关闭）
                  </span>
                </label>
              </Row>

              <Row label="MCP 连接">
                <label
                  className={`flex items-center gap-2 ${
                    isProxyConfigured(config.proxy?.url) && !config.proxy?.global
                      ? 'cursor-pointer'
                      : 'opacity-40 cursor-not-allowed'
                  }`}
                >
                  <input
                    type="checkbox"
                    className="w-4 h-4 rounded accent-gray-500"
                    checked={
                      isProxyConfigured(config.proxy?.url) &&
                      (config.proxy?.global || config.mcp_use_proxy)
                    }
                    disabled={
                      !isProxyConfigured(config.proxy?.url) || !!config.proxy?.global
                    }
                    onChange={(e) => updateConfig({ mcp_use_proxy: e.target.checked })}
                  />
                  <span className="text-xs text-gray-400">
                    MCP 进程（npx 等）使用代理
                    {config.proxy?.global ? ' · 全局已强制' : ''}
                  </span>
                </label>
              </Row>

              <Row label="Skill 下载">
                <label
                  className={`flex items-center gap-2 ${
                    isProxyConfigured(config.proxy?.url) && !config.proxy?.global
                      ? 'cursor-pointer'
                      : 'opacity-40 cursor-not-allowed'
                  }`}
                >
                  <input
                    type="checkbox"
                    className="w-4 h-4 rounded accent-gray-500"
                    checked={
                      isProxyConfigured(config.proxy?.url) &&
                      (config.proxy?.global || config.skills_use_proxy)
                    }
                    disabled={
                      !isProxyConfigured(config.proxy?.url) || !!config.proxy?.global
                    }
                    onChange={(e) => updateConfig({ skills_use_proxy: e.target.checked })}
                  />
                  <span className="text-xs text-gray-400">
                    第三方 Skill git 克隆走代理
                    {config.proxy?.global ? ' · 全局已强制' : ''}
                  </span>
                </label>
              </Row>
            </div>
          </section>

          {/* ── Provider 配置 ── */}
          <section>
            <div className="flex items-center gap-1 mb-5 border-b border-border">
              {PROVIDER_KEYS.map((k) => (
                <button
                  key={k}
                  className={`px-4 py-2 text-xs transition-colors border-b-2 -mb-px ${
                    activeTab === k
                      ? 'border-gray-400 text-gray-200'
                      : 'border-transparent text-gray-500 hover:text-gray-400'
                  }`}
                  onClick={() => setActiveTab(k)}
                >
                  {PROVIDER_LABELS[k]}
                </button>
              ))}
            </div>

            {PROVIDER_KEYS.map((k) => {
              if (activeTab !== k) return null;
              const prov = config.providers[k];
              return (
                <div key={k} className="space-y-5">
                  <Row label="启用">
                    <label className="flex items-center gap-2 cursor-pointer">
                      <input type="checkbox" className="w-4 h-4 rounded accent-gray-500"
                        checked={prov.enabled}
                        onChange={(e) => updateProvider(k, { enabled: e.target.checked })} />
                      <span className="text-xs text-gray-400">启用 {PROVIDER_LABELS[k]}</span>
                    </label>
                  </Row>

                  <Row label="使用代理">
                    <label
                      className={`flex items-center gap-2 ${
                        isProxyConfigured(config.proxy?.url) && !config.proxy?.global
                          ? 'cursor-pointer'
                          : 'opacity-40 cursor-not-allowed'
                      }`}
                    >
                      <input
                        type="checkbox"
                        className="w-4 h-4 rounded accent-gray-500"
                        checked={
                          isProxyConfigured(config.proxy?.url) &&
                          (config.proxy?.global || !!prov.use_proxy)
                        }
                        disabled={
                          !isProxyConfigured(config.proxy?.url) || !!config.proxy?.global
                        }
                        onChange={(e) =>
                          updateProvider(k, { use_proxy: e.target.checked })
                        }
                      />
                      <span className="text-xs text-gray-400">
                        该渠道请求走代理
                        {!isProxyConfigured(config.proxy?.url)
                          ? '（请先配置有效代理）'
                          : config.proxy?.global
                            ? ' · 全局已强制'
                            : ''}
                      </span>
                    </label>
                  </Row>

                  <Row label="API 密钥">
                    <input type="password"
                      className="w-full bg-card border border-border rounded-lg px-3 py-2.5 text-sm text-gray-200 focus:outline-none focus:border-gray-500 font-mono"
                      value={prov.api_key}
                      onChange={(e) => updateProvider(k, { api_key: e.target.value })}
                      placeholder="sk-..." />
                  </Row>

                  <Row label="接口地址">
                    <input type="text"
                      className="w-full bg-card border border-border rounded-lg px-3 py-2.5 text-sm text-gray-200 focus:outline-none focus:border-gray-500 font-mono"
                      value={prov.base_url}
                      onChange={(e) => updateProvider(k, { base_url: e.target.value })}
                      placeholder="https://api.example.com/v1" />
                  </Row>

                  <Row label="模型" action={
                    <button className="text-xs text-gray-500 hover:text-gray-300 transition-colors"
                      onClick={() => handleFetchModels(k)} disabled={fetchingProvider === k}>
                      {fetchingProvider === k ? '获取中...' : (fetchedModels[k]?.length ? `已加载 ${fetchedModels[k].length} 个` : '获取列表')}
                    </button>
                  }>
                    {(() => {
                      const opts = getModelOptions(k);
                      const preferred =
                        config.active_provider === k
                          ? config.default_model
                          : prov.model;
                      const selectValue = opts.some((m) => m.id === preferred)
                        ? preferred
                        : opts[0]?.id || '';
                      const fetched = fetchedModels[k] || [];
                      return (
                        <>
                          <select
                            className="w-full bg-card border border-border rounded-lg px-3 py-2.5 text-sm text-gray-200 focus:outline-none focus:border-gray-500 disabled:opacity-40"
                            disabled={!prov.enabled || opts.length === 0}
                            value={selectValue}
                            onChange={(e) => {
                              const id = e.target.value;
                              updateProvider(k, { model: id });
                              if (prov.enabled) setDefaultModel(id, k);
                            }}
                          >
                            {opts.length === 0 ? (
                              <option value="">暂无模型 — 点「获取列表」</option>
                            ) : (
                              opts.map((m) => (
                                <option key={m.id} value={m.id}>
                                  {m.label}
                                </option>
                              ))
                            )}
                          </select>

                          {/* Scrollable list so long API results are actually readable */}
                          {fetched.length > 0 && (
                            <div className="mt-2 rounded-lg border border-border bg-card/60 max-h-48 overflow-y-auto">
                              <div className="sticky top-0 px-2.5 py-1.5 text-[10px] text-gray-500 border-b border-border/60 bg-card/95 backdrop-blur-sm flex items-center justify-between">
                                <span>已扫描 {fetched.length} 个真实模型（点击选用）</span>
                                <button
                                  type="button"
                                  className="text-gray-600 hover:text-gray-300"
                                  onClick={() => clearFetchedModels(k)}
                                >
                                  清除缓存
                                </button>
                              </div>
                              {opts.map((m) => {
                                const selected = m.id === selectValue;
                                return (
                                  <button
                                    key={m.id}
                                    type="button"
                                    disabled={!prov.enabled}
                                    onClick={() => {
                                      updateProvider(k, { model: m.id });
                                      if (prov.enabled) setDefaultModel(m.id, k);
                                    }}
                                    className={`w-full text-left px-2.5 py-1.5 text-[11px] font-mono border-b border-white/[0.03] last:border-0 transition-colors disabled:opacity-40 ${
                                      selected
                                        ? 'bg-gray-600/40 text-gray-100'
                                        : 'text-gray-400 hover:bg-white/[0.04] hover:text-gray-200'
                                    }`}
                                  >
                                    {m.id}
                                  </button>
                                );
                              })}
                            </div>
                          )}

                          {!prov.enabled && (
                            <p className="mt-1.5 text-[10px] text-gray-600">
                              渠道未启用 — 启用后才会出现在默认模型/输入框列表
                            </p>
                          )}
                          {prov.enabled && config.active_provider === k && (
                            <p className="mt-1.5 text-[10px] text-emerald-500/80">
                              正在作为默认/输入框模型使用
                            </p>
                          )}
                          {prov.enabled && !fetched.length && (
                            <p className="mt-1.5 text-[10px] text-gray-600">
                              点击「获取列表」从接口加载完整模型（显示在下方列表）
                            </p>
                          )}
                          {fetchMsg && activeTab === k && (
                            <p className={`mt-1.5 text-xs ${fetchMsg.startsWith('已加载') ? 'text-emerald-500/80' : 'text-red-400/90'}`}>
                              {fetchMsg}
                            </p>
                          )}
                        </>
                      );
                    })()}
                  </Row>
                </div>
              );
            })}
          </section>

        </div>
      </div>
    </div>
  );
}

function Row({ label, action, children }: { label: string; action?: React.ReactNode; children: React.ReactNode }) {
  return (
    <div className="flex items-start gap-6">
      <div className="w-28 shrink-0 pt-2.5 flex items-center justify-between">
        <span className="text-xs text-gray-400">{label}</span>
        {action}
      </div>
      <div className="flex-1">{children}</div>
    </div>
  );
}
