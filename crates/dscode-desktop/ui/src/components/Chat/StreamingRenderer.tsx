import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';

interface Props {
  content: string;
}

/** Neutral dark markdown — no blue table borders / accents. */
export default function StreamingRenderer({ content }: Props) {
  if (!content?.trim()) return <span className="text-gray-500 italic text-xs">...</span>;

  return (
    <div className="markdown-body text-sm text-gray-200">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          h1: ({ children }) => (
            <h1 className="text-[1.35rem] font-semibold text-gray-100 mt-0 mb-3 pb-2 border-b border-white/[0.08] leading-tight tracking-tight">
              {children}
            </h1>
          ),
          h2: ({ children }) => (
            <h2 className="text-[1.15rem] font-semibold text-gray-100 mt-5 mb-2.5 pb-1.5 border-b border-white/[0.06] leading-tight">
              {children}
            </h2>
          ),
          h3: ({ children }) => (
            <h3 className="text-base font-semibold mt-4 mb-2 leading-snug text-gray-100">
              {children}
            </h3>
          ),
          h4: ({ children }) => (
            <h4 className="text-sm font-semibold text-gray-200 mt-3.5 mb-1.5">{children}</h4>
          ),
          p: ({ children }) => (
            <p className="my-2.5 leading-[1.7] text-gray-200/95 whitespace-pre-wrap">{children}</p>
          ),
          ul: ({ children }) => (
            <ul className="my-2.5 pl-5 list-disc space-y-1 text-gray-200 marker:text-gray-600 leading-[1.65]">
              {children}
            </ul>
          ),
          ol: ({ children }) => (
            <ol className="my-2.5 pl-5 list-decimal space-y-1 text-gray-200 marker:text-gray-600 leading-[1.65]">
              {children}
            </ol>
          ),
          li: ({ children }) => <li className="text-gray-200/95 pl-0.5">{children}</li>,
          strong: ({ children }) => <strong className="font-semibold text-gray-100">{children}</strong>,
          em: ({ children }) => <em className="italic text-gray-400">{children}</em>,
          hr: () => <hr className="my-4 border-0 border-t border-white/[0.08]" />,

          code({ className, children, ...props }: any) {
            const inline = !className || !className.includes('language-');
            if (inline) {
              return (
                <code
                  className="bg-white/[0.06] text-gray-200 px-1.5 py-0.5 rounded text-[12.5px] font-mono border border-white/[0.06]"
                  style={{ wordBreak: 'keep-all', whiteSpace: 'pre-wrap' }}
                  {...props}
                >
                  {children}
                </code>
              );
            }
            const lang = className?.replace('language-', '') || 'text';
            return (
              <div className="my-3 rounded-md overflow-hidden border border-white/[0.08] bg-[#14151a]">
                <div className="flex items-center justify-between bg-white/[0.03] text-gray-500 text-[10px] px-3 py-1 font-mono uppercase tracking-wider">
                  <span>{lang}</span>
                </div>
                <pre className="p-3.5 overflow-x-auto m-0">
                  <code className="text-[12.5px] leading-[1.55] font-mono text-gray-300">{children}</code>
                </pre>
              </div>
            );
          },

          pre: ({ children }) => <>{children}</>,

          a: ({ href, children }) => (
            <a
              href={href}
              className="text-gray-200 underline decoration-gray-600 hover:decoration-gray-400 underline-offset-2 hover:text-white transition-colors"
              target="_blank"
              rel="noopener noreferrer"
            >
              {children}
            </a>
          ),

          table: ({ children }) => (
            <div className="overflow-x-auto my-3.5 rounded-md border border-white/[0.08]">
              <table className="w-full text-[12.5px] border-collapse">{children}</table>
            </div>
          ),
          thead: ({ children }) => <thead className="bg-white/[0.04]">{children}</thead>,
          tbody: ({ children }) => <tbody className="divide-y divide-white/[0.05]">{children}</tbody>,
          tr: ({ children }) => (
            <tr className="even:bg-white/[0.015] hover:bg-white/[0.03] transition-colors">{children}</tr>
          ),
          th: ({ children }) => (
            <th className="border-b border-white/[0.1] px-3 py-2 text-left font-medium text-gray-200 whitespace-nowrap">
              {children}
            </th>
          ),
          td: ({ children }) => (
            <td className="border-b border-white/[0.05] px-3 py-2 text-gray-400 align-top leading-relaxed">
              {children}
            </td>
          ),

          blockquote: ({ children }) => (
            <blockquote className="border-l-2 border-gray-600 bg-white/[0.03] my-3 py-2 px-3.5 rounded-r-md text-gray-400 not-italic leading-[1.65]">
              {children}
            </blockquote>
          ),

          img: ({ src, alt }) => (
            <img
              src={src}
              alt={alt}
              className="max-w-full rounded-md my-3 border border-white/[0.08]"
            />
          ),
        }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}
