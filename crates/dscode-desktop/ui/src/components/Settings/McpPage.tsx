import { useState, useEffect } from 'react';
import * as tauri from '@/lib/tauri';

interface Props { onBack: () => void; }

interface ToolItem { name: string; description: string; }
interface McpServer { name: string; command: string; args: string; }

export default function McpPage({ onBack }: Props) {
  const [tools, setTools] = useState<ToolItem[]>([]);
  const [builtinOpen, setBuiltinOpen] = useState(false);
  const [extOpen, setExtOpen] = useState(true);
  const [showAdd, setShowAdd] = useState(false);
  const [servers, setServers] = useState<McpServer[]>([]);
  const [newName, setNewName] = useState('');
  const [newCmd, setNewCmd] = useState('');
  const [newArgs, setNewArgs] = useState('');

  useEffect(() => { tauri.listTools().then(setTools).catch(() => {}); }, []);

  const addServer = () => {
    if (!newName || !newCmd) return;
    setServers([...servers, { name: newName, command: newCmd, args: newArgs }]);
    setNewName(''); setNewCmd(''); setNewArgs('');
    setShowAdd(false);
  };

  const removeServer = (name: string) => {
    setServers(servers.filter((s) => s.name !== name));
  };

  return (
    <div className="flex-1 flex flex-col bg-main h-full">
      <div className="flex items-center gap-3 px-6 py-3.5 border-b border-border shrink-0">
        <button className="text-gray-400 hover:text-gray-200" onClick={onBack}>
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><polyline points="15 18 9 12 15 6" /></svg>
        </button>
        <h2 className="text-base font-medium text-gray-200">MCP 工具</h2>
        <span className="text-xs text-gray-500 ml-auto">{tools.length + servers.length} 工具</span>
      </div>

      <div className="flex-1 overflow-y-auto">
        <div className="max-w-xl mx-auto px-8 py-6 space-y-4">

          {/* 内置工具 — collapsed by default */}
          <div className="bg-card border border-border rounded-lg overflow-hidden">
            <button
              className="w-full flex items-center gap-2 px-4 py-3 hover:bg-white/[0.02] transition-colors"
              onClick={() => setBuiltinOpen(!builtinOpen)}
            >
              <svg className={`w-3 h-3 text-gray-500 transition-transform ${builtinOpen ? 'rotate-90' : ''}`}
                viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><polyline points="9 18 15 12 9 6" /></svg>
              <span className="text-sm text-gray-200">内置工具</span>
              <span className="text-xs text-gray-600 ml-auto">{tools.length}</span>
            </button>
            {builtinOpen && (
              <div className="border-t border-border/50 px-4 py-2 space-y-1">
                {tools.map((t) => (
                  <div key={t.name} className="flex items-center gap-3 py-1.5">
                    <span className="text-xs font-mono text-gray-300 w-28 shrink-0">{t.name}</span>
                    <span className="text-xs text-gray-500 truncate">{t.description}</span>
                  </div>
                ))}
              </div>
            )}
          </div>

          {/* 第三方 MCP */}
          <div className="bg-card border border-border rounded-lg overflow-hidden">
            <button
              className="w-full flex items-center gap-2 px-4 py-3 hover:bg-white/[0.02] transition-colors"
              onClick={() => setExtOpen(!extOpen)}
            >
              <svg className={`w-3 h-3 text-gray-500 transition-transform ${extOpen ? 'rotate-90' : ''}`}
                viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><polyline points="9 18 15 12 9 6" /></svg>
              <span className="text-sm text-gray-200">第三方 MCP 服务器</span>
              <span className="text-xs text-gray-600 ml-auto">{servers.length}</span>
            </button>
            {extOpen && (
              <div className="border-t border-border/50 px-4 py-3 space-y-2">
                {servers.map((s) => (
                  <div key={s.name} className="flex items-center gap-2 bg-input rounded-lg px-3 py-2">
                    <span className="text-xs font-mono text-gray-300 flex-1">{s.name}</span>
                    <span className="text-[10px] text-gray-500">{s.command} {s.args}</span>
                    <button className="text-gray-600 hover:text-red-400 text-xs" onClick={() => removeServer(s.name)}>✕</button>
                  </div>
                ))}
                {showAdd ? (
                  <div className="space-y-2 bg-input rounded-lg p-3">
                    <input className="w-full bg-card border border-border rounded px-2 py-1.5 text-xs text-gray-200 placeholder-gray-500" placeholder="服务器名称" value={newName} onChange={(e) => setNewName(e.target.value)} />
                    <input className="w-full bg-card border border-border rounded px-2 py-1.5 text-xs text-gray-200 placeholder-gray-500" placeholder="命令 (如 npx)" value={newCmd} onChange={(e) => setNewCmd(e.target.value)} />
                    <input className="w-full bg-card border border-border rounded px-2 py-1.5 text-xs text-gray-200 placeholder-gray-500" placeholder="参数 (如 -y @anthropic/mcp-server)" value={newArgs} onChange={(e) => setNewArgs(e.target.value)} />
                    <div className="flex gap-2">
                      <button className="flex-1 py-1.5 text-xs text-white bg-gray-600 hover:bg-gray-500 rounded" onClick={addServer}>添加</button>
                      <button className="px-3 py-1.5 text-xs text-gray-400 hover:text-gray-200" onClick={() => setShowAdd(false)}>取消</button>
                    </div>
                  </div>
                ) : (
                  <button className="w-full py-2 text-xs text-gray-500 hover:text-gray-300 border border-dashed border-border rounded-lg" onClick={() => setShowAdd(true)}>
                    + 添加 MCP 服务器
                  </button>
                )}
              </div>
            )}
          </div>

        </div>
      </div>
    </div>
  );
}
