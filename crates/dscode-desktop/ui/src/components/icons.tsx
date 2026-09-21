/**
 * Shared icon set — filled 16px glyphs, no emoji, no icon library.
 *
 * Design rules (follow these when adding a glyph):
 *  - Every glyph is drawn on a **16×16 grid** and named with its size suffix
 *    (`IconSearch16`). The viewBox is the grid, never a scaled-up 24px one.
 *  - Geometry is `fill="currentColor"` with **no stroke**. Line weight is baked
 *    into the shapes as a bar, so a glyph stays crisp at 16px. **The bar is not
 *    a constant** — it is chosen per glyph: most primary strokes are 1.3–1.6
 *    units (the gear's spokes reach 1.8), while interior details such as a
 *    file's text rules sit lower, ~1.1–1.2.
 *    `size` scales the whole grid, so the on-screen stroke is
 *    `size / 16 × that glyph's own bar`. Measure the paths before reasoning
 *    about a glyph's rendered weight — the nominal figure is a design rule, not
 *    a per-glyph value.
 *  - Colour always comes from the caller via `className` (`text-muted`,
 *    `text-primary`, …). Nothing here bakes in a colour.
 *  - `size` is an escape hatch for the few call sites that need a different
 *    pixel box; it scales the whole grid and defaults to 16.
 *
 * 45 glyphs. See `AttachmentKindIcon` for the attachment dispatcher.
 */

import type { ReactNode } from 'react';

export type IconProps = {
  /** Rendered box in px. Defaults to the glyph's native 16px grid. */
  size?: number;
  className?: string;
  title?: string;
};

function Icon({
  size = 16,
  className,
  title,
  children,
}: IconProps & { children: ReactNode }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="currentColor"
      className={className}
      aria-hidden={title ? undefined : true}
      role={title ? 'img' : undefined}
      focusable="false"
    >
      {title ? <title>{title}</title> : null}
      {children}
    </svg>
  );
}

/**
 * Filled circle used as a separator / status dot. A primitive rather than a
 * glyph, so it keeps its own default size and its own radius ratio.
 */
export function IconDot({ className, size = 10, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <circle cx="8" cy="8" r="3.33" />
    </Icon>
  );
}

/** Chevron pointing right. */
export function IconChevronRight16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M5.68 3.03L11.56 8L5.68 12.97L4.72 11.83L9.24 8L4.72 4.17Z" />
    </Icon>
  );
}

/** Chevron pointing left. */
export function IconChevronLeft16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M11.28 4.17L6.76 8L11.28 11.83L10.32 12.97L4.44 8L10.32 3.03Z" />
    </Icon>
  );
}

/** Chevron pointing down. */
export function IconChevronDown16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M4.17 4.72L8 9.24L11.83 4.72L12.97 5.68L8 11.56L3.03 5.68Z" />
    </Icon>
  );
}

/** Chevron pointing up. */
export function IconChevronUp16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M3.03 10.32L8 4.44L12.97 10.32L11.83 11.28L8 6.76L4.17 11.28Z" />
    </Icon>
  );
}

/** Close / dismiss. */
export function IconClose16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M4.53 3.47L12.53 11.47L11.47 12.53L3.47 4.53ZM12.53 4.53L4.53 12.53L3.47 11.47L11.47 3.47Z" />
    </Icon>
  );
}

/** Check / success. */
export function IconCheck16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M3.37 7.83L6.34 10.81L12.58 3.29L13.82 4.31L6.46 13.19L2.23 8.97Z" />
    </Icon>
  );
}

/** Add. */
export function IconPlus16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M8.75 3L8.75 13L7.25 13L7.25 3ZM3 7.25L13 7.25L13 8.75L3 8.75Z" />
    </Icon>
  );
}

/** Remove. */
export function IconMinus16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M3.2 7.25L12.8 7.25L12.8 8.75L3.2 8.75Z" />
    </Icon>
  );
}

