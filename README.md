# DS Code

> DeepSeek-native, cross-model **code agent**.
> Plan → MAGI 三脑轮转 → Reflect 螺旋编码范式。
> Cache-stable 架构推命中率到 80%+，让"越用越省、越用越准"成为可量化承诺。

详细 PRD：参见 Obsidian vault `wiki/项目/DS Code PRD.md`。

## 核心理念

- **三段编码范式**：
  - 前期：Plan（trellis 目录契约 + grill-me 风深度访谈）
  - 中期：MAGI 三脑（审视 → 执行 → 提升）螺旋上升
  - 后期：Reflect（沉淀 SOP / SKILL / 概念到记忆图谱）
- **三层记忆**：热（session）/温（已验证事实）/冷（图谱）
- **缓存稳定性铁律**：消息装配顺序固定，吃满 DeepSeek prefix cache 1/50 折扣
- **能力扩展**：MCP 一等公民 + SKILL 协议（兼容 Anthropic / mattpocock 格式）

## 快速开始

```bash
# 1. 安装
uv sync

# 2. 配置 API key
export DEEPSEEK_API_KEY=...

# 3. 初始化项目
dscode init

# 4. 制定计划（grill-me 深度访谈）
dscode plan "重构 user_service.py，拆分为三个模块"

# 5. 自动执行（MAGI 螺旋上升）
dscode run <task-id> --hours 3

# 6. 验收
dscode report <task-id>

# 7. 沉淀
dscode reflect
```

## 支持的模型后端

| 后端 | 调用方式 | 缓存优化 |
|------|----------|----------|
| DeepSeek v4-flash/pro | 原生 OpenAI 兼容 | ✅ 完整 |
| Claude (Anthropic) | litellm | 部分（cache_control） |
| GPT-5 (OpenAI) | litellm | 部分（prefix cache） |
| Gemini 3 | litellm | ❌ |
| 本地（Ollama/vLLM） | litellm | ❌ |

## 项目结构

```
src/dscode/
├── core/        # Forge / Scribe / Anvil 引擎（最小版）
├── deepseek/    # DeepSeek 专属优化层（cache_stable / auto_router / FIM）
├── plan/        # Plan 阶段（grill-me + spec 加载 + PRD 生成）
├── magi/        # MAGI 三脑调度器（scrutinize / execute / promote）
├── tools/       # 编码工具集（grep/edit/test/git/side_git）
├── providers/   # 跨模型适配层
├── safety/      # 安全栈
├── skills/      # SKILL 协议加载器
├── graph/       # 记忆图谱（v1 占位）
└── tui/         # Textual TUI
```

## 许可

MIT
