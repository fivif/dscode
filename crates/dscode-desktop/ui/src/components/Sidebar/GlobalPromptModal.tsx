import { useEffect, useState } from 'react';
import * as tauri from '@/lib/tauri';
import { IconChevronRight16, IconClose16, IconPencil16 } from '@/components/icons';

interface Props {
  onClose: () => void;
}

export default function GlobalPromptModal({ onClose }: Props) {
  const [text, setText] = useState('');
  const [replace, setReplace] = useState(false);
  const [defaultPrompt, setDefaultPrompt] = useState('');
  const [showDefault, setShowDefault] = useState(false);
  const [showPreview, setShowPreview] = useState(false);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [success, setSuccess] = useState('');

  useEffect(() => {
    let cancelled = false;
    (async () => {
      setLoading(true);
      setError('');
      try {
        const info = await tauri.getGlobalPrompt();
        if (cancelled) return;
        setText(info.global_prompt || '');
        setReplace(!!info.replace_system_prompt);
        setDefaultPrompt(info.default_prompt || '');
      } catch (e: any) {
        if (!cancelled) setError(String(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  const effectivePreview = (() => {
    const custom = text.trim();
    if (!custom) return defaultPrompt;
    if (replace) return custom;
    return `${defaultPrompt}\n\n## User global instructions\n${custom}`;
  })();

  const handleSave = async () => {
    setSaving(true);
    setError('');
    setSuccess('');
    try {
      await tauri.setGlobalPrompt(text, replace);
      setSuccess('已保存，新对话回合立即生效');
      setTimeout(() => onClose(), 600);
    } catch (e: any) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const handleClear = () => {
    if (!text.trim()) return;
    if (!confirm('清空自定义全局提示词？将恢复为仅使用内置提示词。')) return;
    setText('');
    setReplace(false);
  };

  return (
    <div
      className="fixed inset-0 z-50 bg-black/50 backdrop-blur-sm flex items-center justify-center p-4"
      onClick={onClose}
    >
      <div
        className="menu w-full max-w-2xl shadow-modal flex flex-col max-h-[90vh]"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center gap-3 px-5 py-4 border-b border-border shrink-0">
          <IconPencil16 size={18} className="text-accent shrink-0" />
          <div className="min-w-0 flex-1">
            <h3 className="text-[15px] font-semibold text-primary">全局提示词</h3>
            <p className="text-[11px] text-muted mt-0.5">
              作用于所有会话的系统提示词；可追加到内置说明，或完全替换
            </p>
          </div>
          <button
            className="icon-btn"
            onClick={onClose}
            title="关闭"
          >
            <IconClose16 size={18} />
          </button>
        </div>

        <div className="px-5 py-4 overflow-y-auto flex-1 space-y-4">
          {loading ? (
            <div className="text-[13px] text-muted py-8 text-center">加载中…</div>
          ) : (
            <>
              {error && (
                <div className="p-2.5 bg-danger/10 border border-danger/40 rounded-control text-danger text-[13px]">
                  {error}
                </div>
              )}
              {success && (
                <div className="p-2.5 bg-success/10 border border-success/40 rounded-control text-success text-[13px]">
                  {success}
                </div>
              )}

              <div className="flex flex-wrap items-center gap-3">
                <label className="inline-flex items-center gap-2 text-[13px] text-secondary cursor-pointer select-none">
                  <input
                    type="checkbox"
                    className="rounded border-border bg-input focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
                    checked={replace}
                    onChange={(e) => setReplace(e.target.checked)}
                  />
                  替换内置系统提示词
                </label>
                <span className="text-[11px] text-faint">
                  {replace
                    ? '仅使用下方内容作为 system prompt'
                    : '下方内容会追加在内置提示词之后'}
                </span>
              </div>

              <div>
                <div className="flex items-center justify-between mb-1.5">
                  <label className="text-[13px] text-secondary">
                    {replace ? '系统提示词' : '自定义指令（追加）'}
                  </label>
                  <span className="text-[11px] text-faint">{text.length} 字符</span>
                </div>
                <textarea
                  className="field min-h-[180px] max-h-[40vh] font-mono leading-relaxed resize-y"
                  placeholder={
                    replace
                      ? '完整系统提示词…\n例如：You are a careful coding agent. Always…'
                      : '追加规则，例如：\n- 始终使用中文回复\n- 修改代码前先说明计划\n- 优先复用现有工具与 skill'
                  }
                  value={text}
                  onChange={(e) => setText(e.target.value)}
                  spellCheck={false}
                />
              </div>

              <div className="panel overflow-hidden">
                <button
                  type="button"
                  className="w-full flex items-center gap-2 px-3 py-2 text-left text-[13px] text-secondary hover:bg-hover transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
                  onClick={() => setShowDefault((v) => !v)}
                >
                  <IconChevronRight16
                    size={12}
                    className={`transition-transform ${showDefault ? 'rotate-90' : ''}`}
                  />
                  内置默认提示词（只读）
                </button>
                {showDefault && (
                  <pre className="px-3 pb-3 text-[11px] text-muted font-mono whitespace-pre-wrap max-h-40 overflow-y-auto border-t border-border/40 pt-2">
                    {defaultPrompt || '（无）'}
                  </pre>
                )}
              </div>

              <div className="panel overflow-hidden">
                <button
                  type="button"
                  className="w-full flex items-center gap-2 px-3 py-2 text-left text-[13px] text-secondary hover:bg-hover transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
                  onClick={() => setShowPreview((v) => !v)}
                >
                  <IconChevronRight16
                    size={12}
                    className={`transition-transform ${showPreview ? 'rotate-90' : ''}`}
                  />
                  生效预览
                  <span className="text-[11px] text-faint ml-auto">
                    {effectivePreview.length} 字符
                  </span>
                </button>
                {showPreview && (
                  <pre className="px-3 pb-3 text-[11px] text-muted font-mono whitespace-pre-wrap max-h-48 overflow-y-auto border-t border-border/40 pt-2">
                    {effectivePreview}
                  </pre>
                )}
              </div>
            </>
          )}
        </div>

        <div className="flex items-center gap-2 px-5 py-3.5 border-t border-border shrink-0">
          <button
            type="button"
            className="btn btn-danger disabled:opacity-40"
            onClick={handleClear}
            disabled={loading || saving || !text.trim()}
          >
            清空
          </button>
          <div className="flex-1" />
          <button
            type="button"
            className="btn btn-ghost"
            onClick={onClose}
            disabled={saving}
          >
            取消
          </button>
          <button
            type="button"
            className="btn btn-primary px-4 disabled:opacity-40"
            onClick={handleSave}
            disabled={loading || saving}
          >
            {saving ? '保存中…' : '保存'}
          </button>
        </div>
      </div>
    </div>
  );
}
