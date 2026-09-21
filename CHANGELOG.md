# Changelog

## 0.4.1 — 全量审查修复 · 图像生成 · UI token 化 (2026-09-21)

一轮覆盖 31k 行 Rust + 8.5k 行前端的全量审查（289 条 finding）之后的修复。
408 个 Rust 测试通过、0 失败；前端 `tsc && vite build` 通过。

### Security
- **安全分类器重写** —— 从「正则套原始命令串」改为「shell 分词后判定」：切段、
  引号规则分词、`$'…'` 解码十六进制转义、`${IFS}` 当分隔符、`$()`/反引号递归分类。
- **Windows 破坏性原语此前整片 Allow**（PowerShell 是未装 Git Bash 时的回退 shell）。
  现覆盖递归强制删除、磁盘与分区操作、卷影副本删除、ACL 改写、注册表删除、
  以及下载后直接执行的两种写法（含 base64 编码命令的解码识别）。
- **外传原语提升为 Confirm** —— 把私钥或凭据文件用 curl/scp 送出本机。
- **配置里的 fork bomb 默认规则此前压根没加载** —— 把该模式用字边界包裹后拼出的
  字符串不是合法正则（`repetition quantifier expects a valid decimal`），编译失败
  只 `warn!` 后丢弃，是条死规则。现按原样编译，失败降级成字面量子串继续拦，
  并收进 `invalid_patterns()` 让 UI 可见。
- **修掉误伤** —— 提交信息里提到「递归删除根目录」那类命令的 commit message 曾被判
  HardBlock（导致无法提交）；这个误伤正是把用户推向 `absolute_trust` 的压力。
- **不再承诺做不到的事** —— 文件头写明这是尽力而为的文本黑名单、不是沙箱并列出绕过；
  `HardBlock` 文案从 "never allowed, even in absolute trust" 改为诚实版本。
- **`permission.rs` pending 条目在调用方 future 被 drop 时永久泄漏** —— 改用 RAII 守卫。

### dscode-web
- **此前零鉴权 + `Access-Control-Allow-Origin: *`，且权限请求 id 就在自己广播的
  SSE 流里**。攻击链：任意网页 → 发一条触发 Confirm 的消息 → 从 SSE 读到自己的
  `perm_xxx` → 自己批准 → 本机任意命令执行；同一入口还能取 `api_key` 明文、
  写 `absolute_trust = true`、加 MCP server、清空 skills 目录。
  现加 token 中间件（`Bearer` 头或 `?token=`，EventSource 不能设头）。
- `GET /api/image` 新增路径白名单，越界一律 404（含 `..` 穿越与指向 `config.toml`
  的请求），带合法 token 也读不到 images 目录之外的文件。

### Skills / MCP
- **技能删除工具的守卫失效，能把整个 skills 树清掉** —— 守卫用的是
  `c.starts_with(&root_canon)`，而候选集合的第一个元素就是该路径自身，
  `starts_with` 对自己恒为真。现要求严格子路径（必须比基准多至少一层）、
  显式拒绝「候选 == 基准」、拒绝含 `..` 的候选，且动手前必须确有 `SKILL.md`
  且 frontmatter `name` 与请求名一致。
- **从 GitHub 装的 skill 可用 symlink 把宿主私钥拷进 skills 树** —— 四个遍历函数
  全部跟随 symlink，现改 `symlink_metadata` 并跳过。
- **MCP stderr 排空的字节切点 panic** —— 中文日志超 4KB 时约 2/3 概率 panic，
  排空任务一死 → 管道写满 → 子进程阻塞 → 之后每个 `tools/call` 都超时。
- **`git clone` 无超时且不禁用凭据提示** —— 会停在凭据提示上读 `/dev/tty` 使 ReAct
  循环无限挂起。现加 `GIT_TERMINAL_PROMPT=0` + `stdin(null)` + 有界超时。
- **重装静默毁掉用户对 skill 的本地修改** —— 新鲜度比的是刚 clone 出来的 mtime，
  「更新」恒为真。现比对远端 commit，覆盖前先备份。
