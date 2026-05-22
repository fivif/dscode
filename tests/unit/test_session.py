"""ChatSession 单元测试。

用 AsyncMock 模拟 LLMProviderProtocol / ToolRegistry，验证事件流正确性。
"""
from __future__ import annotations

import json
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from dscode.core.types import (
    LLMResponse,
    Message,
    ToolCallSpec,
    ToolFunctionSpec,
    ToolResult,
    ToolStatus,
    Usage,
)
from dscode.tui.events import (
    SessionEventType,
    chat_chunk,
    chat_stream,
    system_msg,
)


# ============================================================
# Helpers
# ============================================================

def _make_tool_call(idx: str, name: str, arguments: str) -> ToolCallSpec:
    return ToolCallSpec(
        id=idx,
        function=ToolFunctionSpec(name=name, arguments=arguments),
    )


def _mock_llm_response(content: str = "", tool_calls=None, finish="stop"):
    return LLMResponse(
        content=content,
        tool_calls=tool_calls,
        finish_reason=finish,
        usage=Usage(prompt_tokens=10, completion_tokens=5, total_tokens=15),
        model="mock",
    )


async def _mock_stream(events: list[LLMResponse]):
    for ev in events:
        yield ev


# ============================================================
# Fixture: a ChatSession with all internals mocked
# ============================================================

@pytest.fixture
def session(tmp_path: Path) -> "ChatSession":
    """构造 ChatSession 并注入 mock 的内部组件。"""
    from dscode.tui.session import ChatSession

    sess = ChatSession(project_root=tmp_path, model="deepseek-v4-flash")

    # Mock all lazy resources
    sess._llm = AsyncMock()
    sess._scribe = MagicMock()
    sess._scribe.write_raw = AsyncMock()
    sess._tools = MagicMock()
    sess._tools.to_openai_tools.return_value = []
    sess._forge = MagicMock()
    sess._step_counter = 0

    return sess


# ============================================================
# Tests
# ============================================================

class TestChatSessionCommands:
    """命令处理测试。"""

    async def test_chat_session_help_command(self, session):
        """ /help 返回命令列表。"""
        events = [e async for e in session.send("/help")]
        assert len(events) >= 1
        assert events[0].type == SessionEventType.SYSTEM
        assert "help" in events[0].data["content"].lower()

    async def test_chat_session_plan_command(self, session):
        """ /plan 返回 phase_change。"""
        events = [e async for e in session.send("/plan 做一个登录页")]
        types = [e.type for e in events]
        assert SessionEventType.PHASE_CHANGE in types
        assert SessionEventType.SYSTEM in types

    async def test_chat_session_clear_command(self, session):
        """ /clear 清空 messages。"""
        session.messages = [Message(role="user", content="hello")]
        session._step_counter = 5
        events = [e async for e in session.send("/clear")]
        assert session.messages == []
        # send() 结束时 increment 了 step_counter → 结果为 1
        assert session._step_counter == 1
        assert any(e.type == SessionEventType.SYSTEM for e in events)

    async def test_chat_session_model_switch(self, session):
        """ /model gpt-4o 切换模型。"""
        events = [e async for e in session.send("/model gpt-4o")]
        assert session.model == "gpt-4o"
        status_events = [
            e for e in events if e.type == SessionEventType.STATUS_UPDATE
        ]
        assert len(status_events) >= 1
        assert status_events[0].data["model"] == "gpt-4o"

    async def test_chat_session_model_switch_no_arg(self, session):
        """ /model without arg -> usage hint. """
        events = [e async for e in session.send("/model")]
        assert len(events) >= 1
        assert "Usage" in events[0].data["content"]

    async def test_chat_session_quit_command(self, session):
        """ /quit returns goodbye. """
        events = [e async for e in session.send("/quit")]
        assert len(events) >= 1
        assert events[0].type == SessionEventType.SYSTEM
        assert "Goodbye" in events[0].data["content"]

    async def test_chat_session_unknown_command(self, session):
        """ Unknown command -> hint. """
        events = [e async for e in session.send("/unknown")]
        assert len(events) >= 1
        assert "Unknown command" in events[0].data["content"]


