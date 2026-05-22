"""MAGI 三脑单元测试。

- Scrutinizer：mock LLM，校验输出对齐 ScrutinizeOutput
- Executor：mock Forge stream events，校验汇总 ExecutionResult
- Promoter：连续 3 轮无进展应自动停止
- MAGIScheduler：mock 三脑，校验 2 轮后退出 + raw_event 写入
"""
from __future__ import annotations

import json
from collections.abc import AsyncGenerator
from pathlib import Path
from typing import Any

import pytest

from dscode.core import Scribe
from dscode.core.types import (
    ExecutionResult,
    LLMResponse,
    MAGIRoundLog,
    Message,
    PRDDocument,
    PromoteOutput,
    ScrutinizeOutput,
    StreamEvent,
    StreamEventType,
    ToolResult,
    ToolStatus,
)
from dscode.magi import Executor, MAGIScheduler, Promoter, Scrutinizer

# ============================================================
# Fakes
# ============================================================

class FakeLLM:
    """脚本化 LLM，按预设字符串序列依次返回。"""

    def __init__(self, contents: list[str]) -> None:
        self._contents = list(contents)
        self.received: list[list[Message]] = []
        self.call_count = 0

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
        self.call_count += 1
        self.received.append(list(messages))
        if not self._contents:
            return LLMResponse(content="", finish_reason="stop", model=model)
        return LLMResponse(
            content=self._contents.pop(0),
            finish_reason="stop",
            model=model,
        )

    async def chat_stream(self, *args: Any, **kwargs: Any) -> AsyncGenerator[LLMResponse, None]:  # pragma: no cover
        if False:
            yield  # type: ignore[unreachable]
        raise NotImplementedError


class FakeForge:
    """脚本化 Forge：execute() 直接 yield 预设事件序列。"""

    def __init__(self, events: list[StreamEvent]) -> None:
        self._events = events
        self.last_task: str | None = None
        self.last_session: str | None = None
        self.call_count = 0

    async def execute(
        self,
        task: str,
        session_id: str,
        task_id: str | None = None,
        max_steps: int | None = None,
    ) -> AsyncGenerator[StreamEvent, None]:
        self.last_task = task
        self.last_session = session_id
        self.call_count += 1
        for ev in self._events:
            yield ev


# ============================================================
# Fixtures
# ============================================================

@pytest.fixture
async def scribe(tmp_path: Path):
    s = Scribe(db_path=tmp_path / "state.db", mirror_dir=tmp_path / "raw")
    yield s
    s.close()


def _make_prd() -> PRDDocument:
    return PRDDocument(
        task_id="t-001",
        task_description="重构 user_service",
        goals=["拆分模块"],
        constraints=[],
        acceptance_criteria=["pytest 通过"],
    )


# ============================================================
# Scrutinizer
# ============================================================

class TestScrutinizer:
    async def test_returns_well_formed_output(self) -> None:
        payload = json.dumps(
            {
                "questions": ["q1", "q2", "q3"],
                "next_action": "读 src/user_service.py 并拆分 User 类",
                "risk_flags": ["regression"],
            }
        )
        llm = FakeLLM([payload])
        scrutinizer = Scrutinizer(llm=llm)
        out = await scrutinizer.scrutinize(
            prd=_make_prd(),
            previous_round=None,
            spec_text="spec",
        )
        assert isinstance(out, ScrutinizeOutput)
        assert out.questions == ["q1", "q2", "q3"]
        assert "user_service" in out.next_action
        assert out.risk_flags == ["regression"]

    async def test_returns_safe_defaults_on_llm_error(self) -> None:
        class ExplodingLLM(FakeLLM):
            async def chat(self, *a: Any, **kw: Any) -> LLMResponse:
                raise RuntimeError("boom")

        scrutinizer = Scrutinizer(llm=ExplodingLLM([]))
        out = await scrutinizer.scrutinize(
            prd=_make_prd(),
            previous_round=None,
            spec_text="",
        )
        assert isinstance(out, ScrutinizeOutput)
        assert out.questions  # 非空
        assert out.next_action  # 非空
        assert any(r.startswith("scrutinizer_error") for r in out.risk_flags)

    async def test_uses_previous_round_in_prompt(self) -> None:
        prev = MAGIRoundLog(
            round_number=1,
            scrutinize=ScrutinizeOutput(
                questions=["q-old"],
                next_action="old action",
            ),
            execute_result=ExecutionResult(
                success=True,
                summary="did X",
                steps_taken=3,
                tokens_used=100,
                wall_time_ms=500,
            ),
            promote=PromoteOutput(quality_score=60, should_stop=False),
        )
        payload = json.dumps(
            {"questions": ["q1"], "next_action": "next", "risk_flags": []}
        )
        llm = FakeLLM([payload])
        scrutinizer = Scrutinizer(llm=llm)
        await scrutinizer.scrutinize(
            prd=_make_prd(),
            previous_round=prev,
            spec_text="",
        )
        # 上一轮信息应该出现在 user prompt 里
        sent = llm.received[0][-1].content or ""
        assert "did X" in sent
        assert "prev_quality_score" in sent


