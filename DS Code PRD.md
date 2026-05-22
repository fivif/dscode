---
tags: [项目, dscode, prd, agent, deepseek, code-agent]
created: 2026-05-22
updated: 2026-05-22
status: draft
---

# DS Code 项目 PRD

> **DS Code** — DeepSeek 原生、跨模型可用的编码 Agent。最少 token 做最好的事。
> 不是第七个 Agent 框架，是 [[ARES PRD|ARES]] 的编码特化变体 + 三段编码范式 + 记忆图谱 + DeepSeek 专属优化。

> **✅ 2026-05-22 状态更新**：**Phase 1 MVP 已实施完成**——详见 [[DS Code 实施报告]]。
> 工作目录：`/Users/zay/Desktop/work-ml/DS code/`，152 测试全过，8 CLI 命令可用。

> **✅ 2026-05-22 状态更新**：**Phase 2-3  已实施完成**。

前置阅读：[[ARES PRD]]、[[ARES 新范式设计]]、[[Claude Code泄露源码分析]]、[[六框架终极对比]]、[[DS Code 实施报告]]

---

## 一、项目定义

### 1.1 一句话

**DS Code 是一个以 DeepSeek 为原生后端、复用 ARES 三引擎的编码 Agent——通过 Plan→MAGI 三脑→记忆图谱固化的螺旋编码范式，把上下文缓存命中率推到 90%+，让"越用越省、越用越准"成为可量化的承诺。**

### 1.2 为什么需要它

| 现有编码 Agent 的痛点 | DS Code 的回答 |
|---|---|
| 一次性 ReAct，没有阶段分工，前期乱、后期糙 | 三段范式：Plan（trellis 契约）→ MAGI（审/执/提轮转）→ 沉淀（图谱固化） |
| 模型成本不可控，每次都从零思考 | 缓存稳定性架构 + Flash/Pro Auto 路由 + Prefix Completion 结构化 |
| 记忆只是 RAG 或文件塔，不可视、不可管理 | 三层记忆 + sigma.js 图谱 UI + Review 队列 |
| 技能锁死在框架内，换 Agent 等于归零 | MCP 一等公民 + SKILL 协议（trellis/grill-me 风） |
| 长时间自主运行容易漂移 | 定时验收（Anvil 自基准 + side-git 快照 + grill-me 自我拷问） |
| 强绑定单一模型供应商 | DeepSeek 原生，OpenAI/Claude/Gemini 兼容层 |

### 1.3 不是什么

- ❌ 不是新框架——底层引擎复用 ARES（Forge/Scribe/Anvil）
- ❌ 不是 Claude Code 的克隆——专注 DeepSeek 缓存极致优化
- ❌ 不是只支持 DeepSeek——OpenAI/Anthropic/Gemini 都通过 litellm 接入
- ❌ 不是多 Agent 协作平台——单 Agent + sub-agent 委派
- ❌ 不是桌面 IDE——CLI/TUI 优先，桌面 UI 是 v2

### 1.4 与 ARES 的关系

```
┌─────────────────────────────────────────────────────────────┐
│                       DS Code（本项目）                       │
│                                                             │
│  Plan 层（trellis 契约）                                     │
│  MAGI 调度层（审视/执行/提升 轮转）                          │
│  编码工具集（grep/edit/test/git/lsp）                        │
│  记忆图谱 UI（TUI + 静态 HTML 导出）                         │
│  DeepSeek 优化层（缓存稳定 + Auto 路由 + Prefix Completion）  │
│  跨模型适配层（litellm + Anthropic 兼容端点）                │
│  ───────────────────────────────────────────────────────── │
│                       ARES 底层引擎（复用）                  │
│                                                             │
│  Forge（ReAct 执行引擎）                                     │
│  Scribe（三级记忆引擎 + SQLite + FTS5）                      │
│  Anvil（异步反思 + 自基准测试）                              │
└─────────────────────────────────────────────────────────────┘
```

**复用边界**：ARES 是通用 Agent 底座，DS Code 在其上做编码特化。所有"如何执行"、"如何记忆"、"如何反思"的引擎机制不重写——DS Code 提供 Plan 编排、编码工具、图谱 UI、DeepSeek 适配。

---

## 二、目标用户与场景

### 2.1 目标用户

| 用户 | 需求 | 使用方式 |
|---|---|---|
| **DeepSeek 重度用户** | 用最低成本跑长任务 | 缓存命中率 ≥ 80%，1M ctx 充分利用 |
| **个人开发者** | 一个能托管编码工作流的 Agent | Plan 后说"开始"，5 小时后回来验收 |
| **Agent 研究者** | 验证"螺旋上升 + 记忆固化"假设 | 跑同模异构基准，对比命中率/质量曲线 |
| **跨模型实用主义者** | 不想被单一供应商绑死 | 同样代码切 DeepSeek/Claude/GPT |

### 2.2 核心用户旅程

```
第 1 步：dscode init → 生成 .dscode/spec/ 目录契约 + 配置 API key
第 2 步：dscode plan "重构 user_service.py 拆分为三个模块"
        → grill-me 风格深度访谈 → 写入 .dscode/tasks/<id>/prd.md
第 3 步：dscode run <id> --hours 3
        → MAGI 轮转：审视 → 执行 → 提升 → 审视…
        → 每轮 side-git 快照，质量曲线实时更新
第 4 步：3 小时后回来 → 查看 dscode report
        → 看 MAGI 轮次日志、质量曲线、记忆图谱新增节点
第 5 步：dscode graph export → 静态 HTML 图谱，浏览器查看
第 6 步：dscode reflect → 把这次任务的 SOP 沉淀到记忆图谱
```