- **中文名建不了 skill**（`代码审查` → `------` → 报错），而产品文案就是中文 ——
  现允许 Unicode 字母/数字。YAML 的 BOM、值内 `---`、`|`/`>` 块标量、列表形式、
  引号转义往返一并修正。

### Agent 核心
- 上下文压缩切点不得低于最后一条 user 消息（此前会把「当前用户指令」整条删掉）。
- 修掉压缩后循环检测的下标切片 panic。
- 工具链校验：加载时 + 运行时清理孤立工具调用。
- 压缩改为「provider 说超限 → 压缩 → 重试」，不再完全信任用户手填的
  `window_tokens`；L0 阈值不再绕过用户配置。新增 `/compact` 内置命令。

### Providers
- Anthropic：回传 thinking 块 + 合并并行 `tool_result` 到单条 user 消息，
  否则 Claude 通道的工具调用基本不可用。
- DeepSeek：对齐 Responses API 语义化 SSE 事件。

### 存储 / 记忆
- `add_message` 的并发 `SQLITE_BUSY` 不再被 `.ok()` 吞掉。
- FTS5 查询串转义；`memory_fts` 增加 `session_id`。
- `updated_at` 不再同时承担「排序键」与「保留期键」，修掉「重命名即免除清理」。

### Teams / Auto / MAGI
- 子 agent 失败不再被改写成成功（改用 `had_token` 判定，`Blocked` 并入 terminal fail）。
- 三处 `String::drain(..byte)` 改字符边界安全。

### 新增：图像生成
- agent 工具 `do_image_generate`，模型按需自行调用；图片落在 `~/.dscode/images/`，
  文件名为时间戳 + prompt 摘要，原子写入。
- 走已配置渠道的 `base_url` + `api_key`，不写死官方地址，第三方中转站可直接用。
  渠道名不合法或缺 key 时明确报错，不静默回退。
- `image_enabled = false` 等于**真的不注册**工具，不占 tool-definition token。
- 配置项：`[generation]` 下的 `image_enabled` / `image_model` / `image_size` /
  `image_provider`。
- 修掉 Windows 路径被 CommonMark 反斜杠转义打碎的问题（`.dscode` 前的反斜杠被当
  转义序列吞掉且不可逆）—— 路径在进 markdown 前 `encodeURIComponent`。
- 补 `protocol-asset` feature：缺失会让整个 workspace 构建失败，或让图片静默加载不到。

### 桌面 UI
- Token 层重写：`tailwind.config.js` 定义全部颜色 / 圆角 / 阴影 / 字体，
  `globals.css` 定义底色与 `.row` / `.btn` / `.field` / `.panel` / `.menu` /
  `.icon-btn` 原语。组件从各自挑颜色迁到统一 token（旧 UI 有约 5 种近似灰、
  8 种不同的发丝线透明度）。
- 修掉保存设置会重置 `config.toml` 中 UI 不拥有的小节（`agent.git_bash_path`、
  `agent.read_before_edit`、`context.max_agent_iterations`、整个 `[teams]`）。
  改为保留注释与未知键的局部更新（`config/patch.rs`，基于 `toml_edit`）。
- 修掉 `loading` 标志丢弃请求而非排队的问题。

### TUI / CLI
- TUI 输入中文必 panic（游标用字节下标），且 panic 后终端留在 raw mode ——
  现装 panic hook、游标改字符下标、三处字节截断改 `chars().take()`。
- TUI 出错后进入不可退出的重发死循环 —— 清 `is_streaming`、补 `should_quit` 前提。

### Windows 终端
- ConPTY 输出不再用 `from_utf8_lossy` 解码（中文 Windows 上 PowerShell/cmd 默认
  CP936，汉字全变 `U+FFFD`），且非法 UTF-8 不再让 `next_line()` 静默 break
  导致后半段输出整个丢失。
- `do_bash` 输出上限前置到读线程，不再读完才判。
- ConPTY 改为树杀（此前不杀子进程树）。
- 修掉 ConPTY 未在收集输出前关闭的问题（否则 `echo hello` 也返回空）。