class TestChatSessionChatTurn:
    """对话轮次测试。"""

    async def test_chat_session_simple_turn(self, session):
        """ 用户发消息 → mock LLM 返回 → 收到 chat_chunk。"""
        # 模拟 chat_stream 返回一段文本
        async def fake_stream(**kwargs):
            yield LLMResponse(content="你好！", finish_reason="stop", model="mock")

        session._llm.chat_stream = fake_stream

        events = [e async for e in session.send("你好")]

        types = [e.type for e in events]
        assert SessionEventType.CHAT_STREAM in types
        assert SessionEventType.CHAT_CHUNK in types

        # chat_chunk 应包含完整内容
        chunk_events = [e for e in events if e.type == SessionEventType.CHAT_CHUNK]
        assert len(chunk_events) == 1
        assert chunk_events[0].data["content"] == "你好！"

    async def test_chat_session_streaming(self, session):
        """ chat_stream 正确拼接增量内容。"""
        async def fake_stream(**kwargs):
            yield LLMResponse(content="今天", model="mock")
            yield LLMResponse(content="天气", model="mock")
            yield LLMResponse(content="很好", finish_reason="stop", model="mock")

        session._llm.chat_stream = fake_stream

        events = [e async for e in session.send("今天天气怎么样")]

        stream_events = [
            e for e in events if e.type == SessionEventType.CHAT_STREAM
        ]
        assert len(stream_events) == 3
        assert stream_events[0].data["content"] == "今天"
        assert stream_events[1].data["content"] == "天气"
        assert stream_events[2].data["content"] == "很好"

        chunk_events = [e for e in events if e.type == SessionEventType.CHAT_CHUNK]
        assert len(chunk_events) == 1
        assert chunk_events[0].data["content"] == "今天天气很好"

    async def test_chat_session_tool_use(self, session):
        """ mock LLM 带 tool_calls → 工具被调用 → tool_start/tool_end 事件。"""
        # 注册一个 fake echo 工具
        async def echo_handler(args: dict):
            text = args.get("text", "")
            return ToolResult(
                status=ToolStatus.SUCCESS,
                content=f"echo: {text}",
                elapsed_ms=5,
            )

        # Mock tool registry: to_openai_tools + get_handler
        session._tools.to_openai_tools.return_value = [
            {
                "type": "function",
                "function": {
                    "name": "echo",
                    "description": "echo",
                    "parameters": {},
                },
            }
        ]
        session._tools.get_handler = MagicMock(return_value=echo_handler)

        # 两轮 LLM 响应
        call_count = 0

        async def fake_stream(**kwargs):
            nonlocal call_count
            call_count += 1
            if call_count == 1:
                # 第一轮：返回 tool_call
                yield LLMResponse(
                    content="我来调用 echo。",
                    tool_calls=[
                        _make_tool_call("call-1", "echo", '{"text": "hello"}')
                    ],
                    finish_reason="tool_calls",
                    model="mock",
                )
            else:
                # 第二轮：纯文本回复
                yield LLMResponse(
                    content="echo 执行完毕。",
                    finish_reason="stop",
                    model="mock",
                )

        session._llm.chat_stream = fake_stream

        events = [e async for e in session.send("帮我 echo hello")]

        types = [e.type for e in events]
        assert SessionEventType.TOOL_START in types
        assert SessionEventType.TOOL_END in types
        assert SessionEventType.CHAT_CHUNK in types

        # 验证 tool_start
        start_ev = [e for e in events if e.type == SessionEventType.TOOL_START][0]
        assert start_ev.data["tool_name"] == "echo"
        assert "hello" in start_ev.data["args"]

        # 验证 tool_end
        end_ev = [e for e in events if e.type == SessionEventType.TOOL_END][0]
        assert end_ev.data["tool_name"] == "echo"
        assert end_ev.data["status"] == "success"

        # 验证 tool message 已追加
        tool_msgs = [m for m in session.messages if m.role == "tool"]
        assert len(tool_msgs) == 1
        assert tool_msgs[0].name is None  # tool 消息不应有 name（OpenAI API 规范）

    async def test_chat_session_tool_use_streaming_accumulation(self, session):
        """ 流式 tool_calls delta 正确累积。"""
        async def echo_handler(args: dict):
            return ToolResult(status=ToolStatus.SUCCESS, content="ok")

        session._tools.to_openai_tools.return_value = []
        session._tools.get_handler = MagicMock(return_value=echo_handler)

        call_count = 0

        async def fake_stream(**kwargs):
            nonlocal call_count
            call_count += 1
            if call_count == 1:
                # 模拟流式 tool_call 增量
                yield LLMResponse(
                    content="",
                    tool_calls=[
                        ToolCallSpec(
                            id="tc-1",
                            function=ToolFunctionSpec(
                                name="echo", arguments=""
                            ),
                        )
                    ],
                    model="mock",
                )
                yield LLMResponse(
                    content="",
                    tool_calls=[
                        ToolCallSpec(
                            id="",
                            function=ToolFunctionSpec(
                                name="", arguments='{"text":'
                            ),
                        )
                    ],
                    model="mock",
                )
                yield LLMResponse(
                    content="",
                    tool_calls=[
                        ToolCallSpec(
                            id="",
                            function=ToolFunctionSpec(
                                name="", arguments='"world"}'
                            ),
                        )
                    ],
                    finish_reason="tool_calls",
                    model="mock",
                )
            else:
                yield LLMResponse(
                    content="done.", finish_reason="stop", model="mock"
                )

        session._llm.chat_stream = fake_stream

        events = [e async for e in session.send("echo world")]

        # 应该有 tool_start + tool_end
        types = [e.type for e in events]
        assert SessionEventType.TOOL_START in types
        assert SessionEventType.TOOL_END in types

    async def test_chat_session_llm_error(self, session):
        """ LLM error -> yield system_msg. """
        async def fake_stream(**kwargs):
            raise RuntimeError("API unavailable")
            yield  # unreachable

        session._llm.chat_stream = fake_stream

        events = [e async for e in session.send("hello")]

        assert len(events) >= 1
        assert events[0].type == SessionEventType.SYSTEM
        assert "Error" in events[0].data["content"]
        assert "API unavailable" in events[0].data["content"]

    async def test_chat_session_max_rounds_exhausted(self, session):
        """ 15 轮全是 tool_calls → yield [max rounds exhausted]。"""
        session._tools.to_openai_tools.return_value = []
        session._tools.get_handler = MagicMock(
            return_value=AsyncMock(
                return_value=ToolResult(status=ToolStatus.SUCCESS, content="ok")
            )
        )

        async def fake_stream(**kwargs):
            yield LLMResponse(
                tool_calls=[
                    _make_tool_call("c1", "noop", "{}"),
                ],
                finish_reason="tool_calls",
                model="mock",
            )

        session._llm.chat_stream = fake_stream

        events = [e async for e in session.send("loop")]

        system_events = [
            e for e in events if e.type == SessionEventType.SYSTEM
        ]
        assert any(
            "max rounds" in e.data["content"].lower() for e in system_events
        )

    async def test_chat_session_unknown_tool(self, session):
        """ 未知工具 → tool_end status=error。"""
        session._tools.to_openai_tools.return_value = []
        session._tools.get_handler = MagicMock(return_value=None)

        call_count = 0

        async def fake_stream(**kwargs):
            nonlocal call_count
            call_count += 1
            if call_count == 1:
                yield LLMResponse(
                    tool_calls=[
                        _make_tool_call("c1", "nonexistent", "{}"),
                    ],
                    finish_reason="tool_calls",
                    model="mock",
                )
            else:
                yield LLMResponse(
                    content="fallback", finish_reason="stop", model="mock"
                )

        session._llm.chat_stream = fake_stream

        events = [e async for e in session.send("test")]

        end_events = [e for e in events if e.type == SessionEventType.TOOL_END]
        assert len(end_events) >= 1
        assert end_events[0].data["status"] == "error"

    async def test_chat_session_writes_user_message_to_scribe(self, session):
        """ _chat_turn 写 user_message RawEvent。"""
        async def fake_stream(**kwargs):
            yield LLMResponse(content="ok", finish_reason="stop", model="mock")

        session._llm.chat_stream = fake_stream

        _events = [e async for e in session.send("scribe test")]

        # send() 结束时也会写一次；总共至少 2 次（_chat_turn + send 结尾）
        assert session._scribe.write_raw.call_count >= 1
        # 第一次调用应为 user_message
        first_call_args = session._scribe.write_raw.call_args_list[0][0]
        raw_event = first_call_args[0]
        assert raw_event.event_type == "user_message"
