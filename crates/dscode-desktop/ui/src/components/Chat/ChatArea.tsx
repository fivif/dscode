import { useEffect, useRef, useMemo, memo } from 'react';
import MessageBubble from './MessageBubble';
import ThinkingBlockView from './ThinkingBlock';
import ToolCallCard from './ToolCallCard';
import FactCard from './FactCard';
import TeamPanel from './TeamPanel';
import PlanChoiceCard from './PlanChoiceCard';
import { useChatStore } from '@/stores/chatStore';
import type { Message } from '@/lib/types';

/** Only re-render a message row when that message reference changes. */
const MessageRow = memo(function MessageRow({
  msg,
  isLast,
  isStreaming,
}: {
  msg: Message;
  isLast: boolean;
  isStreaming: boolean;
}) {
  const thinking = msg.thinking_blocks || [];
  const toolCalls = msg.tool_calls || [];
  const teamAgentsOnMsg = msg.team_agents || [];
  // Tools ABOVE content: if a bubble ever carries both (stream-split miss),
  // the final answer must not sit above its tool cards.
  return (
    <div className="mb-4">
      {thinking.length > 0 && (
        <ThinkingBlockView blocks={thinking} streaming={isStreaming && isLast} />
      )}
      {toolCalls.map((tc) => (
        <ToolCallCard key={tc.id} tool={tc} />
      ))}
      {msg.content && <MessageBubble message={msg} />}
      {msg.fact_cards && msg.fact_cards.length > 0 && <FactCard facts={msg.fact_cards} />}
      {msg.plan_choice && <PlanChoiceCard messageId={msg.id} choice={msg.plan_choice} />}
      {teamAgentsOnMsg.length > 0 && (
        <TeamPanel
          agents={teamAgentsOnMsg}
          kind={msg.agent_panel_kind || 'teams'}
          compact
        />
      )}
    </div>
  );
});

export default function ChatArea() {
  const messages = useChatStore((s) => s.messages);
  const isStreaming = useChatStore((s) => s.isStreaming);
  const streamError = useChatStore((s) => s.streamError);
  const bottomRef = useRef<HTMLDivElement>(null);
  const scrollRaf = useRef<number>(0);
  const containerRef = useRef<HTMLDivElement>(null);

  // Scroll key: only last message length + count — not full messages identity thrash
  const scrollKey = useMemo(() => {
    const last = messages[messages.length - 1];
    const len = last?.content?.length ?? 0;
    const tools = last?.tool_calls?.length ?? 0;
    const tOut = last?.team_agents?.reduce((n, a) => n + (a.output?.length ?? 0), 0) ?? 0;
    return `${messages.length}:${len}:${tools}:${tOut}:${isStreaming}`;
  }, [messages, isStreaming]);

  useEffect(() => {
    if (scrollRaf.current) cancelAnimationFrame(scrollRaf.current);
    scrollRaf.current = requestAnimationFrame(() => {
      bottomRef.current?.scrollIntoView({ behavior: 'auto' });
    });
    return () => {
      if (scrollRaf.current) cancelAnimationFrame(scrollRaf.current);
    };
  }, [scrollKey]);

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

  const lastId = messages[messages.length - 1]?.id;

  return (
    <div ref={containerRef} className="flex-1 overflow-y-auto px-4 py-4">
      {messages.map((msg) => (
        <MessageRow
          key={msg.id}
          msg={msg}
          isLast={msg.id === lastId}
          isStreaming={isStreaming}
        />
      ))}

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
