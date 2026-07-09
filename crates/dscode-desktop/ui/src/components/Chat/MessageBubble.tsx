import StreamingRenderer from './StreamingRenderer';
import type { Message } from '@/lib/types';
import { AttachmentKindIcon } from '@/components/icons';

interface Props {
  message: Message;
}

export default function MessageBubble({ message }: Props) {
  const isUser = message.role === 'user';

  // Assistant: no bubble, clean flat text with subtle left line
  if (!isUser) {
    if (!message.content && !((message as any).reasoning_content)) return null;
    return (
      <div className="flex justify-start pl-1">
        <div className="max-w-[90%] border-l-2 border-gray-800 pl-3 py-0.5">
          <div className="text-gray-200 text-sm leading-relaxed">
            {message.content ? (
              <StreamingRenderer content={message.content} />
            ) : (
              <span className="text-gray-500 italic text-xs leading-relaxed whitespace-pre-wrap">
                {(message as any).reasoning_content}
              </span>
            )}
          </div>
        </div>
      </div>
    );
  }

  // User: left-aligned subtle bubble (neutral accent, no blue cast)
  const atts = message.attachments || [];
  return (
    <div className="flex justify-start mb-3">
      <div className="max-w-[85%] bg-card/80 border border-border/50 rounded-xl px-4 py-2.5 shadow-[inset_2px_0_0_0_rgba(156,163,175,0.25)]">
        {atts.length > 0 && (
          <div className="flex flex-wrap gap-1 mb-2">
            {atts.map((a) => (
              <span
                key={a.id}
                className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded bg-gray-800/80 text-[10px] font-mono text-gray-400 max-w-[12rem] truncate"
                title={a.path}
              >
                <AttachmentKindIcon kind={a.kind} size={12} className="shrink-0 text-gray-500" />
                {a.name}
              </span>
            ))}
          </div>
        )}
        <div className="text-gray-100 text-sm leading-relaxed">
          <StreamingRenderer content={message.content} />
        </div>
      </div>
    </div>
  );
}