---

## 三、架构设计

### 3.1 系统全景

```
┌────────────────────────────────────────────────────────────────────┐
│                         用户 / CLI / TUI                            │
└──────────────────────────────┬─────────────────────────────────────┘
                               │
                               ▼
┌────────────────────────────────────────────────────────────────────┐
│                       DS Code 调度层                                │
│                                                                    │
│   ┌──────────┐    ┌──────────────────┐    ┌──────────────────┐    │
│   │ Plan 阶段 │ →  │ MAGI 轮转调度器   │ →  │ Reflect 阶段     │    │
│   │          │    │                  │    │                  │    │
│   │ trellis  │    │ ① Scrutinize     │    │ 沉淀 SOP →L3     │    │
│   │ 目录契约 │    │ ② Execute        │    │ 沉淀 SKILL       │    │
│   │ grill-me │    │ ③ Promote        │    │ 更新图谱关联     │    │
│   │ 深度访谈 │    │ ↻ 螺旋上升        │    │                  │    │
│   └──────────┘    └─────────┬────────┘    └──────────────────┘    │
└─────────────────────────────┼──────────────────────────────────────┘
                              │
                              ▼
┌────────────────────────────────────────────────────────────────────┐
│                  ARES 底层引擎（复用）                              │
│                                                                    │
│  ┌──────────┐    ┌──────────────┐    ┌──────────────┐             │
│  │  Scribe  │◄──►│    Forge     │    │    Anvil     │             │
│  │  记忆引擎 │    │  执行引擎     │    │  反思引擎    │             │
│  │          │    │              │    │              │             │
│  │  L1 raw  │    │  ReAct 流式  │    │  压缩管道    │             │
│  │  L2 facts│    │  命名约定路由 │    │  模式提取    │             │
│  │  L3 graph│    │  安全栈      │    │  自基准测试  │             │
│  │  SQLite+ │    │              │    │              │             │
│  │  FTS5    │    │              │    │              │             │
│  └──────────┘    └──────────────┘    └──────────────┘             │
└────────────────────────────────────────────────────────────────────┘
                              │
                              ▼
┌────────────────────────────────────────────────────────────────────┐
│                    模型适配层（DS Code 新增）                       │
│                                                                    │
│   DeepSeek 原生           │     兼容后端                          │
│   ─────────────           │     ─────────                          │
│   · v4-flash / v4-pro     │     · OpenAI (litellm)                 │
│   · Auto 路由             │     · Anthropic (litellm)              │
│   · Prefix Completion     │     · Gemini (litellm)                 │
│   · FIM 补全              │     · 本地 (Ollama/vLLM)               │
│   · 思考模式              │                                        │
│   · 缓存稳定性 telemetry  │                                        │
└────────────────────────────────────────────────────────────────────┘
```

### 3.2 三段编码范式

#### 3.2.1 前期：Plan 阶段（trellis 契约 + grill-me 拷问）

**目标**：把模糊需求变成结构化 PRD + 上下文清单，前置消除歧义。

**机制**：
- 目录契约 `.dscode/`（trellis 借鉴）：
  ```
  .dscode/
  ├── spec/                # 一次写好、每次会话注入的项目规范
  │   ├── conventions.md   # 代码风格、命名、测试规范
  │   ├── architecture.md  # 项目架构高层描述
  │   └── safety.md        # 危险操作清单
  ├── tasks/
  │   └── <task-id>/
  │       ├── prd.md           # Plan 阶段生成
  │       ├── implement.jsonl  # 实现阶段上下文清单
  │       ├── check.jsonl      # 验证阶段上下文清单
  │       └── magi-log.md      # MAGI 轮次日志
  └── workspace/
      └── <session-id>/
          └── transcript.md    # 当前会话日志
  ```

- grill-me 风格深度访谈（mattpocock SKILL 借鉴）：
  - 单线程提问，每次一个分支
  - **每问一题，先给推荐答案**（不是空白让用户填）
  - 不确定时**先读 codebase 再问**
  - 直到达成 shared understanding 才退出

- 启动命令：
  ```bash
  dscode plan "重构 user_service.py"
  → 触发 plan-agent sub-agent
  → 读 spec/* 注入上下文
  → grill-me 拷问 5-10 轮
  → 生成 prd.md + implement.jsonl + check.jsonl
  ```

**输出物**：一份 PRD + 两个 JSONL 上下文清单（精筛过的相关文件/代码片段）。

#### 3.2.2 中期：MAGI 三脑轮转

**目标**：项目螺旋上升，每轮三个阶段都过一遍，质量逐轮抬升。

**MAGI 三阶段定义**：

| 阶段 | 别名 | 输入 | 任务 | 输出 |
|---|---|---|---|---|
| ① **Scrutinize**（审视） | Magi-Casper | 当前状态 + 上轮成果 | 提出 N 个关键问题（grill-me 风），找出缺陷、矛盾、未验证假设 | 问题清单 |
| ② **Execute**（执行） | Magi-Balthasar | 问题清单 + spec + 工具 | 用 Forge 引擎实际编码、跑测试、修 bug、提交 | 代码变更 + 测试结果 |
| ③ **Promote**（提升） | Magi-Melchior | 执行结果 + 历史轮次 | 指引方向：下一轮该聚焦什么？是否可以收尾？是否需要重写 spec？ | 下一轮计划 + 评分 |