/** More actions. */
export function IconEllipsis16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M2.6 8a1.4 1.4 0 1 0 2.8 0a1.4 1.4 0 1 0 -2.8 0ZM6.6 8a1.4 1.4 0 1 0 2.8 0a1.4 1.4 0 1 0 -2.8 0ZM10.6 8a1.4 1.4 0 1 0 2.8 0a1.4 1.4 0 1 0 -2.8 0Z" />
    </Icon>
  );
}

/** Arrow up. */
export function IconArrowUp16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M7.25 13.4L7.25 3.2L8.75 3.2L8.75 13.4ZM3.66 6.48L8 1.91L12.34 6.48L11.26 7.52L8 4.09L4.74 7.52Z" />
    </Icon>
  );
}

/** Stop / abort. */
export function IconStop16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M5.6 4H10.4A1.6 1.6 0 0 1 12 5.6V10.4A1.6 1.6 0 0 1 10.4 12H5.6A1.6 1.6 0 0 1 4 10.4V5.6A1.6 1.6 0 0 1 5.6 4Z" />
    </Icon>
  );
}

/** Search. */
export function IconSearch16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M2.15 6.8a4.65 4.65 0 1 0 9.3 0a4.65 4.65 0 1 0 -9.3 0ZM3.65 6.8a3.15 3.15 0 1 0 6.3 0a3.15 3.15 0 1 0 -6.3 0Z" fillRule="evenodd" /><path d="M10.37 9.23L13.97 12.83L12.83 13.97L9.23 10.37Z" />
    </Icon>
  );
}

/** Settings / gear. */
export function IconSettings16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M3.25 8a4.75 4.75 0 1 0 9.5 0a4.75 4.75 0 1 0 -9.5 0ZM4.75 8a3.25 3.25 0 1 0 6.5 0a3.25 3.25 0 1 0 -6.5 0Z" fillRule="evenodd" /><path d="M11.7 7.1L14.2 7.1L14.2 8.9L11.7 8.9ZM11.25 9.98L13.02 11.75L11.75 13.02L9.98 11.25ZM8.9 11.7L8.9 14.2L7.1 14.2L7.1 11.7ZM6.02 11.25L4.25 13.02L2.98 11.75L4.75 9.98ZM4.3 8.9L1.8 8.9L1.8 7.1L4.3 7.1ZM4.75 6.02L2.98 4.25L4.25 2.98L6.02 4.75ZM7.1 4.3L7.1 1.8L8.9 1.8L8.9 4.3ZM9.98 4.75L11.75 2.98L13.02 4.25L11.25 6.02Z" />
    </Icon>
  );
}

/** Model / provider. */
export function IconSun16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M5.1 8a2.9 2.9 0 1 0 5.8 0a2.9 2.9 0 1 0 -5.8 0ZM12.4 7.35L14.2 7.35L14.2 8.65L12.4 8.65ZM11.57 10.65L12.84 11.92L11.92 12.84L10.65 11.57ZM8.65 12.4L8.65 14.2L7.35 14.2L7.35 12.4ZM5.35 11.57L4.08 12.84L3.16 11.92L4.43 10.65ZM3.6 8.65L1.8 8.65L1.8 7.35L3.6 7.35ZM4.43 5.35L3.16 4.08L4.08 3.16L5.35 4.43ZM7.35 3.6L7.35 1.8L8.65 1.8L8.65 3.6ZM10.65 4.43L11.92 3.16L12.84 4.08L11.57 5.35Z" />
    </Icon>
  );
}

/** Folder. */
export function IconFolder16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M1.9 13.9L1.9 3.8L4.02 1.8L7.86 1.8L9.26 3.8L14.1 3.8L14.1 13.9ZM3.3 12.5L3.3 4.4L4.58 3.2L7.14 3.2L8.54 5.2L12.7 5.2L12.7 12.5Z" fillRule="evenodd" />
    </Icon>
  );
}

