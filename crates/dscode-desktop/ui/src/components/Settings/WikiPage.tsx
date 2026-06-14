import { useState, useEffect } from 'react';
import { wikiSearch, wikiGraph, wikiIngest } from '@/lib/tauri';
import type { WikiNode, WikiGraph } from '@/lib/types';
import WikiGraphView from './WikiGraph';
import StreamingRenderer from '@/components/Chat/StreamingRenderer';
import { useChatStore } from '@/stores/chatStore';

export default function WikiPage({ onBack }: { onBack: () => void }) {
  const [tab, setTab] = useState<'browse' | 'graph'>('browse');
  const [query, setQuery] = useState('');
  const [nodes, setNodes] = useState<WikiNode[]>([]);
  const [graph, setGraph] = useState<WikiGraph | null>(null);
  const [selected, setSelected] = useState<WikiNode | null>(null);
  const [backlinks, setBacklinks] = useState<WikiNode[]>([]);
  const [loading, setLoading] = useState(false);
  const [ingesting, setIngesting] = useState(false);
  const activeSessionId = useChatStore((s) => s.activeSessionId);

  useEffect(() => { wikiGraph().then(setGraph).catch(() => {}); }, []);

  const handleSearch = async () => {
    if (!query.trim()) return;
    setLoading(true);
    try {
      const results = await wikiSearch(query);
      setNodes(results);
      setSelected(null);
    } catch {}
    setLoading(false);
  };

  const selectNode = async (n: WikiNode) => {
    setSelected(n);
    setLoading(true);
    try {
      // Find nodes that link to this one
      const bl = await wikiSearch(n.title);
      setBacklinks(bl.filter((b: WikiNode) => b.id !== n.id));
    } catch { setBacklinks([]); }
    setLoading(false);
  };

  const handleIngest = async () => {
    if (!activeSessionId) return;
    setIngesting(true);
    try { await wikiIngest(activeSessionId); await wikiGraph().then(setGraph); }
    catch {}
    setIngesting(false);
  };

  return (
    <div className="flex-1 flex flex-col bg-main">
      <div className="flex items-center gap-4 px-4 py-3 border-b border-border">
        <button onClick={onBack} className="text-gray-400 hover:text-gray-200 text-sm">← 返回</button>
        <h2 className="text-sm font-medium text-gray-200">Wiki 知识库</h2>
        <div className="flex gap-2 ml-auto">
          <button className={`text-xs px-3 py-1 rounded ${tab==='browse'?'bg-gray-700 text-gray-200':'text-gray-500'}`}
            onClick={() => setTab('browse')}>浏览</button>
          <button className={`text-xs px-3 py-1 rounded ${tab==='graph'?'bg-gray-700 text-gray-200':'text-gray-500'}`}
            onClick={() => setTab('graph')}>图谱</button>
        </div>
      </div>

      {/* Search bar */}
      <div className="flex gap-2 px-4 py-3 border-b border-border">
        <input className="flex-1 bg-input border border-border rounded-lg px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-gray-500"
          placeholder="搜索知识节点..." value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && handleSearch()} />
        <button className="px-4 py-2 bg-gray-700 text-sm text-gray-200 rounded-lg hover:bg-gray-600"
          onClick={handleSearch} disabled={loading}>{loading ? '...' : '搜索'}</button>
        <button className="px-4 py-2 bg-blue-700 text-sm text-gray-200 rounded-lg hover:bg-blue-600 disabled:opacity-50"
          onClick={handleIngest} disabled={ingesting || !activeSessionId}
          title={activeSessionId ? '从当前会话提取知识' : '无活跃会话'}>
          {ingesting ? '摄入中...' : '自动摄入'}
        </button>
      </div>

      <div className="flex-1 overflow-y-auto">
        {tab === 'browse' && (
          <div className="p-4">
            {selected ? (
              /* Node detail view */
              <div>
                <button onClick={() => setSelected(null)} className="text-xs text-gray-500 hover:text-gray-300 mb-3 block">
                  ← 返回列表
                </button>
                <article className="bg-card border border-border rounded-xl p-5 mb-4">
                  <div className="flex items-center gap-2 mb-3">
                    <span className="text-[10px] px-1.5 py-0.5 rounded bg-gray-700 text-gray-400 uppercase">
                      {selected.node_type || 'fact'}
                    </span>
                    <h1 className="text-base font-semibold text-gray-100">{selected.title}</h1>
                  </div>
                  <div className="text-sm text-gray-300 leading-relaxed">
                    <StreamingRenderer content={selected.content || ''} />
                  </div>
                </article>

                {/* Backlinks */}
                {backlinks.length > 0 && (
                  <div>
                    <h3 className="text-xs font-medium text-gray-500 mb-2 uppercase tracking-wide">反向链接 ({backlinks.length})</h3>
                    <div className="space-y-1">
                      {backlinks.map((b, i) => (
                        <button key={i} onClick={() => selectNode(b)}
                          className="w-full text-left px-3 py-2 rounded-lg bg-gray-800/50 hover:bg-gray-800 text-sm text-gray-300 transition-colors">
                          <span className="text-[10px] text-gray-500 mr-2">{b.node_type || 'fact'}</span>
                          {b.title}
                        </button>
                      ))}
                    </div>
                  </div>
                )}
              </div>
            ) : nodes.length > 0 ? (
              /* Node list */
              <div className="space-y-2">
                {nodes.map((n, i) => (
                  <button key={i} onClick={() => selectNode(n)}
                    className="w-full text-left p-4 rounded-lg bg-gray-800/30 hover:bg-gray-800 border border-border/50 hover:border-border transition-all">
                    <div className="flex items-center gap-2 mb-1">
                      <span className="text-[10px] px-1.5 py-0.5 rounded bg-gray-700 text-gray-400 uppercase">{n.node_type || 'fact'}</span>
                      <h3 className="text-sm font-medium text-gray-200">{n.title}</h3>
                    </div>
                    <p className="text-xs text-gray-500 line-clamp-2">{(n.content || '').slice(0, 120)}</p>
                  </button>
                ))}
              </div>
            ) : (
              <p className="text-gray-500 text-sm text-center py-12">
                {query ? '无结果' : '输入关键词搜索，或点击「自动摄入」从当前会话提取知识'}
              </p>
            )}
          </div>
        )}

        {tab === 'graph' && (
          <div className="p-4">
            {graph && graph.nodes?.length > 0 ? (
              <WikiGraphView graph={graph} highlightId={selected?.id} />
            ) : (
              <p className="text-gray-500 text-sm text-center py-12">图谱加载中...</p>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