### 网络工具
- Bing 搜索不再硬编码 `setlang=en-US&mkt=en-US&cc=US` + `Accept-Language: en-US`
  （中文查询被强行送进美国市场，结果被无关内容污染）。
- `MAX_FETCH_BYTES` 同样改为上限前置。

### 其他
- 新增 `THIRD_PARTY_NOTICES.md` 记录被并入产物的第三方作品（非仅依赖）。
- 新增 `crates/dscode-core/tests/user_reported_bugs.rs` 覆盖用户直接报的两个 bug。

## 0.3.1 — 修复已知错误 (2026-07-17)

### Fixed
- 修复 ChatArea 与 chatStore 已知问题

## 0.3.0 — Teams v2 & agent quality (2026-07-11)

### Multi-agent Teams v2
- **`TeamRuntime`** is the **only** production path for pure `/teams` (v1 body **retired**).
- Shared **TaskBoard** DAG scheduling (`claim`, deps, `parallel_layers`, blocked on failed deps).
- **Roles**: Explore / Implement / Verify with tool allow|deny snapshots on `ToolRegistry`.
- **FileOwnership** (K18): empty `owned_paths` = unrestricted; optional enforce + soft log.
- **Multi-wave**: research → synthesize → implement → **auto verify-1** → merge.
- **Control plane**: cooperative cancel + select drop; **nudge injected** into ReAct loop as user messages.
- Desktop: TeamPanel **stop** + **nudge**; session **abort** also `stop_all` sub-agents.
- Sub-agents **inherit SafetyGuard + PermissionHub**.

### /auto
- Decomposer outputs real **dependencies** (`deps: 1,2`); cycle detection clears bad graphs.
- `/auto`+TEAM still uses **AutoRunner parallel MAGI only** (never TeamRuntime).

### Context & tools
- Compression **L0 snip** of stale tool results (>60% window).
- Optional **read-before-edit** + ownership hooks on `ToolContext` / file tools.
- **Memory auto-ingest** when `agent.memory_auto_ingest = true` (user + assistant turn → Scribe).
- Config: `[teams]`, `agent.read_before_edit`, `agent.memory_auto_ingest`.

### Tests
- 156 lib unit tests + 8 teams_v2 tests + 4 forge_magi tests — all green.

# Changelog (prior)

All notable changes to DS Code will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

---

## [0.2.2] — 2026-07-09

### Removed
- **Wiki subsystem removed** — dual-layer knowledge wiki, graph UI, `do_wiki_*` tools, auto-ingest, and context injection (noise / negative ROI)

---

## [0.2.1] — 2026-07-09

### Plan / Stream / Tests
- **`/plan` LLM dynamic interview** — grill-me questions generated each turn from goal + phase + project snapshot + Q&A
- **Forge SSE streaming** — primary path uses `chat_stream` with fallback to `chat()`
- **Integration tests** — `tests/forge_magi_integration.rs`

---

## [0.2.0] — 2026-07-09

### Production wiring

#### Core Engine
- **`/plan` multi-turn interview** — real `InterviewEngine` state machine (Scope→…→Quality), persisted under `~/.dscode/plans/`, produces structured PRD (not prompt-only)
- **`/auto` MAGI spiral** — wires `AutoRunner` + `MagiScheduler` (Casper→Balthasar→Melchior) with live progress events
- **Wiki context injection** — search dual-layer wiki + memory facts into system prompt (token-capped)
- **Compression → wiki** — L3 conversation summaries written back to session wiki
- **Provider factory** — routes Anthropic native vs OpenAI-compatible from model id (Desktop/CLI/TUI)
- **Memory system (Scribe)** — raw → fact → pattern pipeline with SQLite + FTS5
- **Teams** — unlimited-style decomposition (2–12+), parallel up to 8 agents
- **Safety** — hard-block critical destructive shell patterns always-on

