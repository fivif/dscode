import { useState, useCallback, useRef, useEffect, useMemo } from 'react';
import type { ComponentType } from 'react';
import { useChatStore } from '@/stores/chatStore';
import { useConfigStore } from '@/stores/configStore';
import { useSessionStore } from '@/stores/sessionStore';
import { listSkills, stageUpload, type SkillInfo } from '@/lib/tauri';
import type { FileAttachment } from '@/lib/types';
import {
  availableModels,
  modelDisplayName,
  resolveProviderForModel,
  type ModelOption,
} from '@/lib/models';
import {
  AttachmentKindIcon,
  IconClose16,
  IconExpand16,
  IconFileText16,
  IconFolder16,
  IconLock16,
  IconPaperclip16,
  IconRefresh16,
  IconSend16,
  IconSparkles16,
  IconStop16,
  IconSun16,
  IconUnlock16,
  IconUsers16,
} from '@/components/icons';

const MAX_ATTACH = 20;
const MAX_BYTES = 40 * 1024 * 1024;

function fileKind(name: string, mime?: string): FileAttachment['kind'] {
  const m = (mime || '').toLowerCase();
  const ext = name.split('.').pop()?.toLowerCase() || '';
  if (m.startsWith('image/') || ['png', 'jpg', 'jpeg', 'gif', 'webp', 'bmp', 'svg', 'ico', 'heic'].includes(ext)) {
    return 'image';
  }
  if (
    m.startsWith('text/') ||
    [
      'ts', 'tsx', 'js', 'jsx', 'py', 'rs', 'go', 'md', 'txt', 'json', 'yaml', 'yml', 'toml',
      'css', 'html', 'sql', 'sh', 'c', 'cpp', 'h', 'java', 'xml', 'csv', 'log', 'env',
    ].includes(ext)
  ) {
    return 'text';
  }
  return 'binary';
}

function bytesToBase64(buf: ArrayBuffer): string {
  const bytes = new Uint8Array(buf);
  let binary = '';
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    binary += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(binary);
}

type SlashItem = {
  cmd: string;
  desc: string;
  kind: 'builtin' | 'skill';
  /** Icon component for builtin commands (skills use the sparkles glyph). */
  icon?: ComponentType<{ className?: string; size?: number }>;
};

const BUILTIN_COMMANDS: SlashItem[] = [
  {
    cmd: '/plan',
    desc: '五阶段需求评审 — 深度访谈生成 PRD',
    kind: 'builtin',
    icon: IconFileText16,
  },
  {
    cmd: '/auto',
    desc: 'Auto 螺旋（开 TEAM 时并行子任务）',
    kind: 'builtin',
    icon: IconRefresh16,
  },
  {
    cmd: '/teams',
    desc: 'Teams 多 Agent；与 /auto 可同时开',
    kind: 'builtin',
    icon: IconUsers16,
  },
  {
    // Kept last so adding it doesn't shift the position of the three mode
    // commands users already reach by muscle memory (↓ × N + Enter).
    cmd: '/compact',
    desc: '立即压缩上下文 — 手动触发，不等自动阈值',
    kind: 'builtin',
    icon: IconExpand16,
  },
];

function skillToSlashItem(s: SkillInfo): SlashItem {
  const desc =
    s.description?.trim() ||
    (s.triggers?.length ? `触发: ${s.triggers.slice(0, 3).join(', ')}` : 'Agent Skill');
  return {
    cmd: `/${s.name}`,
    desc: desc.length > 80 ? desc.slice(0, 80) + '…' : desc,
    kind: 'skill',
  };
}

