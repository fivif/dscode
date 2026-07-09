import { useState, useEffect, useCallback } from 'react';
import * as tauri from '@/lib/tauri';
import type { McpServerInfo } from '@/lib/tauri';

interface Props {
  onBack: () => void;
}

interface ToolItem {
  name: string;
  description: string;
}

type FormMode = 'closed' | 'add' | 'edit';

export default function McpPage({ onBack }: Props) {
  const [tools, setTools] = useState<ToolItem[]>([]);
  const [servers, setServers] = useState<McpServerInfo[]>([]);
  const [status, setStatus] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState('');
  const [builtinOpen, setBuiltinOpen] = useState(false);
  const [extOpen, setExtOpen] = useState(true);

  const [formMode, setFormMode] = useState<FormMode>('closed');
  /** Original name when editing (for rename) */
  const [editOriginal, setEditOriginal] = useState('');
  const [formName, setFormName] = useState('');
  const [formCmd, setFormCmd] = useState('npx');
  const [formArgs, setFormArgs] = useState('');

  const refresh = useCallback(async () => {
    try {
      const [t, s] = await Promise.all([
        tauri.listTools().catch(() => [] as ToolItem[]),
        tauri.listMcpServers().catch(() => [] as McpServerInfo[]),
      ]);
      setTools(t);
      setServers(s);
    } catch (e: any) {
      setError(String(e));
    }
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const builtin = tools.filter((t) => !t.name.startsWith('mcp_'));
  const mcpTools = tools.filter((t) => t.name.startsWith('mcp_'));

  const closeForm = () => {
    setFormMode('closed');
    setEditOriginal('');
    setFormName('');
    setFormCmd('npx');
    setFormArgs('');
  };

  const openAdd = () => {
    setFormMode('add');
    setEditOriginal('');
    setFormName('');
    setFormCmd('npx');
    setFormArgs('');
    setError('');
  };

  const openEdit = (s: McpServerInfo) => {
    setFormMode('edit');
    setEditOriginal(s.name);
    setFormName(s.name);
    setFormCmd(s.command);
    setFormArgs((s.args || []).join(' '));
    setError('');
    setExtOpen(true);
  };

  const saveForm = async () => {
    if (!formName.trim() || !formCmd.trim()) {
      setError('名称和命令不能为空');
      return;
    }
    setLoading(true);
    setError('');
    try {
      const r =
        formMode === 'edit'
          ? await tauri.updateMcpServer(
              editOriginal,
              formName.trim(),
              formCmd.trim(),
              formArgs.trim(),
            )
          : await tauri.addMcpServer(formName.trim(), formCmd.trim(), formArgs.trim());
      setStatus(r.status);
      closeForm();
      await refresh();
    } catch (e: any) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  const removeServer = async (name: string) => {
    if (!confirm(`确定删除 MCP 服务器「${name}」？`)) return;
    setLoading(true);
    setError('');
    try {
      if (formMode === 'edit' && editOriginal === name) closeForm();
      const r = await tauri.removeMcpServer(name);
      setStatus(r.status);
      await refresh();
    } catch (e: any) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  const reload = async () => {
    setLoading(true);
    setError('');
    try {
      const r = await tauri.reloadMcp();
      setStatus(r.status);
      await refresh();
    } catch (e: any) {
      setError(String(e));
    } finally {
      setLoading(false);
    }
  };

  return (
    <div className="flex-1 flex flex-col bg-main h-full">
      <div className="flex items-center gap-3 px-6 py-3.5 border-b border-border shrink-0">
        <button className="text-gray-400 hover:text-gray-200" onClick={onBack}>
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <polyline points="15 18 9 12 15 6" />
          </svg>
        </button>
        <h2 className="text-base font-medium text-gray-200">MCP 工具</h2>
        <span className="text-xs text-gray-500 ml-auto">
          {mcpTools.length} MCP · {builtin.length} 内置
        </span>
        <button
          className="text-xs px-2 py-1 rounded bg-gray-700 text-gray-200 hover:bg-gray-600 disabled:opacity-40"
          onClick={reload}
          disabled={loading}
        >
          {loading ? '连接中…' : '重新连接'}
        </button>
      </div>

      <div className="flex-1 overflow-y-auto">
        <div className="max-w-xl mx-auto px-8 py-6 space-y-4">
          <p className="text-[11px] text-gray-600 leading-relaxed">
            MCP 工具会注册为 <code className="text-gray-500">mcp_服务器_工具名</code> 并出现在 Agent
            的可用工具列表中。配置保存在{' '}
            <code className="text-gray-500">~/.dscode/mcp_servers.json</code> 与 config.toml。
          </p>

          {error && (
            <div className="text-xs text-red-400 bg-red-900/20 border border-red-900/40 rounded px-3 py-2">
              {error}
            </div>
          )}
          {status.length > 0 && (
            <div className="text-[11px] text-gray-400 bg-card border border-border rounded px-3 py-2 space-y-1.5 font-mono max-h-48 overflow-y-auto whitespace-pre-wrap break-all">
              {status.map((l, i) => (
                <div
                  key={i}
                  className={
                    l.startsWith('[ok]') || l.startsWith('✓')
                      ? 'text-emerald-400/90'
                      : l.startsWith('[err]') || l.startsWith('✗')
                        ? 'text-red-400/90'
                        : ''
                  }
                >
                  {l}
                </div>
              ))}
            </div>
          )}

          {/* Built-in */}
          <div className="bg-card border border-border rounded-lg overflow-hidden">
            <button
              className="w-full flex items-center gap-2 px-4 py-3 hover:bg-white/[0.02] transition-colors"
              onClick={() => setBuiltinOpen(!builtinOpen)}
            >
              <svg
                className={`w-3 h-3 text-gray-500 transition-transform ${builtinOpen ? 'rotate-90' : ''}`}
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
              >
                <polyline points="9 18 15 12 9 6" />
              </svg>
              <span className="text-sm text-gray-200">内置工具</span>
              <span className="text-xs text-gray-600 ml-auto">{builtin.length}</span>
            </button>
            {builtinOpen && (
              <div className="border-t border-border/50 px-4 py-2 space-y-1">
                {builtin.map((t) => (
                  <div key={t.name} className="flex items-center gap-3 py-1.5">
                    <span className="text-xs font-mono text-gray-300 w-36 shrink-0 truncate">
                      {t.name}
                    </span>
                    <span className="text-xs text-gray-500 truncate">{t.description}</span>
                  </div>
                ))}
              </div>
            )}
          </div>

          {/* MCP servers */}
          <div className="bg-card border border-border rounded-lg overflow-hidden">
            <button
              className="w-full flex items-center gap-2 px-4 py-3 hover:bg-white/[0.02] transition-colors"
              onClick={() => setExtOpen(!extOpen)}
            >
              <svg
                className={`w-3 h-3 text-gray-500 transition-transform ${extOpen ? 'rotate-90' : ''}`}
                viewBox="0 0 24 24"
                fill="none"
                stroke="currentColor"
                strokeWidth="2"
              >
                <polyline points="9 18 15 12 9 6" />
              </svg>
              <span className="text-sm text-gray-200">第三方 MCP 服务器</span>
              <span className="text-xs text-gray-600 ml-auto">{servers.length}</span>
            </button>
            {extOpen && (
              <div className="border-t border-border/50 px-4 py-3 space-y-2">
                {servers.length === 0 && formMode === 'closed' && (
                  <p className="text-[11px] text-gray-600 py-2">
                    尚未配置。可添加 Context7：命令 <code className="text-gray-500">npx</code>，参数{' '}
                    <code className="text-gray-500">-y @upstash/context7-mcp</code>
                  </p>
                )}

                {servers.map((s) => {
                  const editing = formMode === 'edit' && editOriginal === s.name;
                  return (
                    <div
                      key={s.name}
                      className={`rounded-lg px-3 py-2 space-y-1 ${
                        editing ? 'bg-gray-700/40 border border-gray-600/50' : 'bg-input'
                      }`}
                    >
                      <div className="flex items-center gap-2">
                        <span
                          className={`w-1.5 h-1.5 rounded-full shrink-0 ${
                            s.connected ? 'bg-emerald-400' : 'bg-gray-600'
                          }`}
                        />
                        <span className="text-xs font-mono text-gray-200 flex-1 truncate">
                          {s.name}
                        </span>
                        <span className="text-[10px] text-gray-500 shrink-0">
                          {s.connected ? `${s.tool_count} tools` : '未连接'}
                        </span>
                        <button
                          type="button"
                          className="text-[10px] text-gray-500 hover:text-gray-200 px-1.5 py-0.5 rounded hover:bg-white/[0.06]"
                          onClick={() => openEdit(s)}
                          disabled={loading}
                          title="编辑"
                        >
                          编辑
                        </button>
                        <button
                          type="button"
                          className="text-gray-600 hover:text-red-400 text-xs px-1"
                          onClick={() => removeServer(s.name)}
                          disabled={loading}
                          title="删除"
                        >
                          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden>
                            <line x1="18" y1="6" x2="6" y2="18" /><line x1="6" y1="6" x2="18" y2="18" />
                          </svg>
                        </button>
                      </div>
                      <div className="text-[10px] text-gray-600 font-mono truncate pl-3.5">
                        {s.command} {s.args.join(' ')}
                      </div>
                    </div>
                  );
                })}

                {/* Add / Edit form */}
                {formMode !== 'closed' && (
                  <div className="space-y-2 bg-input rounded-lg p-3 border border-border/60">
                    <div className="text-[11px] text-gray-400 font-medium">
                      {formMode === 'edit' ? `编辑：${editOriginal}` : '添加 MCP 服务器'}
                    </div>
                    <input
                      className="w-full bg-card border border-border rounded px-2 py-1.5 text-xs text-gray-200 placeholder-gray-500 focus:outline-none focus:border-gray-500"
                      placeholder="服务器名称 (如 Context7)"
                      value={formName}
                      onChange={(e) => setFormName(e.target.value)}
                    />
                    <input
                      className="w-full bg-card border border-border rounded px-2 py-1.5 text-xs text-gray-200 placeholder-gray-500 focus:outline-none focus:border-gray-500 font-mono"
                      placeholder="命令 (如 npx)"
                      value={formCmd}
                      onChange={(e) => setFormCmd(e.target.value)}
                    />
                    <input
                      className="w-full bg-card border border-border rounded px-2 py-1.5 text-xs text-gray-200 placeholder-gray-500 focus:outline-none focus:border-gray-500 font-mono"
                      placeholder="参数 (如 -y @upstash/context7-mcp)"
                      value={formArgs}
                      onChange={(e) => setFormArgs(e.target.value)}
                    />
                    <div className="flex gap-2">
                      <button
                        className="flex-1 py-1.5 text-xs text-white bg-gray-600 hover:bg-gray-500 rounded disabled:opacity-40"
                        onClick={saveForm}
                        disabled={loading}
                      >
                        {formMode === 'edit' ? '保存并重连' : '添加并连接'}
                      </button>
                      <button
                        className="px-3 py-1.5 text-xs text-gray-400 hover:text-gray-200"
                        onClick={closeForm}
                        disabled={loading}
                      >
                        取消
                      </button>
                    </div>
                  </div>
                )}

                {mcpTools.length > 0 && (
                  <div className="pt-2 border-t border-border/40 space-y-1">
                    <div className="text-[10px] text-gray-600 uppercase tracking-wide">
                      已注册 MCP 工具（Agent 可见）
                    </div>
                    {mcpTools.map((t) => (
                      <div key={t.name} className="flex items-start gap-2 py-1">
                        <span className="text-[11px] font-mono text-emerald-400/90 shrink-0 max-w-[45%] truncate">
                          {t.name}
                        </span>
                        <span className="text-[11px] text-gray-500 truncate">{t.description}</span>
                      </div>
                    ))}
                  </div>
                )}

                {formMode === 'closed' && (
                  <button
                    className="w-full py-2 text-xs text-gray-500 hover:text-gray-300 border border-dashed border-border rounded-lg"
                    onClick={openAdd}
                  >
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
