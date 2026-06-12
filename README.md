<p align="center">
  <img src="xt_logo.png" alt="DS Code" width="128" class="gh-dark-mode-only" />
  <img src="ct_logo.png" alt="DS Code" width="128" class="gh-light-mode-only" />
</p>

<h1 align="center">DS Code</h1>

<p align="center">
  <strong>Universal AI Code Agent</strong> — DeepSeek-native, cross-model.
  <br/>
  TUI + native Desktop GUI. Rust core, React frontend, Tauri shell.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-1.85+-orange.svg" alt="Rust" />
  <img src="https://img.shields.io/badge/Tauri-2.x-blue.svg" alt="Tauri" />
  <img src="https://img.shields.io/badge/React-18-61dafb.svg" alt="React" />
  <img src="https://img.shields.io/badge/license-MIT-green.svg" alt="License" />
</p>

---

## Architecture

```
┌──────────────────────────────────────────────────────┐
│                    dscode-core                        │
│  ┌─────────┐ ┌──────────┐ ┌────────┐ ┌───────────┐  │
│  │  Forge   │ │ Provider │ │Session │ │   Tools    │  │
│  │ReAct Loop│ │ Open AI  │ │Manager │ │bash/fs/mcp │  │
│  └─────────┘ └──────────┘ └────────┘ └───────────┘  │
│  ┌─────────┐ ┌──────────┐ ┌────────┐ ┌───────────┐  │
│  │  MAGI    │ │  Auto    │ │ Teams  │ │   Wiki     │  │
│  │3-Brain   │ │Decompose │ │Dispatch│ │ Knowledge  │  │
│  └─────────┘ └──────────┘ └────────┘ └───────────┘  │
├──────────────────────────────────────────────────────┤
│                   Interfaces                          │
│  ┌──────────┐  ┌──────────────┐  ┌───────────────┐  │
│  │ dscode-tui│  │dscode-desktop│  │  dscode-cli   │  │
│  │ ratatui   │  │Tauri+React  │  │  single-shot  │  │
│  └──────────┘  └──────────────┘  └───────────────┘  │
└──────────────────────────────────────────────────────┘
```

## Features

### Core Engine
- **ReAct Agent Loop** — Streaming reasoning + tool-calling execution with stall detection
- **Context Window** — Configurable up to 1M tokens with threshold-based compression
- **Tool Chain Validation** — Automatic orphaned tool-call cleanup at load + runtime
- **Multi-Provider** — DeepSeek V4, OpenAI, Anthropic Claude, local Ollama

### MAGI Auto-Spiral
- **Scrutinize** → **Execute** → **Promote** three-brain loop
- Autonomous task decomposition with progress scoring
- Stall detection with automatic re-decomposition

### /plan — 5-Phase PRD
- Deep interview: scope → requirements → design → risks → quality
- Auto-infers files and project structure
- Generates structured product requirement documents

### /auto — Decomposer + Runner
- LLM-driven task decomposition into subtasks
- Parallel execution with stall detection
- Automatic re-decomposition on failure

### /teams — Multi-Agent Dispatch
- Unlimited sub-agent spawning with real-time monitoring
- Tool-role assignment per agent
- Merge instructions for result aggregation

### Wiki — Two-Layer Knowledge Graph
- **Global Layer** — Cross-project patterns, facts, decisions
- **Session Layer** — Per-session file edits, tool outputs, reasoning
- FTS5 full-text search + inductive theme clustering
- Quartz-compatible export

### Extensions
- **MCP** — Model Context Protocol servers (connect + call_tool)
- **SKILLS** — YAML frontmatter skill files with trigger routing

## Quick Start

### Prerequisites
- Rust 1.85+
- Node.js 18+
- macOS / Linux / Windows

### Terminal UI
```bash
cargo run -p dscode-tui
```

### Desktop GUI
```bash
cd crates/dscode-desktop/ui && npm install
cd .. && cargo tauri dev
```

### CLI
```bash
cargo run -p dscode-cli -- "analyze src/main.rs"
```

## Configuration

Config stored at `~/.dscode/config.toml`:

```toml
default_model = "deepseek/deepseek-v4-pro"

[providers.deepseek]
api_key = "your-api-key"
base_url = "https://dskey.xzay.de/v1"
enabled = true

[context]
window_tokens = 1000000
compress_threshold = 0.8

[generation]
max_tokens = 8192
temperature = 0.7
reasoning_effort = "medium"

[safety]
tool_timeout_secs = 120
```

## Project Structure

```
DS_code/
├── crates/
│   ├── dscode-core/          # Core engine (agent, providers, tools, wiki, memory)
│   ├── dscode-desktop/       # Tauri 2.x desktop app
│   │   ├── src/              # Rust backend (commands, state, events)
│   │   └── ui/               # React 18 frontend (TypeScript + Tailwind)
│   ├── dscode-tui/           # ratatui terminal interface
│   └── dscode-cli/           # Single-shot command-line interface
├── Cargo.toml                # Workspace root
└── README.md
```

## Tech Stack

| Layer | Technology |
|---|---|
| Core Engine | Rust (tokio, reqwest, rusqlite) |
| Desktop GUI | Tauri 2.x + React 18 + TypeScript + Tailwind CSS |
| Terminal UI | ratatui + crossterm |
| Knowledge Graph | SQLite + FTS5 + Sigma.js |
| Markdown | react-markdown + remark-gfm |
| Config | serde + TOML |

## License

MIT — see [LICENSE](LICENSE)