# ============================================================
# Executor
# ============================================================

class TestExecutor:
    async def test_collects_events_and_summarizes(self) -> None:
        events = [
            StreamEvent(type=StreamEventType.THOUGHT, data={"content": "thinking"}),
            StreamEvent(
                type=StreamEventType.TOOL_START,
                data={"name": "do_grep", "tool_call_id": "tc1", "arguments": {}},
            ),
            StreamEvent(
                type=StreamEventType.TOOL_RESULT,
                data={"name": "do_grep", "tool_call_id": "tc1", "status": "success", "content": ""},
            ),
            StreamEvent(
                type=StreamEventType.USAGE,
                data={
                    "prompt_tokens": 100,
                    "completion_tokens": 50,
                    "cache_hit_tokens": 80,
                    "cache_miss_tokens": 20,
                },
            ),
            StreamEvent(
                type=StreamEventType.COMPLETE,
                data={
                    "summary": "done well",
                    "steps_taken": 4,
                    "tool_call_count": 1,
                    "error_count": 0,
                    "wall_time_ms": 1234,
                },
            ),
        ]
        forge = FakeForge(events)
        executor = Executor(forge=forge)  # type: ignore[arg-type]
        result = await executor.execute(
            next_action="grep for foo",
            session_id="sess-x",
            task_id="t-001",
        )
        assert isinstance(result, ExecutionResult)
        assert result.success is True
        assert result.summary == "done well"
        assert result.tokens_used == 150
        assert result.cache_hit_tokens == 80
        assert result.tool_call_count == 1
        assert result.error_count == 0
        assert forge.last_task == "grep for foo"

    async def test_treats_error_event_as_failure(self) -> None:
        events = [
            StreamEvent(type=StreamEventType.ERROR, data={"phase": "x", "error": "boom"}),
            StreamEvent(
                type=StreamEventType.COMPLETE,
                data={
                    "summary": "partial",
                    "steps_taken": 1,
                    "tool_call_count": 0,
                    "error_count": 1,
                    "wall_time_ms": 10,
                },
            ),
        ]
        forge = FakeForge(events)
        executor = Executor(forge=forge)  # type: ignore[arg-type]
        result = await executor.execute(
            next_action="x",
            session_id="sess-y",
        )
        assert result.success is False
        assert result.error_count >= 1

    async def test_handles_forge_exception(self) -> None:
        class ExplodingForge:
            async def execute(
                self, task: str, session_id: str, task_id: str | None = None,
                max_steps: int | None = None,
            ) -> AsyncGenerator[StreamEvent, None]:
                if False:
                    yield  # type: ignore[unreachable]
                raise RuntimeError("forge died")

        executor = Executor(forge=ExplodingForge())  # type: ignore[arg-type]
        result = await executor.execute(next_action="x", session_id="s")
        assert result.success is False
        assert "forge died" in result.summary


# ============================================================
# Promoter
# ============================================================

