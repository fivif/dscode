import { createContext, memo, useContext, useMemo, useState } from 'react';
import ReactMarkdown, { defaultUrlTransform } from 'react-markdown';
import type { UrlTransform } from 'react-markdown';
import remarkGfm from 'remark-gfm';
import rehypeHighlight from 'rehype-highlight';
import { imageSrc } from '@/lib/tauri';
import { IconCheck16, IconCopy16, IconImage16, IconImageOff16 } from '@/components/icons';

interface Props {
  content: string;
  /**
   * True only for the message that is currently growing. The incremental
   * prefix/tail split (see below) is applied to live content only: a finished
   * message is parsed as one document, because splitting can break constructs
   * that span the boundary (reference links / footnotes resolve per document).
   */
  streaming?: boolean;
}

/** Extract plain text from react-markdown children nodes (string | array | element). */
function nodeText(children: unknown): string {
  if (typeof children === 'string' || typeof children === 'number') return String(children);
  if (Array.isArray(children)) return children.map(nodeText).join('');
  if (children && typeof children === 'object') {
    const props = (children as { props?: { children?: unknown } }).props;
    if (props && props.children != null) return nodeText(props.children);
  }
  return '';
}

/** Copy button for code blocks — the app-wide 28px icon button, with inline success feedback. */
function CopyButton({ text, copiedLabel = '已复制' }: { text: string; copiedLabel?: string }) {
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
      aria-label={copied ? copiedLabel : '复制'}
      title={copied ? copiedLabel : '复制'}
      className={`icon-btn ${copied ? 'bg-success/10 text-success' : ''}`}
    >
      {copied ? (
        <IconCheck16 size={14} />
      ) : (
        <IconCopy16 size={14} />
      )}
    </button>
  );
}

/** True while rendering a `<code>` that sits inside a `<pre>` (a real code block). */
const BlockCodeContext = createContext(false);

/**
 * Markdown code: block vs inline.
 *
 * Block-vs-inline cannot be decided from the class name — a fence **without** a
 * language (``` with no info string) gets no `language-*` class from
 * rehype-highlight, so every plain fence used to render as an inline pill.
 * The parent element is the real signal, and `pre` (below) publishes it.
 */
function MarkdownCode({ className, children, node, ...props }: any) {
  const inBlock = useContext(BlockCodeContext);
  const hasLang = typeof className === 'string' && className.includes('language-');
  const text = nodeText(children);

  if (!inBlock && !hasLang) {
    return (
      <code className="md-inline-code" {...props}>
        {children}
      </code>
    );
  }

  // Match the `language-*` token rather than substring-replacing it: by the time
  // this element is rendered, rehype-highlight has already unshifted its own
  // `hljs` class into `className`, so `replace('language-', '')` on
  // `"hljs language-python"` would yield "hljs python" and the header would read
  // HLJS PYTHON. Only an actual `language-` token names the language.
  const lang =
    className?.split(/\s+/).find((c: string) => c.startsWith('language-'))?.slice(9) || 'text';
  return (
    <div className="md-codeblock">
      <div className="md-codeblock-head">
        <span className="md-codeblock-lang">
          <span className="md-codeblock-dot" />
          {lang}
        </span>
        <CopyButton text={text} />
      </div>
      <pre className="md-codeblock-pre">
        {/* `className` already carries rehype-highlight's `hljs`; prefixing it
            again emitted `hljs hljs language-python`. */}
        <code className={className || 'hljs'} {...props}>
          {children}
        </code>
      </pre>
    </div>
  );
}

// ── Generated images (`do_image_generate`) ──
//
// Contract with dscode-core: the tool writes each image under `~/.dscode/images/`
// and reports it in its output as one line per image,
//
//     ![<short description>](dscode-image:<absolute path on disk>)
//
// The path is a real file, never a URL — it has to go through `imageSrc()` to
// become something an `<img>` can load. Both the markdown renderer (assistant
// text) and `ToolCallCard` (raw tool output) read that shape, so the parsing
// lives here and is exported.

/** Scheme marker; not a URL scheme the webview knows about. */
const DSC_IMAGE_PREFIX = 'dscode-image:';

/**
 * Extensions this app is willing to hand to the asset protocol / `/api/image`.
 * Mirrors the contract, so a `dscode-image:` line can never become a generic
 * "read any local file" primitive in the renderer. The authoritative
 * containment is elsewhere: the asset-protocol scope in `tauri.conf.json`
 * (`$HOME/.dscode/images/**`) on the desktop, and the equivalent check inside
 * `/api/image` on the web — this is only a shape filter.
 */
