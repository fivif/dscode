/** Shared stroke SVG icons — no emoji. */

import type { ReactNode } from 'react';

type IconProps = {
  className?: string;
  size?: number;
  title?: string;
};

const base = (
  size: number,
  className: string | undefined,
  title: string | undefined,
  children: ReactNode,
) => (
  <svg
    width={size}
    height={size}
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="1.8"
    strokeLinecap="round"
    strokeLinejoin="round"
    className={className}
    aria-hidden={title ? undefined : true}
    role={title ? 'img' : undefined}
  >
    {title ? <title>{title}</title> : null}
    {children}
  </svg>
);

export function IconPaperclip({ className, size = 14, title }: IconProps) {
  return base(
    size,
    className,
    title,
    <path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />,
  );
}

export function IconFileText({ className, size = 14, title }: IconProps) {
  return base(
    size,
    className,
    title,
    <>
      <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
      <polyline points="14 2 14 8 20 8" />
      <line x1="8" y1="13" x2="16" y2="13" />
      <line x1="8" y1="17" x2="14" y2="17" />
    </>,
  );
}

export function IconImage({ className, size = 14, title }: IconProps) {
  return base(
    size,
    className,
    title,
    <>
      <rect x="3" y="3" width="18" height="18" rx="2" />
      <circle cx="9" cy="9" r="2" />
      <path d="m21 15-3.5-3.5a2 2 0 0 0-2.8 0L8 18" />
    </>,
  );
}

export function IconFile({ className, size = 14, title }: IconProps) {
  return base(
    size,
    className,
    title,
    <>
      <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
      <polyline points="14 2 14 8 20 8" />
    </>,
  );
}

export function IconX({ className, size = 12, title }: IconProps) {
  return base(
    size,
    className,
    title,
    <>
      <line x1="18" y1="6" x2="6" y2="18" />
      <line x1="6" y1="6" x2="18" y2="18" />
    </>,
  );
}

export function IconCheck({ className, size = 12, title }: IconProps) {
  return base(size, className, title, <polyline points="20 6 9 17 4 12" />);
}

export function IconLoader({ className, size = 12, title }: IconProps) {
  return base(
    size,
    className,
    title,
    <>
      <path d="M12 2v4" />
      <path d="M12 18v4" />
      <path d="m4.93 4.93 2.83 2.83" />
      <path d="m16.24 16.24 2.83 2.83" />
      <path d="M2 12h4" />
      <path d="M18 12h4" />
      <path d="m4.93 19.07 2.83-2.83" />
      <path d="m16.24 7.76 2.83-2.83" />
    </>,
  );
}

export function IconDot({ className, size = 10, title }: IconProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      className={className}
      aria-hidden={title ? undefined : true}
    >
      {title ? <title>{title}</title> : null}
      <circle cx="12" cy="12" r="5" fill="currentColor" />
    </svg>
  );
}

export function IconPlane({ className, size = 18, title }: IconProps) {
  return base(
    size,
    className,
    title,
    <path d="M17.8 19.2 16 11l3.5-3.5C21 6 21.5 4 21 3c-1-.5-3 0-4.5 1.5L13 8 4.8 6.2c-.5-.1-.9.1-1.1.5l-.3.5c-.2.5-.1 1 .3 1.3L9 12l-2 3H4l-1 1 3 2 2 3 1-1v-3l3-2 3.5 5.3c.3.4.8.5 1.3.3l.5-.2c.4-.3.6-.7.5-1.2z" />,
  );
}

export function IconFolder({ className, size = 14, title }: IconProps) {
  return base(
    size,
    className,
    title,
    <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />,
  );
}

/** Pick file-type glyph for attachments. */
export function AttachmentKindIcon({
  kind,
  className,
  size = 14,
}: {
  kind: 'image' | 'text' | 'binary';
  className?: string;
  size?: number;
}) {
  if (kind === 'image') return <IconImage className={className} size={size} />;
  if (kind === 'text') return <IconFileText className={className} size={size} />;
  return <IconPaperclip className={className} size={size} />;
}
