import { useState } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import rehypeHighlight from 'rehype-highlight';

interface Props {
  content: string;
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

/** iOS-style copy button with inline success feedback. */
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
      className={`inline-flex items-center justify-center w-6 h-6 rounded-lg transition-all duration-200 select-none ${
        copied
          ? 'bg-emerald-400/15 text-emerald-300 ring-1 ring-emerald-400/30'
          : 'bg-white/[0.06] text-gray-400 hover:bg-white/[0.12] hover:text-gray-200 ring-1 ring-white/[0.08]'
      }`}
    >
      {copied ? (
        <svg className="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
          <path d="M20 6 9 17l-5-5" />
        </svg>
      ) : (
        <svg className="w-3.5 h-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <rect x="9" y="9" width="13" height="13" rx="2" />
          <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
        </svg>
      )}
    </button>
  );
}

/**
 * iOS 26 "Liquid Glass" markdown renderer.
 * Frosted-glass surfaces, soft gradients, generous radii, refined typography.
 * Code blocks get a language badge + one-tap copy; tables/blockquotes get
 * glass card treatment.
 */
export default function StreamingRenderer({ content }: Props) {
  if (!content?.trim()) return <span className="text-gray-500 italic text-xs">...</span>;

  return (
    <div className="md-body text-sm text-gray-200">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[rehypeHighlight]}
        components={{
          h1: ({ children }) => (
            <h1 className="md-h1">
              {children}
            </h1>
          ),
          h2: ({ children }) => (
            <h2 className="md-h2">
              {children}
            </h2>
          ),
          h3: ({ children }) => (
            <h3 className="md-h3">
              {children}
            </h3>
          ),
          h4: ({ children }) => (
            <h4 className="md-h4">{children}</h4>
          ),
          p: ({ children }) => <p className="md-p">{children}</p>,
          ul: ({ children }) => <ul className="md-ul">{children}</ul>,
          ol: ({ children }) => <ol className="md-ol">{children}</ol>,
          li: ({ children }) => <li className="md-li">{children}</li>,
          strong: ({ children }) => (
            <strong className="font-semibold text-gray-50">{children}</strong>
          ),
          em: ({ children }) => <em className="md-em">{children}</em>,
          del: ({ children }) => <del className="opacity-50">{children}</del>,
          hr: () => <hr className="md-hr" />,

          code({ className, children, ...props }: any) {
            const inline = !className || !className.includes('language-');
            const text = nodeText(children);
            if (inline) {
              return (
                <code className="md-inline-code" {...props}>
                  {children}
                </code>
              );
            }
            const lang = className?.replace('language-', '').trim() || 'text';
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
                  <code className={`hljs ${className || ''}`} {...props}>
                    {children}
                  </code>
                </pre>
              </div>
            );
          },

          pre: ({ children }) => <>{children}</>,

          a: ({ href, children }) => (
            <a
              href={href}
              className="md-a"
              target="_blank"
              rel="noopener noreferrer"
            >
              {children}
            </a>
          ),

          table: ({ children }) => (
            <div className="md-table-wrap">
              <table className="md-table">{children}</table>
            </div>
          ),
          thead: ({ children }) => <thead className="md-thead">{children}</thead>,
          tbody: ({ children }) => <tbody className="md-tbody">{children}</tbody>,
          tr: ({ children }) => <tr className="md-tr">{children}</tr>,
          th: ({ children }) => <th className="md-th">{children}</th>,
          td: ({ children }) => <td className="md-td">{children}</td>,

          blockquote: ({ children }) => (
            <blockquote className="md-quote">{children}</blockquote>
          ),

          img: ({ src, alt }) => (
            <img
              src={src}
              alt={alt}
              className="max-w-full rounded-2xl my-3 border border-white/[0.08] shadow-lg shadow-black/20"
              loading="lazy"
            />
          ),
        }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}
