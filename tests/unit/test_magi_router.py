"""MAGI ↔ AutoRouter 集成单元测试。

测试目标：
- Scrutinizer 不传 auto_router → 行为不变（用默认 model）
- Scrutinizer 传 mock auto_router → 使用 router 推荐的模型 + thinking/effort
- Promoter 同上
- MAGIScheduler 透传 auto_router 给三脑（scrutinizer + promoter）
- auto_router 失败 → fallback 到默认 model
"""
from __future__ import annotations

import json
from collections.abc import AsyncGenerator
from typing import Any

import pytest

from dscode.core.types import (
    ExecutionResult,
    LLMResponse,
    Message,
    PRDDocument,
)
from dscode.deepseek.auto_router import AutoRouter, RouteDecision
from dscode.magi import MAGIScheduler, Promoter, Scrutinizer
from dscode.magi.execute import Executor

# ============================================================
# Fakes
# ============================================================


class RecordingLLM:
    """记录每次 chat 调用的 model / thinking / reasoning_effort。"""

    def __init__(self, contents: list[str]) -> None:
        self._contents = list(contents)
        self.calls: list[dict[str, Any]] = []

    async def chat(
        self,
        messages: list[Message],
        model: str,
        tools: list[dict[str, Any]] | None = None,
        stream: bool = False,
        thinking: bool = False,
        reasoning_effort: Any = None,
        **kwargs: Any,
    ) -> LLMResponse:
        self.calls.append(
            {
                "model": model,
                "thinking": thinking,
                "reasoning_effort": reasoning_effort,
            }
        )
        content = self._contents.pop(0) if self._contents else ""
        return LLMResponse(content=content, finish_reason="stop", model=model)

    async def chat_stream(
        self, *args: Any, **kwargs: Any
    ) -> AsyncGenerator[LLMResponse, None]:  # pragma: no cover
        if False:
            yield  # type: ignore[unreachable]
        raise NotImplementedError


class FakeRouter:
    """返回固定 RouteDecision；按需也可抛异常。"""

    def __init__(
        self,
        decision: RouteDecision | None = None,
        raise_exc: Exception | None = None,
    ) -> None:
        self.decision = decision or RouteDecision(
            recommended_model="deepseek-v4-pro",
            thinking=True,
            reasoning_effort="high",
            rationale="forced complex",
        )
        self.raise_exc = raise_exc
        self.call_count = 0

    async def route(
        self, task: str, context_summary: str = ""
    ) -> RouteDecision:
        self.call_count += 1
        if self.raise_exc is not None:
            raise self.raise_exc
        return self.decision


def _make_prd() -> PRDDocument:
    return PRDDocument(
        task_id="t-router-001",
        task_description="复杂多模块重构 user_service",
        goals=["拆分模块", "保持向后兼容"],
        constraints=[],
        acceptance_criteria=["pytest 通过"],
    )


# ============================================================
# Scrutinizer
# ============================================================