/** File. */
export function IconFile16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M2.5 14.3L2.5 1.7L9.29 1.7L13.5 5.91L13.5 14.3ZM3.9 12.9L3.9 3.1L8.71 3.1L12.1 6.49L12.1 12.9Z" fillRule="evenodd" />
    </Icon>
  );
}

/** Text / document. */
export function IconFileText16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M2.5 14.3L2.5 1.7L9.29 1.7L13.5 5.91L13.5 14.3ZM3.9 12.9L3.9 3.1L8.71 3.1L12.1 6.49L12.1 12.9Z" fillRule="evenodd" /><path d="M5.4 8L8.4 8L8.4 9.2L5.4 9.2ZM5.4 10.6L10.6 10.6L10.6 11.8L5.4 11.8Z" />
    </Icon>
  );
}

/** Image. */
export function IconImage16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M4.1 2.7H11.9A2.2 2.2 0 0 1 14.1 4.9V11.1A2.2 2.2 0 0 1 11.9 13.3H4.1A2.2 2.2 0 0 1 1.9 11.1V4.9A2.2 2.2 0 0 1 4.1 2.7ZM4.1 4.1H11.9A0.8 0.8 0 0 1 12.7 4.9V11.1A0.8 0.8 0 0 1 11.9 11.9H4.1A0.8 0.8 0 0 1 3.3 11.1V4.9A0.8 0.8 0 0 1 4.1 4.1Z" fillRule="evenodd" /><path d="M5 6.8a1.2 1.2 0 1 0 2.4 0a1.2 1.2 0 1 0 -2.4 0ZM4.11 11.17L7.74 7.09L12.42 11.11L11.58 12.09L7.86 8.91L5.09 12.03Z" />
    </Icon>
  );
}

/** Image unavailable. */
export function IconImageOff16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M4.1 2.7H11.9A2.2 2.2 0 0 1 14.1 4.9V11.1A2.2 2.2 0 0 1 11.9 13.3H4.1A2.2 2.2 0 0 1 1.9 11.1V4.9A2.2 2.2 0 0 1 4.1 2.7ZM4.1 4.1H11.9A0.8 0.8 0 0 1 12.7 4.9V11.1A0.8 0.8 0 0 1 11.9 11.9H4.1A0.8 0.8 0 0 1 3.3 11.1V4.9A0.8 0.8 0 0 1 4.1 4.1Z" fillRule="evenodd" /><path d="M5 6.8a1.2 1.2 0 1 0 2.4 0a1.2 1.2 0 1 0 -2.4 0ZM4.11 11.17L7.74 7.09L12.42 11.11L11.58 12.09L7.86 8.91L5.09 12.03Z" /><path d="M2.63 13.11L12.23 1.91L13.37 2.89L3.77 14.09Z" />
    </Icon>
  );
}

/**
 * Attachment / paperclip.
 *
 * DeepSeek Harness's `ic_ds_paperclip_outline_16`. The previous glyph was a
 * vertical arch — two paths both describing the same 2×9 unit shape, which read
 * as a tent at any size and as a speck in the 15px button it is actually used
 * at. A clip is a spiral seen edge-on: an outer loop, an inner loop, and the
 * hook where the wire turns back. That needs the three nested curves this path
 * carries, not an outline of one.
 */
export function IconPaperclip16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M5.5498 9.75V5H6.9502V9.75C6.9502 10.3299 7.4201 10.7998 8 10.7998C8.5799 10.7998 9.0498 10.3299 9.0498 9.75V4.5C9.0498 2.9536 7.7964 1.7002 6.25 1.7002C4.7036 1.7002 3.4502 2.9536 3.4502 4.5V9.75C3.4502 12.2629 5.4871 14.2998 8 14.2998C10.5129 14.2998 12.5498 12.2629 12.5498 9.75V4H13.9502V9.75C13.9502 13.0361 11.2861 15.7002 8 15.7002C4.71391 15.7002 2.0498 13.0361 2.0498 9.75V4.5C2.04981 2.1804 3.9304 0.299806 6.25 0.299805C8.5696 0.299805 10.4502 2.1804 10.4502 4.5V9.75C10.4502 11.1031 9.3531 12.2002 8 12.2002C6.6469 12.2002 5.5498 11.1031 5.5498 9.75Z" />
    </Icon>
  );
}