**轮转调度**：

```python
async def magi_loop(task_id: str, deadline: datetime):
    """
    螺旋上升主循环。在 deadline 之前不停止。
    每轮三阶段都过一遍。每轮结束打 side-git 快照。
    """
    round_num = 0
    quality_history = []

    while datetime.now() < deadline:
        round_num += 1
        log.info(f"MAGI Round {round_num} 开始")

        # ① Scrutinize：审视上轮成果，找问题
        questions = await magi_scrutinize(
            task_id=task_id,
            previous_state=load_state(task_id),
            spec=load_spec(),
        )

        # ② Execute：用 Forge 跑工具，解决问题
        execution_result = await forge.execute(
            task=questions.next_action,
            context=await scribe.read.context_packet(task_id),
            tools=ENCODING_TOOLS,
        )

        # 立即 side-git 快照（Hmbown 验证可行）
        snapshot_id = await side_git.snapshot(
            message=f"MAGI R{round_num} after execute",
        )

        # ③ Promote：评估本轮、决定下轮方向
        promotion = await magi_promote(
            execution_result=execution_result,
            history=quality_history,
            spec=load_spec(),
        )

        quality_history.append({
            "round": round_num,
            "quality": promotion.quality_score,
            "tokens_used": execution_result.tokens,
            "cache_hit_rate": execution_result.cache_hit_rate,
            "snapshot": snapshot_id,
        })

        # 通知 Anvil 异步反思
        asyncio.create_task(anvil.notify_round_complete(task_id, round_num))

        # 提前退出条件
        if promotion.should_stop:
            log.info(f"MAGI 提前退出: {promotion.stop_reason}")
            break

        # 自适应间隔（让 Anvil 有时间做异步反思）
        await asyncio.sleep(promotion.next_round_interval)

    # 最终验收报告
    return await generate_acceptance_report(task_id, quality_history)
```

**时长由 LLM 决定**（你的原话）：

```python
async def estimate_task_hours(task_description: str, codebase_metrics: dict) -> int:
    """
    由 Plan 阶段的 LLM 评估任务复杂度，返回预期小时数。
    例如：小 bug 修复=1h，模块重构=3h，新功能=5h，架构级=8h
    """
    response = await llm.chat(
        model="deepseek-v4-flash",  # 用 flash 做评估，省钱
        messages=[
            {"role": "system", "content": ESTIMATE_PROMPT},
            {"role": "user", "content": f"Task: {task_description}\nCodebase: {codebase_metrics}"},
        ],
    )
    return response.estimated_hours
```

#### 3.2.3 后期：沉淀阶段（reflect → 记忆图谱）

**目标**：把热记忆和温记忆提炼为记忆节点，存入图谱。

**触发**：
- MAGI 循环结束后自动触发
- `dscode reflect` 手动触发
- 系统空闲 30 分钟后自动触发（Anvil idle 触发）

**沉淀产物**：
1. **SOP 节点**（行为模式）：
   - 例："Python 项目添加新依赖时的步骤：read pyproject.toml → uv add → run tests"
   - 写入 Scribe L3 + 文件镜像 `.dscode/graph/sops/<id>.md`
2. **SKILL 节点**（可复用技能）：
   - 例："grill-me 风格的代码审查访谈"
   - 写为 SKILL.md 格式（mattpocock 兼容），可被其他 Agent 加载
3. **概念节点**（项目术语 + 关联）：
   - 例："user_service 模块 ↔ AuthMiddleware ↔ JWT 解析"
   - 写入 Scribe L2，作为图谱节点
4. **关联边**（4 信号自动生成）：
   - Direct link（wikilink）×3.0
   - Source overlap ×4.0
   - Adamic-Adar ×1.5
   - Type affinity ×1.0

### 3.3 三层记忆系统

> **明确定义**：DS Code 的"三层"按"距离当前活动的远近"划分，与 Karpathy 的"raw/wiki/schema"划分方向相反。

| 层 | 别名 | 内容 | 存储 | 持久度 | 检索时机 |
|---|---|---|---|---|---|
| **热** | Hot / 工作记忆 | 当前 session 的 messages + 最近 N 轮思考链 | RAM + Scribe L1 (raw_events) | 会话级 | 实时（每个 ReAct 步） |
| **温** | Warm / 编译知识 | 已验证事实 + SOP 草稿 + Plan 输出 | Scribe L2 (facts) + 文件镜像 | 跨会话持久 | 任务开始时 context_packet 注入 |
| **冷** | Cold / 记忆图谱 | 稳定 SOP + 概念关联 + SKILL + 4 信号关联边 | Scribe L3 (patterns) + graph.db + sigma.js HTML | 永久 | 显式查询 / 同模任务匹配 |

**写入路径**（晋升流水线）：

```
用户操作 / Forge 执行
   ↓
[热] 写入 L1 raw_events（无条件）
   ↓
工具调用成功 + 可验证
   ↓
[温] 写入 L2 facts（Scribe 闸门）
   ↓
跨 5+ session、2+ 任务类型、24h+ 时间跨度
   ↓
[冷] 写入 L3 patterns + 图谱节点（Anvil 稳定性检测器）
   ↓
4 信号自动连边（Direct/Source/AA/Type）
```

