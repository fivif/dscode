import { useState } from 'react';
import type { ThinkingBlock as ThinkingBlockType } from '@/lib/types';

interface Props {
  blocks: ThinkingBlockType[];
  /** When true, thinking is still streaming — the row reports its latest line. */
  streaming?: boolean;
}

/**
 * The reasoning disclosure, modelled on DeepSeek Harness's `ReasoningRow`
 * (MIT, packages/client/ui-chat/src/client/chat/ReasoningRow.tsx).
 *
 * Five things there are deliberate and worth keeping if you edit this:
 *
 *   · **Collapsed by default.** Reasoning is the model talking to itself; the
 *     answer is what the reader came for. The old version opened expanded and
 *     folded itself away after 2.5s, which meant the page reflowed twice per
 *     turn and the reader lost their place both times.
 *   · **The collapsed line follows the text.** While streaming it shows the
 *     *latest* line (so it reads as live), and once finished it snaps back to
 *     the *first* line (so it reads as a stable label). Same row, two jobs.
 *   · **A fixed 24px collapsed height** with `contain: size layout`, so a
 *     streaming block appends without nudging everything below it.
 *   · **No card.** This is a flow row in the message column, not a widget: no
 *     fill, no radius, no border. The old version boxed it, which is what made
 *     a thought look like a tool result.
 *   · **Tertiary text, never italic.** The reasoning body sits one tier under
 *     the answer and relies on colour + indent for that, not on a fake
 *     oblique — Segoe UI Variable has no true italic on Windows, so `italic`
 *     here only ever produced a smeared slant.
 */
export default function ThinkingBlockView({ blocks, streaming }: Props) {
  const [expanded, setExpanded] = useState(false);

  if (!blocks?.length) return null;

  const total = blocks.map((b) => b.content).join('\n');
  const running = Boolean(streaming);

  // `**` is markdown emphasis that never renders inside a single-line summary,
  // so it would just show up as literal asterisks.
  const line = (running ? latestLine(total) : firstLine(total)).replace(/\*\*/g, '');

  return (
    <div
      className={`mb-1 flex flex-col ${expanded ? '' : 'h-6 overflow-hidden'}`}
      style={expanded ? undefined : { contain: 'size layout' }}
      data-state={running ? 'running' : 'ok'}
    >
      {/* The sweep band animates inside this row; `overflow-hidden` clips it. */}
      <button
        type="button"
        aria-expanded={expanded}
        onClick={() => setExpanded((v) => !v)}
        className="group relative flex h-6 w-full shrink-0 items-center overflow-hidden
                   text-left outline-none focus-visible:ring-2 focus-visible:ring-accent/50
                   rounded-[4px]"
      >
        {/* Running indicator: a soft light band crossing the row every 2.6s. */}
        {running && <span className="think-sweep" aria-hidden />}

        <span className="relative mr-1.5 inline-flex h-4 w-4 shrink-0 items-center justify-center text-muted">
          <IconThink className="transition-opacity duration-100 group-hover:opacity-0" />
          <IconChevronDown
            className={`absolute inset-0 m-auto transition-all duration-100 ${
              expanded
                ? 'rotate-180 opacity-100 text-secondary'
                : 'opacity-0 group-hover:opacity-100 text-secondary'
            }`}
          />
        </span>

        <span className="shrink-0 text-[13px] leading-6 text-secondary">思考</span>

        {!expanded && (
          <>
            <span className="mx-2 h-0.5 w-0.5 shrink-0 rounded-full bg-faint" aria-hidden />
            {/* While streaming the summary is pushed to the right edge so the
                text grows toward the leading edge instead of jumping sideways. */}
            <span
              className={`min-w-0 flex-1 overflow-hidden whitespace-nowrap text-[13px] leading-5 text-muted ${
                running ? 'flex justify-end' : ''
              }`}
            >
              <span className={running ? 'w-max min-w-full text-start' : 'block truncate'}>
                {line}
              </span>
            </span>
          </>
        )}

        {running && <span className="sr-only">运行中</span>}
      </button>

      {expanded && (
        // Indent aligns the body under the title, not under the icon:
        // leading box 16px + its 6px gap = 22px.
        <div className="max-h-72 overflow-y-auto whitespace-pre-wrap break-words py-1 pl-[22px] pr-1 text-[13px] leading-5 text-muted">
          {total}
        </div>
      )}
    </div>
  );
}

function firstLine(text: string): string {
  const newline = text.indexOf('\n');
  return newline === -1 ? text : text.slice(0, newline);
}

function latestLine(text: string): string {
  const visible = text.trimEnd();
  const newline = visible.lastIndexOf('\n');
  return newline === -1 ? visible : visible.slice(newline + 1);
}

/* ── Glyphs ──────────────────────────────────────────────────────────────────
   Copied from DeepSeek Harness, MIT — packages/client/ui-primitives/src/icons.
   They are 14px-grid filled glyphs, unlike the 24px stroke set in
   `components/icons.tsx`, which is why they live here rather than there. */

const GLYPH = { fill: 'currentColor', xmlns: 'http://www.w3.org/2000/svg' } as const;

