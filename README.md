<p align="center">
  <img src="ct_logo.png" alt="DS Code" width="128" />
</p>

<h1 align="center">DS Code — Universal Code Agent</h1>

<p align="center">
  DeepSeek-native, cross-model code agent with TUI + GUI desktop.
  <br/>
  Build with Rust + React + Tauri. Minimal tokens, maximum results.
</p>

---

## Features

- **ReAct Agent Loop** — Streaming reasoning-and-acting execution
- **MAGI Three-Brain Auto-Spiral** — Scrutinize → Execute → Promote until done
- **Multi-Agent Teams** — Unlimited sub-agent dispatch with real-time monitoring
- **Two-Layer Knowledge Wiki** — Global + session knowledge graph with Quartz visualization
- **5-Phase /plan** — Deep interview PRD generation (grill-me pattern)
- **Desktop GUI** — Tauri 2.x native app (macOS / Linux / Windows)
- **Terminal TUI** — Claude Code-grade ratatui interface
- **Multi-Provider** — DeepSeek, Anthropic, OpenAI, Ollama
- **MCP + SKILLS** — Third-party extension ecosystem

## Quick Start

```bash
# Terminal UI
cargo run -p dscode-tui

# Desktop GUI
cd crates/dscode-desktop && cargo tauri dev

# CLI (single message)
cargo run -p dscode-cli -- "your message"
```

## Tech Stack

| Layer | Technology |
|---|---|
| Core Engine | Rust (tokio, reqwest, rusqlite) |
| Desktop GUI | Tauri 2.x + React 18 + TypeScript + Tailwind |
| Terminal UI | ratatui + crossterm |
| Knowledge Graph | SQLite + FTS5 + sigma.js |
| Config | serde + TOML (single file ~/.dscode/config.toml) |

## License

MIT