/** Delete / trash. */
export function IconTrash16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M2.6 4.15L13.4 4.15L13.4 5.65L2.6 5.65ZM5.8 5L5.8 2L10.2 2L10.2 5L9 5L9 3.2L7 3.2L7 5ZM5.2 4.84L5.84 12.5L10.16 12.5L10.8 4.84L12.2 4.96L11.44 13.9L4.56 13.9L3.8 4.96Z" />
    </Icon>
  );
}

/** Edit / rename. */
export function IconPencil16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M2.1 13.9 4 13.6 13.4 4.2 11.8 2.6 2.4 12Z" />
    </Icon>
  );
}

/** Copy. */
export function IconCopy16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M2.5 11L2.5 4.21L4.21 2.5L10.2 2.5L10.2 3.9L4.79 3.9L3.9 4.79L3.9 11Z" /><path d="M7.1 4.9H12.3A2.2 2.2 0 0 1 14.5 7.1V12.3A2.2 2.2 0 0 1 12.3 14.5H7.1A2.2 2.2 0 0 1 4.9 12.3V7.1A2.2 2.2 0 0 1 7.1 4.9ZM7.1 6.3H12.3A0.8 0.8 0 0 1 13.1 7.1V12.3A0.8 0.8 0 0 1 12.3 13.1H7.1A0.8 0.8 0 0 1 6.3 12.3V7.1A0.8 0.8 0 0 1 7.1 6.3Z" fillRule="evenodd" />
    </Icon>
  );
}

/** Download. */
export function IconDownload16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M8.75 2.4L8.75 9.6L7.25 9.6L7.25 2.4ZM5.35 6.69L8 9.51L10.65 6.69L11.75 7.71L8 11.69L4.25 7.71ZM3.7 12.6L3.7 12.7L12.3 12.7L12.3 12.6L13.7 12.6L13.7 14.1L2.3 14.1L2.3 12.6Z" />
    </Icon>
  );
}

/** Send. */
export function IconSend16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M14 2 2.2 7.2 7 8.6 8.6 13.8Z" />
    </Icon>
  );
}

/** Message / conversation. */
export function IconMessage16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M4.4 2.55H11.6A2.65 2.65 0 0 1 14.25 5.2V9.2A2.65 2.65 0 0 1 11.6 11.85H4.4A2.65 2.65 0 0 1 1.75 9.2V5.2A2.65 2.65 0 0 1 4.4 2.55ZM4.4 3.85H11.6A1.35 1.35 0 0 1 12.95 5.2V9.2A1.35 1.35 0 0 1 11.6 10.55H4.4A1.35 1.35 0 0 1 3.05 9.2V5.2A1.35 1.35 0 0 1 4.4 3.85Z" fillRule="evenodd" /><path d="M4.6 11.2 4.4 14.4 7.8 11.2Z" />
    </Icon>
  );
}

