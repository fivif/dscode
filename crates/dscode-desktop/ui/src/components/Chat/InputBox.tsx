import { useState, useCallback, useRef, useEffect, useMemo } from 'react';
import { useChatStore } from '@/stores/chatStore';
import { useConfigStore } from '@/stores/configStore';
import { useSessionStore } from '@/stores/sessionStore';
import { KNOWN_MODELS, type ModelDef } from '@/lib/types';

export default function InputBox() {
  const [input, setInput] = useState('');
  const [showModelPicker, setShowModelPicker] = useState(false);
  const [outputFormat, setOutputFormat] = useState<'markdown' | 'html'>('markdown');
  const messages = useChatStore((s) => s.messages);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  const activeSessionId = useChatStore((s) => s.activeSessionId);
  const isStreaming = useChatStore((s) => s.isStreaming);
  const sendMessage = useChatStore((s) => s.sendMessage);
  const abortStream = useChatStore((s) => s.abortStream);
  const sessions = useSessionStore((s) => s.sessions);
  const updateWorkspace = useSessionStore((s) => s.updateWorkspace);
  const activeSession = sessions.find((s) => s.id === activeSessionId);
  const workspace = activeSession?.workspace || '';

  const config = useConfigStore((s) => s.config);
  const updateConfig = useConfigStore((s) => s.updateConfig);
  const activeProvider = config.active_provider;
  const activeModel = config.default_model;
  // Only show models from enabled providers
  const availableModels = useMemo(() => KNOWN_MODELS, []);

  // Context usage donut
  const contextWindow = config.context_window_tokens || 1000000;
  const ctxUsed = useMemo(() => {
    let chars = 0;
    for (const m of messages) chars += (m.content || '').length;
    return Math.min(100, Math.round((chars / 3.5 / contextWindow) * 100));
  }, [messages, contextWindow]);
  const ctxColor = ctxUsed > 80 ? '#ef4444' : ctxUsed > 50 ? '#f59e0b' : '#10b981';
  const circumference = 2 * Math.PI * 7; // radius=7
  const offset = circumference * (1 - ctxUsed / 100);

  useEffect(() => {
    const el = inputRef.current;
    if (!el) return;
    el.style.height = 'auto';
    el.style.height = Math.min(el.scrollHeight, 240) + 'px';
  }, [input]);

  const handleSend = useCallback(() => {
    if (!input.trim() || !activeSessionId || isStreaming) return;
    sendMessage(input);
    setInput('');
  }, [input, activeSessionId, isStreaming, sendMessage]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); handleSend(); }
    }, [handleSend]);

  const handleSelectModel = useCallback((model: ModelDef) => {
    updateConfig({ default_model: model.id, active_provider: model.provider });
    setShowModelPicker(false);
  }, [updateConfig]);

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
        {/* Input area */}
        <div className="bg-input border border-border rounded-xl focus-within:border-gray-500 transition-colors">
          <textarea
            ref={inputRef}
            className="w-full bg-transparent text-sm text-gray-100 placeholder-gray-500 resize-none focus:outline-none px-4 pt-3.5 pb-1 min-h-[60px] max-h-60"
            placeholder="输入消息... (Enter 发送，Shift+Enter 换行)"
            rows={1}
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            disabled={isStreaming}
          />

          {/* Bottom bar inside input box */}
          <div className="flex items-center justify-between px-3 pb-2.5">
            {/* Left: workspace + model selector */}
            <div className="flex items-center gap-2">
              {/* Workspace folder button */}
              <button
                className="text-xs text-gray-500 hover:text-gray-300 flex items-center gap-1 transition-colors max-w-32 truncate"
                onClick={async () => {
                  try {
                    const { open } = await import('@tauri-apps/plugin-dialog');
                    const dir = await open({ directory: true, title: '选择工作目录' });
                    if (dir && typeof dir === 'string' && activeSessionId) {
                      updateWorkspace(activeSessionId, dir);
                    }
                  } catch { /* dialog not available */ }
                }}
                title={workspace || '未设置工作区'}
              >
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round"><path d="M22 19a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5l2 3h9a2 2 0 0 1 2 2z"/></svg>
                <span className="truncate">{workspace ? workspace.split('/').pop() : '...'}</span>
              </button>

              {/* Model selector */}
              <div className="relative">
                <button
                  className="text-xs text-gray-400 hover:text-gray-200 flex items-center gap-1 transition-colors"
                  onClick={() => setShowModelPicker(!showModelPicker)}
                >
                  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                    <circle cx="12" cy="12" r="3" /><path d="M12 1v4M12 19v4M4.22 4.22l2.83 2.83M16.95 16.95l2.83 2.83M1 12h4M19 12h4M4.22 19.78l2.83-2.83M16.95 7.05l2.83-2.83" />
                  </svg>
                  {availableModels.find((m) => m.id === activeModel)?.display || activeModel}
                </button>
              {showModelPicker && (
                <div className="absolute bottom-full left-0 mb-2 w-56 bg-card border border-border rounded-lg shadow-xl overflow-hidden z-50">
                  {availableModels.map((m) => (
                    <button
                      key={m.id}
                      className={`w-full text-left px-3 py-2 text-xs hover:bg-gray-700 transition-colors ${activeProvider === m.provider && activeModel === m.id ? 'text-gray-100 bg-gray-700' : 'text-gray-400'}`}
                      onClick={() => handleSelectModel(m)}
                    >
                      {m.display}<span className="text-gray-500 ml-2">({m.provider})</span>
                    </button>
                  ))}
                </div>
              )}
              </div>
            </div>

            {/* Right: toggles + send */}
            <div className="flex items-center gap-2">
              {/* Context ring */}
              <div className="relative w-5 h-5 flex items-center justify-center" title={`上下文用量 ${ctxUsed}%`}>
                <svg width="20" height="20" viewBox="0 0 20 20" className="-rotate-90">
                  <circle cx="10" cy="10" r="7" fill="none" stroke="#2a2d35" strokeWidth="2.5" />
                  <circle cx="10" cy="10" r="7" fill="none" stroke={ctxColor} strokeWidth="2.5"
                    strokeDasharray={circumference} strokeDashoffset={offset}
                    strokeLinecap="round" className="transition-all duration-500" />
                </svg>
                <span className="absolute text-[7px] text-gray-400 font-mono">{ctxUsed}</span>
              </div>

              {/* Markdown / HTML toggle */}
              <button
                className={`text-xs px-2 py-0.5 rounded transition-colors ${outputFormat === 'markdown' ? 'text-gray-300 bg-gray-700' : 'text-gray-500 hover:text-gray-300'}`}
                onClick={() => setOutputFormat(outputFormat === 'markdown' ? 'html' : 'markdown')}
                title="输出格式切换"
              >
                {outputFormat === 'markdown' ? 'MD' : 'HTML'}
              </button>

              {/* Wiki toggle */}
              <button
                className="text-gray-400 hover:text-gray-200 transition-colors"
                title="知识图谱"
              >
                <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round">
                  <circle cx="12" cy="12" r="10" />
                  <circle cx="12" cy="12" r="3" />
                  <line x1="12" y1="2" x2="12" y2="9" /><line x1="12" y1="15" x2="12" y2="22" />
                  <line x1="2" y1="12" x2="9" y2="12" /><line x1="15" y1="12" x2="22" y2="12" />
                  <line x1="4.93" y1="4.93" x2="9.88" y2="9.88" /><line x1="14.12" y1="14.12" x2="19.07" y2="19.07" />
                </svg>
              </button>

              {/* Send / Stop */}
              {isStreaming ? (
                <button
                  className="w-7 h-7 rounded-full bg-red-700 hover:bg-red-600 flex items-center justify-center shrink-0 transition-colors"
                  onClick={abortStream}
                >
                  <svg width="12" height="12" viewBox="0 0 24 24" fill="currentColor"><rect x="4" y="4" width="16" height="16" rx="2" /></svg>
                </button>
              ) : (
                <button
                  className="w-7 h-7 rounded-full bg-gray-600 hover:bg-gray-500 disabled:opacity-30 flex items-center justify-center shrink-0 transition-colors"
                  onClick={handleSend}
                  disabled={!input.trim()}
                >
                  <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                    <line x1="22" y1="2" x2="11" y2="13" /><polygon points="22 2 15 22 11 13 2 9 22 2" />
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
