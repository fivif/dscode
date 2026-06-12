<p align="center">
  <img src="xt_logo.png" alt="DS Code" width="128" class="gh-dark-mode-only" />
  <img src="ct_logo.png" alt="DS Code" width="128" class="gh-light-mode-only" />
</p>

<h1 align="center">DS Code</h1>

<p align="center">
  <strong>通用 AI 代码助手</strong> · DeepSeek 原生 · 跨模型
  <br/>
  TUI 终端 + 桌面 GUI。Rust 引擎，React 前端，Tauri 外壳。
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-1.85+-orange.svg" alt="Rust" />
  <img src="https://img.shields.io/badge/Tauri-2.x-blue.svg" alt="Tauri" />
  <img src="https://img.shields.io/badge/React-18-61dafb.svg" alt="React" />
  <img src="https://img.shields.io/badge/license-MIT-green.svg" alt="License" />
</p>

---

## 🏗 架构 Architecture

```
┌──────────────────────────────────────────────────────┐
│                    dscode-core (核心引擎)              │
│  ┌─────────┐ ┌──────────┐ ┌────────┐ ┌───────────┐  │
│  │  Forge   │ │ Provider │ │Session │ │   Tools    │  │
│  │ReAct 循环│ │  模型适配 │ │会话管理 │ │bash/fs/mcp │  │
│  └─────────┘ └──────────┘ └────────┘ └───────────┘  │
│  ┌─────────┐ ┌──────────┐ ┌────────┐ ┌───────────┐  │
│  │  MAGI    │ │  Auto    │ │ Teams  │ │   Wiki     │  │
│  │三脑螺旋  │ │任务拆解  │ │多Agent │ │ 知识图谱   │  │
│  └─────────┘ └──────────┘ └────────┘ └───────────┘  │
├──────────────────────────────────────────────────────┤
│                    接入层                              │
│  ┌──────────┐  ┌──────────────┐  ┌───────────────┐  │
│  │ dscode-tui│  │dscode-desktop│  │  dscode-cli   │  │
│  │  终端界面 │  │  桌面应用    │  │  命令行工具   │  │
│  └──────────┘  └──────────────┘  └───────────────┘  │
└──────────────────────────────────────────────────────┘
```

## ✨ 功能 Features

### 🧠 核心引擎 Core Engine
- **ReAct 智能体循环** — 流式推理 + 工具调用，内建死循环检测
- **上下文窗口** — 可配置最高 1M tokens，支持阈值触发自动压缩
- **工具链校验** — 加载时 + 运行时自动清理孤立工具调用，杜绝 400 错误
- **多模型支持** — DeepSeek V4、OpenAI、Anthropic Claude、本地 Ollama

### 🔮 MAGI 三脑自动螺旋 /auto
- **审查** → **执行** → **提炼** 三脑循环，全自动完成任务
- 任务自动拆解 + 进度评分
- 停滞检测 + 自动重新拆解

### 📋 /plan 五阶段需求评审
- 深度访谈：范围 → 需求 → 设计 → 风险 → 质量
- 自动识别项目文件和目录结构
- 生成结构化 PRD 文档

### 🤖 /auto 任务拆解与执行
- LLM 驱动的大任务自动拆解为子任务
- 并行执行 + 停滞检测
- 失败自动重组

### 👥 /teams 多智能体协作
- 无限子 Agent 分发，实时监控进度
- 按角色分配工具权限
- 结果聚合与合并指令

### 📚 Wiki 双层知识图谱
- **全局层** — 跨项目模式、事实、决策沉淀
- **会话层** — 单次会话的文件编辑、工具输出、推理过程
- FTS5 全文检索 + 归纳式主题聚类
- 兼容 Quartz 导出

### 🔌 扩展生态 Extensions
- **MCP** — Model Context Protocol 服务端（连接 + 工具调用）
- **SKILLS** — YAML 前置元数据的技能文件，按触发器路由

## ⚡ 快速开始 Quick Start

### 环境要求 Prerequisites
- Rust 1.85+
- Node.js 18+
- macOS / Linux / Windows

### 终端界面 TUI
```bash
cargo run -p dscode-tui
```

### 桌面应用 Desktop GUI
```bash
cd crates/dscode-desktop/ui && npm install
cd .. && cargo tauri dev
```

### 命令行 CLI
```bash
cargo run -p dscode-cli -- "分析 src/main.rs"
```

## ⚙️ 配置 Configuration

配置文件位于 `~/.dscode/config.toml`：

```toml
default_model = "deepseek/deepseek-v4-pro"

[providers.deepseek]
api_key = "your-api-key"
base_url = "https://dskey.xzay.de/v1"
enabled = true

[context]
window_tokens = 1000000    # 上下文窗口大小
compress_threshold = 0.8   # 压缩触发阈值

[generation]
max_tokens = 8192
temperature = 0.7
reasoning_effort = "medium"

[safety]
tool_timeout_secs = 120    # 工具执行超时
```

## 📂 项目结构 Project Structure

```
DS_code/
├── crates/
│   ├── dscode-core/          # 核心引擎（agent, providers, tools, wiki, memory）
│   ├── dscode-desktop/       # Tauri 2.x 桌面应用
│   │   ├── src/              # Rust 后端（commands, state, events）
│   │   └── ui/               # React 18 前端（TypeScript + Tailwind）
│   ├── dscode-tui/           # ratatui 终端界面
│   └── dscode-cli/           # 单次命令行工具
├── Cargo.toml                # 工作区根配置
└── README.md
```

## 🛠 技术栈 Tech Stack

| 层 Layer | 技术 Technology |
|---|---|
| 核心引擎 | Rust (tokio, reqwest, rusqlite) |
| 桌面 GUI | Tauri 2.x + React 18 + TypeScript + Tailwind CSS |
| 终端界面 | ratatui + crossterm |
| 知识图谱 | SQLite + FTS5 + Sigma.js |
| Markdown | react-markdown + remark-gfm |
| 配置 | serde + TOML |

## 📄 许可证 License

MIT
