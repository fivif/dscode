import { useState } from 'react';
import StreamingRenderer from './StreamingRenderer';
import type { Message } from '@/lib/types';
import { AttachmentKindIcon } from '@/components/icons';

interface Props {
  message: Message;
}

/** Hover-reveal "copy raw markdown" button for a whole message. */
function CopyMarkdownButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  const onCopy = async (e: React.MouseEvent) => {
    e.stopPropagation();
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 1800);
    } catch {
      /* clipboard unavailable — ignore */
    }
  };
  return (
    <button
      onClick={onCopy}
      aria-label={copied ? '已复制' : '复制为 Markdown'}
      title={copied ? '已复制' : '复制为 Markdown'}
      className={`inline-flex items-center justify-center w-6 h-6 rounded-lg transition-all duration-200 select-none ${
        copied
          ? 'bg-emerald-400/15 text-emerald-300 ring-1 ring-emerald-400/30 opacity-100'
          : 'bg-white/[0.05] text-gray-500 hover:bg-white/[0.1] hover:text-gray-300 ring-1 ring-white/[0.07] opacity-0 group-hover:opacity-100 focus-visible:opacity-100'
      }`}
    >
      {copied ? (
        <svg className="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
          <path d="M20 6 9 17l-5-5" />
        </svg>
      ) : (
        <svg className="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
          <path d="M14 2v6h6" />
        </svg>
      )}
    </button>
  );
}

export default function MessageBubble({ message }: Props) {
  const isUser = message.role === 'user';

  // Assistant: clean flat text with a subtle gradient left line, plus
  // hover-reveal "copy as markdown" affordance.
  if (!isUser) {
    if (!message.content && !(message as any).reasoning_content) return null;
    const copyable = message.content || '';
    return (
      <div className="flex justify-start pl-1 group relative">
        <div className="max-w-[90%] border-l border-transparent pl-3 py-0.5 relative">
          {/* iOS liquid-glass accent line */}
          <span
            aria-hidden
            className="absolute left-0 top-1 bottom-1 w-[2px] rounded-full bg-gradient-to-b from-white/25 via-white/10 to-transparent"
          />
          <div className="flex items-start justify-between gap-3">
            <div className="min-w-0 text-gray-200 text-sm leading-relaxed">
              {message.content ? (
                <StreamingRenderer content={message.content} />
              ) : (
                <span className="text-gray-500 italic text-xs leading-relaxed whitespace-pre-wrap">
                  {(message as any).reasoning_content}
                </span>
              )}
            </div>
            {copyable && (
              <div className="shrink-0 pt-0.5">
                <CopyMarkdownButton text={copyable} />
              </div>
            )}
          </div>
        </div>
      </div>
    );
  }

  // User: frosted-glass bubble (neutral accent, iOS tone).
  const atts = message.attachments || [];
  return (
    <div className="flex justify-start mb-3 group relative">
      <div className="max-w-[85%] rounded-2xl px-4 py-2.5 user-bubble">
        {atts.length > 0 && (
          <div className="flex flex-wrap gap-1 mb-2">
            {atts.map((a) => (
              <span
                key={a.id}
                className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-white/[0.06] text-[10px] font-mono text-gray-400 max-w-[12rem] truncate"
                title={a.path}
              >
                <AttachmentKindIcon kind={a.kind} size={12} className="shrink-0 text-gray-500" />
                {a.name}
              </span>
            ))}
          </div>
        )}
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0 text-gray-100 text-sm leading-relaxed">
            <StreamingRenderer content={message.content} />
          </div>
          <div className="shrink-0 pt-0.5">
            <CopyMarkdownButton text={message.content || ''} />
          </div>
        </div>
      </div>
    </div>
  );
}