/** Link. */
export function IconLink16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <g transform="rotate(-45 8 8)"><path d="M4.5 4.7H6.2A3.3 3.3 0 0 1 9.5 8V8A3.3 3.3 0 0 1 6.2 11.3H4.5A3.3 3.3 0 0 1 1.2 8V8A3.3 3.3 0 0 1 4.5 4.7ZM4.5 6.1H6.2A1.9 1.9 0 0 1 8.1 8V8A1.9 1.9 0 0 1 6.2 9.9H4.5A1.9 1.9 0 0 1 2.6 8V8A1.9 1.9 0 0 1 4.5 6.1Z" fillRule="evenodd" /><path d="M9.7 4.7H11.4A3.3 3.3 0 0 1 14.7 8V8A3.3 3.3 0 0 1 11.4 11.3H9.7A3.3 3.3 0 0 1 6.4 8V8A3.3 3.3 0 0 1 9.7 4.7ZM9.7 6.1H11.4A1.9 1.9 0 0 1 13.3 8V8A1.9 1.9 0 0 1 11.4 9.9H9.7A1.9 1.9 0 0 1 7.8 8V8A1.9 1.9 0 0 1 9.7 6.1Z" fillRule="evenodd" /></g>
    </Icon>
  );
}

/** Web / globe. */
export function IconGlobe16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M1.95 8a6.05 6.05 0 1 0 12.1 0a6.05 6.05 0 1 0 -12.1 0ZM3.25 8a4.75 4.75 0 1 0 9.5 0a4.75 4.75 0 1 0 -9.5 0Z" fillRule="evenodd" /><path d="M2.6 7.4L13.4 7.4L13.4 8.6L2.6 8.6Z" /><path d="M4.7 8A3.3 5.4 0 1 0 11.3 8A3.3 5.4 0 1 0 4.7 8ZM5.9 8A2.1 4.2 0 1 1 10.1 8A2.1 4.2 0 1 1 5.9 8Z" fillRule="evenodd" />
    </Icon>
  );
}

/** Terminal / shell. */
export function IconTerminal16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M3.9 2.3H12.1A2.2 2.2 0 0 1 14.3 4.5V11.5A2.2 2.2 0 0 1 12.1 13.7H3.9A2.2 2.2 0 0 1 1.7 11.5V4.5A2.2 2.2 0 0 1 3.9 2.3ZM3.9 3.7H12.1A0.8 0.8 0 0 1 12.9 4.5V11.5A0.8 0.8 0 0 1 12.1 12.3H3.9A0.8 0.8 0 0 1 3.1 11.5V4.5A0.8 0.8 0 0 1 3.9 3.7Z" fillRule="evenodd" /><path d="M5.69 6.11L8.39 8.8L5.69 11.49L4.71 10.51L6.41 8.8L4.71 7.09ZM8.8 10.3L11 10.3L11 11.7L8.8 11.7Z" />
    </Icon>
  );
}

/** Code. */
export function IconCode16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M6.09 5.29L3.39 8L6.09 10.71L5.11 11.69L1.41 8L5.11 4.31ZM10.89 4.31L14.59 8L10.89 11.69L9.91 10.71L12.61 8L9.91 5.29ZM10.03 2.97L7.23 13.37L5.97 13.03L8.77 2.63Z" />
    </Icon>
  );
}

/** Layout / panel. */
export function IconPanel16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M3.9 2.3H12.1A2.2 2.2 0 0 1 14.3 4.5V11.5A2.2 2.2 0 0 1 12.1 13.7H3.9A2.2 2.2 0 0 1 1.7 11.5V4.5A2.2 2.2 0 0 1 3.9 2.3ZM3.9 3.7H12.1A0.8 0.8 0 0 1 12.9 4.5V11.5A0.8 0.8 0 0 1 12.1 12.3H3.9A0.8 0.8 0 0 1 3.1 11.5V4.5A0.8 0.8 0 0 1 3.9 3.7Z" fillRule="evenodd" /><path d="M7.25 3L7.25 13L5.95 13L5.95 3Z" />
    </Icon>
  );
}

/** Menu. */
export function IconMenu16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M2.6 3.45L13.4 3.45L13.4 4.95L2.6 4.95ZM2.6 7.25L13.4 7.25L13.4 8.75L2.6 8.75ZM2.6 11.05L13.4 11.05L13.4 12.55L2.6 12.55Z" />
    </Icon>
  );
}