class TestScrutinizerWithRouter:
    async def test_no_router_uses_default_model(self) -> None:
        payload = json.dumps(
            {"questions": ["q"], "next_action": "do x", "risk_flags": []}
        )
        llm = RecordingLLM([payload])
        scrutinizer = Scrutinizer(llm=llm, model="deepseek-v4-flash")
        await scrutinizer.scrutinize(
            prd=_make_prd(), previous_round=None, spec_text=""
        )
        assert len(llm.calls) == 1
        assert llm.calls[0]["model"] == "deepseek-v4-flash"
        # 未传 router → 不应附带 thinking / reasoning_effort
        assert llm.calls[0]["thinking"] is False
        assert llm.calls[0]["reasoning_effort"] is None

    async def test_router_overrides_model_and_effort(self) -> None:
        payload = json.dumps(
            {"questions": ["q"], "next_action": "do x", "risk_flags": []}
        )
        llm = RecordingLLM([payload])
        router = FakeRouter(
            decision=RouteDecision(
                recommended_model="deepseek-v4-pro",
                thinking=True,
                reasoning_effort="max",
                rationale="deep",
            )
        )
        scrutinizer = Scrutinizer(
            llm=llm,
            model="deepseek-v4-flash",
            auto_router=router,  # type: ignore[arg-type]
        )
        await scrutinizer.scrutinize(
            prd=_make_prd(), previous_round=None, spec_text=""
        )
        assert router.call_count == 1
        assert len(llm.calls) == 1
        assert llm.calls[0]["model"] == "deepseek-v4-pro"
        assert llm.calls[0]["thinking"] is True
        assert llm.calls[0]["reasoning_effort"] == "max"

    async def test_router_failure_falls_back_to_default(self) -> None:
        payload = json.dumps(
            {"questions": ["q"], "next_action": "do x", "risk_flags": []}
        )
        llm = RecordingLLM([payload])
        broken_router = FakeRouter(raise_exc=RuntimeError("router boom"))
        scrutinizer = Scrutinizer(
            llm=llm,
            model="deepseek-v4-flash",
            auto_router=broken_router,  # type: ignore[arg-type]
        )
        # 不应抛异常；应回退到默认 model
        await scrutinizer.scrutinize(
            prd=_make_prd(), previous_round=None, spec_text=""
        )
        assert broken_router.call_count == 1
        assert llm.calls[0]["model"] == "deepseek-v4-flash"


# ============================================================
# Promoter
# ============================================================


class TestPromoterWithRouter:
    async def test_router_overrides_model_and_effort(self) -> None:
        payload = json.dumps(
            {
                "quality_score": 80,
                "should_stop": False,
                "stop_reason": None,
                "next_round_focus": "继续",
                "next_round_interval_s": 0,
            }
        )
        llm = RecordingLLM([payload])
        router = FakeRouter(
            decision=RouteDecision(
                recommended_model="deepseek-v4-pro",
                thinking=True,
                reasoning_effort="high",
                rationale="complex",
            )
        )
        promoter = Promoter(
            llm=llm,
            model="deepseek-v4-pro",
            auto_router=router,  # type: ignore[arg-type]
        )
        await promoter.promote(
            execution_result=ExecutionResult(
                success=True,
                summary="ok",
                steps_taken=1,
                tokens_used=10,
                wall_time_ms=1,
            ),
            history=[],
            prd=_make_prd(),
        )
        assert router.call_count == 1
        assert llm.calls[0]["model"] == "deepseek-v4-pro"
        assert llm.calls[0]["thinking"] is True
        assert llm.calls[0]["reasoning_effort"] == "high"


# ============================================================
# MAGIScheduler 透传
# ============================================================


class TestSchedulerWiresRouter:
    async def test_scheduler_propagates_router_to_brains(self) -> None:
        """构造 scheduler 时传入 auto_router，三脑应被自动绑定。"""
        llm = RecordingLLM([])
        scrutinizer = Scrutinizer(llm=llm)
        promoter = Promoter(llm=llm)
        executor = Executor(forge=None)  # type: ignore[arg-type]

        router = FakeRouter()

        # scribe 这里仅占位，不会被实际使用
        class DummyScribe:
            async def write_raw(self, ev: Any) -> None:
                pass

        scheduler = MAGIScheduler(
            scrutinizer=scrutinizer,
            executor=executor,
            promoter=promoter,
            scribe=DummyScribe(),  # type: ignore[arg-type]
            auto_router=router,  # type: ignore[arg-type]
        )

        assert scheduler.auto_router is router
        assert scrutinizer.auto_router is router
        assert promoter.auto_router is router

    async def test_scheduler_without_router_leaves_brains_alone(self) -> None:
        llm = RecordingLLM([])
        scrutinizer = Scrutinizer(llm=llm)
        promoter = Promoter(llm=llm)
        executor = Executor(forge=None)  # type: ignore[arg-type]

        class DummyScribe:
            async def write_raw(self, ev: Any) -> None:
                pass

        scheduler = MAGIScheduler(
            scrutinizer=scrutinizer,
            executor=executor,
            promoter=promoter,
            scribe=DummyScribe(),  # type: ignore[arg-type]
        )

        assert scheduler.auto_router is None
        assert scrutinizer.auto_router is None
        assert promoter.auto_router is None