class TestPromoter:
    async def test_passthrough_should_stop(self) -> None:
        payload = json.dumps(
            {
                "quality_score": 95,
                "should_stop": True,
                "stop_reason": "task_complete",
                "next_round_focus": "结束",
                "next_round_interval_s": 0,
            }
        )
        llm = FakeLLM([payload])
        prom = Promoter(llm=llm)
        out = await prom.promote(
            execution_result=ExecutionResult(
                success=True, summary="ok", steps_taken=1, tokens_used=10, wall_time_ms=1,
            ),
            history=[],
            prd=_make_prd(),
        )
        assert out.should_stop is True
        assert out.stop_reason == "task_complete"
        assert out.quality_score == pytest.approx(95.0)

    async def test_stalls_after_three_rounds_no_progress(self) -> None:
        """连续 3 轮 quality 都没有超过历史最高 -> 强制停止。"""
        # 历史 2 轮：60, 60
        history = [
            MAGIRoundLog(
                round_number=1,
                promote=PromoteOutput(quality_score=60.0, should_stop=False),
            ),
            MAGIRoundLog(
                round_number=2,
                promote=PromoteOutput(quality_score=60.0, should_stop=False),
            ),
        ]
        # 本轮 LLM 返回 should_stop=False 且 quality=60（没超过）
        payload = json.dumps(
            {
                "quality_score": 60,
                "should_stop": False,
                "stop_reason": None,
                "next_round_focus": "继续",
                "next_round_interval_s": 0,
            }
        )
        llm = FakeLLM([payload])
        prom = Promoter(llm=llm)
        out = await prom.promote(
            execution_result=ExecutionResult(
                success=True, summary="ok", steps_taken=1, tokens_used=10, wall_time_ms=1,
            ),
            history=history,
            prd=_make_prd(),
        )
        assert out.should_stop is True
        assert out.stop_reason is not None
        assert "stalled" in out.stop_reason.lower()

    async def test_does_not_stall_when_quality_improves(self) -> None:
        history = [
            MAGIRoundLog(
                round_number=1,
                promote=PromoteOutput(quality_score=50.0, should_stop=False),
            ),
            MAGIRoundLog(
                round_number=2,
                promote=PromoteOutput(quality_score=55.0, should_stop=False),
            ),
        ]
        # 本轮 quality=80，明显进步 -> 不停
        payload = json.dumps(
            {
                "quality_score": 80,
                "should_stop": False,
                "stop_reason": None,
                "next_round_focus": "继续",
                "next_round_interval_s": 0,
            }
        )
        llm = FakeLLM([payload])
        prom = Promoter(llm=llm)
        out = await prom.promote(
            execution_result=ExecutionResult(
                success=True, summary="ok", steps_taken=1, tokens_used=10, wall_time_ms=1,
            ),
            history=history,
            prd=_make_prd(),
        )
        assert out.should_stop is False

    async def test_returns_safe_default_when_llm_errors(self) -> None:
        class ExplodingLLM(FakeLLM):
            async def chat(self, *a: Any, **kw: Any) -> LLMResponse:
                raise RuntimeError("boom")

        prom = Promoter(llm=ExplodingLLM([]))
        out = await prom.promote(
            execution_result=ExecutionResult(
                success=True, summary="ok", steps_taken=1, tokens_used=10, wall_time_ms=1,
            ),
            history=[],
            prd=_make_prd(),
        )
        # quality 默认 0、should_stop False
        assert out.quality_score == 0.0
        assert out.should_stop is False


# ============================================================
# MAGIScheduler
# ============================================================

class _StubScrutinizer:
    def __init__(self) -> None:
        self.call_count = 0

    async def scrutinize(
        self,
        prd: PRDDocument,
        previous_round: MAGIRoundLog | None,
        spec_text: str,
        codebase_summary: str = "",
    ) -> ScrutinizeOutput:
        self.call_count += 1
        return ScrutinizeOutput(
            questions=[f"round-{self.call_count}-q"],
            next_action=f"do step {self.call_count}",
        )


class _StubExecutor:
    def __init__(self) -> None:
        self.call_count = 0
        self.received_actions: list[str] = []

    async def execute(
        self,
        next_action: str,
        session_id: str,
        task_id: str | None = None,
        max_steps: int = 20,
    ) -> ExecutionResult:
        self.call_count += 1
        self.received_actions.append(next_action)
        return ExecutionResult(
            success=True,
            summary=f"executed {next_action}",
            steps_taken=2,
            tokens_used=10,
            wall_time_ms=5,
        )


class _StubPromoter:
    """第 N 轮 should_stop=True，由初始化时指定。"""

    def __init__(self, stop_at_round: int) -> None:
        self.stop_at_round = stop_at_round
        self.call_count = 0

    async def promote(
        self,
        execution_result: ExecutionResult,
        history: list[MAGIRoundLog],
        prd: PRDDocument,
    ) -> PromoteOutput:
        self.call_count += 1
        should_stop = self.call_count >= self.stop_at_round
        return PromoteOutput(
            quality_score=80.0,
            should_stop=should_stop,
            stop_reason="task_complete" if should_stop else None,
            next_round_focus="next",
            next_round_interval_s=0.0,
        )


