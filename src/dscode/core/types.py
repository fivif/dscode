"""核心数据类和接口契约。

**所有并行开发的模块都基于本文件的接口约定**。
任何对本文件的修改必须同步通知所有依赖模块。

设计哲学：
- 用 Pydantic v2 BaseModel，零额外验证开销（mode='python' 模式）
- 所有数据类不可变（model_config frozen=True）除非显式标注
- 所有 ID 用字符串（uuid4 hex 前 12 位）
- 所有时间戳用 float Unix epoch（兼容 SQLite WAL）
"""
from __future__ import annotations

import time
import uuid
from collections.abc import AsyncGenerator, Awaitable, Callable
from enum import Enum
from typing import Any, Literal, Protocol, runtime_checkable

from pydantic import BaseModel, ConfigDict, Field


def _new_id() -> str:
    """生成短 UUID（12 字符）。"""
    return uuid.uuid4().hex[:12]


def _now() -> float:
    """Unix epoch 时间戳。"""
    return time.time()


# ============================================================
# 消息（与 OpenAI 兼容）
# ============================================================

class Message(BaseModel):
    """聊天消息。与 OpenAI / DeepSeek SDK 兼容。"""

    model_config = ConfigDict(extra="allow")

    role: Literal["system", "user", "assistant", "tool"]
    content: str | None = None
    name: str | None = None
    tool_calls: list[ToolCallSpec] | None = None
    tool_call_id: str | None = None
    # DeepSeek thinking 模式专用——下一轮必须完整回传，否则 400
    reasoning_content: str | None = None


class ToolCallSpec(BaseModel):
    """LLM 发起的工具调用请求。"""

    id: str = Field(default_factory=_new_id)
    type: Literal["function"] = "function"
    function: ToolFunctionSpec


class ToolFunctionSpec(BaseModel):
    name: str
    arguments: str  # JSON-encoded string，与 OpenAI 一致


# Pydantic v2 解决前向引用
Message.model_rebuild()
ToolCallSpec.model_rebuild()


# ============================================================
# 工具系统
# ============================================================

class ToolStatus(str, Enum):
    SUCCESS = "success"
    ERROR = "error"
    TIMEOUT = "timeout"
    BLOCKED = "blocked"


class ToolResult(BaseModel):
    """工具执行结果。"""

    model_config = ConfigDict(extra="allow")

    status: ToolStatus
    content: str           # 给 LLM 看的文本（必填，可空字符串）
    error: str | None = None
    elapsed_ms: int = 0
    metadata: dict[str, Any] = Field(default_factory=dict)

    @property
    def success(self) -> bool:
        return self.status == ToolStatus.SUCCESS


class ToolSpec(BaseModel):
    """工具元数据，注册到 ToolRegistry。"""

    name: str                      # do_xxx 命名约定
    description: str
    parameters: dict[str, Any]     # JSON Schema
    capability: str | None = None  # 抽象能力标签（file_search / code_execute 等）
    requires_confirmation: bool = False
    timeout_s: int = 60


# 工具实现的类型签名
ToolHandler = Callable[[dict[str, Any]], Awaitable[ToolResult]]


@runtime_checkable
class ToolRegistryProtocol(Protocol):
    """工具注册中心契约。"""

    def register(self, spec: ToolSpec, handler: ToolHandler) -> None: ...
    def list_specs(self) -> list[ToolSpec]: ...
    def get_handler(self, name: str) -> ToolHandler | None: ...
    def to_openai_tools(self) -> list[dict[str, Any]]: ...


# ============================================================
# 记忆系统（Scribe）
# ============================================================

class RawEvent(BaseModel):
    """L1 原始事件。无条件写入。"""

    id: str = Field(default_factory=_new_id)
    session_id: str
    task_id: str | None = None
    timestamp: float = Field(default_factory=_now)
    step_number: int
    event_type: Literal[
        "tool_call", "tool_result", "llm_thought", "user_message",
        "error", "safety_block", "magi_round_start", "magi_round_end",
    ]
    data: dict[str, Any]