**读取路径**（注入策略）：

```python
async def assemble_context(task: str, budget: int = 60000) -> str:
    """
    DS Code 上下文装配。
    总预算 60K tokens（v4 1M ctx 的 6%，给 cache 留足空间）。
    
    分配比例（参考 nashsu/llm_wiki 60/20/5/15）：
    - 60%（36K）：固定前缀（spec + 工具定义 + 通用规则）→ 永不修改，吃满缓存
    - 20%（12K）：温记忆（L2 facts + Plan 阶段 PRD）→ 任务起始注入
    - 5%（3K）：冷记忆（L3 patterns + 相关图谱节点）→ 按 task 检索
    - 15%（9K）：热记忆（最近 N 轮对话）→ 滚动窗口
    """
    fixed_prefix = await load_spec_and_tools()          # 60%
    warm = await scribe.read.context_packet(task, max=12000)  # 20%
    cold = await graph.query_relevant(task, max=3000)         # 5%
    hot = await session.recent_messages(max=9000)             # 15%
    
    return fixed_prefix + warm + cold + hot  # 顺序固定！缓存命中关键
```

### 3.4 DeepSeek 专属优化层

#### 3.4.1 缓存稳定性架构（核心武器）

**原则**：缓存命中价是未命中的 **1/50**（v4-flash：￥0.02 vs ￥1）。所有架构决策围绕"前缀稳定"。

**实现**：
```python
class CacheStableMessages:
    """
    DS Code 消息装配器。保证 messages 前缀稳定。
    
    顺序铁律（从前到后）：
    1. system prompt（钉死，整个项目不变）
    2. spec 文件内容（项目级，一周不变）
    3. 工具定义（版本不变就不变）
    4. 仓库摘要（每次启动生成一次，session 内不变）
    5. 温记忆 facts（任务级，本任务内不变）
    6. 冷记忆 patterns（任务级，本任务内不变）
    7. 当前任务 PRD（任务级，本任务内不变）
    --- 以下为变动区 ---
    8. MAGI 轮次历史
    9. 当前轮内的 thought/tool_call/tool_result
    """
    
    def assemble(self, ...) -> list[Message]:
        # 1-7 是稳定前缀，8-9 是滚动尾部
        # 永不在 1-7 中插入新内容
        ...
```

**telemetry**：
```python
class CacheTelemetry:
    """实时跟踪缓存命中率，在 TUI statusline 展示。"""
    
    def on_response(self, usage: dict):
        hit = usage["prompt_cache_hit_tokens"]
        miss = usage["prompt_cache_miss_tokens"]
        self.hit_rate = hit / (hit + miss)
        # 在 TUI 底部展示：[Cache: 87.3% | Saved: $12.45]
```

#### 3.4.2 Auto 路由（Flash + Pro 混合）

```python
class AutoRouter:
    """
    根据任务复杂度自动选模型。
    Flash 成本 ≈ Pro 的 1/12，路由准确率 >90% 时净省 70%+。
    """
    
    async def route(self, task: str, context: str) -> str:
        # 用 Flash 做分类（thinking=off，最便宜）
        classification = await self.client.chat(
            model="deepseek-v4-flash",
            messages=[{"role": "system", "content": ROUTER_PROMPT},
                      {"role": "user", "content": f"Task: {task}"}],
            extra_body={"thinking": {"type": "disabled"}},
        )
        
        # 简单任务 → Flash + thinking=off
        # 中等任务 → Flash + thinking=medium
        # 复杂任务 → Pro + thinking=max
        # 推理任务 → Pro + reasoning_effort=max
        return classification.recommended_model
```

#### 3.4.3 Prefix Completion 结构化输出

```python
class StructuredOutput:
    """
    用 Prefix Completion 替代脆弱的 JSON mode。
    强制 JSON 开头 + stop token，可靠性接近 100%。
    """
    
    async def force_json(self, schema_hint: str) -> dict:
        response = await self.client.chat(
            model="deepseek-v4-flash",
            base_url="https://api.deepseek.com/beta",
            messages=[
                {"role": "user", "content": schema_hint},
                {"role": "assistant", "content": '{"', "prefix": True},
            ],
            stop=["}"],
        )
        return json.loads('{"' + response.content + "}")
```

#### 3.4.4 FIM 补全（场景化使用）

仅在"光标处补全"场景启用（区别于纯 chat agent）：
```python
async def fim_complete(prefix: str, suffix: str) -> str:
    """
    Fill-In-Middle 双向上下文补全。
    场景：自动重构、模板填充、代码补全。
    比 chat 续写更精准。
    """
    return await self.beta_client.completions.create(
        model="deepseek-v4-flash",
        prompt=prefix,
        suffix=suffix,
        max_tokens=4000,
    )
```

#### 3.4.5 思考模式 + Tool Calling

```python
async def reasoning_with_tools(messages: list, tools: list) -> Response:
    """
    V3.2+ 起兼容。注意：工具轮次必须完整回传 reasoning_content，否则 400。
    """
    response = await self.client.chat(
        model="deepseek-v4-pro",
        messages=messages,
        tools=tools,
        extra_body={"thinking": {"type": "enabled"}},
        reasoning_effort="high",
    )
    
    # 把 reasoning_content 完整存回 messages（关键）
    messages.append({
        "role": "assistant",
        "content": response.content,
        "reasoning_content": response.reasoning_content,  # ← 不能丢
        "tool_calls": response.tool_calls,
    })
    return response
```

