import { useState } from 'react';
import type { PlanChoice } from '@/lib/types';
import { useChatStore } from '@/stores/chatStore';

interface Props {
  messageId: string;
  choice: PlanChoice;
}

/**
 * Interactive /plan answers: option buttons + custom input.
 */
export default function PlanChoiceCard({ messageId, choice }: Props) {
  const [custom, setCustom] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const isStreaming = useChatStore((s) => s.isStreaming);
  const sendMessage = useChatStore((s) => s.sendMessage);
  const markPlanAnswered = useChatStore((s) => s.markPlanAnswered);

  const disabled = !!choice.answered || isStreaming || submitting;

  const submit = async (answer: string) => {
    const text = answer.trim();
    if (!text || disabled) return;
    setSubmitting(true);
    markPlanAnswered(messageId, text);
    try {
      await sendMessage(text);
    } finally {
      setSubmitting(false);
    }
  };

  // Build unique options list (recommended first if present)
  const options: string[] = [];
  const push = (s: string) => {
    const t = s.trim();
    if (!t) return;
    if (options.some((o) => o.toLowerCase() === t.toLowerCase())) return;
    options.push(t);
  };
  if (choice.recommended) push(choice.recommended);
  for (const o of choice.options || []) push(o);

  return (
    <div className="mt-2 mb-1 rounded-lg border border-white/[0.1] bg-white/[0.03] overflow-hidden max-w-[90%]">
      <div className="px-3 py-1.5 border-b border-white/[0.06] flex items-center gap-2 text-[11px]">
        <span className="text-gray-300 font-medium">/plan · {choice.phase || 'Interview'}</span>
        {choice.remaining > 0 && (
          <span className="text-gray-600">· ~{choice.remaining} left this phase</span>
        )}
        {choice.answered && (
          <span className="text-emerald-400/80 ml-auto">已选</span>
        )}
      </div>

      {choice.auto_notes?.length > 0 && (
        <div className="px-3 py-2 border-b border-white/[0.04] text-[11px] text-gray-500 space-y-0.5">
          {choice.auto_notes.map((n, i) => (
            <div key={i}>· {n}</div>
          ))}
        </div>
      )}

      <div className="px-3 py-2.5 space-y-2">
        <p className="text-[13px] text-gray-200 leading-relaxed">{choice.question}</p>

        {options.length > 0 && (
          <div className="flex flex-col gap-1.5">
            {options.map((opt, i) => {
              const isRec =
                choice.recommended &&
                opt.toLowerCase() === choice.recommended.trim().toLowerCase();
              const selected = choice.selected === opt;
              return (
                <button
                  key={`${i}-${opt.slice(0, 24)}`}
                  type="button"
                  disabled={disabled}
                  onClick={() => submit(opt)}
                  className={`text-left text-[12px] px-3 py-2 rounded-md border transition-colors ${
                    selected
                      ? 'border-emerald-500/40 bg-emerald-500/10 text-gray-100'
                      : isRec
                        ? 'border-gray-500/50 bg-white/[0.05] text-gray-100 hover:border-gray-400 hover:bg-white/[0.08]'
                        : 'border-white/[0.08] bg-transparent text-gray-300 hover:border-white/20 hover:bg-white/[0.04]'
                  } disabled:opacity-50 disabled:cursor-not-allowed`}
                >
                  <span className="flex items-start gap-2">
                    {isRec && (
                      <span className="text-[10px] text-gray-400 shrink-0 mt-0.5 px-1 rounded bg-white/[0.06]">
                        推荐
                      </span>
                    )}
                    <span className="leading-snug">{opt}</span>
                  </span>
                </button>
              );
            })}
          </div>
        )}

        {/* Custom answer */}
        <div className="pt-1">
          <div className="text-[10px] text-gray-600 mb-1">自定义回答</div>
          <div className="flex gap-2">
            <input
              type="text"
              className="flex-1 min-w-0 bg-black/30 border border-white/[0.08] rounded-md px-2.5 py-1.5 text-[12px] text-gray-200 placeholder-gray-600 focus:outline-none focus:border-gray-500 disabled:opacity-50"
              placeholder={disabled ? '已提交' : '输入自定义答案…'}
              value={custom}
              disabled={disabled}
              onChange={(e) => setCustom(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter' && custom.trim()) {
                  e.preventDefault();
                  submit(custom);
                }
              }}
            />
            <button
              type="button"
              disabled={disabled || !custom.trim()}
              onClick={() => submit(custom)}
              className="shrink-0 px-3 py-1.5 text-[11px] rounded-md bg-gray-600 text-white hover:bg-gray-500 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              提交
            </button>
          </div>
        </div>

        {!choice.answered && (
          <div className="text-[10px] text-gray-600 pt-0.5">
            点选选项或自定义提交 · 也可在输入框直接回复 ·{' '}
            <button
              type="button"
              className="text-gray-500 hover:text-gray-300 underline"
              disabled={isStreaming}
              onClick={() => submit('/plan cancel')}
            >
              取消访谈
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