/** MCP servers / rack. */
export function IconServer16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M3.8 2H12.2A2 2 0 0 1 14.2 4V5.8A2 2 0 0 1 12.2 7.8H3.8A2 2 0 0 1 1.8 5.8V4A2 2 0 0 1 3.8 2ZM3.8 3.2H12.2A0.8 0.8 0 0 1 13 4V5.8A0.8 0.8 0 0 1 12.2 6.6H3.8A0.8 0.8 0 0 1 3 5.8V4A0.8 0.8 0 0 1 3.8 3.2ZM3.8 8.2H12.2A2 2 0 0 1 14.2 10.2V12A2 2 0 0 1 12.2 14H3.8A2 2 0 0 1 1.8 12V10.2A2 2 0 0 1 3.8 8.2ZM3.8 9.4H12.2A0.8 0.8 0 0 1 13 10.2V12A0.8 0.8 0 0 1 12.2 12.8H3.8A0.8 0.8 0 0 1 3 12V10.2A0.8 0.8 0 0 1 3.8 9.4Z" fillRule="evenodd" /><path d="M4.2 4.9a1 1 0 1 0 2 0a1 1 0 1 0 -2 0ZM4.2 11.1a1 1 0 1 0 2 0a1 1 0 1 0 -2 0Z" />
    </Icon>
  );
}

/** MCP connection / plug. */
export function IconPlug16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M6.9 2.2L6.9 5L5.5 5L5.5 2.2ZM10.5 2.2L10.5 5L9.1 5L9.1 2.2ZM8.7 10.4L8.7 13.8L7.3 13.8L7.3 10.4Z" /><path d="M5.7 4.35H10.3A2.15 2.15 0 0 1 12.45 6.5V8.9A2.15 2.15 0 0 1 10.3 11.05H5.7A2.15 2.15 0 0 1 3.55 8.9V6.5A2.15 2.15 0 0 1 5.7 4.35ZM5.7 5.65H10.3A0.85 0.85 0 0 1 11.15 6.5V8.9A0.85 0.85 0 0 1 10.3 9.75H5.7A0.85 0.85 0 0 1 4.85 8.9V6.5A0.85 0.85 0 0 1 5.7 5.65Z" fillRule="evenodd" />
    </Icon>
  );
}

/** Skill package / box. */
export function IconPackage16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M8 1.15L14.65 4.92L14.65 11.08L8 14.85L1.35 11.08L1.35 4.92ZM8 2.65L13.35 5.68L13.35 10.32L8 13.35L2.65 10.32L2.65 5.68Z" fillRule="evenodd" /><path d="M2.3 4.78L8 8.01L13.7 4.78L14.3 5.82L8 9.39L1.7 5.82ZM8.6 8.7L8.6 14.1L7.4 14.1L7.4 8.7Z" />
    </Icon>
  );
}

/** Database / memory. */
export function IconDatabase16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M2.15 4.6A5.85 2.55 0 1 0 13.85 4.6A5.85 2.55 0 1 0 2.15 4.6ZM3.45 4.6A4.55 1.25 0 1 1 12.55 4.6A4.55 1.25 0 1 1 3.45 4.6ZM2.15 11.4A5.85 2.55 0 1 0 13.85 11.4A5.85 2.55 0 1 0 2.15 11.4ZM3.45 11.4A4.55 1.25 0 1 1 12.55 11.4A4.55 1.25 0 1 1 3.45 11.4Z" fillRule="evenodd" /><path d="M3.45 4.6L3.45 11.4L2.15 11.4L2.15 4.6ZM13.85 4.6L13.85 11.4L12.55 11.4L12.55 4.6Z" />
    </Icon>
  );
}