### 3.5 跨模型适配层

```python
class ModelProvider:
    """
    统一接口，下层是 litellm + DeepSeek SDK 双轨。
    DeepSeek 原生功能（FIM/Prefix Completion/strict tools）走 DeepSeek SDK。
    其他模型走 litellm。
    """
    
    BACKENDS = {
        "deepseek-v4-flash": "deepseek-native",
        "deepseek-v4-pro": "deepseek-native",
        "claude-sonnet-4.6": "litellm",
        "gpt-5": "litellm",
        "gemini-3-pro": "litellm",
        "ollama/qwen3-32b": "litellm",
    }
    
    async def chat(self, model: str, **kwargs):
        backend = self.BACKENDS[model]
        if backend == "deepseek-native":
            return await self.deepseek_client.chat(model=model, **kwargs)
        else:
            return await litellm.acompletion(model=model, **kwargs)
```

### 3.6 能力扩展系统

#### 3.6.1 MCP 一等公民

继承 Hello-Agents / Claude Code 的 MCP 实现：
- MCP 工具与原生工具在 ToolRegistry 中平等注册
- 启动时 ping 所有 MCP 端点，标记不可用工具
- 用户配置 `~/.dscode/mcp_servers.json` 手动注册

#### 3.6.2 SKILL 协议（trellis/grill-me 风）

SKILL.md 格式：
```yaml
---
name: pr-review-grill
description: 触发器描述（也是模型决定是否加载的依据）
---
<body：被激活时注入的指令>
```

DS Code 的 SKILL 加载机制：
- `.dscode/skills/` 本地 SKILL
- `~/.dscode/skills/` 全局 SKILL
- 启动时扫描所有 SKILL，把 description 字段嵌入 system prompt
- 模型遇到匹配触发条件时，主动加载 body

**与 Anthropic Skills 兼容**：可直接复用 mattpocock/skills 仓库的现成 SKILL。

### 3.7 安全栈

继承 ARES 四层 + 编码特化：

| 层 | 来源 | DS Code 增强 |
|---|---|---|
| L1 超时 kill (60s) | GenericAgent | — |
| L2 文件操作约束 | GenericAgent | + 禁止修改 `.dscode/spec/`（需 `--unsafe` 才能改） |
| L3 FAIL-CLOSED | Claude Code | + 不允许 git push 到非当前分支 |
| L4 用户确认 | Claude Code | + side-git 快照即时生成，可一键回滚 |

**side-git 快照**（Hmbown 验证可行）：
- 不污染业务 `.git`
- 路径 `.dscode/snapshots/<round-id>.tar.zst`
- 每个 MAGI 轮次自动快照
- `dscode rollback <round-id>` 一键恢复

---

## 四、技术规格

### 4.1 技术栈

| 层 | 选择 | 来源 | 理由 |
|---|---|---|---|
| 语言 | Python 3.12+ | 与 ARES 一致 | 复用引擎 |
| 包管理 | uv | Hermes | 100x pip 速度 |
| 主循环 | asyncio + Generator | ARES Forge | 流式 + 异步 |
| 工具路由 | 命名约定 `do_{name}` | GenericAgent | 零开销 |
| 存储 | SQLite + FTS5 + 文件镜像 | ARES Scribe | 复用 |
| 图谱后端 | NetworkX + graphology JS | nashsu/llm_wiki | Python 计算，JS 渲染 |
| 图谱可视化 | sigma.js + ForceAtlas2 | nashsu/llm_wiki | 静态 HTML 导出 |
| LLM | DeepSeek SDK + litellm | — | 双轨：原生 + 兼容 |
| TUI | Textual | — | Python 原生 |
| 桌面 UI（v2） | Tauri + React + shadcn/ui | nashsu/llm_wiki | 跨平台、性能好 |
| 子代理 | 复用 ARES Forge 实例 | ARES | 复用 |
| 沙箱 | side-git + 文件白名单 | Hmbown | 轻量 |

### 4.2 目录结构

