import { useState, useCallback, useRef, useEffect, useMemo } from 'react';
import { useChatStore } from '@/stores/chatStore';
import { useConfigStore } from '@/stores/configStore';
import { useSessionStore } from '@/stores/sessionStore';
import { listSkills, stageUpload, type SkillInfo } from '@/lib/tauri';
import type { FileAttachment } from '@/lib/types';
import {
  availableModels,
  modelDisplayName,
  type ModelOption,
} from '@/lib/models';
import { AttachmentKindIcon, IconPaperclip, IconX } from '@/components/icons';

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
  /** Heroicon-style path d for builtin icons */
  icon?: string;
};

const BUILTIN_COMMANDS: SlashItem[] = [
  {
    cmd: '/plan',
    desc: '五阶段需求评审 — 深度访谈生成 PRD',
    kind: 'builtin',
    icon: 'M9 12h6m-6 4h6m2 5H7a2 2 0 01-2-2V5a2 2 0 012-2h5.586a1 1 0 01.707.293l5.414 5.414a1 1 0 01.293.707V19a2 2 0 01-2 2z',
  },
  {
    cmd: '/auto',
    desc: 'Auto 螺旋（开 TEAM 时并行子任务）',
    kind: 'builtin',
    icon: 'M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15',
  },
  {
    cmd: '/teams',
    desc: 'Teams 多 Agent；与 /auto 可同时开',
    kind: 'builtin',
    icon: 'M17 20h5v-2a3 3 0 00-5.356-1.857M17 20H7m10 0v-2c0-.656-.126-1.283-.356-1.857M7 20H2v-2a3 3 0 015.356-1.857M7 20v-2c0-.656.126-1.283.356-1.857m0 0a5.002 5.002 0 019.288 0M15 7a3 3 0 11-6 0 3 3 0 016 0zm6 3a2 2 0 11-4 0 2 2 0 014 0zM7 10a2 2 0 11-4 0 2 2 0 014 0z',
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

/** Sparkles icon for Agent Skills (matches sidebar). */
function SkillSparklesIcon({ className }: { className?: string }) {
  return (
    <svg
      width="14"
      height="14"
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.8"
      strokeLinecap="round"
      strokeLinejoin="round"
      className={className}
      aria-hidden
    >
      <path d="M9.937 15.5A2 2 0 0 0 8.5 14.063l-6.135-1.582a.5.5 0 0 1 0-.962L8.5 9.936A2 2 0 0 0 9.937 8.5l1.582-6.135a.5.5 0 0 1 .963 0L14.063 8.5A2 2 0 0 0 15.5 9.937l6.135 1.581a.5.5 0 0 1 0 .964L15.5 14.063a2 2 0 0 0-1.437 1.437l-1.582 6.135a.5.5 0 0 1-.963 0z" />
      <path d="M20 3v4" />
      <path d="M22 5h-4" />
      <path d="M4 17v2" />
      <path d="M5 18H3" />
    </svg>
  );
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
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const savedInputRef = useRef('');

  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const isStreaming = useChatStore((s) => s.isStreaming);
  const sendMessage = useChatStore((s) => s.sendMessage);
  const abortStream = useChatStore((s) => s.abortStream);
  const sessions = useSessionStore((s) => s.sessions);
  const updateWorkspace = useSessionStore((s) => s.updateWorkspace);
  const activeSession = sessions.find((s) => s.id === activeSessionId);
  const workspace = activeSession?.workspace || '';

  const config = useConfigStore((s) => s.config);
  const setDefaultModel = useConfigStore((s) => s.setDefaultModel);
  const fetchedModels = useConfigStore((s) => s.fetchedModels);
  const activeProvider = config.active_provider;
  const activeModel = config.default_model;
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

  // Reset history browse + pending attachments when session changes
  useEffect(() => {
    setHistoryNav(-1);
    draftBeforeHistory.current = '';
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
  const contextWindow = Math.max(1, config.context_window_tokens || 1_000_000);
  const { ctxPct, ctxTokens, ctxLabel } = useMemo(() => {
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
    const tokens = Math.max(0, Math.ceil(chars / 2.5));
    const pct = Math.min(100, (tokens / contextWindow) * 100);
    let label: string;
    if (tokens === 0) label = '0';
    else if (pct < 1) label = pct < 0.1 ? '<0.1' : pct.toFixed(1);
    else if (pct < 10) label = pct.toFixed(1);
    else label = String(Math.round(pct));
    return { ctxPct: pct, ctxTokens: tokens, ctxLabel: label };
  }, [messages, contextWindow]);
  const ctxColor = ctxPct > 80 ? '#ef4444' : ctxPct > 50 ? '#f59e0b' : '#10b981';
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
        if (savedInputRef.current) {
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
      // Always pass the channel the option was scanned under (not name-prefix guess)
      setDefaultModel(model.id, model.provider);
      setShowModelPicker(false);
    },
    [setDefaultModel],
  );

  if (!activeSessionId) {
    return (
      <div className="p-4 border-t border-border bg-main">
        <div className="text-center text-gray-500 text-sm">选择或创建对话以开始</div>
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
          className={`bg-input border rounded-xl focus-within:border-gray-500 transition-colors relative ${
            dragOver ? 'border-sky-500/70 bg-sky-500/5 ring-1 ring-sky-500/30' : 'border-border'
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
            <div className="pointer-events-none absolute inset-0 z-10 flex items-center justify-center rounded-xl bg-sky-500/10 border border-dashed border-sky-400/50">
              <span className="text-xs text-sky-300 font-medium">松开以添加文件</span>
            </div>
          )}
          {attachments.length > 0 && (
            <div className="flex flex-wrap gap-1.5 px-3 pt-2.5">
              {attachments.map((a) => (
                <div
                  key={a.id}
                  className="group flex items-center gap-1.5 max-w-[11rem] pl-1.5 pr-1 py-1 rounded-lg bg-card border border-border text-[11px] text-gray-300"
                  title={a.path}
                >
                  {a.kind === 'image' && a.previewUrl ? (
                    <img src={a.previewUrl} alt="" className="w-6 h-6 rounded object-cover shrink-0" />
                  ) : (
                    <AttachmentKindIcon kind={a.kind} className="text-gray-500 shrink-0" size={14} />
                  )}
                  <span className="truncate font-mono">{a.name}</span>
                  <button
                    type="button"
                    className="text-gray-600 hover:text-red-400 p-0.5 shrink-0"
                    onClick={() => removeAttachment(a.id)}
                    title="移除"
                  >
                    <IconX size={12} />
                  </button>
                </div>
              ))}
            </div>
          )}
          {attachError && (
            <div className="px-3 pt-1.5 text-[11px] text-red-400/90">{attachError}</div>
          )}
          <textarea
            ref={inputRef}
            className="w-full bg-transparent text-sm text-gray-100 placeholder-gray-500 resize-none focus:outline-none px-4 pt-3.5 pb-1 min-h-[60px] max-h-60"
            placeholder="输入消息… 可拖入/粘贴/附加文件 · / 命令"
            rows={1}
            value={input}
            onChange={handleInputChange}
            onKeyDown={handleKeyDown}
            onPaste={onPaste}
          />

          {/* Slash command + skill menu */}
          {showSlashMenu && (
            <div className="px-3 pb-1">
              <div
                ref={slashListRef}
                className="border-t border-border pt-1 flex flex-col gap-0.5 max-h-56 overflow-y-auto"
                role="listbox"
                aria-label="命令与 Skills"
              >
                {filteredCommands.length === 0 ? (
                  <div className="px-2 py-2 text-[11px] text-gray-600">无匹配命令或 Skill</div>
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
                        className={`flex items-center gap-2 px-2 py-1.5 rounded text-xs text-left transition-colors ${
                          selected
                            ? 'bg-gray-600 text-gray-100'
                            : 'text-gray-300 hover:bg-gray-700/80'
                        }`}
                        onMouseEnter={() => setSlashIndex(i)}
                        onClick={() => selectSlashCommand(c.cmd)}
                      >
                        {c.kind === 'skill' ? (
                          <SkillSparklesIcon
                            className={`shrink-0 ${selected ? 'text-violet-300' : 'text-violet-400/80'}`}
                          />
                        ) : (
                          <svg
                            width="14"
                            height="14"
                            viewBox="0 0 24 24"
                            fill="none"
                            stroke="currentColor"
                            strokeWidth="1.8"
                            strokeLinecap="round"
                            strokeLinejoin="round"
                            className={`shrink-0 ${selected ? 'text-gray-200' : 'text-gray-500'}`}
                          >
                            <path d={c.icon || ''} />
                          </svg>
                        )}
                        <span className="font-mono text-gray-100 shrink-0">{c.cmd}</span>
                        {c.kind === 'skill' && (
                          <span className="text-[10px] px-1 rounded bg-emerald-500/15 text-emerald-400/90 shrink-0">
                            skill
                          </span>
                        )}
                        <span className={`truncate ${selected ? 'text-gray-300' : 'text-gray-500'}`}>
                          {c.desc}
                        </span>
                      </button>
                    );
                  })
                )}
              </div>
              {filteredCommands.length > 0 && (
                <div className="px-2 pt-0.5 pb-0.5 text-[10px] text-gray-600 flex gap-3">
                  <span>↑↓ 选择</span>
                  <span>Enter / Tab 填入</span>
                  <span>Esc 关闭</span>
                </div>
              )}
            </div>
          )}

          <div className="flex items-center justify-between px-3 pb-2.5">
            <div className="flex items-center gap-2">
              <button
                type="button"
                className="p-1 rounded-md text-gray-500 hover:text-gray-200 hover:bg-card transition-colors"
                onClick={() => void pickFiles()}
                title="附加文件（多选）"
                disabled={isStreaming || !activeSessionId}
              >
                <IconPaperclip size={15} />
              </button>
              <button
                className="text-xs text-gray-500 hover:text-gray-300 flex items-center gap-1 transition-colors max-w-32 truncate"
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
                <svg
                  width="12"
                  height="12"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                >
                  <path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z" />
                </svg>
                <span className="truncate">{workspace ? workspace.split('/').pop() : '...'}</span>
              </button>

              <div className="relative">
                <button
                  className="text-xs text-gray-400 hover:text-gray-200 flex items-center gap-1 transition-colors max-w-[10rem] truncate"
                  onClick={() => setShowModelPicker(!showModelPicker)}
                  title={`${activeModel} · ${activeProvider}`}
                >
                  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                    <circle cx="12" cy="12" r="3" />
                    <path d="M12 1v4M12 19v4M4.22 4.22l2.83 2.83M16.95 16.95l2.83 2.83M1 12h4M19 12h4M4.22 19.78l2.83-2.83M16.95 7.05l2.83-2.83" />
                  </svg>
                  <span className="truncate">
                    {activeModel
                      ? modelDisplayName(activeModel, modelOptions)
                      : '选择模型'}
                  </span>
                </button>
                {showModelPicker && (
                  <div className="absolute bottom-full left-0 mb-2 w-64 max-h-56 overflow-y-auto bg-card border border-border rounded-lg shadow-xl z-50">
                    {modelOptions.map((m) => (
                      <button
                        key={`${m.provider}:${m.id}`}
                        className={`w-full text-left px-3 py-2 text-xs hover:bg-gray-700 transition-colors ${
                          activeModel === m.id && activeProvider === m.provider
                            ? 'text-gray-100 bg-gray-700'
                            : 'text-gray-400'
                        }`}
                        onClick={() => handleSelectModel(m)}
                      >
                        {m.label}
                        <span className="text-gray-500 ml-2">({m.provider})</span>
                      </button>
                    ))}
                    {modelOptions.length === 0 && (
                      <div className="px-3 py-2 text-[11px] text-gray-600">
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
                  <circle cx="10" cy="10" r="7" fill="none" stroke="#2a2d35" strokeWidth="2.5" />
                  <circle
                    cx="10"
                    cy="10"
                    r="7"
                    fill="none"
                    stroke={ctxColor}
                    strokeWidth="2.5"
                    strokeDasharray={circumference}
                    strokeDashoffset={offset}
                    strokeLinecap="round"
                    className="transition-all duration-500"
                  />
                </svg>
                <span className="absolute text-[6px] text-gray-400 font-mono leading-none">{ctxLabel}</span>
              </div>

              <button
                className={`text-xs px-1.5 py-0.5 rounded transition-colors flex items-center gap-0.5 ${
                  teamsMode ? 'text-purple-400 bg-purple-400/10' : 'text-gray-500 hover:text-gray-300'
                }`}
                title={
                  teamsMode ? '关闭 Teams（本会话记住）' : '开启 Teams 多 Agent（本会话记住）'
                }
                onClick={toggleTeams}
              >
                <svg
                  width="10"
                  height="10"
                  viewBox="0 0 24 24"
                  fill="none"
                  stroke="currentColor"
                  strokeWidth="2"
                  strokeLinecap="round"
                  strokeLinejoin="round"
                >
                  <path d="M17 21v-2a4 4 0 0 0-4-4H5a4 4 0 0 0-4 4v2" />
                  <circle cx="9" cy="7" r="4" />
                  <path d="M23 21v-2a4 4 0 0 0-3-3.87" />
                  <path d="M16 3.13a4 4 0 0 1 0 7.75" />
                </svg>
                <span>{teamsMode ? 'ON' : 'TEAM'}</span>
              </button>

              {isStreaming ? (
                <button
                  className="w-7 h-7 rounded-full bg-red-700 hover:bg-red-600 flex items-center justify-center shrink-0 transition-colors"
                  onClick={abortStream}
                >
                  <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor">
                    <rect x="4" y="4" width="16" height="16" rx="2" />
                  </svg>
                </button>
              ) : (
                <button
                  className="w-7 h-7 rounded-full bg-gray-600 hover:bg-gray-500 disabled:opacity-30 flex items-center justify-center shrink-0 transition-colors"
                  onClick={handleSend}
                  disabled={!input.trim() && attachments.length === 0}
                >
                  <svg
                    width="12"
                    height="12"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2.5"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                  >
                    <line x1="22" y1="2" x2="11" y2="13" />
                    <polygon points="22 2 15 22 11 13 2 9 22 2" />
                  </svg>
                </button>
              )}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
