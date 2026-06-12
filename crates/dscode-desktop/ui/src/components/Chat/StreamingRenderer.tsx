import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import rehypeRaw from 'rehype-raw';

interface Props { content: string; }

export default function StreamingRenderer({ content }: Props) {
  if (!content?.trim()) return <span className="text-gray-500 italic text-xs">...</span>;

  return (
    <div className="text-sm text-gray-200 space-y-1">
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        rehypePlugins={[rehypeRaw]}
        components={{
          h1: ({ children }) => <h1 className="text-lg font-bold text-gray-100 mt-3 mb-2 pb-1 border-b border-border">{children}</h1>,
          h2: ({ children }) => <h2 className="text-base font-semibold text-gray-100 mt-2.5 mb-1.5">{children}</h2>,
          h3: ({ children }) => <h3 className="text-sm font-semibold text-gray-200 mt-2 mb-1">{children}</h3>,
          p: ({ children }) => <p className="my-2 leading-relaxed text-gray-200">{children}</p>,
          ul: ({ children }) => <ul className="my-2 pl-5 list-disc space-y-1 text-gray-200">{children}</ul>,
          ol: ({ children }) => <ol className="my-2 pl-5 list-decimal space-y-1 text-gray-200">{children}</ol>,
          li: ({ children }) => <li className="text-gray-200">{children}</li>,
          strong: ({ children }) => <strong className="font-semibold text-gray-100">{children}</strong>,
          em: ({ children }) => <em className="italic text-gray-300">{children}</em>,
          hr: () => <hr className="my-3 border-border" />,

          code({ node, className, children, ...props }: any) {
            const inline = !className || !className.includes('language-');
            if (inline) {
              return <code className="bg-gray-800 text-gray-200 px-1.5 py-0.5 rounded text-[13px] font-mono" {...props}>{children}</code>;
            }
            const lang = className?.replace('language-', '') || '';
            return (
              <div className="my-2 rounded-md overflow-hidden border border-border">
                {lang && <div className="bg-gray-800 text-gray-500 text-[10px] px-3 py-1 font-mono tracking-wide">{lang}</div>}
                <pre className="bg-gray-900/80 p-3 overflow-x-auto">
                  <code className="text-[13px] leading-relaxed font-mono text-gray-200">{children}</code>
                </pre>
              </div>
            );
          },

          pre: ({ children }) => <>{children}</>,

          a: ({ href, children }) => (
            <a href={href} className="text-blue-400 hover:text-blue-300 underline" target="_blank" rel="noopener noreferrer">{children}</a>
          ),

          table: ({ children }) => (
            <div className="overflow-x-auto my-2 rounded-md border border-border">
              <table className="w-full text-xs">{children}</table>
            </div>
          ),
          th: ({ children }) => <th className="border border-border px-3 py-2 bg-gray-800 text-left font-medium text-gray-200">{children}</th>,
          td: ({ children }) => <td className="border border-border px-3 py-2 text-gray-300">{children}</td>,

          blockquote: ({ children }) => (
            <blockquote className="border-l-3 border-gray-600 pl-4 my-2 text-gray-400 italic">{children}</blockquote>
          ),
        }}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
}