class Fact(BaseModel):
    """L2 已验证事实。需要工具调用 provenance。"""

    id: str = Field(default_factory=_new_id)
    subject: str
    predicate: str
    object: str
    confidence: float = 1.0
    provenance_chain: list[str] = Field(default_factory=list)  # raw_event ids
    source_raw_event_id: str | None = None
    status: Literal["active", "deprecated", "contradicted"] = "active"
    created_at: float = Field(default_factory=_now)
    updated_at: float = Field(default_factory=_now)


class Pattern(BaseModel):
    """L3 已学习模式 = 图谱节点。"""

    id: str = Field(default_factory=_new_id)
    pattern_type: Literal["sop", "rule", "preference", "heuristic", "skill", "concept"]
    trigger_condition: str
    action_template: dict[str, Any]
    success_rate: float = 0.0
    sample_count: int = 0
    confidence: float = 0.0
    last_triggered_at: float | None = None
    session_ids: list[str] = Field(default_factory=list)
    status: Literal["active", "retired"] = "active"
    created_at: float = Field(default_factory=_now)
    updated_at: float = Field(default_factory=_now)


class ContextPacket(BaseModel):
    """Scribe 一次性装配的上下文包，注入 Forge 系统提示。"""

    facts: list[Fact] = Field(default_factory=list)
    patterns: list[Pattern] = Field(default_factory=list)
    recent_events: list[RawEvent] = Field(default_factory=list)
    total_tokens_estimate: int = 0


@runtime_checkable
class ScribeProtocol(Protocol):
    """Scribe 记忆引擎契约。Forge / MAGI / Anvil 均通过此接口访问。"""

    # 读
    async def recent(self, n: int = 50, session_id: str | None = None) -> list[RawEvent]: ...
    async def search_facts(self, query: str, top_k: int = 10) -> list[Fact]: ...
    async def patterns_for_task(self, task_description: str, min_confidence: float = 0.5) -> list[Pattern]: ...
    async def context_packet(self, task_description: str, token_budget: int = 12000) -> ContextPacket: ...

    # 写（分闸门）
    async def write_raw(self, event: RawEvent) -> None: ...
    async def write_fact(self, fact: Fact) -> FactWriteResult: ...
    async def write_pattern(self, pattern: Pattern) -> PatternWriteResult: ...


class FactWriteResult(BaseModel):
    accepted: bool
    fact_id: str | None = None
    reason: str | None = None


class PatternWriteResult(BaseModel):
    accepted: bool
    pattern_id: str | None = None
    reason: str | None = None


# ============================================================
# 执行引擎（Forge）
# ============================================================

class StreamEventType(str, Enum):
    THOUGHT = "thought"
    TOOL_START = "tool_start"
    TOOL_RESULT = "tool_result"
    SAFETY_BLOCK = "safety_block"
    ERROR = "error"
    COMPLETE = "complete"
    USAGE = "usage"


class StreamEvent(BaseModel):
    """Forge 主循环以 generator 流出的事件。给 TUI 渲染。"""

    type: StreamEventType
    data: dict[str, Any] = Field(default_factory=dict)
    timestamp: float = Field(default_factory=_now)


class ExecutionResult(BaseModel):
    """Forge 一次完整执行的最终汇总。"""

    success: bool
    summary: str
    steps_taken: int
    tokens_used: int
    cache_hit_tokens: int = 0
    cache_miss_tokens: int = 0
    wall_time_ms: int
    tool_call_count: int = 0
    error_count: int = 0
    final_messages: list[Message] = Field(default_factory=list)

    @property
    def cache_hit_rate(self) -> float:
        total = self.cache_hit_tokens + self.cache_miss_tokens
        return self.cache_hit_tokens / total if total > 0 else 0.0


