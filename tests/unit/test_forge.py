"""Forge 单元测试。

用 fake LLMProvider + fake ToolRegistry + 真实 Scribe（tmp_path）。
"""
from __future__ import annotations

from pathlib import Path
from typing import Any

import pytest

from dscode.core import (
    Forge,
    LLMResponse,
    Message,
    Scribe,
    StreamEventType,
    ToolCallSpec,
    ToolHandler,
    ToolResult,
    ToolSpec,
    ToolStatus,
    Usage,
)
from dscode.core.types import ToolFunctionSpec

# ============================================================
# Fakes
# ============================================================

class FakeLLM:
    """脚本化 LLM：按预设响应序列依次返回。"""

    def __init__(self, responses: list[LLMResponse]) -> None:
        self._responses = list(responses)
        self.received_messages: list[list[Message]] = []
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
        # 拷贝 messages 用于检查
        self.received_messages.append(list(messages))
        if not self._responses:
            return LLMResponse(content="done", finish_reason="stop", model=model)
        return self._responses.pop(0)

    async def chat_stream(self, *args: Any, **kwargs: Any) -> Any:  # pragma: no cover
        raise NotImplementedError


class FakeToolRegistry:
    """最简注册表。"""

    def __init__(self) -> None:
        self._specs: dict[str, ToolSpec] = {}
        self._handlers: dict[str, ToolHandler] = {}

    def register(self, spec: ToolSpec, handler: ToolHandler) -> None:
        self._specs[spec.name] = spec
        self._handlers[spec.name] = handler

    def list_specs(self) -> list[ToolSpec]:
        return list(self._specs.values())

    def get_handler(self, name: str) -> ToolHandler | None:
        return self._handlers.get(name)

    def to_openai_tools(self) -> list[dict[str, Any]]:
        return [
            {
                "type": "function",
                "function": {
                    "name": s.name,
                    "description": s.description,
                    "parameters": s.parameters,
                },
            }
            for s in self._specs.values()
        ]


async def _echo_handler(args: dict[str, Any]) -> ToolResult:
    text = str(args.get("text", ""))
    return ToolResult(status=ToolStatus.SUCCESS, content=f"echo: {text}", elapsed_ms=1)


def _make_echo_registry() -> FakeToolRegistry:
    reg = FakeToolRegistry()
    reg.register(
        ToolSpec(
            name="echo",
            description="echo back the input",
            parameters={
                "type": "object",
                "properties": {"text": {"type": "string"}},
                "required": ["text"],
            },
        ),
        _echo_handler,
    )
    return reg


# ============================================================
# Fixtures
# ============================================================

@pytest.fixture
async def scribe(tmp_path: Path) -> Scribe:
    s = Scribe(db_path=tmp_path / "state.db", mirror_dir=tmp_path / "raw")
    yield s
    s.close()


# ============================================================
# Tests
# ============================================================

async def test_forge_completes_one_round_with_tool(scribe: Scribe) -> None:
    """一轮 tool call → 一轮纯文本结束。"""
    # 准备 LLM 脚本
    tool_call = ToolCallSpec(
        id="call-1",
        function=ToolFunctionSpec(name="echo", arguments='{"text": "hi"}'),
    )
    responses = [
        LLMResponse(
            content="I will call echo.",
            tool_calls=[tool_call],
            finish_reason="tool_calls",
            usage=Usage(prompt_tokens=10, completion_tokens=5, total_tokens=15),
            model="fake",
        ),
        LLMResponse(
            content="All done.",
            finish_reason="stop",
            usage=Usage(prompt_tokens=20, completion_tokens=5, total_tokens=25),
            model="fake",
        ),
    ]
    llm = FakeLLM(responses)
    reg = _make_echo_registry()

    forge = Forge(llm=llm, scribe=scribe, tool_registry=reg, model="fake", max_steps=5)

    events = []
    async for ev in forge.execute(task="say hi", session_id="sess-x"):
        events.append(ev)

    types = [e.type for e in events]
    assert StreamEventType.TOOL_START in types
    assert StreamEventType.TOOL_RESULT in types
    assert StreamEventType.COMPLETE in types
    # USAGE 也会发出来
    assert StreamEventType.USAGE in types

    # COMPLETE 应该是最后一个
    assert events[-1].type == StreamEventType.COMPLETE
    assert events[-1].data["tool_call_count"] == 1
    assert "All done." in events[-1].data["summary"]


