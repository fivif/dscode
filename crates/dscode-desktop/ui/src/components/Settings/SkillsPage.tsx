import { useState, useEffect } from 'react';
import * as tauri from '@/lib/tauri';

interface Props { onBack: () => void; }

interface SkillInfo { name: string; description: string; triggers: string[]; hidden: boolean; body: string; }

export default function SkillsPage({ onBack }: Props) {
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  const [loading, setLoading] = useState(true);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [showAdd, setShowAdd] = useState(false);
  const [editName, setEditName] = useState('');
  const [editDesc, setEditDesc] = useState('');
  const [editBody, setEditBody] = useState('');
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState('');

  const loadSkills = () => {
    tauri.listSkills().then((s) => { setSkills(s); setLoading(false); }).catch(() => setLoading(false));
  };
  useEffect(() => { loadSkills(); }, []);

  const toggle = (name: string) => {
    setExpanded((prev) => { const n = new Set(prev); n.has(name) ? n.delete(name) : n.add(name); return n; });
  };

  const openAdd = () => { setEditName(''); setEditDesc(''); setEditBody(''); setShowAdd(true); };

  const handleSave = async () => {
    if (!editName.trim()) return;
    setSaving(true);
    setError('');
    try {
      await tauri.saveSkill(editName.trim(), editDesc.trim(), editBody.trim());
      setShowAdd(false);
      loadSkills();
    } catch (e: any) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const handleDelete = async (name: string) => {
    if (!confirm(`删除 skill "${name}"？`)) return;
    setError('');
    try {
      await tauri.deleteSkill(name);
      loadSkills();
    } catch (e: any) {
      setError(String(e));
    }
  };

  return (
    <div className="flex-1 flex flex-col bg-main h-full">
      <div className="flex items-center gap-3 px-6 py-3.5 border-b border-border shrink-0">
        <button className="text-gray-400 hover:text-gray-200" onClick={onBack}>
          <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><polyline points="15 18 9 12 15 6" /></svg>
        </button>
        <h2 className="text-base font-medium text-gray-200">Agent Skills</h2>
        <span className="text-xs text-gray-500 ml-auto">{skills.filter((s) => !s.hidden).length} 活跃</span>
        <button
          className="ml-2 px-3 py-1 text-xs text-white bg-gray-600 hover:bg-gray-500 rounded transition-colors"
          onClick={openAdd}
        >
          + 添加
        </button>
      </div>

      {/* 错误信息 */}
      {error && (
        <div className="mx-8 mt-4 p-3 bg-red-900/15 border border-red-900/30 rounded-lg text-red-400 text-xs">{error}</div>
      )}

      <div className="flex-1 overflow-y-auto">
        <div className="max-w-xl mx-auto px-8 py-6 space-y-3">
          {loading ? (
            <p className="text-gray-500 text-xs text-center py-8">加载中...</p>
          ) : skills.length === 0 && !showAdd ? (
            <div className="text-center py-16">
              <svg className="w-10 h-10 mx-auto mb-3 opacity-20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5"><polygon points="12 2 15.09 8.26 22 9.27 17 14.14 18.18 21.02 12 17.77 5.82 21.02 7 14.14 2 9.27 8.91 8.26 12 2" /></svg>
              <p className="text-gray-500 text-sm">暂无第三方 Skills</p>
              <p className="text-gray-600 text-xs mt-1">放置 SKILL.md 到 ~/.dscode/skills/&lt;name&gt;/ 或点击「添加」</p>
            </div>
          ) : null}

          {/* Add / Edit form */}
          {showAdd && (
            <div className="bg-card border border-border rounded-lg p-4 space-y-3">
              <input
                className="w-full bg-input border border-border rounded px-3 py-2 text-sm text-gray-200 placeholder-gray-500 focus:outline-none focus:border-gray-500"
                placeholder="Skill 名称 (如 code-review)"
                value={editName}
                onChange={(e) => setEditName(e.target.value)}
                autoFocus
              />
              <textarea
                className="w-full bg-input border border-border rounded px-3 py-2 text-sm text-gray-200 placeholder-gray-500 focus:outline-none focus:border-gray-500 h-16 resize-none"
                placeholder="描述 (包含触发关键词)"
                value={editDesc}
                onChange={(e) => setEditDesc(e.target.value)}
              />
              <textarea
                className="w-full bg-input border border-border rounded px-3 py-2 text-sm text-gray-200 placeholder-gray-500 focus:outline-none focus:border-gray-500 h-32 resize-none font-mono"
                placeholder="Skill 指令内容 (Markdown)"
                value={editBody}
                onChange={(e) => setEditBody(e.target.value)}
              />
              <div className="flex gap-2">
                <button
                  className="flex-1 py-2 text-sm text-white bg-gray-600 hover:bg-gray-500 rounded transition-colors disabled:opacity-40"
                  onClick={handleSave}
                  disabled={!editName.trim() || saving}
                >
                  {saving ? '保存中...' : '保存'}
                </button>
                <button className="px-4 py-2 text-sm text-gray-400 hover:text-gray-200" onClick={() => setShowAdd(false)}>取消</button>
              </div>
            </div>
          )}

          {/* Skill list */}
          {skills.map((s) => (
            <div key={s.name} className="bg-card border border-border rounded-lg overflow-hidden">
              <button
                className="w-full flex items-center gap-2 px-4 py-3 hover:bg-white/[0.02] transition-colors text-left"
                onClick={() => toggle(s.name)}
              >
                <svg className={`w-3 h-3 text-gray-500 transition-transform ${expanded.has(s.name) ? 'rotate-90' : ''}`}
                  viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><polyline points="9 18 15 12 9 6" /></svg>
                <span className="text-sm text-gray-200 flex-1">{s.name}</span>
                {s.hidden && <span className="text-[10px] text-gray-600">hidden</span>}
                <button
                  className="text-gray-600 hover:text-red-400 text-xs px-1"
                  onClick={(e) => { e.stopPropagation(); handleDelete(s.name); }}
                >
                  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><polyline points="3 6 5 6 21 6" /><path d="M19 6v14a2 2 0 01-2 2H7a2 2 0 01-2-2V6m3 0V4a2 2 0 012-2h4a2 2 0 012 2v2" /></svg>
                </button>
              </button>
              {expanded.has(s.name) && (
                <div className="border-t border-border/50 px-4 py-3 space-y-3 text-xs">
                  <div>
                    <span className="text-gray-500 block mb-1">描述</span>
                    <p className="text-gray-400 leading-relaxed">{s.description || '无'}</p>
                  </div>
                  {s.triggers.length > 0 && (
                    <div>
                      <span className="text-gray-500 block mb-1">触发词</span>
                      <div className="flex flex-wrap gap-1">
                        {s.triggers.map((t, i) => (
                          <span key={i} className="px-2 py-0.5 bg-input rounded text-gray-400 text-[10px]">{t}</span>
                        ))}
                      </div>
                    </div>
                  )}
                  <div>
                    <span className="text-gray-500 block mb-1">指令内容</span>
                    <pre className="text-gray-400 whitespace-pre-wrap bg-black/20 rounded p-2 text-[11px] max-h-40 overflow-y-auto">{s.body || '无'}</pre>
                  </div>
                </div>
              )}
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
