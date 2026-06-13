import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import rehypeRaw from 'rehype-raw';

interface Props { content: string; }

export default function StreamingRenderer({ content }: Props) {
  if (!content?.trim()) return <span className="text-gray-500 italic text-xs">...</span>;

  return (
    <div className="markdown-body text-sm text-gray-200">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[rehypeRaw]}
        components={{
          h1: ({ children }) => (
            <h1 className="text-2xl font-bold text-gray-100 mt-0 mb-3 pb-2 border-b border-border leading-tight">{children}</h1>
          ),
          h2: ({ children }) => (
            <h2 className="text-xl font-bold text-gray-100 mt-6 mb-3 pb-1.5 border-b border-border leading-tight">{children}</h2>
          ),
          h3: ({ children }) => (
            <h3 className="text-lg font-semibold text-gray-100 mt-5 mb-2 leading-snug">{children}</h3>
          ),
          h4: ({ children }) => (
            <h4 className="text-base font-semibold text-gray-200 mt-4 mb-1.5">{children}</h4>
          ),
          p: ({ children }) => (
            <p className="my-3 leading-relaxed text-gray-200 whitespace-pre-wrap" style={{ lineHeight: 1.65 }}>{children}</p>
          ),
          ul: ({ children }) => (
            <ul className="my-3 pl-6 list-disc space-y-1.5 text-gray-200 marker:text-gray-500" style={{ lineHeight: 1.6 }}>{children}</ul>
          ),
          ol: ({ children }) => (
            <ol className="my-3 pl-6 list-decimal space-y-1.5 text-gray-200 marker:text-gray-500" style={{ lineHeight: 1.6 }}>{children}</ol>
          ),
          li: ({ children }) => (
            <li className="text-gray-200 mb-1.5">{children}</li>
          ),
          strong: ({ children }) => (
            <strong className="font-bold text-gray-100">{children}</strong>
          ),
          em: ({ children }) => (
            <em className="italic text-gray-300">{children}</em>
          ),
          hr: () => (
            <hr className="my-5 border-0 border-t border-border" />
          ),

          code({ node, className, children, ...props }: any) {
            const inline = !className || !className.includes('language-');
            if (inline) {
              return (
                <code className="bg-gray-800/70 text-gray-200 px-1.5 py-0.5 rounded-md text-[13px] font-mono mx-0.5"
                  style={{ wordBreak: 'keep-all', whiteSpace: 'pre-wrap' }} {...props}>
                  {children}
                </code>
              );
            }
            const lang = className?.replace('language-', '') || 'text';
            return (
              <div className="my-3 rounded-lg overflow-hidden border border-border shadow-sm">
                <div className="flex items-center justify-between bg-gray-800 text-gray-400 text-[11px] px-4 py-1.5 font-mono tracking-wide">
                  <span>{lang}</span>
                </div>
                <pre className="bg-[#1a1b26] p-4 overflow-x-auto m-0">
                  <code className="text-[13px] leading-relaxed font-mono text-gray-200" style={{ lineHeight: 1.55 }}>{children}</code>
                </pre>
              </div>
            );
          },

          pre: ({ children }) => <>{children}</>,

          a: ({ href, children }) => (
            <a href={href} className="text-blue-400 hover:text-blue-300 underline decoration-blue-400/30 hover:decoration-blue-300 underline-offset-2" target="_blank" rel="noopener noreferrer">{children}</a>
          ),

          table: ({ children }) => (
            <div className="overflow-x-auto my-4 rounded-lg border border-border">
              <table className="w-full text-xs border-collapse">{children}</table>
            </div>
          ),
          th: ({ children }) => (
            <th className="border border-gray-700 px-4 py-2.5 bg-gray-800 text-left font-semibold text-gray-100">{children}</th>
          ),
          td: ({ children }) => (
            <td className="border border-gray-700 px-4 py-2.5 text-gray-300 whitespace-pre-wrap" style={{ lineHeight: 1.5 }}>{children}</td>
          ),

          blockquote: ({ children }) => (
            <blockquote className="border-l-[3px] border-blue-500/60 bg-gray-800/30 my-4 py-3 px-4 rounded-r-lg text-gray-300 italic" style={{ lineHeight: 1.6 }}>
              {children}
            </blockquote>
          ),

          img: ({ src, alt }) => (
            <img src={src} alt={alt} className="max-w-full rounded-lg my-3 border border-border" />
          ),
        }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}
