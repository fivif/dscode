import { useState, useEffect, useRef, memo, useMemo } from 'react';
import type { ToolCallRecord } from '@/lib/types';
import { IconCheck16, IconChevronRight16, IconClose16, IconDot } from '@/components/icons';
import { DscodeImage, splitGeneratedImages } from './StreamingRenderer';

interface Props { tool: ToolCallRecord; }

/**
 * Status is carried by a coloured left edge plus the icon, never by colour
 * alone — but the three states deliberately do NOT get equal weight.
 *
 * A real session is almost entirely `success`: the run this was designed
 * against had a stretch of 916 consecutive collapsed cards and every one of
 * them was green. Spending a saturated edge and a bright tick on each means
 * the two states that actually need attention — running and error — have
 * nowhere to stand out, and the column reads as one long wall of identical
 * boxes. So `success` goes quiet: the edge is transparent, the tick is
 * neutral, and the fill is the plain card.
 *
 * The edge stays 2px wide even when it is transparent, so every card in the
 * column has identical geometry and nothing shifts as a state changes. The bar
 * is a signal, and a signal that is on for 99% of the cards is not a signal —
 * this way it appears only where it means something.
 *
 * `error` is the only state that also tints its fill. That is what makes a
 * failure findable by scrolling, which is the whole job in a run this long.
 */
const STATUS_EDGE: Record<ToolCallRecord['status'], string> = {
  running: 'border-l-warning/70',
  success: 'border-l-transparent',
  error: 'border-l-danger',
};

const STATUS_TEXT: Record<ToolCallRecord['status'], string> = {
  running: 'text-warning',
  success: 'text-faint',
  error: 'text-danger',
};

const STATUS_FILL: Record<ToolCallRecord['status'], string> = {
  running: 'bg-card',
  success: 'bg-card',
  error: 'bg-danger/[0.06]',
};

function StatusIcon({ status }: { status: ToolCallRecord['status'] }) {
  const cls = `${STATUS_TEXT[status]} ${status === 'running' ? 'animate-pulse' : ''} shrink-0`;
  // All three at 12: a 10px dot renders ~4px across and disappears next to the
  // 12px glyphs it alternates with, which is the one place the row cannot
  // afford to be subtle — "still running" is a state the user is waiting on.
  if (status === 'running') return <IconDot className={cls} size={12} />;
  if (status === 'success') return <IconCheck16 className={cls} size={12} />;
  return <IconClose16 className={cls} size={12} />;
}

function lastProgressLine(text: string): string {
  const lines = text
    .split('\n')
    .map((l) => l.trim())
    .filter(Boolean);
  if (lines.length === 0) return '';
  // Prefer last checkmark / progress line for multi-lane search UX
  for (let i = lines.length - 1; i >= 0; i--) {
    const l = lines[i];
    if (
      l.startsWith('✓') ||
      l.startsWith('✗') ||
      l.startsWith('●') ||
      l.startsWith('⟳') ||
      l.startsWith('▸') ||
      l.includes('…')
    ) {
      return l.length > 72 ? l.slice(0, 72) + '…' : l;
    }
  }
  const last = lines[lines.length - 1];
  return last.length > 72 ? last.slice(0, 72) + '…' : last;
}