const DSC_IMAGE_EXT_RE = /\.(png|jpe?g|webp)$/i;

/** One image line, anchored to its own line (the contract's shape). */
const DSC_IMAGE_LINE_RE = /^!\[([^\]]*)\]\((dscode-image:.+)\)$/;

/** `dscode-image:<path>` → `<path>`, or null when it is not a renderable one. */
function dscodeImagePath(url: string): string | null {
  if (!url.startsWith(DSC_IMAGE_PREFIX)) return null;
  const path = url.slice(DSC_IMAGE_PREFIX.length).trim();
  return DSC_IMAGE_EXT_RE.test(path) ? path : null;
}

/**
 * Percent-encode the path inside every contract image line.
 *
 * This has to happen **before** the markdown parser, not after: CommonMark
 * processes backslash escapes inside a link destination, and a Windows path is
 * made of them. `![a](dscode-image:C:\Users\me\.dscode\images\a.png)` reaches
 * `urlTransform` as `dscode-image:C:%5CUsers%5Cme.dscode%5Cimages%5Ca.png` —
 * every other backslash percent-encoded by the URL encoder, and the `\.` of
 * `\.dscode` **deleted** as an escape. That deletion is unrecoverable (nothing
 * downstream can tell `me.dscode` from `me\.dscode`), so it cannot be repaired
 * in `urlTransform` or `MarkdownImage`; the destination has to be made
 * escape-proof before the parser sees it. `MarkdownImage` decodes it again.
 *
 * Side benefit: an encoded destination cannot be terminated early by a `)` or
 * broken by a space in the path.
 *
 * Only lines that already pass the contract predicate are rewritten, so a
 * malformed `dscode-image:` line stays visible as ordinary text — the same rule
 * `splitGeneratedImages` follows for the tool card.
 *
 * A path the model had already percent-encoded itself would be encoded twice;
 * the contract is a plain absolute path, so that is not a case we accept.
 */
function encodeGeneratedImagePaths(text: string): string {
  if (!text.includes(DSC_IMAGE_PREFIX)) return text;
  return text
    .split('\n')
    .map((line) => {
      const m = DSC_IMAGE_LINE_RE.exec(line.trim());
      if (!m) return line;
      const path = dscodeImagePath(m[2]);
      if (!path) return line;
      const indent = line.slice(0, line.indexOf('!['));
      return `${indent}![${m[1]}](${DSC_IMAGE_PREFIX}${encodeURIComponent(path)})`;
    })
    .join('\n');
}

/**
 * Reverse of `encodeGeneratedImagePaths`.
 *
 * Guarded because `decodeURIComponent` throws on a lone `%`, and the markdown
 * source is model-controlled: a hand-written `dscode-image:100%.png` has to
 * degrade to a wrong-but-visible path, not blow up the whole render.
 */
function decodeImagePath(path: string): string {
  try {
    return decodeURIComponent(path);
  } catch {
    return path;
  }
}

export interface GeneratedImage {
  /** Short description from the contract's `![alt]` slot. */
  alt: string;
  /** Absolute path on disk. */
  path: string;
}

/**
 * Split raw tool output into the images it reported and the log left over.
 *
 * One pass, one predicate: a line is removed from the log **iff** it was
 * extracted as an image, so a malformed `dscode-image:` line (wrong extension,
 * no path) stays visible as text rather than being stripped and then silently
 * dropped by the renderer.
 */
export function splitGeneratedImages(text: string): { images: GeneratedImage[]; log: string } {
  if (!text) return { images: [], log: text || '' };
  const images: GeneratedImage[] = [];
  const keep: string[] = [];
  for (const line of text.split('\n')) {
    const m = DSC_IMAGE_LINE_RE.exec(line.trim());
    const path = m ? dscodeImagePath(m[2]) : null;
    if (m && path) images.push({ alt: m[1].trim(), path });
    else keep.push(line);
  }
  return { images, log: keep.join('\n') };
}

/**
 * react-markdown v9 routes every URL through `defaultUrlTransform`, which
 * returns `''` for any scheme outside its safe list — including
 * `dscode-image:`, so image `src` would arrive empty and the image would
 * vanish. Pass that one scheme through untouched and delegate everything else
 * to the default, so `javascript:` / `data:` / `file:` / `vbscript:` stay
 * filtered exactly as before.
 *
 * Only `src` is passed through: `dscode-image:` is an internal file pointer,
 * and in an `href` it would end up handed to the OS by `MarkdownLink`'s
 * plugin-shell `open()`, which is not a thing it should be asked to resolve.
 *
 * Hoisted to module scope: `MemoMarkdown` is memoised and compares props
 * shallowly, so this reference must not change between renders.
 */
