import { useState, useEffect } from 'react';
import { wikiSearch, wikiGraph } from '@/lib/tauri';
import type { WikiNode, WikiGraph } from '@/lib/types';

interface Props { onBack: () => void; }

export default function WikiPage({ onBack }: Props) {
  const [query, setQuery] = useState('');
  const [nodes, setNodes] = useState<WikiNode[]>([]);
  const [graph, setGraph] = useState<WikiGraph | null>(null);
  const [loading, setLoading] = useState(false);
  const [tab, setTab] = useState<'search' | 'graph'>('search');

  useEffect(() => {
    wikiGraph().then(setGraph).catch(() => {});
  }, []);

  const handleSearch = async () => {
    if (!query.trim()) return;
    setLoading(true);
    try {
      const results = await wikiSearch(query);
      setNodes(results);
    } catch {}
    setLoading(false);
  };

  return (
    <div className="flex-1 flex flex-col bg-main">
      <div className="flex items-center gap-4 px-4 py-3 border-b border-border">
        <button onClick={onBack} className="text-gray-400 hover:text-gray-200 text-sm">← 返回</button>
        <h2 className="text-sm font-medium text-gray-200">知识图谱 Wiki</h2>
        <div className="flex gap-2 ml-auto">
          <button
            className={`text-xs px-3 py-1 rounded ${tab === 'search' ? 'bg-gray-700 text-gray-200' : 'text-gray-500'}`}
            onClick={() => setTab('search')}>搜索</button>
          <button
            className={`text-xs px-3 py-1 rounded ${tab === 'graph' ? 'bg-gray-700 text-gray-200' : 'text-gray-500'}`}
            onClick={() => setTab('graph')}>图谱</button>
        </div>
      </div>

      <div className="flex-1 overflow-y-auto p-4">
        {tab === 'search' && (
          <div>
            <div className="flex gap-2 mb-4">
              <input
                className="flex-1 bg-input border border-border rounded-lg px-3 py-2 text-sm text-gray-200 focus:outline-none focus:border-gray-500"
                placeholder="搜索知识节点..."
                value={query}
                onChange={(e) => setQuery(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && handleSearch()}
              />
              <button
                className="px-4 py-2 bg-gray-700 text-sm text-gray-200 rounded-lg hover:bg-gray-600"
                onClick={handleSearch} disabled={loading}
              >{loading ? '...' : '搜索'}</button>
            </div>
            {nodes.length === 0 && (
              <p className="text-gray-500 text-sm text-center py-8">
                {query ? '无结果' : '输入关键词搜索知识图谱'}
              </p>
            )}
            <div className="space-y-3">
              {nodes.map((n, i) => (
                <div key={i} className="bg-card border border-border rounded-lg p-4">
                  <div className="flex items-center gap-2 mb-1">
                    <span className="text-[10px] px-1.5 py-0.5 rounded bg-gray-700 text-gray-400 uppercase">{n.node_type || 'fact'}</span>
                    <h3 className="text-sm font-medium text-gray-200">{n.title}</h3>
                  </div>
                  <p className="text-xs text-gray-400 leading-relaxed">{n.content}</p>
                  {n.tags && n.tags.length > 0 && (
                    <div className="flex gap-1 mt-2">
                      {n.tags.map((t: string, j: number) => (
                        <span key={j} className="text-[10px] px-1.5 py-0.5 rounded bg-gray-800 text-gray-500">{t}</span>
                      ))}
                    </div>
                  )}
                </div>
              ))}
            </div>
          </div>
        )}

        {tab === 'graph' && (
          <div>
            <p className="text-gray-500 text-sm mb-4">知识图谱概览</p>
            {graph ? (
              <div className="space-y-4">
                <div className="bg-card border border-border rounded-lg p-4">
                  <p className="text-xs text-gray-400">节点: {graph.nodes?.length || 0}  ·  边: {graph.edges?.length || 0}</p>
                </div>
                <pre className="bg-gray-900 rounded-lg p-4 text-xs text-gray-400 overflow-x-auto max-h-96">
                  {JSON.stringify(graph, null, 2)}
                </pre>
              </div>
            ) : (
              <p className="text-gray-500 text-sm text-center py-8">加载中...</p>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