/** Clock / time. */
export function IconClock16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M1.9 8a6.1 6.1 0 1 0 12.2 0a6.1 6.1 0 1 0 -12.2 0ZM3.3 8a4.7 4.7 0 1 0 9.4 0a4.7 4.7 0 1 0 -9.4 0Z" fillRule="evenodd" /><path d="M8.7 4.4L8.7 7.97L11.51 9.37L10.89 10.63L7.3 8.83L7.3 4.4Z" />
    </Icon>
  );
}

/** Agent / bot. */
export function IconBot16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M4.8 4.1H11.2A2.5 2.5 0 0 1 13.7 6.6V11.4A2.5 2.5 0 0 1 11.2 13.9H4.8A2.5 2.5 0 0 1 2.3 11.4V6.6A2.5 2.5 0 0 1 4.8 4.1ZM4.8 5.5H11.2A1.1 1.1 0 0 1 12.3 6.6V11.4A1.1 1.1 0 0 1 11.2 12.5H4.8A1.1 1.1 0 0 1 3.7 11.4V6.6A1.1 1.1 0 0 1 4.8 5.5Z" fillRule="evenodd" /><path d="M7.4 4.8L7.4 2.6L8.6 2.6L8.6 4.8ZM7.05 2a0.95 0.95 0 1 0 1.9 0a0.95 0.95 0 1 0 -1.9 0ZM5.15 8.6a1.05 1.05 0 1 0 2.1 0a1.05 1.05 0 1 0 -2.1 0ZM8.75 8.6a1.05 1.05 0 1 0 2.1 0a1.05 1.05 0 1 0 -2.1 0Z" />
    </Icon>
  );
}

/** Users / teams. */
export function IconUsers16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M2.65 6a2.95 2.95 0 1 0 5.9 0a2.95 2.95 0 1 0 -5.9 0ZM3.95 6a1.65 1.65 0 1 0 3.3 0a1.65 1.65 0 1 0 -3.3 0Z" fillRule="evenodd" /><path d="M1.55 14L1.55 12.89L2.36 10.58L4.2 9.35L7 9.35L8.84 10.58L9.65 12.89L9.65 14L8.35 14L8.35 13.11L7.76 11.42L6.6 10.65L4.6 10.65L3.44 11.42L2.85 13.11L2.85 14Z" /><path d="M9.5 6.4a2.3 2.3 0 1 0 4.6 0a2.3 2.3 0 1 0 -4.6 0ZM10.7 6.4a1.1 1.1 0 1 0 2.2 0a1.1 1.1 0 1 0 -2.2 0Z" fillRule="evenodd" /><path d="M9.6 14L9.6 13.1L10.29 11.14L11.43 10.23L12.17 11.17L11.31 11.86L10.8 13.3L10.8 14ZM13.04 9.8L14.15 11.02L14.6 13.14L14.6 14L13.4 14L13.4 13.26L13.05 11.58L12.16 10.6Z" />
    </Icon>
  );
}

/** Locked / trusted. */
export function IconLock16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M5.2 6.75H10.8A2.45 2.45 0 0 1 13.25 9.2V12A2.45 2.45 0 0 1 10.8 14.45H5.2A2.45 2.45 0 0 1 2.75 12V9.2A2.45 2.45 0 0 1 5.2 6.75ZM5.2 8.05H10.8A1.15 1.15 0 0 1 11.95 9.2V12A1.15 1.15 0 0 1 10.8 13.15H5.2A1.15 1.15 0 0 1 4.05 12V9.2A1.15 1.15 0 0 1 5.2 8.05Z" fillRule="evenodd" /><path d="M4.95 7.4L4.95 5.39L6.18 3.67L8 2.89L9.82 3.67L11.05 5.39L11.05 7.4L9.75 7.4L9.75 5.81L8.98 4.73L8 4.31L7.02 4.73L6.25 5.81L6.25 7.4Z" />
    </Icon>
  );
}

