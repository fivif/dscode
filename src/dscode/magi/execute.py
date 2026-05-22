"""Executor —— MAGI 第二脑（Balthasar），执行阶段。

实现思路是"薄壳"：把 Scrutinize 给出的 `next_action` 喂给已有的 Forge 引擎，
收集所有 StreamEvent，最后汇总成一个 `ExecutionResult`。

为什么薄而不再写 ReAct 循环？
- Forge 已经实现了 ReAct 流程、缓存、事实抽取、RawEvent 写入。
- MAGI 层只关心"跑一遍 + 拿到结果对象"，不关心步骤细节。

设计要点：
- 与 Forge 解耦：Executor 只依赖 Forge.execute(...) 这个 AsyncGenerator 接口。
- 完整收集 USAGE / COMPLETE 事件，得到 ExecutionResult 字段（tokens, cache, time）。
- 任何异常都包装成 ExecutionResult(success=False, ...)，绝不抛给 scheduler。
"""
from __future__ import annotations

import time
from typing import Any

from dscode.core.forge import Forge
from dscode.core.types import ExecutionResult, StreamEvent, StreamEventType


class Executor:
    """Forge 的薄包装，提供 ExecutionResult 汇总。"""

    def __init__(self, forge: Forge) -> None:
        self.forge = forge

    async def execute(
        self,
        next_action: str,
        session_id: str,
        task_id: str | None = None,
        max_steps: int = 20,
    ) -> ExecutionResult:
        """跑一次 Forge，把流式事件压缩成一个 ExecutionResult。"""
        events: list[StreamEvent] = []
        started_at = time.time()

        try:
            async for ev in self.forge.execute(
                task=next_action,
                session_id=session_id,
                task_id=task_id,
                max_steps=max_steps,
            ):
                events.append(ev)
        except Exception as exc:
            wall_ms = int((time.time() - started_at) * 1000)
            return ExecutionResult(
                success=False,
                summary=f"[executor exception] {type(exc).__name__}: {exc}",
                steps_taken=0,
                tokens_used=0,
                wall_time_ms=wall_ms,
                tool_call_count=0,
                error_count=1,
            )

        return _summarize(events, started_at)


# ============================================================
# 事件汇总
# ============================================================

def _summarize(events: list[StreamEvent], started_at: float) -> ExecutionResult:
    """从 StreamEvent 序列中提取汇总字段。"""
    complete_data: dict[str, Any] | None = None
    usage_data: dict[str, Any] | None = None
    error_count = 0
    tool_call_count = 0
    summary_text = ""

    for ev in events:
        if ev.type == StreamEventType.COMPLETE:
            complete_data = ev.data
            summary_text = str(ev.data.get("summary") or "")
            tool_call_count = int(ev.data.get("tool_call_count", 0) or 0)
            error_count = int(ev.data.get("error_count", 0) or 0)
        elif ev.type == StreamEventType.USAGE:
            usage_data = ev.data
        elif ev.type == StreamEventType.ERROR:
            error_count += 1

    # USAGE 字段：优先取 USAGE 事件；缺失则从 COMPLETE.data.usage 取
    if usage_data is None and complete_data is not None:
        usage_data = complete_data.get("usage")

    prompt_tokens = int((usage_data or {}).get("prompt_tokens", 0) or 0)
    completion_tokens = int((usage_data or {}).get("completion_tokens", 0) or 0)
    cache_hit = int((usage_data or {}).get("cache_hit_tokens", 0) or 0)
    cache_miss = int((usage_data or {}).get("cache_miss_tokens", 0) or 0)
    tokens_used = prompt_tokens + completion_tokens

    steps_taken = int((complete_data or {}).get("steps_taken", len(events)))
    wall_ms = int((complete_data or {}).get(
        "wall_time_ms", int((time.time() - started_at) * 1000),
    ))

    # success 判定：有 COMPLETE 事件 + 没有 error_count
    success = complete_data is not None and error_count == 0

    return ExecutionResult(
        success=success,
        summary=summary_text or ("complete" if success else "no completion"),
        steps_taken=steps_taken,
        tokens_used=tokens_used,
        cache_hit_tokens=cache_hit,
        cache_miss_tokens=cache_miss,
        wall_time_ms=wall_ms,
        tool_call_count=tool_call_count,
        error_count=error_count,
    )


__all__ = ["Executor"]
