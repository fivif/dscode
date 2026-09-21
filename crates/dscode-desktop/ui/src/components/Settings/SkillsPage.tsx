import { useState, useEffect } from 'react';
import * as tauri from '@/lib/tauri';
import type { SkillInfo } from '@/lib/tauri';
import { IconChevronLeft16, IconChevronRight16, IconSparkles16 } from '@/components/icons';

interface Props { onBack: () => void; }

interface ScriptDraft {
  filename: string;
  content: string;
}

export default function SkillsPage({ onBack }: Props) {
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [showAdd, setShowAdd] = useState(false);
  const [editName, setEditName] = useState('');
  const [editDesc, setEditDesc] = useState('');
  const [editTriggers, setEditTriggers] = useState('');
  const [editBody, setEditBody] = useState('');
  const [scripts, setScripts] = useState<ScriptDraft[]>([]);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');
  const [success, setSuccess] = useState('');
  const [skillsRoot, setSkillsRoot] = useState('');
  const [installSpec, setInstallSpec] = useState('');
  const [installing, setInstalling] = useState(false);
  const [deletingKey, setDeletingKey] = useState<string | null>(null);

  const loadSkills = async () => {
    setLoading(true);
    setError('');
    try {
      const [s, root] = await Promise.all([
        tauri.listSkills(),
        tauri.skillsDir().catch(() => ''),
      ]);
      setSkills(s || []);
      if (root) setSkillsRoot(root);
    } catch (e: any) {
      setError(String(e));
      setSkills([]);
    } finally {
      setLoading(false);
    }
  };
  useEffect(() => { loadSkills(); }, []);

  const toggle = (name: string) => {
    setExpanded((prev) => { const n = new Set(prev); n.has(name) ? n.delete(name) : n.add(name); return n; });
  };

  const openAdd = () => {
    setEditName('');
    setEditDesc('');
    setEditTriggers('');
    setEditBody('# 指令\n\n1. …\n\n## 脚本\n激活后可用 `do_bash` 运行 `scripts/` 下文件（绝对路径会注入上下文）。\n');
    setScripts([]);
    setError('');
    setSuccess('');
    setShowAdd(true);
  };

  const addScript = () => {
    setScripts((prev) => [...prev, { filename: `script-${prev.length + 1}.sh`, content: '#!/usr/bin/env bash\nset -euo pipefail\necho "hello from skill"\n' }]);
  };

  const handleSave = async () => {
    if (!editName.trim()) {
      setError('请填写 Skill 名称');
      return;
    }
    if (!editBody.trim()) {
      setError('请填写 Skill 指令内容');
      return;
    }
    setSaving(true);
    setError('');
    setSuccess('');
    try {
      const files = scripts
        .filter((s) => s.filename.trim() && s.content.trim())
        .map((s) => {
          let name = s.filename.trim().replace(/\\/g, '/');
          if (!name.startsWith('scripts/') && !name.startsWith('references/') && !name.startsWith('assets/')) {
            name = `scripts/${name}`;
          }
          return { path: name, content: s.content };
        });
      const path = await tauri.saveSkill(
        editName.trim(),
        editDesc.trim() || editName.trim(),
        editBody.trim(),
        editTriggers.trim() || undefined,
        files.length ? files : undefined,
      );
      setShowAdd(false);
      setSuccess(`已保存 skill 包: ${path}`);
      await loadSkills();
    } catch (e: any) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async (s: SkillInfo) => {
    const where = s.root ? `\n路径: ${s.root}` : '';
    if (!confirm(`删除 skill "${s.name}" 及其全部脚本/资源？${where}`)) return;
    const key = s.root || s.name;
    setError('');
    setSuccess('');
    setDeletingKey(key);
    try {
      const msg = await tauri.deleteSkill(s.name, s.root || undefined);
      setSuccess(msg || `已删除 ${s.name}`);
      // Optimistic remove so a slow rescan doesn't look like "delete failed"
      setSkills((prev) => prev.filter((x) => (x.root || x.name) !== key));
      await loadSkills();
    } catch (e: any) {
      setError(String(e));
      await loadSkills();
    } finally {
      setDeletingKey(null);
    }
  };

  const openInFinder = async (root?: string) => {
    const path = root || skillsRoot;
    if (!path) return;
    try {
      const { open } = await import('@tauri-apps/plugin-shell');
      await open(path);
    } catch {
      // fallback: copy path
      try {
        await navigator.clipboard.writeText(path);
        setSuccess(`路径已复制: ${path}`);
      } catch {
        setSuccess(path);
      }
    }
  };

  return (
    <div className="flex-1 flex flex-col bg-main h-full">
      <div className="flex items-center gap-3 px-6 py-3.5 border-b border-border shrink-0">
        <button className="icon-btn" title="返回" aria-label="返回" onClick={onBack}>
          <IconChevronLeft16 size={20} />
        </button>
        <IconSparkles16 size={18} className="text-accent shrink-0" />
        <div className="min-w-0">
          <h2 className="text-[15px] font-semibold text-primary leading-tight">Agent Skills</h2>
          <p className="text-[13px] text-secondary leading-tight">安装与管理 skill 包</p>
        </div>
        <span className="text-[11px] text-muted ml-auto">{skills.filter((s) => !s.hidden).length} 活跃</span>
        {skillsRoot && (
          <button className="btn-ghost px-2 py-1 text-[11px]" onClick={() => openInFinder()} title={skillsRoot}>
            打开目录
          </button>
        )}
        <button
          className="btn-primary ml-2"
          onClick={openAdd}
        >
          + 添加
        </button>
      </div>

      {error && (
        <div className="mx-8 mt-4 p-3 bg-danger/10 border border-danger/30 rounded-card text-danger text-[13px] whitespace-pre-wrap">{error}</div>
      )}
      {success && (
        <div className="mx-8 mt-4 p-3 bg-success/10 border border-success/30 rounded-card text-success text-[13px]">{success}</div>
      )}

      <div className="flex-1 overflow-y-auto">
        <div className="max-w-xl mx-auto px-8 py-6 space-y-3">
          <p className="text-[11px] text-muted leading-relaxed">
            Skill 是一个<strong className="text-secondary">包</strong>：
            <code className="text-secondary"> SKILL.md </code>
            + 可选
            <code className="text-secondary"> scripts/</code>·
            <code className="text-secondary">references/</code>·
            <code className="text-secondary">assets/</code>。
            兼容 <a className="text-accent hover:underline rounded-[3px] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50" href="https://www.skills.sh/" target="_blank" rel="noreferrer">skills.sh</a> /
            Claude Code 生态；并自动扫描 ~/.claude/skills、~/.agents/skills。
          </p>

          {/* Install from skills.sh / GitHub */}
          <div className="panel p-4 space-y-2">
            <div className="text-[13px] font-semibold text-primary">从 skills.sh / GitHub 安装</div>
            <p className="text-[11px] text-muted leading-relaxed">
              填写 <code className="text-secondary">owner/repo</code> 或{' '}
              <code className="text-secondary">owner/repo/skill-path</code>
              ，例如 <code className="text-secondary">mattpocock/skills/grill-me</code>、
              <code className="text-secondary">vercel-labs/agent-skills</code>。
              需本机有 git；安装时只克隆并复制 SKILL.md 包，不执行远程脚本。
            </p>
            <div className="flex gap-2">
              <input
                className="field flex-1 font-mono"
                placeholder="owner/repo 或 owner/repo/skill"
                value={installSpec}
                onChange={(e) => setInstallSpec(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === 'Enter' && installSpec.trim()) {
                    e.preventDefault();
                    (async () => {
                      setInstalling(true);
                      setError('');
                      setSuccess('');
                      try {
                        const msg = await tauri.installSkillPackage(installSpec.trim());
                        setSuccess(msg);
                        setInstallSpec('');
                        await loadSkills();
                      } catch (err: any) {
                        setError(String(err));
                      } finally {
                        setInstalling(false);
                      }
                    })();
                  }
                }}
              />
              <button
                className="btn-primary shrink-0 disabled:opacity-40"
                disabled={!installSpec.trim() || installing}
                onClick={async () => {
                  setInstalling(true);
                  setError('');
                  setSuccess('');
                  try {
                    const msg = await tauri.installSkillPackage(installSpec.trim());
                    setSuccess(msg);
                    setInstallSpec('');
                    await loadSkills();
                  } catch (err: any) {
                    setError(String(err));
                  } finally {
                    setInstalling(false);
                  }
                }}
              >
                {installing ? '安装中…' : '安装'}
              </button>
            </div>
          </div>

          {loading ? (
            <p className="text-muted text-[13px] text-center py-8">加载中...</p>
          ) : skills.length === 0 && !showAdd ? (
            <div className="text-center py-16">
              <p className="text-secondary text-[13px]">暂无 Skills</p>
              <p className="text-muted text-[11px] mt-1">添加包，或手动放文件到 ~/.dscode/skills/</p>
            </div>
          ) : null}

          {showAdd && (
            <div className="panel p-4 space-y-3">
              <div>
                <label className="text-[13px] text-primary mb-1.5 block">名称 *</label>
                <input
                  className="field"
                  placeholder="code-review"
                  value={editName}
                  onChange={(e) => setEditName(e.target.value)}
                  autoFocus
                />
              </div>
              <div>
                <label className="text-[13px] text-primary mb-1.5 block">描述</label>
                <textarea
                  className="field h-12 resize-none"
                  placeholder="做什么"
                  value={editDesc}
                  onChange={(e) => setEditDesc(e.target.value)}
                />
              </div>
              <div>
                <label className="text-[13px] text-primary mb-1.5 block">触发词</label>
                <input
                  className="field"
                  placeholder="代码审查, code review"
                  value={editTriggers}
                  onChange={(e) => setEditTriggers(e.target.value)}
                />
              </div>
              <div>
                <label className="text-[13px] text-primary mb-1.5 block">指令 (Markdown) *</label>
                <textarea
                  className="field h-28 resize-none font-mono"
                  value={editBody}
                  onChange={(e) => setEditBody(e.target.value)}
                />
              </div>

              {/* Scripts */}
              <div className="border border-divider rounded-card p-3 space-y-2">
                <div className="flex items-center justify-between">
                  <span className="text-[13px] text-secondary">脚本 / 资源 files</span>
                  <button type="button" className="text-[11px] text-accent hover:text-accent-hover rounded-control focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50" onClick={addScript}>
                    + 添加脚本
                  </button>
                </div>
                <p className="text-[11px] text-muted">
                  默认写入 <code>scripts/</code>。也可填 <code>references/xxx.md</code> 或 <code>assets/xxx</code>。
                </p>
                {scripts.map((sc, i) => (
                  <div key={i} className="space-y-1.5 bg-main rounded-control p-2">
                    <div className="flex gap-2">
                      <input
                        className="field flex-1 py-1 text-[13px] font-mono"
                        placeholder="scripts/run.sh"
                        value={sc.filename}
                        onChange={(e) => {
                          const next = [...scripts];
                          next[i] = { ...next[i], filename: e.target.value };
                          setScripts(next);
                        }}
                      />
                      <button
                        type="button"
                        className="icon-btn hover:text-danger"
                        onClick={() => setScripts(scripts.filter((_, j) => j !== i))}
                      >
                        删
                      </button>
                    </div>
                    <textarea
                      className="field py-1.5 text-[11px] font-mono h-24 resize-y"
                      value={sc.content}
                      onChange={(e) => {
                        const next = [...scripts];
                        next[i] = { ...next[i], content: e.target.value };
                        setScripts(next);
                      }}
                    />
                  </div>
                ))}
                {scripts.length === 0 && (
                  <p className="text-[11px] text-muted text-center py-2">
                    可选。也可保存后在文件系统往 skill 目录丢脚本。
                  </p>
                )}
              </div>

              <div className="flex gap-2">
                <button
                  className="btn-primary flex-1 disabled:opacity-40"
                  onClick={handleSave}
                  disabled={!editName.trim() || !editBody.trim() || saving}
                >
                  {saving ? '保存中...' : '保存 skill 包'}
                </button>
                <button className="btn-ghost" onClick={() => setShowAdd(false)}>取消</button>
              </div>
            </div>
          )}

          {skills.map((s) => {
            const scripts = (s.resources || []).filter((r) => r.kind === 'script');
            const refs = (s.resources || []).filter((r) => r.kind === 'reference');
            const assets = (s.resources || []).filter((r) => r.kind === 'asset');
            return (
              <div key={`${s.root || s.name}`} className="panel group overflow-hidden">
                <button
                  className="w-full flex items-center gap-2 px-4 py-3 hover:bg-hover transition-colors text-left focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
                  onClick={() => toggle(s.root || s.name)}
                >
                  <IconChevronRight16
                    size={12}
                    className={`text-muted transition-transform ${expanded.has(s.root || s.name) ? 'rotate-90' : ''}`}
                  />
                  <span className="text-[13px] text-primary flex-1 font-mono">{s.name}</span>
                  {scripts.length > 0 && (
                    <span className="text-[11px] text-success">{scripts.length} 脚本</span>
                  )}
                  <span className="text-[11px] text-muted">{s.triggers?.length || 0} 触发</span>
                  <button
                    className="btn-ghost px-2 py-0.5 text-[11px] text-muted hover:text-danger opacity-0 group-hover:opacity-100 focus-visible:opacity-100 transition-opacity disabled:opacity-40"
                    disabled={deletingKey === (s.root || s.name)}
                    onClick={(e) => { e.stopPropagation(); handleDelete(s); }}
                    title={s.root ? `删除 ${s.root}` : '删除'}
                  >
                    {deletingKey === (s.root || s.name) ? '…' : '删'}
                  </button>
                </button>
                {expanded.has(s.root || s.name) && (
                  <div className="border-t border-divider px-4 py-3 space-y-3 text-[13px]">
                    <div>
                      <span className="text-[11px] text-muted block mb-1">描述</span>
                      <p className="text-secondary leading-relaxed">{s.description || '无'}</p>
                    </div>
                    {s.root && (
                      <div>
                        <span className="text-[11px] text-muted block mb-1">包路径</span>
                        <button
                          className="text-accent hover:text-accent-hover font-mono text-[11px] break-all text-left rounded-control focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
                          onClick={() => openInFinder(s.root)}
                        >
                          {s.root}
                        </button>
                      </div>
                    )}
                    {s.triggers?.length > 0 && (
                      <div>
                        <span className="text-[11px] text-muted block mb-1">触发词</span>
                        <div className="flex flex-wrap gap-1">
                          {s.triggers.map((t) => (
                            <span key={t} className="px-1.5 py-0.5 rounded-control bg-input text-secondary font-mono text-[11px]">{t}</span>
                          ))}
                        </div>
                      </div>
                    )}
                    {scripts.length > 0 && (
                      <div>
                        <span className="text-[11px] text-muted block mb-1">脚本 scripts/</span>
                        <ul className="space-y-1">
                          {scripts.map((r) => (
                            <li key={r.relative_path} className="font-mono text-[11px] text-success">
                              {r.relative_path}
                              <span className="text-muted ml-2">{r.executable ? 'exec' : 'file'} · {r.size_bytes}B</span>
                            </li>
                          ))}
                        </ul>
                      </div>
                    )}
                    {refs.length > 0 && (
                      <div>
                        <span className="text-[11px] text-muted block mb-1">参考 references/</span>
                        <ul className="space-y-0.5">
                          {refs.map((r) => (
                            <li key={r.relative_path} className="font-mono text-[11px] text-secondary">{r.relative_path}</li>
                          ))}
                        </ul>
                      </div>
                    )}
                    {assets.length > 0 && (
                      <div>
                        <span className="text-[11px] text-muted block mb-1">资源 assets/</span>
                        <ul className="space-y-0.5">
                          {assets.map((r) => (
                            <li key={r.relative_path} className="font-mono text-[11px] text-secondary">{r.relative_path}</li>
                          ))}
                        </ul>
                      </div>
                    )}
                    {s.body && (
                      <div>
                        <span className="text-[11px] text-muted block mb-1">SKILL.md 预览</span>
                        <pre className="text-secondary whitespace-pre-wrap font-mono text-[11px] max-h-32 overflow-y-auto bg-main rounded-control p-2">{s.body.slice(0, 600)}{s.body.length > 600 ? '…' : ''}</pre>
                      </div>
                    )}
                  </div>
                )}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