function IconThink({ className }: { className?: string }) {
  return (
    <svg width={14} height={14} viewBox="0 0 14 14" className={className} aria-hidden {...GLYPH}>
      <path d="M7.06431 5.93342C7.68763 5.93342 8.19307 6.43904 8.19322 7.06233C8.19322 7.68573 7.68772 8.19123 7.06431 8.19123C6.44099 8.19113 5.9354 7.68567 5.9354 7.06233C5.93555 6.43911 6.44108 5.93353 7.06431 5.93342Z" />
      <path
        fillRule="evenodd"
        clipRule="evenodd"
        d="M8.6815 0.963693C10.1169 0.447019 11.6266 0.374829 12.5633 1.31135C13.5 2.24805 13.4277 3.75776 12.911 5.19319C12.7126 5.74431 12.4386 6.31796 12.0965 6.89729C12.4969 7.54638 12.8141 8.19018 13.036 8.80647C13.5527 10.2419 13.6251 11.7516 12.6883 12.6883C11.7516 13.625 10.242 13.5527 8.8065 13.036C8.19022 12.8141 7.54641 12.4969 6.89732 12.0965C6.31797 12.4386 5.74435 12.7125 5.19322 12.911C3.75777 13.4276 2.2481 13.5 1.31138 12.5633C0.374859 11.6266 0.447049 10.1168 0.963724 8.68147C1.17185 8.10338 1.46321 7.50063 1.82896 6.8924C1.52182 6.35711 1.27235 5.82825 1.08872 5.31819C0.572068 3.88278 0.499714 2.37306 1.43638 1.43635C2.37308 0.499655 3.8828 0.572044 5.31822 1.08869C5.82828 1.27232 6.35715 1.5218 6.89243 1.82893C7.50066 1.46318 8.10341 1.17181 8.6815 0.963693ZM11.3573 8.01154C10.9083 8.62253 10.3901 9.22873 9.80943 9.8094C9.22877 10.3901 8.62255 10.9083 8.01158 11.3572C8.4257 11.5841 8.8287 11.7688 9.21275 11.9071C10.5456 12.3868 11.4246 12.2547 11.8397 11.8397C12.2548 11.4246 12.3869 10.5456 11.9071 9.21272C11.7688 8.82866 11.5841 8.42568 11.3573 8.01154ZM2.56529 8.02912C2.37344 8.39322 2.21495 8.74796 2.09263 9.08772C1.61291 10.4204 1.74512 11.2995 2.16001 11.7147C2.57505 12.1297 3.45415 12.2618 4.78697 11.7821C5.11057 11.6656 5.44786 11.5164 5.7938 11.3367C5.249 10.9223 4.70922 10.4533 4.19029 9.9344C3.57578 9.31987 3.03169 8.67633 2.56529 8.02912ZM6.90708 3.2469C6.24065 3.70479 5.5646 4.26321 4.91392 4.91389C4.26325 5.56456 3.70482 6.24063 3.24693 6.90705C3.72674 7.63325 4.32777 8.37459 5.03892 9.08576C5.64943 9.69627 6.28183 10.2265 6.90806 10.6678C7.59368 10.2025 8.2908 9.63076 8.96079 8.96076C9.6308 8.29075 10.2025 7.59366 10.6678 6.90803C10.2265 6.2818 9.69631 5.6494 9.08579 5.03889C8.37462 4.32773 7.63328 3.72672 6.90708 3.2469ZM11.7147 2.15998C11.2996 1.74509 10.4204 1.61288 9.08775 2.0926C8.74835 2.21479 8.39382 2.37271 8.03013 2.56428C8.67728 3.03065 9.31995 3.5758 9.93443 4.19026C10.4534 4.7092 10.9223 5.24896 11.3368 5.79377C11.5164 5.44785 11.6656 5.11052 11.7821 4.78694C12.2618 3.45416 12.1297 2.57502 11.7147 2.15998ZM4.91197 2.2176C3.57922 1.73788 2.70004 1.86995 2.28501 2.28498C1.87001 2.70003 1.73791 3.5792 2.21763 4.91194C2.31709 5.18822 2.44112 5.47427 2.58677 5.7674C3.01931 5.1887 3.51474 4.6158 4.06529 4.06526C4.61584 3.5147 5.18872 3.01928 5.76743 2.58674C5.47431 2.4411 5.18824 2.31706 4.91197 2.2176Z"
      />
    </svg>
  );
}

function IconChevronDown({ className }: { className?: string }) {
  return (
    <svg width={14} height={14} viewBox="0 0 14 14" className={className} aria-hidden {...GLYPH}>
      <path d="M11.8486 5.5L11.4238 5.92383L8.69727 8.65137C8.44157 8.90706 8.21562 9.13382 8.01172 9.29785C7.79912 9.46883 7.55595 9.61756 7.25 9.66602C7.08435 9.69222 6.91565 9.69222 6.75 9.66602C6.44405 9.61756 6.20088 9.46883 5.98828 9.29785C5.78438 9.13382 5.55843 8.90706 5.30273 8.65137L2.57617 5.92383L2.15137 5.5L3 4.65137L3.42383 5.07617L6.15137 7.80273C6.42595 8.07732 6.59876 8.24849 6.74023 8.3623C6.87291 8.46904 6.92272 8.47813 6.9375 8.48047C6.97895 8.48703 7.02105 8.48703 7.0625 8.48047C7.07728 8.47813 7.12709 8.46904 7.25977 8.3623C7.40124 8.24849 7.57405 8.07732 7.84863 7.80273L10.5762 5.07617L11 4.65137L11.8486 5.5Z" />
    </svg>
  );
}