const urlTransform: UrlTransform = (url, key) => {
  if (key === 'src' && typeof url === 'string' && url.startsWith(DSC_IMAGE_PREFIX)) return url;
  return defaultUrlTransform(url);
};

/**
 * A locally generated image, rendered **immediately**.
 *
 * This is not a remote URL: this app just wrote the bytes to
 * `~/.dscode/images/` on this machine, and the desktop shell serves them over
 * the asset protocol (web shell: `/api/image`). Rendering it makes no request
 * that leaves the machine and leaks no Referer, so the click-to-load gate used
 * below for model/web URLs has nothing to protect here — and applying it would
 * make a finished generation look like it failed.
 *
 * `loading="lazy"` stays: that is the browser deferring decode for images below
 * the fold, not a user-facing gate.
 */
export function DscodeImage({ path, alt }: { path: string; alt?: string }) {
  // Failed *path*, not a boolean: a different image rendered at the same tree
  // position starts clean without needing an effect to reset the flag.
  const [failedPath, setFailedPath] = useState<string | null>(null);
  const src = DSC_IMAGE_EXT_RE.test(path) ? imageSrc(path) : null;

  if (!src || failedPath === path) {
    return (
      <div className="my-3 max-w-full rounded-card border border-danger/40 bg-danger/10 px-3 py-2.5">
        <div className="flex items-center gap-2 text-[13px] text-danger">
          <IconImageOff16 size={14} className="shrink-0" />
          <span>图片无法加载</span>
        </div>
        <div className="mt-1 break-all font-mono text-[11px] text-faint">{path}</div>
      </div>
    );
  }

  return (
    <img
      src={src}
      alt={alt || '生成的图片'}
      loading="lazy"
      onError={() => setFailedPath(path)}
      /* `shadow-card` already carries a 0.5px hairline as its first layer; a
         real border here would draw that edge twice. */
      className="max-w-full rounded-card my-3 shadow-card"
    />
  );
}

/**
 * Images are **not** auto-fetched — *unless* this app produced them.
 *
 * `src` normally comes from the model or from a page/tool result it read, and
 * react-markdown only filters the URL *scheme* (`javascript:`/`data:`/`file:`
 * are dropped by `urlTransform` above) — an `http(s)` URL would still make the
 * webview issue a request the moment the bubble renders, confirming the client
 * is online and leaking a Referer plus whatever was placed in the URL. Render
 * an inert placeholder and let the user opt in.
 *
 * `dscode-image:` takes the other path: see `DscodeImage`.
 */
function MarkdownImage({ src, alt, node, ...props }: any) {
  const [loaded, setLoaded] = useState(false);
  const url = typeof src === 'string' ? src : '';
  if (!url) return null;

  if (url.startsWith(DSC_IMAGE_PREFIX)) {
    const altText = typeof alt === 'string' ? alt : '';
    const localPath = dscodeImagePath(url);
    // Markdown source carries the percent-encoded form (see
    // `encodeGeneratedImagePaths`); undo that before this reaches the disk.
    // A malformed line still goes to `DscodeImage`, which shows the path it
    // could not load — never the click-to-load gate, which would be a dead end
    // (there is no URL behind this scheme for the webview to fetch).
    return (
      <DscodeImage
        path={decodeImagePath(localPath ?? url.slice(DSC_IMAGE_PREFIX.length).trim())}
        alt={altText}
      />
    );
  }

  const shown = url.length > 80 ? `${url.slice(0, 80)}…` : url;

  if (loaded) {
    return (
      <img
        src={url}
        alt={alt || ''}
        className="max-w-full rounded-card my-3 shadow-card"
        loading="lazy"
        {...props}
      />
    );
  }

  return (
    <button
      type="button"
      onClick={() => setLoaded(true)}
      title="图片来自模型输出/网页内容，点击后才加载"
      className="my-2 inline-flex max-w-full items-center gap-2 rounded-card border border-border bg-card px-3 py-2 text-[13px] text-secondary hover:bg-hover hover:text-primary transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
    >
      <IconImage16 size={14} className="shrink-0" />
      <span className="shrink-0">点击加载图片</span>
      {alt ? <span className="truncate text-muted">{alt}</span> : null}
      <span className="truncate font-mono text-[11px] text-faint">{shown}</span>
    </button>
  );
}