```
dscode/
├── pyproject.toml
├── README.md
├── src/dscode/
│   ├── __init__.py
│   ├── cli.py                 # CLI 入口
│   ├── tui.py                 # Textual TUI
│   │
│   ├── magi/                  # MAGI 三脑（新增）
│   │   ├── __init__.py
│   │   ├── scheduler.py       # 轮转调度器
│   │   ├── scrutinize.py      # 审视阶段
│   │   ├── execute.py         # 执行阶段（薄壳，调用 Forge）
│   │   └── promote.py         # 提升阶段
│   │
│   ├── plan/                  # Plan 阶段（新增）
│   │   ├── __init__.py
│   │   ├── grill_me.py        # 深度访谈引擎
│   │   ├── spec_loader.py     # 加载 .dscode/spec/
│   │   └── prd_generator.py   # 生成 prd.md + jsonl
│   │
│   ├── ares/                  # ARES 引擎（复用，子模块或包依赖）
│   │   ├── forge.py
│   │   ├── scribe.py
│   │   ├── anvil.py
│   │   └── ...
│   │
│   ├── tools/                 # 编码工具集（新增）
│   │   ├── grep.py
│   │   ├── edit.py
│   │   ├── test_runner.py
│   │   ├── git_ops.py
│   │   ├── lsp_query.py       # 通过 pyright / pylsp 查类型
│   │   └── side_git.py        # side-git 快照
│   │
│   ├── deepseek/              # DeepSeek 优化层（新增）
│   │   ├── client.py          # 原生 SDK
│   │   ├── cache_stable.py    # 消息装配器
│   │   ├── auto_router.py     # Flash/Pro 路由
│   │   ├── prefix_completion.py
│   │   ├── fim.py
│   │   └── telemetry.py       # 缓存命中率
│   │
│   ├── providers/             # 跨模型适配（新增）
│   │   ├── deepseek_native.py
│   │   ├── litellm_adapter.py
│   │   └── anthropic_compat.py
│   │
│   ├── graph/                 # 记忆图谱（新增）
│   │   ├── builder.py         # 4 信号关联计算
│   │   ├── louvain.py         # 社区检测
│   │   ├── insights.py        # Surprising/Gap 检测
│   │   └── exporter.py        # 导出 sigma.js HTML
│   │
│   ├── skills/                # SKILL 协议（新增）
│   │   ├── loader.py
│   │   └── registry.py
│   │
│   └── safety/                # 安全栈（复用 + 增强）
│       ├── timeout.py
│       ├── file_guard.py
│       └── fail_closed.py
│
├── .dscode/                   # 运行时配置（每个项目）
│   ├── spec/
│   ├── tasks/
│   ├── workspace/
│   └── snapshots/             # side-git 快照
│
├── benchmarks/
│   └── coding_tasks.json      # 编码专用基准集
│
└── tests/
```

### 4.3 依赖

```toml
[project]
name = "dscode"
version = "0.1.0"
requires-python = ">=3.12"
dependencies = [
    # ARES 引擎（复用）
    "ares>=0.1",              # 假设 ARES 已发布为包，否则用 git submodule
    
    # 模型后端
    "deepseek-sdk>=1.0",      # DeepSeek 原生 SDK
    "litellm>=1.50",          # 跨模型适配
    
    # UI
    "textual>=1.0",
    
    # 图谱
    "networkx>=3.0",
    
    # 工具
    "tree-sitter>=0.21",      # 代码 AST 解析
    "pygments>=2.17",         # 语法高亮
    
    # 沙箱
    "zstandard>=0.22",        # side-git 快照压缩
]

[project.optional-dependencies]
desktop = ["tauri-app"]  # v2 桌面 UI
```

### 4.4 性能目标

| 指标 | 目标 | 测量方式 |
|---|---|---|
| 冷启动 | <800ms | `time dscode run "hello"` |
| 缓存命中率 | ≥80% | telemetry 24h 累计 |
| 单轮 ReAct 延迟（缓存命中） | <1.2s | wall clock |
| MAGI 单轮总耗时（小任务） | <90s | 三阶段总和 |
| 记忆检索 | <100ms | SQLite FTS5 |
| 图谱构建（1K 节点） | <3s | NetworkX 计算 |
| 静态图谱 HTML 导出 | <1s | sigma.js 序列化 |
| 跨模型切换延迟 | <50ms | provider 路由 |

---

## 五、功能清单

### Phase 1：MVP（2 周）

**目标**：跑通 Plan → 单轮 MAGI → Reflect 的最小闭环，仅 DeepSeek 后端。

| ID | 功能 | 优先级 | 依赖 |
|---|---|---|---|
| F1 | CLI 入口：`dscode init/plan/run/report` | P0 | — |
| F2 | `.dscode/` 目录契约生成 | P0 | F1 |
| F3 | 复用 ARES Forge/Scribe（v0.1 子集即可） | P0 | ARES Phase 1 |
| F4 | DeepSeek 原生客户端 + cache_stable 装配 | P0 | — |
| F5 | 缓存命中率 telemetry | P0 | F4 |
| F6 | Plan 阶段：grill-me 风格 5-10 轮访谈 → prd.md | P0 | F3, F4 |
| F7 | 编码工具集（grep/edit/test/git）8 个 | P0 | F3 |
| F8 | 单轮 MAGI 三阶段（仅一轮，不循环） | P0 | F6, F7 |
| F9 | side-git 快照 + 回滚 | P0 | F7 |
| F10 | Textual TUI 基础框架 | P1 | F1 |

**交付物**：能从 CLI 接收编码任务、生成 PRD、跑一轮 MAGI、回滚的最小 Agent。

### Phase 2：螺旋上升 + 记忆图谱（2 周）

**目标**：MAGI 多轮循环、定时验收、温→冷晋升、图谱可视化。

| ID | 功能 | 优先级 | 依赖 |
|---|---|---|---|
| F11 | MAGI 多轮循环 + 提前退出条件 | P0 | F8 |
| F12 | LLM 任务时长估算（estimate_task_hours） | P0 | F4 |
| F13 | Anvil 异步反思（复用 ARES）+ 压缩管道 | P0 | ARES Phase 2 |
| F14 | L3 模式提取 → 图谱节点 | P0 | F13 |
| F15 | 图谱构建器（4 信号 + Louvain） | P0 | F14 |
| F16 | 静态 HTML 图谱导出（sigma.js） | P0 | F15 |
| F17 | Graph Insights（Surprising/Gap） | P1 | F15 |
| F18 | Review 队列（矛盾/不确定项） | P1 | F13 |
| F19 | Auto 路由（Flash/Pro 混调） | P1 | F4 |
| F20 | TUI statusline（缓存率/成本/轮次） | P1 | F5 |

