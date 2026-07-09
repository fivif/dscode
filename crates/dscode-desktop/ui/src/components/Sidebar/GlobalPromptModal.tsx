import { useEffect, useState } from 'react';
import * as tauri from '@/lib/tauri';

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
      className="fixed inset-0 z-50 bg-black/55 flex items-center justify-center p-4"
      onClick={onClose}
    >
      <div
        className="bg-card border border-border rounded-xl w-full max-w-2xl shadow-2xl flex flex-col max-h-[90vh]"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-center gap-3 px-5 py-4 border-b border-border shrink-0">
          <svg
            width="18"
            height="18"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="1.8"
            strokeLinecap="round"
            strokeLinejoin="round"
            className="text-sky-400/90 shrink-0"
            aria-hidden
          >
            <path d="M12 20h9" />
            <path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z" />
          </svg>
          <div className="min-w-0 flex-1">
            <h3 className="text-base font-medium text-gray-200">全局提示词</h3>
            <p className="text-[11px] text-gray-500 mt-0.5">
              作用于所有会话的系统提示词；可追加到内置说明，或完全替换
            </p>
          </div>
          <button
            className="text-gray-500 hover:text-gray-300 p-1"
            onClick={onClose}
            title="关闭"
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        </div>

        <div className="px-5 py-4 overflow-y-auto flex-1 space-y-4">
          {loading ? (
            <div className="text-sm text-gray-500 py-8 text-center">加载中…</div>
          ) : (
            <>
              {error && (
                <div className="p-2.5 bg-red-900/15 border border-red-900/30 rounded text-red-400 text-xs">
                  {error}
                </div>
              )}
              {success && (
                <div className="p-2.5 bg-emerald-900/15 border border-emerald-900/30 rounded text-emerald-400 text-xs">
                  {success}
                </div>
              )}

              <div className="flex flex-wrap items-center gap-3">
                <label className="inline-flex items-center gap-2 text-xs text-gray-300 cursor-pointer select-none">
                  <input
                    type="checkbox"
                    className="rounded border-border bg-input"
                    checked={replace}
                    onChange={(e) => setReplace(e.target.checked)}
                  />
                  替换内置系统提示词
                </label>
                <span className="text-[11px] text-gray-600">
                  {replace
                    ? '仅使用下方内容作为 system prompt'
                    : '下方内容会追加在内置提示词之后'}
                </span>
              </div>

              <div>
                <div className="flex items-center justify-between mb-1.5">
                  <label className="text-xs text-gray-400">
                    {replace ? '系统提示词' : '自定义指令（追加）'}
                  </label>
                  <span className="text-[10px] text-gray-600">{text.length} 字符</span>
                </div>
                <textarea
                  className="w-full min-h-[180px] max-h-[40vh] bg-input border border-border rounded-lg px-3 py-2.5 text-sm text-gray-200 font-mono leading-relaxed resize-y focus:outline-none focus:border-gray-500"
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

              <div className="border border-border/60 rounded-lg overflow-hidden">
                <button
                  type="button"
                  className="w-full flex items-center gap-2 px-3 py-2 text-left text-xs text-gray-400 hover:bg-white/[0.02]"
                  onClick={() => setShowDefault((v) => !v)}
                >
                  <svg
                    className={`w-3 h-3 transition-transform ${showDefault ? 'rotate-90' : ''}`}
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2"
                  >
                    <polyline points="9 18 15 12 9 6" />
                  </svg>
                  内置默认提示词（只读）
                </button>
                {showDefault && (
                  <pre className="px-3 pb-3 text-[11px] text-gray-500 font-mono whitespace-pre-wrap max-h-40 overflow-y-auto border-t border-border/40 pt-2">
                    {defaultPrompt || '（无）'}
                  </pre>
                )}
              </div>

              <div className="border border-border/60 rounded-lg overflow-hidden">
                <button
                  type="button"
                  className="w-full flex items-center gap-2 px-3 py-2 text-left text-xs text-gray-400 hover:bg-white/[0.02]"
                  onClick={() => setShowPreview((v) => !v)}
                >
                  <svg
                    className={`w-3 h-3 transition-transform ${showPreview ? 'rotate-90' : ''}`}
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2"
                  >
                    <polyline points="9 18 15 12 9 6" />
                  </svg>
                  生效预览
                  <span className="text-[10px] text-gray-600 ml-auto">
                    {effectivePreview.length} 字符
                  </span>
                </button>
                {showPreview && (
                  <pre className="px-3 pb-3 text-[11px] text-gray-500 font-mono whitespace-pre-wrap max-h-48 overflow-y-auto border-t border-border/40 pt-2">
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
            className="text-xs text-gray-500 hover:text-red-400 px-2 py-1.5 disabled:opacity-40"
            onClick={handleClear}
            disabled={loading || saving || !text.trim()}
          >
            清空
          </button>
          <div className="flex-1" />
          <button
            type="button"
            className="px-3 py-1.5 text-sm text-gray-400 hover:text-gray-200"
            onClick={onClose}
            disabled={saving}
          >
            取消
          </button>
          <button
            type="button"
            className="px-4 py-1.5 text-sm text-white bg-gray-600 hover:bg-gray-500 rounded-lg transition-colors disabled:opacity-40"
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