/**
 * Links open in the OS browser instead of navigating the app's own webview
 * (there is no navigation guard in the Tauri shell, and the webview holds
 * `shell:allow-open`). Middle-click is neutralised too.
 */
function MarkdownLink({ href, children, node, ...props }: any) {
  const url = typeof href === 'string' ? href : '';
  const openExternal = async (e: React.MouseEvent) => {
    e.preventDefault();
    if (!url) return;
    try {
      const { open } = await import('@tauri-apps/plugin-shell');
      await open(url);
    } catch {
      try {
        window.open(url, '_blank', 'noopener,noreferrer');
      } catch {
        /* ignore */
      }
    }
  };
  return (
    <a
      href={url}
      className="md-a"
      rel="noopener noreferrer"
      title={url}
      onClick={openExternal}
      onAuxClick={(e) => e.preventDefault()}
      {...props}
    >
      {children}
    </a>
  );
}

/**
 * An unterminated fenced code block, rendered as plain text.
 *
 * While a long code answer streams, re-running the markdown + highlight pass over
 * the whole accumulated block every frame is quadratic (a 3000-line answer visibly
 * stalls the main thread). The open block is the part that keeps growing, so it is
 * rendered unhighlighted until its closing fence arrives — then the normal path
 * highlights it once.
 */
function LiveCodeBlock({ raw }: { raw: string }) {
  const firstNl = raw.indexOf('\n');
  const opener = firstNl >= 0 ? raw.slice(0, firstNl) : raw;
  const body = firstNl >= 0 ? raw.slice(firstNl + 1) : '';
  const lang = opener.replace(/^\s{0,3}(`{3,}|~{3,})/, '').trim() || 'text';
  return (
    <div className="md-codeblock">
      <div className="md-codeblock-head">
        <span className="md-codeblock-lang">
          <span className="md-codeblock-dot" />
          {lang}
        </span>
        <CopyButton text={body} />
      </div>
      <pre className="md-codeblock-pre">
        <code className="hljs">{body}</code>
      </pre>
    </div>
  );
}

/** Memoised markdown: completed messages (and the stable prefix of a live one) never re-parse. */
const MemoMarkdown = memo(ReactMarkdown);
const REMARK_PLUGINS = [remarkGfm];
const REHYPE_PLUGINS = [rehypeHighlight];

/** Minimum live tail kept out of the cached prefix. */
const MIN_TAIL = 1200;

interface MarkdownSplit {
  /** Cacheable prefix: complete, self-contained markdown. */
  stable: string;
  /** The still-growing tail. */
  tail: string;
  /** `tail` starts at an unterminated fence opener → render it raw (no highlighting). */
  tailIsOpenCode: boolean;
}

const NO_SPLIT: MarkdownSplit = { stable: '', tail: '', tailIsOpenCode: false };

/**
 * Split streamed markdown at a *safe* boundary so the prefix can be cached and
 * only the tail re-parsed. A boundary is a blank line outside any fenced code
 * block where neither side is mid-construct (list item, blockquote, table,
 * indented code) — anything else would render differently than the same text
 * parsed as one document.
 */
function splitStreamingMarkdown(content: string): MarkdownSplit {
  if (content.length < MIN_TAIL * 2) return { ...NO_SPLIT, tail: content };

  const lines = content.split('\n');
  let offset = 0;
  let fenceStart = -1;
  let fenceMarker = '';
  let safeEnd = -1;
  let prevNonBlank = '';

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const lineStart = offset;
    offset += line.length + 1;

    const fence = /^\s{0,3}(`{3,}|~{3,})/.exec(line);
    if (fence) {
      const marker = fence[1][0];
      if (fenceStart < 0) {
        fenceStart = lineStart;
        fenceMarker = marker;
      } else if (marker === fenceMarker) {
        fenceStart = -1;
        fenceMarker = '';
        safeEnd = offset; // a closed fence is a complete, safe prefix
      }
      prevNonBlank = line;
      continue;
    }

    if (line.trim() === '') {
      if (fenceStart < 0) {
        const next = lines[i + 1] ?? '';
        const prevContinues =
          /^\s{0,3}([-*+]|\d{1,9}[.)])\s/.test(prevNonBlank) ||
          /^\s{0,3}>/.test(prevNonBlank) ||
          /^\s{0,3}\|/.test(prevNonBlank) ||
          /^( {4,}|\t)/.test(prevNonBlank) ||
          /^\s{0,3}(=+|-{2,})\s*$/.test(prevNonBlank);
        const nextContinues = /^(\s|[$|])/.test(next) || /^\s{0,3}([-*+]|\d{1,9}[.)])\s/.test(next);
        if (!prevContinues && !nextContinues) safeEnd = offset;
      }
      continue;
    }

    prevNonBlank = line;
  }

  // Prefer cutting before an unterminated fence: that is the block that grows.
  if (fenceStart >= 0 && content.length - fenceStart > MIN_TAIL) {
    return {
      stable: content.slice(0, fenceStart),
      tail: content.slice(fenceStart),
      tailIsOpenCode: true,
    };
  }
  if (safeEnd > 0 && content.length - safeEnd > MIN_TAIL) {
    return { stable: content.slice(0, safeEnd), tail: content.slice(safeEnd), tailIsOpenCode: false };
  }
  return { ...NO_SPLIT, tail: content };
}

