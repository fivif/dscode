import { useState, useEffect, useMemo, useCallback } from 'react';
import { wikiGraph, wikiIngest } from '@/lib/tauri';
import type { WikiNode, WikiGraph } from '@/lib/types';
import WikiGraphView from './WikiGraph';
import StreamingRenderer from '@/components/Chat/StreamingRenderer';
import { useChatStore } from '@/stores/chatStore';

// ── constants ──
const TYPE_ORDER = ['fact', 'concept', 'pattern', 'decision', 'rule'] as const;
const TYPE_LABELS: Record<string, string> = {
  fact: 'Facts',
  concept: 'Concepts',
  pattern: 'Patterns',
  decision: 'Decisions',
  rule: 'Rules',
};

const TYPE_COLORS: Record<string, string> = {
  fact: '#34d399',
  concept: '#a78bfa',
  pattern: '#fbbf24',
  decision: '#60a5fa',
  rule: '#f472b6',
};

// ── helpers ──
function extractWikilinks(content: string): string[] {
  if (!content) return [];
  const re = /\[\[([^\]]+)\]\]/g;
  const out: string[] = [];
  let m: RegExpExecArray | null;
  while ((m = re.exec(content)) !== null) {
    const title = m[1].trim();
    if (title) out.push(title);
  }
  return [...new Set(out)];
}

