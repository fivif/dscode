# Changelog

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