async def test_forge_writes_raw_events(scribe: Scribe) -> None:
    """工具调用前后应写入 RawEvent。"""
    tool_call = ToolCallSpec(
        id="call-2",
        function=ToolFunctionSpec(name="echo", arguments='{"text": "world"}'),
    )
    llm = FakeLLM(
        [
            LLMResponse(
                tool_calls=[tool_call],
                finish_reason="tool_calls",
                usage=Usage(prompt_tokens=5, completion_tokens=2),
                model="fake",
            ),
            LLMResponse(
                content="done.",
                finish_reason="stop",
                usage=Usage(prompt_tokens=8, completion_tokens=3),
                model="fake",
            ),
        ]
    )
    reg = _make_echo_registry()
    forge = Forge(llm=llm, scribe=scribe, tool_registry=reg, model="fake")

    sid = "sess-raw"
    collected = []
    async for ev in forge.execute(task="echo world", session_id=sid):
        collected.append(ev)

    raws = await scribe.recent(n=50, session_id=sid)
    event_types = [r.event_type for r in raws]
    assert "user_message" in event_types
    assert "tool_call" in event_types
    assert "tool_result" in event_types
    # 步数严格递增
    steps = [r.step_number for r in raws]
    assert steps == sorted(steps)


async def test_forge_handles_unknown_tool(scribe: Scribe) -> None:
    """未知工具名应产生错误结果，但循环继续。"""
    bad_call = ToolCallSpec(
        id="call-3",
        function=ToolFunctionSpec(name="nonexistent_tool", arguments="{}"),
    )
    llm = FakeLLM(
        [
            LLMResponse(
                tool_calls=[bad_call],
                finish_reason="tool_calls",
                model="fake",
            ),
            LLMResponse(content="recovered.", finish_reason="stop", model="fake"),
        ]
    )
    reg = _make_echo_registry()
    forge = Forge(llm=llm, scribe=scribe, tool_registry=reg, model="fake")

    events = []
    async for ev in forge.execute(task="x", session_id="sess-err"):
        events.append(ev)

    # 应该看到一个 status=error 的 TOOL_RESULT
    tool_results = [e for e in events if e.type == StreamEventType.TOOL_RESULT]
    assert len(tool_results) == 1
    assert tool_results[0].data["status"] == "error"
    # 但 COMPLETE 仍然产生
    assert events[-1].type == StreamEventType.COMPLETE


async def test_forge_completes_without_tool_calls(scribe: Scribe) -> None:
    """模型直接回纯文本时，应只 yield THOUGHT + USAGE + COMPLETE。"""
    llm = FakeLLM(
        [
            LLMResponse(
                content="hello there",
                finish_reason="stop",
                usage=Usage(prompt_tokens=3, completion_tokens=2),
                model="fake",
            )
        ]
    )
    reg = _make_echo_registry()
    forge = Forge(llm=llm, scribe=scribe, tool_registry=reg, model="fake")

    types = []
    async for ev in forge.execute(task="just say hi", session_id="sess-plain"):
        types.append(ev.type)

    assert StreamEventType.TOOL_START not in types
    assert StreamEventType.THOUGHT in types
    assert StreamEventType.COMPLETE in types


async def test_forge_extracts_fact_for_file_read(scribe: Scribe) -> None:
    """file_read 工具成功时，Forge 应尝试写一条 fact。"""
    reg = FakeToolRegistry()

    async def read_handler(args: dict[str, Any]) -> ToolResult:
        return ToolResult(
            status=ToolStatus.SUCCESS,
            content="file contents...",
            elapsed_ms=2,
        )

    reg.register(
        ToolSpec(
            name="file_read",
            description="read a file",
            parameters={
                "type": "object",
                "properties": {"path": {"type": "string"}},
                "required": ["path"],
            },
        ),
        read_handler,
    )

    tool_call = ToolCallSpec(
        id="rd-1",
        function=ToolFunctionSpec(name="file_read", arguments='{"path": "/tmp/foo.txt"}'),
    )
    llm = FakeLLM(
        [
            LLMResponse(tool_calls=[tool_call], finish_reason="tool_calls", model="fake"),
            LLMResponse(content="done.", finish_reason="stop", model="fake"),
        ]
    )

    forge = Forge(llm=llm, scribe=scribe, tool_registry=reg, model="fake")
    async for _ev in forge.execute(task="read foo", session_id="sess-fact"):
        pass

    # 搜索 fact 应能命中
    facts = await scribe.search_facts("/tmp/foo.txt")
    assert any("foo.txt" in f.subject for f in facts)
    # 必须带 provenance
    for f in facts:
        if "foo.txt" in f.subject:
            assert len(f.provenance_chain) >= 1
