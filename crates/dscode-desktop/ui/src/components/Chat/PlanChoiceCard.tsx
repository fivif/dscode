import { useState } from 'react';
import type { PlanChoice } from '@/lib/types';
import { useChatStore } from '@/stores/chatStore';
import { IconCheck16 } from '@/components/icons';

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
    <div className="mt-2 mb-1 panel overflow-hidden max-w-[90%]">
      <div className="px-3 py-1.5 border-b border-divider flex items-center gap-2 text-[11px]">
        <span className="text-primary font-medium">/plan · {choice.phase || 'Interview'}</span>
        {choice.remaining > 0 && (
          <span className="text-muted">· ~{choice.remaining} left this phase</span>
        )}
        {choice.answered && (
          <span className="ml-auto text-accent bg-accent-soft border border-accent/40 rounded-full px-1.5 py-0.5">
            已选
          </span>
        )}
      </div>

      {choice.auto_notes?.length > 0 && (
        <div className="px-3 py-2 border-b border-divider text-[11px] text-muted space-y-0.5">
          {choice.auto_notes.map((n, i) => (
            <div key={i}>· {n}</div>
          ))}
        </div>
      )}

      <div className="px-3 py-2.5 space-y-2">
        <p className="text-[13px] text-primary leading-relaxed">{choice.question}</p>

        {options.length > 0 && (
          <div className="flex flex-col gap-1.5">
            {options.map((opt, i) => {
              const isRec =
                choice.recommended &&
                opt.toLowerCase() === choice.recommended.trim().toLowerCase();
              const selected = choice.selected === opt;
              const stateCls = selected
                ? 'bg-accent-soft border-accent/40 text-primary'
                : isRec
                  ? 'border-accent/40 bg-card text-primary hover:bg-hover'
                  : 'border-border bg-transparent text-secondary hover:bg-hover hover:border-accent/40 hover:text-primary';
              return (
                <button
                  key={`${i}-${opt.slice(0, 24)}`}
                  type="button"
                  disabled={disabled}
                  onClick={() => submit(opt)}
                  className={`w-full text-left text-[13px] px-3 py-2 rounded-control border transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50 disabled:opacity-50 disabled:cursor-not-allowed ${stateCls}`}
                >
                  <span className="flex items-start gap-2">
                    {selected && (
                      <IconCheck16 size={12} className="text-accent shrink-0 mt-0.5" />
                    )}
                    {isRec && (
                      <span className="text-[11px] text-accent shrink-0 mt-0.5 px-1.5 py-0.5 rounded-full border border-accent/40">
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
          <div className="text-[11px] text-muted mb-1">自定义回答</div>
          <div className="flex gap-2">
            <input
              type="text"
              className="field flex-1 min-w-0"
              placeholder={disabled ? '已提交' : '输入自定义答案…'}
              value={custom}
              disabled={disabled}
              onChange={(e) => setCustom(e.target.value)}
              onKeyDown={(e) => {
                // IME: Enter that commits a candidate must not submit the answer.
                const native = e.nativeEvent as KeyboardEvent & { isComposing?: boolean };
                if (native?.isComposing || (e as unknown as { keyCode?: number }).keyCode === 229) {
                  return;
                }
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
              className="btn btn-primary shrink-0 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              提交
            </button>
          </div>
        </div>

        {!choice.answered && (
          <div className="text-[11px] text-muted pt-0.5">
            点选选项或自定义提交 · 也可在输入框直接回复 ·{' '}
            <button
              type="button"
              className="text-secondary hover:text-primary underline rounded transition-colors duration-150 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50 disabled:opacity-50"
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