export default function InputBox() {
  const [input, setInput] = useState('');
  const [attachments, setAttachments] = useState<FileAttachment[]>([]);
  const [attachError, setAttachError] = useState('');
  const [dragOver, setDragOver] = useState(false);
  const [showModelPicker, setShowModelPicker] = useState(false);
  const [showSlashMenu, setShowSlashMenu] = useState(false);
  const [slashFilter, setSlashFilter] = useState('');
  const [slashIndex, setSlashIndex] = useState(0);
  const [skills, setSkills] = useState<SkillInfo[]>([]);
  /** -1 = not browsing history; else index into userHistory */
  const [historyNav, setHistoryNav] = useState(-1);
  const draftBeforeHistory = useRef('');
  const slashListRef = useRef<HTMLDivElement>(null);
  const itemRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const fileInputRef = useRef<HTMLInputElement>(null);

  const teamsMode = useChatStore((s) => s.teamsMode);
  const toggleTeams = useChatStore((s) => s.toggleTeams);
  const messages = useChatStore((s) => s.messages);
  const contextUsage = useChatStore((s) => s.contextUsage);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const savedInputRef = useRef('');

  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const isStreaming = useChatStore((s) => s.isStreaming);
  const sendMessage = useChatStore((s) => s.sendMessage);
  const abortStream = useChatStore((s) => s.abortStream);
  const sessions = useSessionStore((s) => s.sessions);
  const updateWorkspace = useSessionStore((s) => s.updateWorkspace);
  const updateSessionModel = useSessionStore((s) => s.updateModel);
  const activeSession = sessions.find((s) => s.id === activeSessionId);
  const workspace = activeSession?.workspace || '';

  const config = useConfigStore((s) => s.config);
  const updateConfig = useConfigStore((s) => s.updateConfig);
  const fetchedModels = useConfigStore((s) => s.fetchedModels);
  const absoluteTrust = !!config.absolute_trust;
  // Per-session model; legacy empty → global default
  const activeModel =
    (activeSession?.model || '').trim() || config.default_model;
  const activeProvider = useMemo(
    () => resolveProviderForModel(config, activeModel, fetchedModels),
    [config, activeModel, fetchedModels],
  );
  const modelOptions = useMemo(
    () => availableModels(config, fetchedModels),
    [config, fetchedModels],
  );

  // Chronological user messages for ↑/↓ history (shell-style)
  const userHistory = useMemo(
    () =>
      messages
        .filter((m) => m.role === 'user' && (m.content || '').trim())
        .map((m) => m.content),
    [messages],
  );

  // Reset history browse + pending attachments when session changes.
  // The draft goes too: keeping the text but dropping its attachments meant the
  // next Enter sent "总结这个文件" to an agent that never received the file.
  useEffect(() => {
    setHistoryNav(-1);
    draftBeforeHistory.current = '';
    setInput('');
    setShowSlashMenu(false);
    savedInputRef.current = '';
    setAttachments((prev) => {
      prev.forEach((a) => a.previewUrl?.startsWith('blob:') && URL.revokeObjectURL(a.previewUrl));
      return [];
    });
    setAttachError('');
  }, [activeSessionId]);

  const addPathAttachment = useCallback((path: string, size = 0) => {
    const name = path.split(/[/\\]/).pop() || path;
    setAttachments((prev) => {
      if (prev.some((a) => a.path === path)) return prev;
      if (prev.length >= MAX_ATTACH) {
        setAttachError(`最多 ${MAX_ATTACH} 个附件`);
        return prev;
      }
      setAttachError('');
      return [
        ...prev,
        {
          id: `p-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`,
          path,
          name,
          size,
          kind: fileKind(name),
        },
      ];
    });
  }, []);

  const addBrowserFile = useCallback(
    async (file: File) => {
      if (!activeSessionId) {
        setAttachError('请先选择会话');
        return;
      }
      if (file.size > MAX_BYTES) {
        setAttachError(`${file.name} 超过 40MB 限制`);
        return;
      }
      try {
        const buf = await file.arrayBuffer();
        const b64 = bytesToBase64(buf);
        const path = await stageUpload(activeSessionId, file.name, b64);
        const previewUrl =
          file.type.startsWith('image/') ? URL.createObjectURL(file) : undefined;
        setAttachments((prev) => {
          if (prev.length >= MAX_ATTACH) return prev;
          if (prev.some((a) => a.path === path)) return prev;
          return [
            ...prev,
            {
              id: `f-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`,
              path,
              name: file.name,
              size: file.size,
              mime: file.type,
              kind: fileKind(file.name, file.type),
              previewUrl,
            },
          ];
        });
        setAttachError('');
      } catch (e: any) {
        setAttachError(String(e));
      }
    },
    [activeSessionId],
  );

  const pickFiles = useCallback(async () => {
    setAttachError('');
    try {
      const { open } = await import('@tauri-apps/plugin-dialog');
      const selected = await open({
        multiple: true,
        directory: false,
        title: '选择要上传的文件',
      });
      if (!selected) return;
      const list = Array.isArray(selected) ? selected : [selected];
      for (const p of list) {
        if (typeof p === 'string') addPathAttachment(p);
      }
    } catch {
      // Fallback: hidden HTML file input
      fileInputRef.current?.click();
    }
  }, [addPathAttachment]);

  const removeAttachment = useCallback((id: string) => {
    setAttachments((prev) => {
      const target = prev.find((a) => a.id === id);
      if (target?.previewUrl?.startsWith('blob:')) URL.revokeObjectURL(target.previewUrl);
      return prev.filter((a) => a.id !== id);
    });
  }, []);

  /**
   * Tauri intercepts OS file drops — HTML5 dataTransfer.files is usually empty.
   * Must use native onDragDropEvent to get filesystem paths.
   */
  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;

    (async () => {
      try {
        const { getCurrentWebviewWindow } = await import('@tauri-apps/api/webviewWindow');
        const win = getCurrentWebviewWindow();
        const stop = await win.onDragDropEvent((event) => {
          if (disposed) return;
          const p = event.payload as {
            type: string;
            paths?: string[];
            position?: { x: number; y: number };
          };
          if (p.type === 'enter' || p.type === 'over') {
            setDragOver(true);
            return;
          }
          if (p.type === 'leave' || p.type === 'cancel') {
            setDragOver(false);
            return;
          }
          if (p.type === 'drop') {
            setDragOver(false);
            const paths = Array.isArray(p.paths) ? p.paths : [];
            if (paths.length === 0) {
              setAttachError('未识别到文件路径，请用回形针按钮选择');
              return;
            }
            if (!activeSessionId) {
              setAttachError('请先选择或创建会话再拖入文件');
              return;
            }
            let n = 0;
            for (const path of paths) {
              if (typeof path === 'string' && path.trim()) {
                addPathAttachment(path.trim());
                n += 1;
              }
            }
            if (n > 0) {
              setAttachError('');
            } else {
              setAttachError('拖入的路径无效');
            }
          }
        });
        if (disposed) {
          stop();
        } else {
          unlisten = stop;
        }
      } catch {
        // Browser / non-tauri: HTML5 handlers remain
      }
    })();

    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [activeSessionId, addPathAttachment]);

  // Context usage
  const contextWindow = Math.max(1, contextUsage?.window || config.context_window_tokens || 1_000_000);
  const { ctxPct, ctxTokens, ctxLabel } = useMemo(() => {
    let tokens: number;
    if (contextUsage) {
      // 后端已压缩：直接使用压缩后的占用
      tokens = contextUsage.tokens;
    } else {
      let chars = 0;
      for (const m of messages) {
        chars += (m.content || '').length;
        if (m.thinking_blocks) {
          for (const t of m.thinking_blocks) chars += (t.content || '').length;
        }
        if (m.tool_calls) {
          for (const tc of m.tool_calls) {
            chars += (tc.name || '').length + (tc.description || '').length + (tc.result || '').length;
          }
        }
        if (m.stream_state) {
          chars += (m.stream_state.text || '').length;
          for (const t of m.stream_state.thinking || []) chars += (t.content || '').length;
        }
      }
      tokens = Math.max(0, Math.ceil(chars / 2.5));
    }
    const pct = Math.min(100, (tokens / contextWindow) * 100);
    let label: string;
    if (tokens === 0) label = '0';
    else if (pct < 1) label = pct < 0.1 ? '<0.1' : pct.toFixed(1);
    else if (pct < 10) label = pct.toFixed(1);
    else label = String(Math.round(pct));
    return { ctxPct: pct, ctxTokens: tokens, ctxLabel: label };
  }, [messages, contextWindow, contextUsage]);
  const ctxStroke = ctxPct > 80 ? 'stroke-danger' : ctxPct > 50 ? 'stroke-warning' : 'stroke-success';
  const circumference = 2 * Math.PI * 7;
  const ringPct = ctxTokens === 0 ? 0 : Math.max(ctxPct, 2);
  const offset = circumference * (1 - ringPct / 100);
  const fmtTokens = (n: number) =>
    n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n);

  // Load skills for slash menu (on mount + when menu opens)
  const refreshSkills = useCallback(() => {
    listSkills()
      .then((list) => setSkills(list.filter((s) => !s.hidden)))
      .catch(() => setSkills([]));
  }, []);

  useEffect(() => {
    refreshSkills();
  }, [refreshSkills]);

  useEffect(() => {
    if (showSlashMenu) refreshSkills();
  }, [showSlashMenu, refreshSkills]);

  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = Math.min(el.scrollHeight, 240) + 'px';
  }, [input]);

  const allSlashItems = useMemo((): SlashItem[] => {
    const skillItems = skills.map(skillToSlashItem);
    // Prefer skills that don't collide with builtin cmd names
    const builtinCmds = new Set(BUILTIN_COMMANDS.map((c) => c.cmd.toLowerCase()));
    const uniqueSkills = skillItems.filter((s) => !builtinCmds.has(s.cmd.toLowerCase()));
    return [...BUILTIN_COMMANDS, ...uniqueSkills];
  }, [skills]);

  const filteredCommands = useMemo(() => {
    const q = slashFilter.toLowerCase(); // includes leading "/"
    if (!q || q === '/') return allSlashItems;
    return allSlashItems.filter((c) => {
      const cmd = c.cmd.toLowerCase();
      const desc = c.desc.toLowerCase();
      // match "/foo", "foo", partial after slash
      return cmd.includes(q) || cmd.slice(1).includes(q.slice(1)) || desc.includes(q.slice(1));
    });
  }, [slashFilter, allSlashItems]);

  // Keep selection in range when list changes
  useEffect(() => {
    setSlashIndex(0);
  }, [slashFilter, allSlashItems.length]);

  useEffect(() => {
    if (slashIndex >= filteredCommands.length) {
      setSlashIndex(Math.max(0, filteredCommands.length - 1));
    }
  }, [filteredCommands.length, slashIndex]);

  // Scroll highlighted item into view
  useEffect(() => {
    if (!showSlashMenu) return;
    const el = itemRefs.current[slashIndex];
    el?.scrollIntoView({ block: 'nearest' });
  }, [slashIndex, showSlashMenu, filteredCommands]);

  const handleSend = useCallback(() => {
    if ((!input.trim() && attachments.length === 0) || !activeSessionId || isStreaming) return;
    savedInputRef.current = input;
    const paths = attachments.map((a) => a.path);
    sendMessage(input, paths);
    setInput('');
    attachments.forEach((a) => {
      if (a.previewUrl?.startsWith('blob:')) URL.revokeObjectURL(a.previewUrl);
    });
    setAttachments([]);
    setAttachError('');
    setShowSlashMenu(false);
    setHistoryNav(-1);
    draftBeforeHistory.current = '';
  }, [input, attachments, activeSessionId, isStreaming, sendMessage]);

  const onPaste = useCallback(
    (e: React.ClipboardEvent) => {
      const items = e.clipboardData?.items;
      if (!items) return;
      const files: File[] = [];
      for (let i = 0; i < items.length; i++) {
        const it = items[i];
        if (it.kind === 'file') {
          const f = it.getAsFile();
          if (f) files.push(f);
        }
      }
      if (files.length === 0) return;
      e.preventDefault();
      files.forEach((f) => void addBrowserFile(f));
    },
    [addBrowserFile],
  );

  /** HTML5 fallback (browser / when Tauri yields File objects with content). */
  const onDrop = useCallback(
    (e: React.DragEvent) => {
      e.preventDefault();
      e.stopPropagation();
      setDragOver(false);
      const files = Array.from(e.dataTransfer?.files || []).filter((f) => f && f.size > 0);
      if (files.length) {
        files.forEach((f) => void addBrowserFile(f));
        return;
      }
      // Some environments put paths in text data
      const uriList =
        e.dataTransfer?.getData('text/uri-list') || e.dataTransfer?.getData('text/plain') || '';
      if (uriList) {
        let n = 0;
        uriList
          .split('\n')
          .map((s) => s.trim())
          .filter((s) => s && !s.startsWith('#'))
          .forEach((u) => {
            let path = u;
            if (path.startsWith('file://')) {
              path = decodeURIComponent(path.replace(/^file:\/\//, ''));
              // macOS file:///Users/... → /Users/...
              if (path.startsWith('localhost/')) path = path.slice('localhost'.length);
            }
            if (path.startsWith('/')) {
              addPathAttachment(path);
              n += 1;
            }
          });
        if (n > 0) {
          setAttachError('');
          return;
        }
      }
      // In Tauri, empty FileList is normal — native listener handles OS drops
    },
    [addBrowserFile, addPathAttachment],
  );

  const handleInputChange = useCallback((e: React.ChangeEvent<HTMLTextAreaElement>) => {
    const val = e.target.value;
    setInput(val);
    // Leaving history mode on manual edit
    if (historyNav >= 0) setHistoryNav(-1);
    // Show slash menu when "/" is typed at start or after space/newline
    const lastSlash = val.lastIndexOf('/');
    if (lastSlash >= 0 && (lastSlash === 0 || val[lastSlash - 1] === ' ' || val[lastSlash - 1] === '\n')) {
      const afterSlash = val.slice(lastSlash);
      if (!afterSlash.includes(' ')) {
        setSlashFilter(afterSlash);
        setShowSlashMenu(true);
        return;
      }
    }
    setShowSlashMenu(false);
  }, [historyNav]);

  const selectSlashCommand = useCallback(
    (cmd: string) => {
      const lastSlash = input.lastIndexOf('/');
      const before = lastSlash >= 0 ? input.slice(0, lastSlash) : input;
      // Insert command + trailing space so user can continue typing the task
      setInput(before + cmd + ' ');
      setShowSlashMenu(false);
      setSlashIndex(0);
      requestAnimationFrame(() => inputRef.current?.focus());
    },
    [input],
  );

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      // IME guard (Chinese/Japanese/Korean input): the Enter that commits a
      // candidate — and the legacy keyCode 229 the IME fires while composing —
      // must not send the half-composed message, nor pick a slash command.
      const native = e.nativeEvent as KeyboardEvent & { isComposing?: boolean };
      if (native?.isComposing || (e as unknown as { keyCode?: number }).keyCode === 229) {
        return;
      }

      if (showSlashMenu && filteredCommands.length > 0) {
        if (e.key === 'ArrowDown') {
          e.preventDefault();
          setSlashIndex((i) => (i + 1) % filteredCommands.length);
          return;
        }
        if (e.key === 'ArrowUp') {
          e.preventDefault();
          setSlashIndex((i) => (i - 1 + filteredCommands.length) % filteredCommands.length);
          return;
        }
        if (e.key === 'Enter' || e.key === 'Tab') {
          e.preventDefault();
          const item = filteredCommands[slashIndex] ?? filteredCommands[0];
          if (item) selectSlashCommand(item.cmd);
          return;
        }
        if (e.key === 'Escape') {
          e.preventDefault();
          setShowSlashMenu(false);
          return;
        }
      }

      // Message history (shell-style): ↑ previous · ↓ next — only when not in slash menu
      // and caret is at start (or already navigating history), so multi-line edit still works.
      const el = inputRef.current;
      const atStart = !el || el.selectionStart === 0;
      if (!showSlashMenu && userHistory.length > 0 && (e.key === 'ArrowUp' || e.key === 'ArrowDown')) {
        if (e.key === 'ArrowUp' && (historyNav >= 0 || atStart)) {
          e.preventDefault();
          if (historyNav < 0) {
            draftBeforeHistory.current = input;
            const idx = userHistory.length - 1;
            setHistoryNav(idx);
            setInput(userHistory[idx]);
          } else if (historyNav > 0) {
            const idx = historyNav - 1;
            setHistoryNav(idx);
            setInput(userHistory[idx]);
          }
          requestAnimationFrame(() => {
            const t = inputRef.current;
            if (t) {
              const len = t.value.length;
              t.setSelectionRange(len, len);
            }
          });
          return;
        }
        if (e.key === 'ArrowDown' && historyNav >= 0) {
          e.preventDefault();
          if (historyNav < userHistory.length - 1) {
            const idx = historyNav + 1;
            setHistoryNav(idx);
            setInput(userHistory[idx]);
          } else {
            setHistoryNav(-1);
            setInput(draftBeforeHistory.current);
          }
          requestAnimationFrame(() => {
            const t = inputRef.current;
            if (t) {
              const len = t.value.length;
              t.setSelectionRange(len, len);
            }
          });
          return;
        }
      }

      if (e.key === 'Escape' && isStreaming) {
        e.preventDefault();
        // Only restore the sent text when the box is empty — the textarea stays
        // editable while streaming, so anything typed meanwhile is a new draft
        // the user would otherwise lose.
        if (!input.trim() && savedInputRef.current) {
          setInput(savedInputRef.current);
          savedInputRef.current = '';
        }
        abortStream();
        return;
      }
      if (e.key === 'Enter' && !e.shiftKey) {
        e.preventDefault();
        handleSend();
      }
    },
    [
      handleSend,
      isStreaming,
      abortStream,
      showSlashMenu,
      filteredCommands,
      slashIndex,
      selectSlashCommand,
      userHistory,
      historyNav,
      input,
    ],
  );

  const handleSelectModel = useCallback(
    (model: ModelOption) => {
      // Bind model to the current session only (does not change global default for new chats).
      if (activeSessionId) {
        void updateSessionModel(activeSessionId, model.id);
      }
      setShowModelPicker(false);
    },
    [activeSessionId, updateSessionModel],
  );

  if (!activeSessionId) {
    return (
      <div className="p-4 border-t border-border bg-main">
        <div className="text-center text-muted text-[13px]">选择或创建对话以开始</div>
      </div>
    );
  }

  return (
    <div className="p-3 border-t border-border bg-main">
      <div className="max-w-3xl mx-auto">
        <input
          ref={fileInputRef}
          type="file"
          multiple
          className="hidden"
          onChange={(e) => {
            const files = Array.from(e.target.files || []);
            files.forEach((f) => void addBrowserFile(f));
            e.target.value = '';
          }}
        />
        <div
          className={`panel relative transition-colors focus-within:border-accent/60 focus-within:ring-1 focus-within:ring-accent/25 ${
            dragOver ? 'border-accent/70 bg-accent-soft ring-1 ring-accent/30' : ''
          }`}
          onDragOver={(e) => {
            e.preventDefault();
            e.stopPropagation();
            setDragOver(true);
          }}
          onDragEnter={(e) => {
            e.preventDefault();
            e.stopPropagation();
            setDragOver(true);
          }}
          onDragLeave={(e) => {
            e.preventDefault();
            // only clear when leaving the container
            if (e.currentTarget.contains(e.relatedTarget as Node)) return;
            setDragOver(false);
          }}
          onDrop={onDrop}
        >
          {dragOver && (
            <div className="pointer-events-none absolute inset-0 z-10 flex items-center justify-center rounded-card bg-accent/10 border border-dashed border-accent/50">
              <span className="text-[13px] text-accent font-medium">松开以添加文件</span>
            </div>
          )}
          {attachments.length > 0 && (
            <div className="flex flex-wrap gap-1.5 px-3 pt-2.5">
              {attachments.map((a) => (
                <div
                  key={a.id}
                  className="group flex items-center gap-1.5 max-w-[11rem] pl-1.5 pr-1 py-1 rounded-control bg-main border border-border text-[11px] text-secondary"
                  title={a.path}
                >
                  {a.kind === 'image' && a.previewUrl ? (
                    <img src={a.previewUrl} alt="" className="w-6 h-6 rounded object-cover shrink-0" />
                  ) : (
                    <AttachmentKindIcon kind={a.kind} className="text-muted shrink-0" size={14} />
                  )}
                  <span className="truncate font-mono">{a.name}</span>
                  <button
                    type="button"
                    className="text-faint hover:text-danger p-0.5 shrink-0 rounded focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50"
                    onClick={() => removeAttachment(a.id)}
                    title="移除"
                  >
                    <IconClose16 size={12} />
                  </button>
                </div>
              ))}
            </div>
          )}
          {attachError && (
            <div className="px-3 pt-1.5 text-[11px] text-danger">{attachError}</div>
          )}
          {/*
            Left exactly as it was: 60px floor, 14px above the text and 4px
            below. An earlier pass shortened this box and balanced its padding,
            which was the wrong reading of "the bottom area is too tall" — that
            was about the toolbar strip's share of the composer, not the input
            box, and the input box's height is not mine to trade away.
          */}
          <textarea
            ref={inputRef}
            className="w-full bg-transparent text-[13px] text-primary placeholder-muted resize-none focus:outline-none px-4 pt-3.5 pb-1 min-h-[60px] max-h-60"
            placeholder="输入消息… 可拖入/粘贴/附加文件 · / 命令"
            rows={1}
            value={input}
            onChange={handleInputChange}
            onKeyDown={handleKeyDown}
            onPaste={onPaste}
          />

          {/* Slash command + skill menu */}
          {showSlashMenu && (
            <div
              className="menu absolute left-2 right-2 bottom-full mb-2 z-50 overflow-hidden"
              role="listbox"
              aria-label="命令与 Skills"
            >
              <div
                ref={slashListRef}
                className="flex flex-col gap-0.5 p-1 max-h-56 overflow-y-auto"
              >
                {filteredCommands.length === 0 ? (
                  <div className="px-2 py-2 text-[11px] text-muted">无匹配命令或 Skill</div>
                ) : (
                  filteredCommands.map((c, i) => {
                    const selected = i === slashIndex;
                    return (
                      <button
                        key={`${c.kind}-${c.cmd}`}
                        ref={(el) => {
                          itemRefs.current[i] = el;
                        }}
                        type="button"
                        role="option"
                        aria-selected={selected}
                        className={`flex items-center gap-2 px-2 py-1.5 rounded-control text-[13px] text-left transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50 ${
                          selected
                            ? 'bg-accent-soft text-primary'
                            : 'text-secondary hover:bg-hover hover:text-primary'
                        }`}
                        onMouseEnter={() => setSlashIndex(i)}
                        onClick={() => selectSlashCommand(c.cmd)}
                      >
                        {c.kind === 'skill' ? (
                          <IconSparkles16
                            size={14}
                            className={`shrink-0 ${selected ? 'text-accent' : 'text-muted'}`}
                          />
                        ) : c.icon ? (
                          <c.icon
                            size={14}
                            className={`shrink-0 ${selected ? 'text-primary' : 'text-muted'}`}
                          />
                        ) : null}
                        <span className="font-mono text-primary shrink-0">{c.cmd}</span>
                        {c.kind === 'skill' && (
                          <span className="text-[11px] px-1.5 py-px rounded-full bg-accent/15 text-accent shrink-0">
                            skill
                          </span>
                        )}
                        <span className={`truncate ${selected ? 'text-secondary' : 'text-muted'}`}>
                          {c.desc}
                        </span>
                      </button>
                    );
                  })
                )}
              </div>
              {filteredCommands.length > 0 && (
                <div className="px-2.5 py-1.5 border-t border-divider text-[11px] text-muted flex gap-3">
                  <span>↑↓ 选择</span>
                  <span>Enter / Tab 填入</span>
                  <span>Esc 关闭</span>
                </div>
              )}
            </div>
          )}

          {/*
            `py-1`, not `py-2`: this strip is the composer's "bottom area", and
            at 8px of padding it was 45px — taller than the input box above it
            is comfortable with, for a row whose tallest control is a 28px round
            send button. 4px takes the strip to 37px, so it reads as a strip
            under the input rather than a second panel.

            4px is also the floor: below it the send button starts to collide
            with the divider over it. Shrinking the button itself is the only
            way past that, and a smaller hit target is not a trade this row
            should make. The buttons' hit targets come from their own classes,
            so this padding costs them nothing.
          */}
          <div className="flex items-center justify-between gap-2 px-3 py-1 border-t border-divider">
            <div className="flex items-center gap-2">
              <button
                type="button"
                className="icon-btn disabled:opacity-40"
                onClick={() => void pickFiles()}
                title="附加文件（多选）"
                disabled={isStreaming || !activeSessionId}
              >
                <IconPaperclip16 size={15} />
              </button>
              <button
                className="btn btn-ghost px-2 py-1 text-[11px] max-w-32 truncate"
                onClick={async () => {
                  try {
                    const { open } = await import('@tauri-apps/plugin-dialog');
                    const dir = await open({ directory: true, title: '选择工作目录' });
                    if (dir && typeof dir === 'string' && activeSessionId) {
                      updateWorkspace(activeSessionId, dir);
                    }
                  } catch {
                    /* dialog not available */
                  }
                }}
                title={workspace || '未设置工作区'}
              >
                <IconFolder16 size={12} />
                <span className="truncate">{workspace ? workspace.split('/').pop() : '...'}</span>
              </button>

              <div className="relative">
                <button
                  className="btn btn-ghost px-2 py-1 text-[11px] max-w-[10rem] truncate"
                  onClick={() => setShowModelPicker(!showModelPicker)}
                  title={`${activeModel} · ${activeProvider}`}
                >
                  <IconSun16 size={12} />
                  <span className="truncate">
                    {activeModel
                      ? modelDisplayName(activeModel, modelOptions)
                      : '选择模型'}
                  </span>
                </button>
                {showModelPicker && (
                  <div className="menu absolute bottom-full left-0 mb-2 w-64 max-h-56 overflow-y-auto z-50">
                    {modelOptions.map((m) => (
                      <button
                        key={`${m.provider}:${m.id}`}
                        className={`w-full text-left px-3 py-2 text-[13px] transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50 ${
                          activeModel === m.id && activeProvider === m.provider
                            ? 'bg-accent-soft text-primary'
                            : 'text-secondary hover:bg-hover hover:text-primary'
                        }`}
                        onClick={() => handleSelectModel(m)}
                      >
                        {m.label}
                        <span className="text-muted ml-2">({m.provider})</span>
                      </button>
                    ))}
                    {modelOptions.length === 0 && (
                      <div className="px-3 py-2 text-[11px] text-muted">
                        没有可选模型：设置里启用渠道并点「获取列表」扫描真实模型
                      </div>
                    )}
                  </div>
                )}
              </div>
            </div>

            <div className="flex items-center gap-2">
              <div
                className="relative w-5 h-5 flex items-center justify-center cursor-default"
                title={`上下文约 ${fmtTokens(ctxTokens)} / ${fmtTokens(contextWindow)} tokens（${
                  ctxPct < 1 && ctxTokens > 0 ? ctxPct.toFixed(2) : Math.round(ctxPct)
                }%）\n按消息字符估算，含思考/工具输出`}
              >
                <svg width="20" height="20" viewBox="0 0 20 20" className="-rotate-90">
                  <circle cx="10" cy="10" r="7" fill="none" className="stroke-border" strokeWidth="2.5" />
                  <circle
                    cx="10"
                    cy="10"
                    r="7"
                    fill="none"
                    className={`${ctxStroke} transition-all duration-500`}
                    strokeWidth="2.5"
                    strokeDasharray={circumference}
                    strokeDashoffset={offset}
                    strokeLinecap="round"
                  />
                </svg>
                <span className="absolute text-[6px] text-secondary font-mono leading-none">{ctxLabel}</span>
              </div>

              <button
                type="button"
                className={`text-[11px] px-1.5 py-0.5 rounded-control transition-colors flex items-center gap-1 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50 ${
                  absoluteTrust
                    ? 'text-warning bg-warning/10'
                    : 'text-muted hover:text-primary hover:bg-hover'
                }`}
                title={
                  absoluteTrust
                    ? '绝对信任：危险命令不再确认（硬拦截仍生效）。点击切回需确认'
                    : '安全模式：危险命令需确认。点击开启绝对信任（会持久化）'
                }
                onClick={() => {
                  if (!absoluteTrust) {
                    if (
                      !confirm(
                        '开启「绝对信任」后，危险命令将不再弹出确认（rm -rf / 等硬拦截仍生效）。是否继续？',
                      )
                    ) {
                      return;
                    }
                  }
                  void updateConfig({ absolute_trust: !absoluteTrust });
                }}
              >
                {absoluteTrust ? <IconUnlock16 size={12} /> : <IconLock16 size={12} />}
                <span>{absoluteTrust ? '信任' : '安全'}</span>
              </button>

              <button
                className={`text-[11px] px-1.5 py-0.5 rounded-control transition-colors flex items-center gap-0.5 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-accent/50 ${
                  teamsMode ? 'text-accent bg-accent-soft' : 'text-muted hover:text-primary hover:bg-hover'
                }`}
                title={
                  teamsMode ? '关闭 Teams（本会话记住）' : '开启 Teams 多 Agent（本会话记住）'
                }
                onClick={toggleTeams}
              >
                {/* 12, matching the lock and the send glyph beside it. At 10 the
                    two heads and two shoulders of this one merged into a single
                    smudge — it is the densest glyph in the row and needs the
                    pixels the others do not. */}
                <IconUsers16 size={12} />
                <span>{teamsMode ? 'ON' : 'TEAM'}</span>
              </button>

              {isStreaming ? (
                <button
                  className="btn w-7 h-7 rounded-full p-0 bg-danger/15 text-danger hover:bg-danger/25 shrink-0"
                  onClick={abortStream}
                >
                  <IconStop16 size={12} />
                </button>
              ) : (
                <button
                  className="btn btn-primary w-7 h-7 rounded-full p-0 disabled:opacity-30 shrink-0"
                  onClick={handleSend}
                  disabled={!input.trim() && attachments.length === 0}
                >
                  <IconSend16 size={12} />
                </button>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