/** Hoisted so `MemoMarkdown`'s shallow prop comparison stays stable across renders. */
const MD_COMPONENTS = {
  h1: ({ children }: any) => <h1 className="md-h1">{children}</h1>,
  h2: ({ children }: any) => <h2 className="md-h2">{children}</h2>,
  h3: ({ children }: any) => <h3 className="md-h3">{children}</h3>,
  h4: ({ children }: any) => <h4 className="md-h4">{children}</h4>,
  p: ({ children }: any) => <p className="md-p">{children}</p>,
  ul: ({ children }: any) => <ul className="md-ul">{children}</ul>,
  ol: ({ children }: any) => <ol className="md-ol">{children}</ol>,
  li: ({ children }: any) => <li className="md-li">{children}</li>,
  strong: ({ children }: any) => <strong className="font-semibold text-primary">{children}</strong>,
  em: ({ children }: any) => <em className="md-em">{children}</em>,
  del: ({ children }: any) => <del className="text-muted">{children}</del>,
  hr: () => <hr className="md-hr" />,

  code: MarkdownCode,
  // Publish "we are inside a code block" to the `code` element rendered below.
  pre: ({ children }: any) => <BlockCodeContext.Provider value={true}>{children}</BlockCodeContext.Provider>,

  a: MarkdownLink,
  img: MarkdownImage,

  table: ({ children }: any) => (
    <div className="md-table-wrap">
      <table className="md-table">{children}</table>
    </div>
  ),
  thead: ({ children }: any) => <thead className="md-thead">{children}</thead>,
  tbody: ({ children }: any) => <tbody className="md-tbody">{children}</tbody>,
  tr: ({ children }: any) => <tr className="md-tr">{children}</tr>,
  th: ({ children }: any) => <th className="md-th">{children}</th>,
  td: ({ children }: any) => <td className="md-td">{children}</td>,

  blockquote: ({ children }: any) => <blockquote className="md-quote">{children}</blockquote>,
};

/**
 * Markdown renderer.
 * Node styling lives in globals.css (`.md-*`); this file only wires the
 * interactive pieces — code-block copy, link handling and the image gate.
 */
export default function StreamingRenderer({ content, streaming = false }: Props) {
  // Escape-proof the image destinations before anything else touches the text —
  // including the split below, so both halves carry the encoded form. The split
  // only cuts at blank lines outside fences, so a contract line always stays
  // whole and can never be encoded on one side and decoded on the other.
  const source = useMemo(() => encodeGeneratedImagePaths(content || ''), [content]);

  const split = useMemo(
    () => (streaming ? splitStreamingMarkdown(source) : { ...NO_SPLIT, tail: source }),
    [source, streaming],
  );

  if (!content?.trim()) return <span className="text-muted italic text-[13px]">...</span>;

  return (
    <div className="md-body">
      {split.stable ? (
        <MemoMarkdown
          remarkPlugins={REMARK_PLUGINS}
          rehypePlugins={REHYPE_PLUGINS}
          urlTransform={urlTransform}
          components={MD_COMPONENTS as any}
        >
          {split.stable}
        </MemoMarkdown>
      ) : null}
      {split.tailIsOpenCode ? (
        <LiveCodeBlock raw={split.tail} />
      ) : split.tail ? (
        <MemoMarkdown
          remarkPlugins={REMARK_PLUGINS}
          rehypePlugins={REHYPE_PLUGINS}
          urlTransform={urlTransform}
          components={MD_COMPONENTS as any}
        >
          {split.tail}
        </MemoMarkdown>
      ) : null}
    </div>
  );
}