**交付物**：能跑 3-5 小时的螺旋上升任务，每轮快照可回滚，结束后生成图谱 HTML。

### Phase 3：跨模型 + 能力扩展（2 周）

**目标**：litellm 接入其他模型、MCP/SKILL 生态、Prefix Completion/FIM 高级功能。

| ID | 功能 | 优先级 | 依赖 |
|---|---|---|---|
| F21 | litellm 适配 OpenAI/Claude/Gemini/Ollama | P0 | F4 |
| F22 | Anthropic 兼容端点（复用 Claude Agent SDK） | P1 | F21 |
| F23 | Prefix Completion 结构化输出 | P0 | F4 |
| F24 | Strict Tool Calls（Beta） | P0 | F4 |
| F25 | 思考模式 + tool calling 完整回传 | P0 | F4 |
| F26 | FIM 补全工具（光标处场景） | P1 | F4 |
| F27 | MCP 一等公民 | P0 | F7 |
| F28 | SKILL 加载器（兼容 mattpocock 格式） | P0 | — |
| F29 | 自基准测试（编码专用 15 任务集） | P0 | F13 |
| F30 | 跨模型同任务对比报告 | P1 | F21, F29 |

**交付物**：完整 DS Code v1.0，支持 5+ 模型后端，MCP/SKILL 生态接入，可验证"DeepSeek 缓存优化 vs 其他模型"的成本曲线。

### Phase 4：桌面 UI（v2，远期）

- Tauri + React + shadcn/ui（参考 nashsu/llm_wiki）
- 三栏布局 + 图标边栏
- 实时图谱（不只是静态 HTML）
- Activity Panel / Review 队列 / Deep Research

---

## 六、Benchmark 设计

### 6.1 编码专用基准（15 任务）

继承 ARES PRD 的 A/B/C 三类思想，全部聚焦编码：

#### A 类：项目操作（5 个）
| # | 任务 | 工具链 | 验证 |
|---|---|---|---|
| A1 | 给现有 Python 项目添加 logging | file_read → edit × N → pytest | 测试通过 + logger 存在 |
| A2 | 把 print 全部替换为 logging（保留语义） | grep → edit × N → pytest | 测试通过 + 无 print 残留 |
| A3 | 给 CLI 工具加 --verbose flag | edit → pytest | 行为正确 |
| A4 | 从 requirements.txt 迁移到 pyproject.toml | edit → uv lock → uv sync | 依赖完整 |
| A5 | 给项目加 pre-commit hook（black + ruff） | edit → git config → pre-commit run | hook 工作 |

#### B 类：Bug 修复 + 重构（5 个）
| # | 任务 | 工具链 | 验证 |
|---|---|---|---|
| B1 | 修复 3 个隐式 bug | grep → edit × 3 → pytest | 全测试通过 |
| B2 | 重构循环为生成器 | edit × N → pytest → 性能基准 | 行为不变 + 性能改善 |
| B3 | 提取重复代码为函数 | grep → edit × 3 → pytest | 通过 + 代码量减少 |
| B4 | 处理 Race Condition | grep → edit → 并发测试 | 通过 |
| B5 | 修复内存泄漏 | profiler → edit → memprofile | RSS 平稳 |

#### C 类：同模异构（5 组 × 2 变体 = 10 个）
- **变体 C1a**：给 Flask 项目加 JWT 认证
- **变体 C1b**：给 FastAPI 项目加 JWT 认证（同模式，不同框架）
- 类似地：C2a/b（添加 retry 装饰器：requests vs httpx）、C3a/b（CSV→DB：pandas vs sqlite3）等

**关键指标**：C1b 第 1 轮应明显快于 C1a 第 1 轮（L3 模式迁移证据）。

### 6.2 评估指标

继承 ARES 6 项 + 编码特化 2 项：

| 指标 | 类型 | 方向 |
|---|---|---|
| 成功率 | 布尔 | ↑ |
| 完成轮次 | 整数 | ↓ |
| Token 消耗 | 整数 | ↓ |
| **缓存命中率** | 百分比 | ↑ |
| **实际花费（CNY）** | 浮点 | ↓ |
| 工具调用数 | 整数 | ↓ |
| 错误恢复次数 | 整数 | ↓ |
| 耗时（秒） | 浮点 | ↓ |

### 6.3 跨模型对比实验

| 实验组 | 模型 | 期望 |
|---|---|---|
| **DS-Optimized** | DeepSeek v4-flash + Auto 路由 + cache stable | 成本最低、缓存率 ≥80% |
| **DS-Naive** | DeepSeek v4-flash 无优化 | 成本约 3x DS-Optimized |
| **Claude** | claude-sonnet-4.6 via litellm | 质量基准、成本中等 |
| **GPT** | gpt-5 via litellm | 质量基准、成本最高 |
| **Local** | qwen3-32b via Ollama | 隐私优先、成本零、质量低 |

**接受标准**：DS-Optimized 在 ARES 6 项 + 缓存率 ≥80% + 实际花费 ≤ Claude 组 1/10 时通过。

---

## 七、已知风险与缓解

