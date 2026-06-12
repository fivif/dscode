import StreamingRenderer from './StreamingRenderer';
import type { Message } from '@/lib/types';

interface Props { message: Message; }

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
              <span className="text-gray-500 italic text-xs leading-relaxed whitespace-pre-wrap">{(message as any).reasoning_content}</span>
            )}
          </div>
        </div>
      </div>
    );
  }

  // User: subtle bubble
  return (
    <div className="flex justify-start mb-3">
      <div className="max-w-[85%] bg-card border border-border/50 rounded-xl px-4 py-2.5">
        <div className="text-gray-100 text-sm leading-relaxed">
          <StreamingRenderer content={message.content} />
        </div>
      </div>
    </div>
  );
}
