import { useState, useEffect, useCallback } from 'react';
import * as tauri from '@/lib/tauri';
import type { McpServerInfo } from '@/lib/tauri';
import { IconChevronLeft16, IconChevronRight16, IconClose16 } from '@/components/icons';

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
  /** Full MCP tool name dump — collapsed by default (noise; Agent already has tools). */
  const [mcpToolsOpen, setMcpToolsOpen] = useState(false);

  const [formMode, setFormMode] = useState<FormMode>('closed');
  /** Original name when editing (for rename) */
  const [editOriginal, setEditOriginal] = useState('');
  const [formName, setFormName] = useState('');
  const [formCmd, setFormCmd] = useState('npx');
  /**
   * argv kept as an array, one input per element. A single free-text field
   * round-tripped `args.join(' ')` → `split_whitespace()`, so an argument
   * containing a space (`C:/Program Files/x`) silently changed boundaries and
   * the server failed to start. The wire format is still one space-joined
   * string (see `addMcpServer`), so keep the boundaries visible here.
   */
  const [formArgs, setFormArgs] = useState<string[]>([]);

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
    setFormArgs([]);
  };

  const openAdd = () => {
    setFormMode('add');
    setEditOriginal('');
    setFormName('');
    setFormCmd('npx');
    setFormArgs([]);
    setError('');
  };

  const openEdit = (s: McpServerInfo) => {
    setFormMode('edit');
    setEditOriginal(s.name);
    setFormName(s.name);
    setFormCmd(s.command);
    setFormArgs([...(s.args || [])]);
    setError('');
    setExtOpen(true);
  };

  const saveForm = async () => {
    if (!formName.trim() || !formCmd.trim()) {
      setError('名称和命令不能为空');
      return;
    }
    // IPC contract: one space-joined argv string (the server splits on whitespace).
    const args = formArgs.map((a) => a.trim()).filter(Boolean).join(' ');
    setLoading(true);
    setError('');
    try {
      const r =
        formMode === 'edit'
          ? await tauri.updateMcpServer(
              editOriginal,
              formName.trim(),
              formCmd.trim(),
              args,
            )
          : await tauri.addMcpServer(formName.trim(), formCmd.trim(), args);
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
        <button className="icon-btn" title="返回" aria-label="返回" onClick={onBack}>
          <IconChevronLeft16 size={20} />
        </button>
        <div className="min-w-0">
          <h2 className="text-[15px] font-semibold text-primary leading-tight">MCP 工具</h2>
          <p className="text-[13px] text-secondary leading-tight">管理 MCP 服务器连接</p>
        </div>
        <span className="text-[11px] text-muted ml-auto">
          {mcpTools.length} MCP · {builtin.length} 内置
        </span>
        <button
          className="btn-secondary disabled:opacity-40"
          onClick={reload}
          disabled={loading}
        >
          {loading ? '连接中…' : '重新连接'}
        </button>
      </div>

      <div className="flex-1 overflow-y-auto">
        <div className="max-w-xl mx-auto px-8 py-6 space-y-4">
          <p className="text-[11px] text-muted leading-relaxed">
            管理 MCP 服务器连接即可。工具会自动注册给 Agent，无需在此浏览完整清单。配置：{' '}
            <code className="text-secondary">~/.dscode/mcp_servers.json</code>
          </p>

          {error && (
            <div className="text-[13px] text-danger bg-danger/10 border border-danger/30 rounded-card px-3 py-2">
              {error}
            </div>
          )}
          {status.length > 0 && (
            <div className="panel text-[11px] text-secondary px-3 py-2 space-y-1.5 font-mono max-h-48 overflow-y-auto whitespace-pre-wrap break-all">
              {status.map((l, i) => (
                <div
                  key={i}
                  className={
                    l.startsWith('[ok]') || l.startsWith('✓')
                      ? 'text-success'
                      : l.startsWith('[err]') || l.startsWith('✗')
                        ? 'text-danger'
                        : ''
                  }
                >
                  {l}
                </div>
              ))}
            </div>
          )}

          {/* Built-in */}
          <div className="panel overflow-hidden">
            <button
              className="w-full flex items-center gap-2 px-4 py-3 hover:bg-hover transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
              onClick={() => setBuiltinOpen(!builtinOpen)}
            >
              <IconChevronRight16
                size={12}
                className={`text-muted transition-transform ${builtinOpen ? 'rotate-90' : ''}`}
              />
              <span className="text-[13px] font-semibold text-primary">内置工具</span>
              <span className="text-[11px] text-muted ml-auto">{builtin.length}</span>
            </button>
            {builtinOpen && (
              <div className="border-t border-divider px-4 py-2 space-y-1">
                {builtin.map((t) => (
                  <div key={t.name} className="flex items-center gap-3 py-1.5">
                    <span className="text-[13px] font-mono text-secondary w-36 shrink-0 truncate">
                      {t.name}
                    </span>
                    <span className="text-[11px] text-muted truncate">{t.description}</span>
                  </div>
                ))}
              </div>
            )}
          </div>

          {/* MCP servers */}
          <div className="panel overflow-hidden">
            <button
              className="w-full flex items-center gap-2 px-4 py-3 hover:bg-hover transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
              onClick={() => setExtOpen(!extOpen)}
            >
              <IconChevronRight16
                size={12}
                className={`text-muted transition-transform ${extOpen ? 'rotate-90' : ''}`}
              />
              <span className="text-[13px] font-semibold text-primary">第三方 MCP 服务器</span>
              <span className="text-[11px] text-muted ml-auto">{servers.length}</span>
            </button>
            {extOpen && (
              <div className="border-t border-divider px-4 py-3 space-y-2">
                {servers.length === 0 && formMode === 'closed' && (
                  <p className="text-[11px] text-muted py-2">
                    尚未配置。可添加 Context7：命令 <code className="text-secondary">npx</code>，参数{' '}
                    <code className="text-secondary">-y @upstash/context7-mcp</code>
                  </p>
                )}

                {servers.map((s) => {
                  const editing = formMode === 'edit' && editOriginal === s.name;
                  return (
                    <div
                      key={s.name}
                      className={`group rounded-control px-3 py-2 space-y-1 transition-colors ${
                        editing ? 'bg-accent-soft border border-accent/40' : 'bg-input hover:bg-hover'
                      }`}
                    >
                      <div className="flex items-center gap-2">
                        <span
                          className={`w-1.5 h-1.5 rounded-full shrink-0 ${
                            s.connected ? 'bg-success' : 'bg-faint'
                          }`}
                        />
                        <span className="text-[13px] font-mono text-primary flex-1 truncate">
                          {s.name}
                        </span>
                        <span className="text-[11px] text-muted shrink-0">
                          {s.connected ? `${s.tool_count} tools` : '未连接'}
                        </span>
                        <button
                          type="button"
                          className="btn-ghost px-2 py-0.5 text-[11px] opacity-0 group-hover:opacity-100 focus-visible:opacity-100 transition-opacity disabled:opacity-40"
                          onClick={() => openEdit(s)}
                          disabled={loading}
                          title="编辑"
                        >
                          编辑
                        </button>
                        <button
                          type="button"
                          className="btn-ghost px-2 py-0.5 text-[11px] text-muted hover:text-danger opacity-0 group-hover:opacity-100 focus-visible:opacity-100 transition-opacity disabled:opacity-40"
                          onClick={() => removeServer(s.name)}
                          disabled={loading}
                          title="删除"
                        >
                          <IconClose16 size={12} />
                        </button>
                      </div>
                      <div className="text-[11px] text-muted font-mono truncate pl-3.5">
                        {s.command}{' '}
                        {s.args.map((a) => (/\s/.test(a) ? `"${a}"` : a)).join(' ')}
                      </div>
                    </div>
                  );
                })}

                {/* Add / Edit form */}
                {formMode !== 'closed' && (
                  <div className="space-y-2 bg-input rounded-card p-3 border border-border">
                    <div className="text-[13px] font-semibold text-primary">
                      {formMode === 'edit' ? `编辑：${editOriginal}` : '添加 MCP 服务器'}
                    </div>
                    <input
                      className="w-full field py-1.5"
                      placeholder="服务器名称 (如 Context7)"
                      value={formName}
                      onChange={(e) => setFormName(e.target.value)}
                    />
                    <input
                      className="w-full field py-1.5 font-mono"
                      placeholder="命令 (如 npx)"
                      value={formCmd}
                      onChange={(e) => setFormCmd(e.target.value)}
                    />
                    <div className="space-y-1">
                      {formArgs.map((arg, i) => (
                        <div key={i} className="flex items-center gap-1">
                          <input
                            className="flex-1 min-w-0 field py-1.5 font-mono"
                            placeholder={`参数 ${i + 1}（含空格也作为一个参数）`}
                            value={arg}
                            onChange={(e) =>
                              setFormArgs((prev) =>
                                prev.map((a, j) => (j === i ? e.target.value : a)),
                              )
                            }
                          />
                          <button
                            type="button"
                            className="icon-btn shrink-0 hover:text-danger"
                            title="删除该参数"
                            onClick={() => setFormArgs((prev) => prev.filter((_, j) => j !== i))}
                          >
                            <IconClose16 size={11} />
                          </button>
                        </div>
                      ))}
                      <button
                        type="button"
                        className="w-full py-1.5 text-[11px] text-muted hover:text-primary hover:bg-hover border border-dashed border-border rounded-control transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
                        onClick={() => setFormArgs((prev) => [...prev, ''])}
                      >
                        + 添加参数
                      </button>
                    </div>
                    <div className="flex gap-2">
                      <button
                        className="btn-primary flex-1 disabled:opacity-40"
                        onClick={saveForm}
                        disabled={loading}
                      >
                        {formMode === 'edit' ? '保存并重连' : '添加并连接'}
                      </button>
                      <button
                        className="btn-ghost disabled:opacity-40"
                        onClick={closeForm}
                        disabled={loading}
                      >
                        取消
                      </button>
                    </div>
                  </div>
                )}

                {/* Compact summary only — full catalog is Agent-side, not a settings task */}
                {mcpTools.length > 0 && (
                  <div className="pt-2 border-t border-divider">
                    <button
                      type="button"
                      className="w-full flex items-center gap-2 py-1 text-left rounded-control focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
                      onClick={() => setMcpToolsOpen((v) => !v)}
                    >
                      <IconChevronRight16
                        size={10}
                        className={`text-muted transition-transform shrink-0 ${
                          mcpToolsOpen ? 'rotate-90' : ''
                        }`}
                      />
                      <span className="text-[11px] text-muted">
                        已注册 {mcpTools.length} 个工具（Agent 可调用）
                      </span>
                      <span className="text-[11px] text-faint ml-auto">
                        {mcpToolsOpen ? '收起' : '展开名称'}
                      </span>
                    </button>
                    {mcpToolsOpen && (
                      <div className="mt-1 max-h-36 overflow-y-auto rounded-control bg-main px-2 py-1.5 space-y-0.5">
                        {mcpTools.map((t) => (
                          <div
                            key={t.name}
                            className="text-[11px] font-mono text-muted truncate"
                            title={t.description}
                          >
                            {t.name}
                          </div>
                        ))}
                      </div>
                    )}
                  </div>
                )}

                {formMode === 'closed' && (
                  <button
                    className="w-full py-2 text-[13px] text-muted hover:text-primary hover:bg-hover border border-dashed border-border rounded-card transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
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