/** Unlocked / safety mode. */
export function IconUnlock16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M5.2 6.75H10.8A2.45 2.45 0 0 1 13.25 9.2V12A2.45 2.45 0 0 1 10.8 14.45H5.2A2.45 2.45 0 0 1 2.75 12V9.2A2.45 2.45 0 0 1 5.2 6.75ZM5.2 8.05H10.8A1.15 1.15 0 0 1 11.95 9.2V12A1.15 1.15 0 0 1 10.8 13.15H5.2A1.15 1.15 0 0 1 4.05 12V9.2A1.15 1.15 0 0 1 5.2 8.05Z" fillRule="evenodd" /><path d="M4.95 7.4L4.95 5.39L6.18 3.67L8 2.89L9.82 3.67L10.93 5.22L9.87 5.98L8.98 4.73L8 4.31L7.02 4.73L6.25 5.81L6.25 7.4Z" />
    </Icon>
  );
}

/** Agent skills / AI. */
export function IconSparkles16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M7 2.6 8.06 7.14 12.4 8.2 8.06 9.26 7 13.8 5.94 9.26 1.6 8.2 5.94 7.14Z" /><path d="M14 1.8L14 5L12.8 5L12.8 1.8ZM11.8 2.8L15 2.8L15 4L11.8 4Z" /><path d="M3.55 11.6L3.55 14L2.45 14L2.45 11.6ZM1.8 12.25L4.2 12.25L4.2 13.35L1.8 13.35Z" />
    </Icon>
  );
}

/** Warning / alert. */
export function IconAlert16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M8 1.97L14.6 13.7L1.4 13.7ZM8 4.83L12.2 12.3L3.8 12.3Z" fillRule="evenodd" /><path d="M8.7 6.6L8.7 9.8L7.3 9.8L7.3 6.6ZM7.15 11.6a0.85 0.85 0 1 0 1.7 0a0.85 0.85 0 1 0 -1.7 0Z" />
    </Icon>
  );
}

/** Refresh / retry. */
export function IconRefresh16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M12.37 4.34A5.7 5.7 0 0 1 7.01 13.61L7.25 12.23A4.3 4.3 0 0 0 11.29 5.24ZM3.63 11.66A5.7 5.7 0 0 1 8.99 2.39L8.75 3.77A4.3 4.3 0 0 0 4.71 10.76Z" /><path d="M5.16 12.58L6.92 14.15L7.35 11.7ZM10.84 3.42L9.08 1.85L8.65 4.3Z" />
    </Icon>
  );
}

/** Expand. */
export function IconExpand16({ size, className, title }: IconProps) {
  return (
    <Icon size={size} className={className} title={title}>
      <path d="M1.9 6.4L1.9 1.9L6.4 1.9L6.4 3.3L3.3 3.3L3.3 6.4ZM3.09 2.11L6.69 5.71L5.71 6.69L2.11 3.09ZM9.6 1.9L14.1 1.9L14.1 6.4L12.7 6.4L12.7 3.3L9.6 3.3ZM13.89 3.09L10.29 6.69L9.31 5.71L12.91 2.11ZM3.3 9.6L3.3 12.7L6.4 12.7L6.4 14.1L1.9 14.1L1.9 9.6ZM2.11 12.91L5.71 9.31L6.69 10.29L3.09 13.89ZM9.6 12.7L12.7 12.7L12.7 9.6L14.1 9.6L14.1 14.1L9.6 14.1ZM12.91 13.89L9.31 10.29L10.29 9.31L13.89 12.91Z" />
    </Icon>
  );
}

/** Pick the attachment file-type glyph. */
export function AttachmentKindIcon({
  kind,
  className,
  size = 14,
  title,
}: IconProps & { kind: 'image' | 'text' | 'binary' }) {
  if (kind === 'image') {
    return <IconImage16 className={className} size={size} title={title} />;
  }
  if (kind === 'text') {
    return <IconFileText16 className={className} size={size} title={title} />;
  }
  return <IconPaperclip16 className={className} size={size} title={title} />;
}