function ToolCallCardInner({ tool }: Props) {
  // Default collapsed while running — expanding huge streaming logs freezes the UI.
  const isSearch = tool.name === 'do_web_search';
  const [expanded, setExpanded] = useState(false);
  const userToggledRef = useRef(false);

  /**
   * Images are pulled out of the **full** result, before the display cap below.
   *
   * The cap keeps the *tail* of a running result (`slice(-4000)`) while the
   * contract puts each image line wherever the tool wrote it — typically near
   * the top of its report. Splitting after truncation would make an image show
   * up and then disappear as the log grows past the cap, so `tool.result` is
   * split first and only the leftover log text is capped.
   *
   * `splitGeneratedImages` also removes exactly the lines it extracted, so the
   * user never sees the raw `![…](dscode-image:…)` alongside the image.
   *
   * Declared before the effect below, which reads `images.length` in its dep
   * array during render — a `const` further down would still be in its TDZ.
   */
  const { images, log } = useMemo(() => splitGeneratedImages(tool.result), [tool.result]);

  useEffect(() => {
    if (userToggledRef.current) return;
    // Search shows live progress, and a generated image *is* its card — neither
    // should sit behind a click. A manual collapse still sticks (ref above).
    if (images.length > 0 || (isSearch && tool.status === 'running')) {
      setExpanded(true);
    }
  }, [isSearch, tool.status, images.length]);

  const handleToggle = () => {
    userToggledRef.current = true;
    setExpanded(!expanded);
  };

  // Cap DOM text while streaming (still keep full string in store for tool_end).
  const displayResult = useMemo(() => {
    if (!log) return '';
    if (tool.status === 'running' && log.length > 4000) {
      return '…\n' + log.slice(-4000);
    }
    if (log.length > 30_000) {
      return log.slice(0, 12_000) + '\n…[truncated for display]…\n' + log.slice(-8_000);
    }
    return log;
  }, [log, tool.status]);

  const liveHint = useMemo(() => {
    if (tool.status !== 'running' || !log) return '';
    return lastProgressLine(log);
  }, [tool.status, log]);

  return (
    /*
     * `shadow-lift`, not a border, and deliberately not `shadow-hairline`.
     *
     * tailwind.config.js is explicit that a surface must carry either a real
     * border or one of the hairline shadows, never both — the old shell had
     * `border border-border` *and* a 2px status bar, which draws the left edge
     * twice, and `border-border` is white 12% against the hairline's 20%: the
     * same edge, twice, at two different strengths.
     *
     * `lift` is the variant with the hairline stripped out, and the config
     * names this exact case for it: a surface that draws its own coloured edge
     * would otherwise get a white seam laid over the status bar. The second
     * reason is scale. 937 of these stack in one column, and `hairline` is
     * white 20% — brighter than the border it replaces — so giving every card a
     * full stroke would put ~900 bright rules in a column and rebuild the wall
     * of boxes this redesign is trying to break up. At 4% black the lift only
     * softens; the fill against the canvas is what separates one card from the
     * next, and the only lit edge in the column is the one carrying state.
     */
    <div
      data-tool-card={tool.status}
      className={`mb-1.5 ml-1 border-l-2 ${STATUS_EDGE[tool.status]} ${STATUS_FILL[tool.status]} rounded-control shadow-lift overflow-hidden transition-colors`}
    >
      <button
        aria-expanded={expanded}
        className="w-full flex items-center gap-2 px-2.5 py-1.5 text-left hover:bg-hover transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
        onClick={handleToggle}
      >
        <StatusIcon status={tool.status} />
        <span className={`text-[11px] font-mono truncate shrink-0 max-w-[9rem] ${tool.status === 'error' ? 'text-danger' : 'text-secondary'}`}>{tool.name}</span>
        {tool.status === 'running' && liveHint ? (
          <span className="text-[11px] text-warning/80 truncate flex-1 font-mono animate-pulse">
            {liveHint}
          </span>
        ) : tool.status === 'running' ? (
          <span className="text-[11px] text-warning/70 animate-pulse flex-1">running</span>
        ) : (
          <span className="flex-1" />
        )}
        <IconChevronRight16
          size={12}
          className={`text-faint transition-transform shrink-0 ${expanded ? 'rotate-90' : ''}`}
        />
      </button>
      {expanded && (
        <div className="px-2.5 pb-2 pt-0.5 border-t border-divider">
          {/* Above the log: the image is the result, the log is the receipt. */}
          {images.length > 0 && (
            <div className="pt-1.5">
              {images.map((img, i) => (
                <DscodeImage key={`${img.path}#${i}`} path={img.path} alt={img.alt} />
              ))}
            </div>
          )}
          {displayResult ? (
            /*
             * The log sits on the `input` token — the same ground the markdown
             * code blocks use — so a tool's output and a code block in the
             * answer read as the same kind of thing. It was `bg-black/30`, a
             * raw black that is not in the token layer and that on a #232324
             * card is darker than any other well in the app.
             */
            <pre className="text-[11px] text-secondary bg-input rounded-control p-2 max-h-44 overflow-y-auto whitespace-pre-wrap font-mono leading-relaxed">
              {displayResult}
            </pre>
          ) : null}
          {!displayResult && images.length === 0 && tool.status === 'running' ? (
            <div className="text-[11px] text-muted px-1 py-1.5 animate-pulse">
              准备并发检索…
            </div>
          ) : null}
        </div>
      )}
    </div>
  );
}

export default memo(ToolCallCardInner);