class TestMAGIScheduler:
    async def test_stops_after_two_rounds(self, scribe: Scribe) -> None:
        scrut = _StubScrutinizer()
        execr = _StubExecutor()
        prom = _StubPromoter(stop_at_round=2)
        sched = MAGIScheduler(
            scrutinizer=scrut,  # type: ignore[arg-type]
            executor=execr,     # type: ignore[arg-type]
            promoter=prom,      # type: ignore[arg-type]
            scribe=scribe,
        )
        history = await sched.run(
            prd=_make_prd(),
            session_id="sess-magi",
            max_rounds=10,
        )
        assert len(history) == 2
        assert scrut.call_count == 2
        assert execr.call_count == 2
        assert prom.call_count == 2
        # 每轮三脑结果都写到 log 里
        for h in history:
            assert h.scrutinize is not None
            assert h.execute_result is not None
            assert h.promote is not None
            assert h.ended_at is not None
        # 最后一轮 should_stop=True
        assert history[-1].promote.should_stop is True

    async def test_writes_magi_round_events_to_scribe(self, scribe: Scribe) -> None:
        sched = MAGIScheduler(
            scrutinizer=_StubScrutinizer(),   # type: ignore[arg-type]
            executor=_StubExecutor(),         # type: ignore[arg-type]
            promoter=_StubPromoter(stop_at_round=1),  # type: ignore[arg-type]
            scribe=scribe,
        )
        await sched.run(
            prd=_make_prd(),
            session_id="sess-events",
            max_rounds=5,
        )
        raws = await scribe.recent(n=50, session_id="sess-events")
        types = [r.event_type for r in raws]
        assert "magi_round_start" in types
        assert "magi_round_end" in types

    async def test_max_rounds_hard_cap(self, scribe: Scribe) -> None:
        """should_stop 永远 False 时，必须被 max_rounds 兜底退出。"""
        sched = MAGIScheduler(
            scrutinizer=_StubScrutinizer(),   # type: ignore[arg-type]
            executor=_StubExecutor(),         # type: ignore[arg-type]
            promoter=_StubPromoter(stop_at_round=999),  # type: ignore[arg-type]
            scribe=scribe,
        )
        history = await sched.run(
            prd=_make_prd(),
            session_id="sess-cap",
            max_rounds=3,
        )
        assert len(history) == 3

    async def test_side_git_handler_called_per_round(self, scribe: Scribe) -> None:
        snapshot_calls: list[dict[str, Any]] = []

        async def fake_snapshot(args: dict[str, Any]) -> ToolResult:
            snapshot_calls.append(args)
            return ToolResult(
                status=ToolStatus.SUCCESS,
                content="snap ok",
                elapsed_ms=1,
            )

        sched = MAGIScheduler(
            scrutinizer=_StubScrutinizer(),   # type: ignore[arg-type]
            executor=_StubExecutor(),         # type: ignore[arg-type]
            promoter=_StubPromoter(stop_at_round=2),  # type: ignore[arg-type]
            scribe=scribe,
            side_git_handler=fake_snapshot,
        )
        history = await sched.run(
            prd=_make_prd(),
            session_id="sess-snap",
            max_rounds=5,
        )
        assert len(snapshot_calls) == 2
        # snapshot_id 被记到每轮 log
        assert all(h.snapshot_id is not None for h in history)

    async def test_estimate_hours_returns_float(self) -> None:
        payload = json.dumps({"estimated_hours": 4.5, "rationale": "module refactor"})
        llm = FakeLLM([payload])
        sched = MAGIScheduler(
            scrutinizer=_StubScrutinizer(),   # type: ignore[arg-type]
            executor=_StubExecutor(),         # type: ignore[arg-type]
            promoter=_StubPromoter(stop_at_round=1),  # type: ignore[arg-type]
            scribe=None,  # type: ignore[arg-type]  # estimate_hours 不用 scribe
        )
        hours = await sched.estimate_hours("重构整个 Forge", llm=llm)
        assert hours == pytest.approx(4.5)

    async def test_estimate_hours_fallback_on_error(self) -> None:
        class ExplodingLLM(FakeLLM):
            async def chat(self, *a: Any, **kw: Any) -> LLMResponse:
                raise RuntimeError("boom")

        sched = MAGIScheduler(
            scrutinizer=_StubScrutinizer(),   # type: ignore[arg-type]
            executor=_StubExecutor(),         # type: ignore[arg-type]
            promoter=_StubPromoter(stop_at_round=1),  # type: ignore[arg-type]
            scribe=None,  # type: ignore[arg-type]
        )
        hours = await sched.estimate_hours("x", llm=ExplodingLLM([]))
        assert hours == pytest.approx(1.0)
