import { useState } from 'react';
import StreamingRenderer from './StreamingRenderer';
import type { Message } from '@/lib/types';
import { AttachmentKindIcon, IconCheck16, IconCopy16 } from '@/components/icons';

interface Props {
  message: Message;
  /** True while this exact message is the one still being generated. */
  streaming?: boolean;
  /**
   * Last message of a consecutive same-role run, so this bubble carries the
   * tail. Defaults to true: a lone message, or any caller that does not track
   * runs, gets the tailed shape.
   */
  isLastOfRun?: boolean;
}

/**
 * Hover-reveal "copy raw markdown" button for a whole message.
 *
 * Both call sites position this absolutely, in space the message was not using,
 * so it takes no part in the message's layout — see the note on the assistant
 * branch. Nothing in here reserves width or height, which is what lets it sit
 * off the end of the text without changing where the text wraps.
 */
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
      className={`icon-btn transition-[opacity,color,background-color] duration-200 ease-ios ${
        copied
          ? 'opacity-100 bg-success/10 text-success'
          : 'opacity-0 group-hover:opacity-100 focus-visible:opacity-100'
      }`}
    >
      {copied ? <IconCheck16 size={14} /> : <IconCopy16 size={14} />}
    </button>
  );
}

export default function MessageBubble({ message, streaming = false, isLastOfRun = true }: Props) {
  const isUser = message.role === 'user';

  // Assistant: pure markdown on the canvas, plus a "copy as markdown"
  // affordance that appears on hover or keyboard focus.
  //
  // There is deliberately no avatar, no bubble and no turn rail here. The role
  // is already carried by alignment (this column is flush left, the user's is
  // flush right) and by the user bubble's fill; a decorated assistant turn on
  // top of that adds a third signal for one distinction and, worse, indents the
  // whole transcript — every wrapped paragraph, list and table shifted 12px off
  // the margin that the tool cards and thinking blocks above it sit on. Flat
  // text on the shared left edge is what makes a long answer read as one column.
  //
  // The copy button is lifted out of the flow and into the gutter, so the body
  // gets the full 90% column and wraps where it would with no button at all. It
  // used to be a flex sibling of the body, which meant every message in the
  // transcript paid a standing 28px + 12px gap in its text width for a control
  // that is invisible until hover — 58% of the row in a one-word question.
  //
  // The trigger is the whole message row, not the button: `group` is on the
  // outer div and `.icon-btn` reveals on `group-hover`. Only the button itself
  // takes pointer events, so the text stays selectable and the rest of the row
  // is not a click target.
  //
  // Vertically it hangs off the body's bottom edge, which is where the reply
  // ends — the point the reader has just arrived at when they want to copy it.
  // Its centre lands on the last line's centre because it is pushed 3px below
  // that edge: the trailing line box is 22px (14px at `leading-relaxed`), so the
  // line's centre sits 11px above the edge, and an 28px button whose bottom is
  // 3px below it is centred 11px above it too. No measurement involved, and the
  // 3px overhang is absorbed by the row's own `py-0.5` and the transcript's
  // inter-message gap, so nothing shifts. `left-full` puts it past the body's
  // right edge — in a column that is 90% wide, that is a gutter that was empty.
  if (!isUser) {
    if (!message.content && !(message as any).reasoning_content) return null;
    const copyable = message.content || '';
    return (
      <div className="flex justify-start group">
        <div className="relative max-w-[90%] min-w-0 py-0.5">
          <div className="min-w-0 text-primary text-sm leading-relaxed">
            {message.content ? (
              <StreamingRenderer content={message.content} streaming={streaming} />
            ) : (
              <span className="text-muted italic text-[13px] leading-relaxed whitespace-pre-wrap">
                {(message as any).reasoning_content}
              </span>
            )}
          </div>
          {copyable && (
            <div className="absolute left-full bottom-0 -mb-[3px] ml-1">
              <CopyMarkdownButton text={copyable} />
            </div>
          )}
        </div>
      </div>
    );
  }

  // User: a raised bubble, right-aligned and tailed.
  //
  // Three things here carry the "this is the user speaking" signal, in
  // descending order of how much work they do:
  //
  //   · **Right alignment.** This is the primary cue and it is load-bearing:
  //     with both roles left-aligned the transcript reads as one undifferentiated
  //     column and it becomes hard to find your own questions when scrolling
  //     back. The assistant branch above stays `justify-start` for exactly that
  //     contrast.
  //   · **The tail.** A 6px square rotated 45° with two corners squared off,
  //     sitting on the bubble's bottom-right corner so the join is invisible
  //     against the bubble's own fill. It is drawn *outside* the bubble
  //     (`-right-1`, above it in paint order) so it cannot clip the message text.
  //     A tail-less bubble is used instead for a message that is not the last of
  //     a consecutive run from the same sender — otherwise a burst of three
  //     messages grows three tails down its right edge.
  //   · **The fill.** `bg-hover`, one step above the canvas. Doing least of the
  //     three, which is why it is also the one that could change without harm.
  //
  // The radius is 22px — dsh's value, and deliberately larger than the 18px
  // `rounded-bubble` token the old version applied. A rounder bubble reads as
  // more conversational at this size; the token is left at 18px because six
  // other surfaces were sized against it and this was the only consumer.
  //
  // The copy button follows the same rule as the assistant's: just outside the
  // message's own box, at its end, on whichever side has room. The bubble is
  // right-aligned and capped at 85% wide, so the room is on its left — where its
  // text can never be, because the box stops at the bubble's edge. Floating it in
  // the bubble's bottom padding instead (the first attempt) put it over the last
  // line of a long message, since only 10px of that padding is below the text and
  // the button is 28px tall.
  //
  // It costs the message nothing: as a flex sibling it was reserving 40px of the
  // bubble's inner width in every user message, and on a one-line question
  // ("现在做什么") that was 58% of the row.
  const atts = message.attachments || [];
  const tail = isLastOfRun;
  return (
    <div className="flex justify-end mb-3 group relative">
      <div className="relative max-w-[85%] px-4 py-2.5 user-bubble rounded-[22px]">
        {tail && (
          <span
            aria-hidden
            data-tail="user"
            className="absolute -right-1 bottom-2.5 h-3 w-3 rotate-45 bg-hover rounded-br-[3px]"
          />
        )}
        {atts.length > 0 && (
          <div className="flex flex-wrap gap-1 mb-2">
            {atts.map((a) => (
              <span
                key={a.id}
                className="inline-flex items-center gap-1 px-1.5 py-0.5 rounded-control border border-border bg-card text-[11px] font-mono text-secondary max-w-[12rem] truncate"
                title={a.path}
              >
                <AttachmentKindIcon kind={a.kind} size={12} className="shrink-0 text-muted" />
                {a.name}
              </span>
            ))}
          </div>
        )}
        <div className="min-w-0 text-primary text-sm leading-relaxed">
          <StreamingRenderer content={message.content} />
        </div>
        <div className="absolute right-full bottom-0 -mb-[3px] mr-1">
          <CopyMarkdownButton text={message.content || ''} />
        </div>
      </div>
    </div>
  );
}
