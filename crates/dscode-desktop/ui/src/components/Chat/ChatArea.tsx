import { useEffect, useRef, useMemo, useCallback, memo } from 'react';
import MessageBubble from './MessageBubble';
import ThinkingBlockView from './ThinkingBlock';
import ToolCallCard from './ToolCallCard';
import FactCard from './FactCard';
import TeamPanel from './TeamPanel';
import PlanChoiceCard from './PlanChoiceCard';
import { useChatStore } from '@/stores/chatStore';
import type { Message } from '@/lib/types';
import { IconClose16 } from '@/components/icons';

/** Only re-render a message row when that message reference changes. */
const MessageRow = memo(function MessageRow({
  msg,
  isLast,
  isStreaming,
  isLastOfRun,
}: {
  msg: Message;
  isLast: boolean;
  isStreaming: boolean;
  /** Last of a consecutive run from the same role — gets the bubble tail. */
  isLastOfRun: boolean;
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
      {msg.content && (
        <MessageBubble
          message={msg}
          streaming={isStreaming && isLast}
          isLastOfRun={isLastOfRun}
        />
      )}
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
  const bridgeLaggedCount = useChatStore((s) => s.bridgeLaggedCount);
  const clearBridgeLagged = useChatStore((s) => s.clearBridgeLagged);
  const bottomRef = useRef<HTMLDivElement>(null);
  const scrollRaf = useRef<number>(0);
  const containerRef = useRef<HTMLDivElement>(null);
  /** True while the viewport is pinned to the bottom (i.e. the user has not scrolled up). */
  const pinnedRef = useRef(true);

  // Scroll key: only last message length + count — not full messages identity thrash
  const scrollKey = useMemo(() => {
    const last = messages[messages.length - 1];
    const len = last?.content?.length ?? 0;
    const tools = last?.tool_calls?.length ?? 0;
    const tOut = last?.team_agents?.reduce((n, a) => n + (a.output?.length ?? 0), 0) ?? 0;
    return `${messages.length}:${len}:${tools}:${tOut}:${isStreaming}`;
  }, [messages, isStreaming]);

  // A new turn means the user just acted — follow it again even if they had
  // scrolled up to read earlier output.
  useEffect(() => {
    if (isStreaming) pinnedRef.current = true;
  }, [isStreaming]);

  useEffect(() => {
    // Don't yank the view back down while the user is reading scrolled-up
    // content (tool output they just got, an earlier answer, …).
    if (!pinnedRef.current) return;
    if (scrollRaf.current) cancelAnimationFrame(scrollRaf.current);
    scrollRaf.current = requestAnimationFrame(() => {
      bottomRef.current?.scrollIntoView({ behavior: 'auto' });
    });
    return () => {
      if (scrollRaf.current) cancelAnimationFrame(scrollRaf.current);
    };
  }, [scrollKey]);

  const handleScroll = useCallback(() => {
    const el = containerRef.current;
    if (!el) return;
    pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
  }, []);

  if (messages.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <div className="text-center select-none px-6 pb-16">
          <img
            src="/xt_logo.png"
            alt="DS Code"
            className="w-32 h-32 mx-auto mb-7 opacity-[0.12] pointer-events-none"
          />
          <h1 className="text-[15px] font-semibold tracking-[-0.01em] text-primary">
            开始新的对话
          </h1>
          <p className="mt-2 text-[13px] leading-relaxed text-muted">
            输入消息，Enter 发送 · 也可以拖入文件
          </p>
        </div>
      </div>
    );
  }

  return (
    <div
      ref={containerRef}
      className="flex-1 overflow-y-auto px-4 py-4"
      onScroll={handleScroll}
    >
      {messages.map((msg, i) => (
        <MessageRow
          key={msg.id || `row-${i}`}
          msg={msg}
          isLast={i === messages.length - 1}
          isStreaming={isStreaming}
          // A tail is only correct on the last bubble of a run. Without this, a
          // burst of three messages from the same sender grows three tails down
          // its right edge — which is the tell that a tail was applied per
          // message rather than per run.
          isLastOfRun={messages[i + 1]?.role !== msg.role}
        />
      ))}

      {isStreaming && (
        <div className="flex items-center gap-2 pl-3 py-1 text-[13px] text-muted animate-pulse">
          <span className="w-1.5 h-1.5 rounded-full bg-muted" />生成中...
        </div>
      )}

      {streamError && (
        <div className="mx-4 my-2 p-3 bg-danger/10 border border-danger/40 rounded-card text-danger text-[13px]">{streamError}</div>
      )}

      {bridgeLaggedCount !== null && (
        <div className="mx-4 my-2 p-2.5 bg-warning/10 border border-warning/40 rounded-card text-warning text-[13px] flex items-start gap-2">
          <span className="flex-1">
            事件流繁忙，已跳过 {bridgeLaggedCount} 个流式事件，当前显示可能不完整（重新打开该会话会从数据库恢复完整内容）。
          </span>
          <button
            type="button"
            className="shrink-0 text-warning/70 hover:text-warning transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50 rounded-control"
            onClick={clearBridgeLagged}
            aria-label="忽略"
            title="忽略"
          >
            <IconClose16 size={12} />
          </button>
        </div>
      )}

      <div ref={bottomRef} />
    </div>
  );
}