@runtime_checkable
class ForgeProtocol(Protocol):
    """Forge 执行引擎契约。"""

    def execute(
        self,
        task: str,
        session_id: str,
        task_id: str | None = None,
        max_steps: int = 40,
    ) -> AsyncGenerator[StreamEvent, None]: ...


# ============================================================
# LLM Provider
# ============================================================

class Usage(BaseModel):
    """Token 消耗。与 OpenAI 兼容。"""

    prompt_tokens: int = 0
    completion_tokens: int = 0
    total_tokens: int = 0
    # DeepSeek 缓存字段（OpenAI 兼容字段名）
    prompt_cache_hit_tokens: int = 0
    prompt_cache_miss_tokens: int = 0
    # DeepSeek thinking 模式
    reasoning_tokens: int = 0


class LLMResponse(BaseModel):
    """LLM 一次响应。"""

    model_config = ConfigDict(extra="allow")

    content: str = ""
    reasoning_content: str | None = None
    tool_calls: list[ToolCallSpec] | None = None
    finish_reason: Literal["stop", "length", "tool_calls", "content_filter"] | None = None
    usage: Usage = Field(default_factory=Usage)
    model: str = ""
    # 是否命中 DeepSeek caching（统计用，与 usage 字段冗余）
    cache_hit_rate: float = 0.0


@runtime_checkable
class LLMProviderProtocol(Protocol):
    """LLM 提供方契约。DeepSeek 原生 / litellm 包装 / Anthropic 适配 都实现此协议。"""

    async def chat(
        self,
        messages: list[Message],
        model: str,
        tools: list[dict[str, Any]] | None = None,
        stream: bool = False,
        thinking: bool = False,
        reasoning_effort: Literal["low", "medium", "high", "max"] | None = None,
        **kwargs: Any,
    ) -> LLMResponse: ...

    async def chat_stream(
        self,
        messages: list[Message],
        model: str,
        tools: list[dict[str, Any]] | None = None,
        **kwargs: Any,
    ) -> AsyncGenerator[LLMResponse, None]: ...


# ============================================================
# Plan 阶段输出
# ============================================================

class PRDDocument(BaseModel):
    """Plan 阶段产出的 PRD。"""

    task_id: str = Field(default_factory=_new_id)
    task_description: str
    goals: list[str]
    constraints: list[str]
    acceptance_criteria: list[str]
    related_files: list[str] = Field(default_factory=list)
    estimated_hours: float = 1.0
    risk_notes: list[str] = Field(default_factory=list)
    created_at: float = Field(default_factory=_now)


class ContextManifest(BaseModel):
    """implement.jsonl / check.jsonl 的内存对象。"""

    phase: Literal["implement", "check"]
    files: list[str]
    snippets: list[dict[str, Any]] = Field(default_factory=list)
    related_facts: list[str] = Field(default_factory=list)


# ============================================================
# MAGI 三脑
# ============================================================

class MAGIPhase(str, Enum):
    SCRUTINIZE = "scrutinize"
    EXECUTE = "execute"
    PROMOTE = "promote"


class ScrutinizeOutput(BaseModel):
    """审视阶段输出。"""

    questions: list[str]
    next_action: str
    risk_flags: list[str] = Field(default_factory=list)


class PromoteOutput(BaseModel):
    """提升阶段输出。"""

    quality_score: float  # 0-100
    should_stop: bool
    stop_reason: str | None = None
    next_round_focus: str | None = None
    next_round_interval_s: float = 0.0


class MAGIRoundLog(BaseModel):
    """一轮 MAGI 完整记录，写入 magi-log.md。"""

    round_number: int
    started_at: float = Field(default_factory=_now)
    ended_at: float | None = None
    scrutinize: ScrutinizeOutput | None = None
    execute_result: ExecutionResult | None = None
    promote: PromoteOutput | None = None
    snapshot_id: str | None = None


# ============================================================
# 安全
# ============================================================

class SafetyDecision(BaseModel):
    """安全栈一次校验结果。"""

    allowed: bool
    denied: bool = False
    reason: str | None = None
    requires_confirmation: bool = False
