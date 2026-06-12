import { useEffect, useRef } from 'react';
import MessageBubble from './MessageBubble';
import ThinkingBlockView from './ThinkingBlock';
import ToolCallCard from './ToolCallCard';
import { useChatStore } from '@/stores/chatStore';

export default function ChatArea() {
  const messages = useChatStore((s) => s.messages);
  const isStreaming = useChatStore((s) => s.isStreaming);
  const streamError = useChatStore((s) => s.streamError);
  const bottomRef = useRef<HTMLDivElement>(null);
  const scrollRaf = useRef<number>(0);
  const containerRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (scrollRaf.current) cancelAnimationFrame(scrollRaf.current);
    scrollRaf.current = requestAnimationFrame(() => {
      const container = containerRef.current;
      if (container) {
        const { scrollTop, scrollHeight, clientHeight } = container;
        const isNearBottom = scrollHeight - scrollTop - clientHeight < 80;
        if (isNearBottom) {
          bottomRef.current?.scrollIntoView({ behavior: isStreaming ? 'auto' : 'smooth' });
        }
      }
    });
    return () => { if (scrollRaf.current) cancelAnimationFrame(scrollRaf.current); };
  }, [messages, isStreaming]);

  if (messages.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center bg-main">
        <div className="text-center select-none">
          <img src="/xt_logo.png" alt="DS Code" className="w-44 h-44 mx-auto mb-5 opacity-20 pointer-events-none" />
          <p className="text-gray-500 text-sm">输入消息开始对话</p>
        </div>
      </div>
    );
  }

  return (
    <div ref={containerRef} className="flex-1 overflow-y-auto px-4 py-4">
      {messages.map((msg) => {
        const thinking = msg.thinking_blocks || [];
        const toolCalls = msg.tool_calls || [];
        const isToolOnly = toolCalls.length > 0 && !msg.content && !thinking.length;
        return (
          <div key={msg.id} className="mb-4">
            {thinking.length > 0 && <ThinkingBlockView blocks={thinking} />}
            {!isToolOnly && <MessageBubble message={msg} />}
            {toolCalls.map((tc: any) => <ToolCallCard key={tc.id} tool={tc} />)}
          </div>
        );
      })}

      {isStreaming && (
        <div className="flex items-center gap-2 pl-3 text-gray-500 text-xs animate-pulse py-1">
          <span className="w-1.5 h-1.5 rounded-full bg-gray-400" />生成中...
        </div>
      )}

      {streamError && (
        <div className="mx-4 my-2 p-3 bg-red-900/20 border border-red-900/40 rounded-lg text-red-400 text-sm">{streamError}</div>
      )}

      <div ref={bottomRef} />
    </div>
  );
}
