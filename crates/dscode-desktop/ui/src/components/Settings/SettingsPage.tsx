import { useMemo, useState } from 'react';
import { IconChevronLeft16 } from '@/components/icons';
import { useConfigStore } from '@/stores/configStore';
import { availableModels } from '@/lib/models';
import type { FieldContext, SectionId } from './fieldSchema';
import { SECTIONS, SectionFields } from './settingsFields';
import ChannelSection from './ChannelSection';

interface Props {
  onBack: () => void;
}

/**
 * Settings shell: a section rail on the left, the selected section's fields on
 * the right. The field list itself is the descriptor table in
 * `settingsFields.tsx` — this file only decides which section is showing.
 */
export default function SettingsPage({ onBack }: Props) {
  const {
    config,
    updateConfig,
    updateProvider,
    setDefaultModel,
    error,
    fetchedModels,
  } = useConfigStore();
  const [section, setSection] = useState<SectionId>('general');

  const defaultModelOptions = useMemo(
    () => availableModels(config, fetchedModels),
    [config, fetchedModels],
  );

  const ctx = useMemo<FieldContext>(
    () => ({
      config,
      fetchedModels,
      defaultModelOptions,
      set: (patch) => {
        void updateConfig(patch);
      },
      setProvider: (key, patch) => {
        void updateProvider(key, patch);
      },
      setDefaultModel: (id, provider) => {
        void setDefaultModel(id, provider);
      },
    }),
    [config, fetchedModels, defaultModelOptions, updateConfig, updateProvider, setDefaultModel],
  );

  const active = SECTIONS.find((s) => s.id === section) ?? SECTIONS[0];

  return (
    <div className="flex-1 flex flex-col bg-main h-full">
      {/* 顶栏 */}
      <div className="flex items-center gap-3 px-6 py-3.5 border-b border-border shrink-0">
        <button className="icon-btn" title="返回" aria-label="返回" onClick={onBack}>
          <IconChevronLeft16 size={20} />
        </button>
        <div className="min-w-0">
          <h2 className="text-[15px] font-semibold text-primary leading-tight tracking-wide">设置</h2>
          <p className="text-[13px] text-secondary leading-tight">模型、渠道、代理与图像生成</p>
        </div>
      </div>

      {/* 错误信息 */}
      {error && (
        <div className="mx-8 mt-4 p-3 bg-danger/10 border border-danger/30 rounded-card text-danger text-[13px]">{error}</div>
      )}

      <div className="flex-1 flex min-h-0">
        {/* 分区导航 */}
        <nav
          aria-label="设置分区"
          className="w-44 shrink-0 border-r border-border overflow-y-auto py-3"
        >
          {SECTIONS.map((s) => (
            <button
              key={s.id}
              type="button"
              aria-current={section === s.id ? 'page' : undefined}
              className={`row focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50 ${
                section === s.id ? 'active' : ''
              }`}
              onClick={() => setSection(s.id)}
            >
              {s.label}
            </button>
          ))}
        </nav>

        {/* 分区内容 */}
        <div className="flex-1 min-w-0 overflow-y-auto">
          <div className="max-w-xl px-8 py-6">
            <section className="panel p-5">
              <div className="mb-5">
                <h3 className="text-[13px] font-semibold text-primary">{active.label}</h3>
                <p className="text-[11px] text-muted mt-1">{active.description}</p>
              </div>
              {section === 'channels' ? (
                <ChannelSection ctx={ctx} />
              ) : (
                <SectionFields section={section} ctx={ctx} />
              )}
            </section>
          </div>
        </div>
      </div>
    </div>
  );
}