| 风险 | 严重性 | 缓解 |
|---|---|---|
| ARES v0.1 未发布，DS Code 依赖空中楼阁 | 高 | Phase 1 与 ARES Phase 1 并行开发，git submodule 引用，DS Code 也是 ARES 第一个真实使用者 |
| 缓存命中率达不到 80% | 高 | 严格遵守消息装配顺序铁律 + telemetry 监控 + lint 工具检查 prompt 漂移 |
| MAGI 三阶段成本爆炸 | 中 | Scrutinize/Promote 用 Flash + thinking=off；Execute 用 Pro + reasoning=high |
| 长时间自主运行漂移 | 中 | 每轮 side-git 快照 + Anvil 质量曲线 + 提前退出条件（连续 3 轮无进展 → halt） |
| 图谱节点数爆炸 | 中 | <5 引用的概念不独立成节点（Karpathy 折叠原则） |
| litellm Anthropic 兼容端点 cache_control 被忽略 | 中 | 文档明确：跨模型时缓存效果只在 DeepSeek/OpenAI 上保证 |
| DeepSeek API 重大变更（旧名 2026-07-24 弃用） | 高 | Provider 抽象层 + 适配器版本化 + 在 spec/ 中文档化模型选择 |
| 思考模式 + tool call 回传错误 → 400 | 高 | 单元测试覆盖 + warning 钩子，发现丢失 reasoning_content 立即 alert |
| SKILL 描述触发污染主 system prompt | 中 | SKILL 加载延迟到匹配触发条件时，不全部预加载 |
| side-git 快照磁盘占用 | 低 | zstd 压缩 + 保留最近 50 个快照，更早的归档到冷存储 |
| GPL-3.0 借鉴 llm_wiki 风险 | 中 | 仅借鉴设计模式（4 信号、Louvain、UI 布局），不复制代码；如需 Tauri UI 在 v2 重新实现 |

---

## 八、不做的事（Out of Scope）

1. ❌ 多 Agent 协作（单 Agent + sub-agent 委派足够）
2. ❌ 消息渠道集成（Telegram/Discord/微信）
3. ❌ 模型训练 / 微调
4. ❌ Docker 沙箱（side-git + 文件白名单足够）
5. ❌ 桌面 UI v1（v2 才有）
6. ❌ 插件市场（用 MCP/SKILL 生态即可）
7. ❌ 浏览器自动化（用 MCP 服务接入第三方）
8. ❌ 语音交互
9. ❌ 实时协作（多用户编辑）
10. ❌ 自举生成 DS Code 本身的代码

---

## 九、与 ARES 的差异化

| 维度 | ARES | DS Code |
|---|---|---|
| **定位** | 通用 Agent 框架 | 编码专用 Agent |
| **范式** | 双循环（Forge + Anvil） | 三段（Plan → MAGI → Reflect） |
| **记忆** | 三级 L1/L2/L3 | 三层 热/温/冷 + 图谱 UI |
| **模型** | litellm 100+ 模型 | DeepSeek 原生 + 跨模型 |
| **基准** | 通用 15 任务 | 编码专用 15 任务（A/B/C） |
| **工具** | 9 原子工具 | 编码工具集（含 LSP/test runner） |
| **UI** | TUI | TUI + 静态图谱 HTML + (v2) Tauri |
| **沙箱** | 超时 + file_patch | + side-git 快照 |
| **优化** | 通用 | 缓存稳定性 + Auto 路由 + Prefix Completion |

**复用关系**：DS Code 完整复用 ARES 的 Forge/Scribe/Anvil 引擎，在其上加 5 个新层（Plan/MAGI/编码工具/DeepSeek 优化/图谱 UI）。

---

## 十、关联页面

- [[ARES PRD]] — 底层引擎 PRD
- [[ARES 新范式设计]] — 三引擎完整设计
- [[Claude Code泄露源码分析]] — 工业级编码 Agent 参照
- [[六框架终极对比]] — 框架选型参照
- [[Agent 范式空白分析]] — 七个空白的根因分析
- [[Hello-Agents-第10章]] — MCP 协议
- [[Hermes Agent架构分析]] — 自学习闭环参照
- [[Wiki 综述]] — 当前理解全景

---

## 十一、外部参考

- **DeepSeek 官方文档**：https://api-docs.deepseek.com/zh-cn/
- **Hmbown/DeepSeek-TUI**：https://github.com/Hmbown/DeepSeek-TUI — Rust 参考实现
- **Mindfold/Trellis**：https://github.com/mindfold-ai/trellis — 目录契约范式
- **mattpocock/skills**：https://github.com/mattpocock/skills — SKILL 格式
- **Karpathy LLM Wiki Gist**：https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f — 知识复利思想
- **nashsu/llm_wiki**：https://github.com/nashsu/llm_wiki — 图谱 UI 工程实现

---

## 十二、一句话总结

DS Code 是一个**以 DeepSeek 为原生后端、复用 ARES 引擎、用三段范式（Plan → MAGI → Reflect）编码、用三层记忆（热/温/冷+图谱）积累、用缓存稳定性架构省钱**的编码 Agent。它的差异化武器是别人抄不走的三件套：**缓存命中率 1/50 折扣 + Flash/Pro Auto 路由 + Prefix Completion 强制结构化**。它不是新框架——它是已有 6 框架的经验在编码场景的最佳合成体。
