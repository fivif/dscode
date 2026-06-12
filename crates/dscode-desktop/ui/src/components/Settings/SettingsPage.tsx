import { useState, useRef, useCallback, useEffect } from 'react';
import { useConfigStore } from '@/stores/configStore';
import { KNOWN_MODELS } from '@/lib/types';
import * as tauri from '@/lib/tauri';
import type { AppConfig } from '@/lib/types';

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
  const { config, updateConfig, updateProvider, error } = useConfigStore();
  const [activeTab, setActiveTab] = useState<string>('deepseek');
  const [fetchedModels, setFetchedModels] = useState<Record<string, string[]>>({});
  const [fetchingProvider, setFetchingProvider] = useState<string | null>(null);
  const [fetchMsg, setFetchMsg] = useState<string | null>(null);
  const debounceRef = useRef<ReturnType<typeof setTimeout>>();
  const [localTemp, setLocalTemp] = useState(config.temperature);

  useEffect(() => {
    setLocalTemp(config.temperature);
  }, [config.temperature]);

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
      setFetchedModels((prev) => ({ ...prev, [provider]: models }));
      setFetchMsg(`已加载 ${models.length} 个模型`);
    } catch (e: any) {
      setFetchMsg(String(e));
    }
    setFetchingProvider(null);
  };

  const getModelOptions = (provider: string) => {
    const known = KNOWN_MODELS
      .filter((m) => m.provider === provider)
      .map((m) => ({ id: m.id, label: m.display }));
    const fetched = (fetchedModels[provider] || []).map((id) => {
      const knownMatch = known.find((k) => k.id === id);
      return knownMatch || { id, label: id };
    });
    // Merge: fetched first, then known that aren't in fetched
    const seen = new Set(fetched.map((f) => f.id));
    const extra = known.filter((k) => !seen.has(k.id));
    return [...fetched, ...extra];
  };

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
              {/* 默认模型 */}
              <Row label="默认模型">
                <select
                  className="w-full bg-card border border-border rounded-lg px-3 py-2.5 text-sm text-gray-200 focus:outline-none focus:border-gray-500"
                  value={config.default_model}
                  onChange={(e) => updateConfig({ default_model: e.target.value })}
                >
                  {(() => {
                    const opts = PROVIDER_KEYS.filter((p) => config.providers[p]?.enabled).flatMap((p) => getModelOptions(p));
                    const hasCurrent = opts.some((m) => m.id === config.default_model);
                    if (!hasCurrent && config.default_model) {
                      opts.unshift({ id: config.default_model, label: config.default_model });
                    }
                    return opts.map((m) => (
                      <option key={m.id} value={m.id}>{m.label}</option>
                    ));
                  })()}
                </select>
              </Row>

              {/* 推理深度 */}
              <Row label="推理深度">
                <div className="flex gap-1.5">
                  {REASONING_LEVELS.map((l) => (
                    <button
                      key={l.value}
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
                    <select
                      className="w-full bg-card border border-border rounded-lg px-3 py-2.5 text-sm text-gray-200 focus:outline-none focus:border-gray-500"
                      value={prov.model}
                      onChange={(e) => updateProvider(k, { model: e.target.value })}
                    >
                      {getModelOptions(k).map((m) => (
                        <option key={m.id} value={m.id}>{m.label}</option>
                      ))}
                    </select>
                    {fetchMsg && (
                      <p className="mt-1.5 text-xs text-gray-500">{fetchMsg}</p>
                    )}
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