export default function WikiPage({ onBack }: { onBack: () => void }) {
  const [tab, setTab] = useState<'browse' | 'graph'>('browse');
  const [query, setQuery] = useState('');
  const [allNodes, setAllNodes] = useState<WikiNode[]>([]);
  const [graphEdges, setGraphEdges] = useState<WikiGraph['edges']>([]);
  const [selected, setSelected] = useState<WikiNode | null>(null);
  const [loading, setLoading] = useState(true);
  const [ingesting, setIngesting] = useState(false);
  const [expandedTypes, setExpandedTypes] = useState<Set<string>>(new Set(TYPE_ORDER));
  const activeSessionId = useChatStore((s) => s.activeSessionId);

  // ── load all nodes on mount ──
  const loadGraph = useCallback(async () => {
    setLoading(true);
    try {
      const g = await wikiGraph();
      // Normalise graph nodes into WikiNode shape (the graph response has fewer fields typed)
      const nodes: WikiNode[] = (g.nodes || []).map((n: any) => ({
        id: n.id,
        title: n.title || n.id,
        content: n.content || '',
        node_type: n.node_type || 'fact',
        tags: n.tags || [],
        links: n.links || extractWikilinks(n.content || ''),
      }));
      setAllNodes(nodes);
      setGraphEdges(g.edges || []);
    } catch {
      setAllNodes([]);
      setGraphEdges([]);
    }
    setLoading(false);
  }, []);

  useEffect(() => { loadGraph(); }, [loadGraph]);

  // ── client-side filtering ──
  const q = query.trim().toLowerCase();
  const filteredNodes = useMemo(() => {
    if (!q) return allNodes;
    return allNodes.filter((n) => {
      if (n.title.toLowerCase().includes(q)) return true;
      if ((n.content || '').toLowerCase().includes(q)) return true;
      if (n.tags?.some((t) => t.toLowerCase().includes(q))) return true;
      return false;
    });
  }, [allNodes, q]);

  // ── group filtered nodes by type ──
  const grouped = useMemo(() => {
    const map: Record<string, WikiNode[]> = {};
    for (const t of TYPE_ORDER) map[t] = [];
    for (const n of filteredNodes) {
      const t = n.node_type || 'fact';
      if (!map[t]) map[t] = [];
      map[t].push(n);
    }
    return map;
  }, [filteredNodes]);

  // ── backlinks for selected node ──
  const backlinks = useMemo(() => {
    if (!selected) return [];
    const title = selected.title.toLowerCase();
    return allNodes.filter((n) => {
      if (n.id === selected.id) return false;
      const links = n.links.length > 0 ? n.links : extractWikilinks(n.content || '');
      return links.some((l) => l.toLowerCase() === title);
    });
  }, [selected, allNodes]);

  // ── linked-pages for selected node ──
  const linkedPages = useMemo(() => {
    if (!selected) return [];
    const links = selected.links.length > 0 ? selected.links : extractWikilinks(selected.content || '');
    const lowerLinks = links.map((l) => l.toLowerCase());
    return allNodes.filter((n) => n.id !== selected.id && lowerLinks.includes(n.title.toLowerCase()));
  }, [selected, allNodes]);

  // ── ingest ──
  const handleIngest = async () => {
    if (!activeSessionId) return;
    setIngesting(true);
    try {
      await wikiIngest(activeSessionId);
      await loadGraph();
    } catch { /* ignore */ }
    setIngesting(false);
  };

  const toggleType = (t: string) => {
    setExpandedTypes((prev) => {
      const next = new Set(prev);
      if (next.has(t)) next.delete(t);
      else next.add(t);
      return next;
    });
  };

  const selectNode = (n: WikiNode) => {
    setSelected(n);
  };

  // ── graph data for the graph tab ──
  const graphData: WikiGraph | null = useMemo(() => {
    if (!allNodes.length) return null;
    return { nodes: allNodes, edges: graphEdges };
  }, [allNodes, graphEdges]);

  return (
    <div className="flex-1 flex flex-col bg-main">
      {/* ── header ── */}
      <div className="flex items-center gap-4 px-4 py-3 border-b border-border shrink-0">
        <button onClick={onBack} className="text-gray-400 hover:text-gray-200 text-sm transition-colors">
          &larr; Back
        </button>
        <h2 className="text-sm font-semibold text-gray-200">Digital Garden</h2>
        <div className="flex gap-1 ml-auto bg-gray-900 rounded-lg p-0.5">
          <button
            className={`text-xs px-3 py-1.5 rounded-md transition-colors ${tab === 'browse' ? 'bg-gray-700 text-gray-100' : 'text-gray-500 hover:text-gray-300'}`}
            onClick={() => setTab('browse')}
          >
            Browse
          </button>
          <button
            className={`text-xs px-3 py-1.5 rounded-md transition-colors ${tab === 'graph' ? 'bg-gray-700 text-gray-100' : 'text-gray-500 hover:text-gray-300'}`}
            onClick={() => setTab('graph')}
          >
            Graph
          </button>
        </div>
      </div>

      {/* ── search bar ── */}
      <div className="flex gap-2 px-4 py-3 border-b border-border shrink-0">
        <div className="flex-1 relative">
          <svg className="absolute left-3 top-1/2 -translate-y-1/2 w-3.5 h-3.5 text-gray-600" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
            <path strokeLinecap="round" strokeLinejoin="round" d="M21 21l-6-6m2-5a7 7 0 11-14 0 7 7 0 0114 0z" />
          </svg>
          <input
            className="w-full bg-input border border-border rounded-lg pl-9 pr-3 py-2 text-sm text-gray-200 placeholder-gray-600 focus:outline-none focus:border-gray-500 transition-colors"
            placeholder="Filter pages..."
            value={query}
            onChange={(e) => { setQuery(e.target.value); setSelected(null); }}
          />
        </div>
        <button
          className="px-4 py-2 bg-blue-700 text-sm text-gray-100 rounded-lg hover:bg-blue-600 disabled:opacity-40 transition-colors shrink-0"
          onClick={handleIngest}
          disabled={ingesting || !activeSessionId}
          title={activeSessionId ? 'Auto-ingest from current session' : 'No active session'}
        >
          {ingesting ? '...' : 'Ingest'}
        </button>
      </div>

      {/* ── main content ── */}
      <div className="flex-1 overflow-y-auto">
        {tab === 'browse' && (
          <div className="p-4">
            {loading ? (
              <p className="text-gray-500 text-sm text-center py-12">Loading garden...</p>
            ) : selected ? (
              /* ── detail view ── */
              <div>
                <button
                  onClick={() => setSelected(null)}
                  className="text-xs text-gray-500 hover:text-gray-300 mb-3 flex items-center gap-1 transition-colors"
                >
                  <svg className="w-3 h-3" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
                    <path strokeLinecap="round" strokeLinejoin="round" d="M15 19l-7-7 7-7" />
                  </svg>
                  Back to list
                </button>

                <article className="bg-card border border-border rounded-xl p-5 mb-4">
                  <div className="flex items-center gap-2 mb-1">
                    <span
                      className="text-[10px] px-1.5 py-0.5 rounded font-medium uppercase tracking-wider"
                      style={{ background: TYPE_COLORS[selected.node_type] + '22', color: TYPE_COLORS[selected.node_type] || '#888' }}
                    >
                      {selected.node_type || 'fact'}
                    </span>
                    {selected.tags?.length > 0 && selected.tags.map((t, i) => (
                      <span key={i} className="text-[10px] px-1.5 py-0.5 rounded bg-gray-800 text-gray-500">{t}</span>
                    ))}
                  </div>
                  <h1 className="text-lg font-semibold text-gray-100 mb-4">{selected.title}</h1>
                  <div className="text-sm text-gray-300 leading-relaxed">
                    <StreamingRenderer content={selected.content || ''} />
                  </div>
                </article>

                {/* linked pages (outgoing) */}
                {linkedPages.length > 0 && (
                  <div className="mb-4">
                    <h3 className="text-[11px] font-semibold text-gray-500 mb-2 uppercase tracking-wider">
                      Links to ({linkedPages.length})
                    </h3>
                    <div className="space-y-1">
                      {linkedPages.map((b) => (
                        <button key={b.id} onClick={() => selectNode(b)}
                          className="w-full text-left px-3 py-2 rounded-lg bg-gray-800/40 hover:bg-gray-800 border border-border/30 hover:border-border/60 text-sm text-gray-300 transition-all">
                          <span
                            className="text-[10px] px-1 py-0.5 rounded mr-2 font-medium"
                            style={{ background: TYPE_COLORS[b.node_type] + '22', color: TYPE_COLORS[b.node_type] || '#888' }}
                          >
                            {b.node_type || 'fact'}
                          </span>
                          {b.title}
                        </button>
                      ))}
                    </div>
                  </div>
                )}

                {/* backlinks (incoming) */}
                {backlinks.length > 0 && (
                  <div>
                    <h3 className="text-[11px] font-semibold text-gray-500 mb-2 uppercase tracking-wider">
                      Linked from ({backlinks.length})
                    </h3>
                    <div className="space-y-1">
                      {backlinks.map((b) => (
                        <button key={b.id} onClick={() => selectNode(b)}
                          className="w-full text-left px-3 py-2 rounded-lg bg-gray-800/40 hover:bg-gray-800 border border-border/30 hover:border-border/60 text-sm text-gray-300 transition-all">
                          <span
                            className="text-[10px] px-1 py-0.5 rounded mr-2 font-medium"
                            style={{ background: TYPE_COLORS[b.node_type] + '22', color: TYPE_COLORS[b.node_type] || '#888' }}
                          >
                            {b.node_type || 'fact'}
                          </span>
                          {b.title}
                        </button>
                      ))}
                    </div>
                  </div>
                )}

                {linkedPages.length === 0 && backlinks.length === 0 && (
                  <p className="text-xs text-gray-600 text-center py-4">No links yet</p>
                )}
              </div>
            ) : filteredNodes.length > 0 ? (
              /* ── browse: grouped list ── */
              <div className="space-y-2">
                {TYPE_ORDER.map((type) => {
                  const items = grouped[type] || [];
                  if (items.length === 0) return null;
                  const expanded = expandedTypes.has(type);
                  return (
                    <div key={type}>
                      <button
                        onClick={() => toggleType(type)}
                        className={`w-full flex items-center gap-2 px-3 py-2 rounded-lg text-left transition-colors ${
                          expanded ? 'bg-gray-800/60' : 'hover:bg-gray-800/30'
                        }`}
                      >
                        <svg
                          className={`w-3 h-3 text-gray-500 transition-transform ${expanded ? 'rotate-90' : ''}`}
                          fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}
                        >
                          <path strokeLinecap="round" strokeLinejoin="round" d="M9 5l7 7-7 7" />
                        </svg>
                        <span className="text-xs font-medium" style={{ color: TYPE_COLORS[type] || '#888' }}>
                          {TYPE_LABELS[type] || type}
                        </span>
                        <span className="text-[10px] text-gray-600 ml-auto">{items.length}</span>
                      </button>
                      {expanded && (
                        <div className="mt-1 ml-5 space-y-1">
                          {items.map((n) => (
                            <button key={n.id} onClick={() => selectNode(n)}
                              className="w-full text-left px-3 py-2.5 rounded-lg hover:bg-gray-800/50 border border-transparent hover:border-border/40 transition-all group">
                              <div className="flex items-center gap-2 mb-0.5">
                                <span
                                  className="text-[10px] px-1.5 py-0.5 rounded font-medium uppercase tracking-wider"
                                  style={{ background: TYPE_COLORS[n.node_type] + '22', color: TYPE_COLORS[n.node_type] || '#888' }}
                                >
                                  {n.node_type || 'fact'}
                                </span>
                                <h3 className="text-sm font-medium text-gray-200 group-hover:text-gray-100 transition-colors">
                                  {n.title}
                                </h3>
                                {n.links?.length > 0 && (
                                  <span className="text-[10px] text-gray-600 ml-auto">{n.links.length} link{n.links.length !== 1 ? 's' : ''}</span>
                                )}
                              </div>
                              {(n.content || '').trim() && (
                                <p className="text-xs text-gray-500 line-clamp-2 mt-1">
                                  {(n.content || '').replace(/\[\[([^\]]+)\]\]/g, '$1').slice(0, 140)}
                                </p>
                              )}
                            </button>
                          ))}
                        </div>
                      )}
                    </div>
                  );
                })}
              </div>
            ) : (
              <div className="text-center py-16">
                <p className="text-gray-500 text-sm mb-1">
                  {query ? 'No pages match your filter.' : 'Your garden is empty.'}
                </p>
                {!query && (
                  <p className="text-xs text-gray-600">
                    Use <span className="text-blue-400">Ingest</span> to extract knowledge from the current session.
                  </p>
                )}
              </div>
            )}
          </div>
        )}

        {tab === 'graph' && (
          <div className="p-4">
            {graphData && graphData.nodes.length > 0 ? (
              <WikiGraphView graph={graphData} highlightId={selected?.id} />
            ) : loading ? (
              <p className="text-gray-500 text-sm text-center py-16">Loading graph...</p>
            ) : (
              <p className="text-gray-500 text-sm text-center py-16">Graph is empty.</p>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