#### Desktop / UX
- Thinking cards auto-collapse when stream ends
- HTML output sanitization + streaming fragment wrapper
- User bubbles left-aligned with subtle accent
- Session retention default **30 days** (PRD)
- App icons wired (`ct_logo`)

### Fixed
- TUI handles TeamAgent stream events
- `/plan cancel` aborts active interview

---

## [0.1.0] — 2025-06-14

### Added

#### Core Engine (`dscode-core`)
- **ReAct Agent Loop** (`agent::forge`) — streaming reasoning + tool-calling with built-in infinite-loop detection
- **Multi-Provider LLM Adapters** — DeepSeek V4, OpenAI, Anthropic Claude, Ollama (local)
- **Context Window Management** — configurable up to 1M tokens with threshold-triggered auto-compression
- **Toolchain Validation** — load-time + runtime orphan tool-call cleanup to prevent 400 errors
- **Sandboxed Tool Execution** — `do_bash`, `do_file_read`, `do_file_write`, `do_file_edit`, `do_background`, `do_task_status`
  - Dangerous command blacklist (`rm -rf /`, `mkfs`, fork bomb, etc.)
  - Path traversal prevention (canonicalize + ancestor check)
  - Process group management with `kill_on_drop`
  - Configurable timeouts (default 120s, max 600s)
  - Atomic file editing with unique-match enforcement
- **MAGI Three-Brain Auto-Spiral** (`/auto`) — Scrutinize → Execute → Promote loop with quality scoring (0–100)
- **Five-Phase Plan Interview** (`/plan`) — Scope → Requirements → Design → Risks → Quality → Approved
- **Task Decomposition** (`/auto`) — LLM-driven large-task breakdown into sub-tasks with stall detection
- **Multi-Agent Teams** (`/teams`) — unlimited sub-agent dispatch with role-based tool permissions & result aggregation
- **Two-Layer Knowledge Wiki** — global cross-project layer + per-session layer with FTS5 full-text search
- **Three-Tier Memory System** (Scribe) — raw messages → structured facts → cross-session patterns (storage layer ready)
- **MCP Client** — Model Context Protocol over JSON-RPC 2.0 stdio
- **SKILLS System** — YAML frontmatter skill files with trigger-based routing
- **SafetyGuard** — regex-based command filtering + path boundary enforcement
- **Session Management** — SQLite WAL mode persistence with time-grouped listing

#### CLI (`dscode-cli`)
- Single-message CLI invocation: `cargo run -p dscode-cli -- "analyze src/main.rs"`
- `--teams` flag for multi-agent mode
- Real-time streaming output to terminal

#### TUI (`dscode-tui`)
- Full terminal UI with ratatui + crossterm
- Chat panel with streaming rendering
- Session sidebar with time-grouped listing
- Thinking animation, tool-call cards, status bar
- Keyboard + mouse event handling

#### Desktop GUI (`dscode-desktop`)
- Tauri 2.x desktop application (macOS / Linux / Windows)
- React 18 frontend with TypeScript + Tailwind CSS
- Zustand state management (chat, session, config stores)
- Streaming renderer with ThinkingBlock, ToolCallCard, TeamPanel
- Settings pages: MCP management, Skills management, Wiki graph visualization
- Per-session mutex + CancellationToken abort support

#### Auxiliary: `llm_wiki`
- Independent knowledge wiki desktop application (Tauri 2 + React 19)
- Chrome MV3 browser extension for web clipping
- MCP server for AI tool integration
- PDF, DOCX, Excel, image extraction pipeline
- Vector embedding (LanceDB) + FTS5 hybrid search
- Sigma.js WebGL knowledge graph visualization
- i18n support (English, Chinese, Japanese, Korean)
- Milkdown WYSIWYG Markdown editor

---

## Types of changes

| Tag | Meaning |
|-----|---------|
| `Added` | New features |
| `Changed` | Changes in existing functionality |
| `Deprecated` | Soon-to-be removed features |
| `Removed` | Removed features |
| `Fixed` | Bug fixes |
| `Security` | Security vulnerability fixes |

---

> Initial release: 2025-06-14
