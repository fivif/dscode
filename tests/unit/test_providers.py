"""跨模型适配层（providers/）单元测试。

全部用 mock 替换底层 SDK 调用——绝不打真实 API。
"""
from __future__ import annotations

import json
import sys
import types
from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from dscode.core.types import (
    LLMProviderProtocol,
    Message,
    ToolCallSpec,
    ToolFunctionSpec,
)

# ============================================================
# 测试夹具：litellm fake module
# ============================================================


@pytest.fixture
def fake_litellm(monkeypatch: pytest.MonkeyPatch) -> Any:
    """伪造 litellm 模块，注入到 sys.modules。

    返回 mock，调用方可以设置 .acompletion 的行为。
    """
    fake = types.ModuleType("litellm")
    fake.acompletion = AsyncMock()  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "litellm", fake)
    return fake


def _mock_litellm_completion(
    content: str = "hello",
    reasoning: str | None = None,
    tool_calls: list[dict[str, Any]] | None = None,
    usage: dict[str, Any] | None = None,
    model: str = "openai/gpt-4o",
    finish_reason: str = "stop",
) -> dict[str, Any]:
    """造一个 litellm 风格的非流式响应（dict）。

    litellm 的 ModelResponse 是 pydantic-ish，但 dict 也能走通我们的 _to_dict。
    """
    msg: dict[str, Any] = {"role": "assistant", "content": content}
    if reasoning is not None:
        msg["reasoning_content"] = reasoning
    if tool_calls is not None:
        msg["tool_calls"] = tool_calls
    return {
        "id": "cmpl-1",
        "model": model,
        "choices": [
            {
                "index": 0,
                "message": msg,
                "finish_reason": finish_reason,
            }
        ],
        "usage": usage
        or {
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
        },
    }


def _async_gen(chunks: list[Any]) -> Any:
    """把同步 list 包成 async generator。"""

    async def _g() -> Any:
        for c in chunks:
            yield c

    return _g()


# ============================================================
# LiteLLMProvider
# ============================================================


class TestLiteLLMProvider:
    def test_construct_requires_litellm(self, monkeypatch: pytest.MonkeyPatch) -> None:
        """litellm 不可导入时，构造应该报清晰错误。"""
        # 清掉缓存里可能存在的 litellm
        monkeypatch.setitem(sys.modules, "litellm", None)
        from dscode.providers.litellm_adapter import LiteLLMProvider

        with pytest.raises(ImportError, match="litellm"):
            LiteLLMProvider(api_key="sk-test")

    def test_construct_succeeds_with_fake_litellm(self, fake_litellm: Any) -> None:
        from dscode.providers.litellm_adapter import LiteLLMProvider

        p = LiteLLMProvider(api_key="sk-test", api_base="http://localhost:11434")
        assert p.api_key == "sk-test"
        assert p.api_base == "http://localhost:11434"

    def test_messages_to_litellm_conversion(self, fake_litellm: Any) -> None:
        from dscode.providers.litellm_adapter import LiteLLMProvider

        p = LiteLLMProvider(api_key="sk-test")
        msgs = [
            Message(role="system", content="be brief"),
            Message(role="user", content="hi"),
            Message(
                role="assistant",
                content="hello",
                tool_calls=[
                    ToolCallSpec(
                        id="call_1",
                        function=ToolFunctionSpec(
                            name="do_x", arguments='{"a": 1}'
                        ),
                    )
                ],
            ),
            Message(role="tool", content="result", tool_call_id="call_1"),
        ]
        out = p._messages_to_litellm(msgs)
        assert out[0] == {"role": "system", "content": "be brief"}
        assert out[1] == {"role": "user", "content": "hi"}
        assert out[2]["tool_calls"][0]["function"]["name"] == "do_x"
        assert out[3]["tool_call_id"] == "call_1"

    def test_build_request_injects_thinking_and_effort(
        self, fake_litellm: Any
    ) -> None:
        from dscode.providers.litellm_adapter import LiteLLMProvider

        p = LiteLLMProvider(api_key="sk-test")
        req = p._build_request(
            messages=[Message(role="user", content="hi")],
            model="openai/gpt-4o",
            tools=None,
            stream=False,
            thinking=True,
            reasoning_effort="high",
        )
        assert req["extra_body"]["thinking"] == {"type": "enabled"}
        assert req["extra_body"]["reasoning_effort"] == "high"
        assert req["api_key"] == "sk-test"
        assert req["stream"] is False

    @pytest.mark.asyncio
    async def test_chat_parses_basic_response(self, fake_litellm: Any) -> None:
        from dscode.providers.litellm_adapter import LiteLLMProvider

        fake_litellm.acompletion.return_value = _mock_litellm_completion(
            content="answer",
            reasoning="think",
            usage={
                "prompt_tokens": 1000,
                "completion_tokens": 200,
                "total_tokens": 1200,
                "cache_read_input_tokens": 800,
                "cache_creation_input_tokens": 200,
            },
            model="openai/gpt-4o",
        )
        p = LiteLLMProvider(api_key="sk-test")
        resp = await p.chat(
            messages=[Message(role="user", content="q")],
            model="openai/gpt-4o",
        )
        assert resp.content == "answer"
        assert resp.reasoning_content == "think"
        assert resp.usage.prompt_cache_hit_tokens == 800
        assert resp.usage.prompt_cache_miss_tokens == 200
        assert resp.cache_hit_rate == pytest.approx(0.8)
        assert resp.finish_reason == "stop"
        assert resp.model == "openai/gpt-4o"

    @pytest.mark.asyncio
    async def test_chat_parses_tool_calls(self, fake_litellm: Any) -> None:
        from dscode.providers.litellm_adapter import LiteLLMProvider

        fake_litellm.acompletion.return_value = _mock_litellm_completion(
            content="",
            tool_calls=[
                {
                    "id": "call_42",
                    "type": "function",
                    "function": {
                        "name": "do_read",
                        "arguments": '{"path": "x.py"}',
                    },
                }
            ],
            finish_reason="tool_calls",
        )
        p = LiteLLMProvider(api_key="sk-test")
        resp = await p.chat(
            messages=[Message(role="user", content="read it")],
            model="openai/gpt-4o",
        )
        assert resp.tool_calls is not None
        assert resp.tool_calls[0].id == "call_42"
        assert resp.tool_calls[0].function.name == "do_read"
        assert resp.finish_reason == "tool_calls"

    @pytest.mark.asyncio
    async def test_chat_wraps_litellm_errors_as_runtime_error(
        self, fake_litellm: Any
    ) -> None:
        from dscode.providers.litellm_adapter import LiteLLMProvider

        fake_litellm.acompletion.side_effect = RuntimeError("rate limited")
        p = LiteLLMProvider(api_key="sk-test")
        with pytest.raises(RuntimeError, match=r"litellm\.acompletion failed"):
            await p.chat(
                messages=[Message(role="user", content="q")],
                model="openai/gpt-4o",
            )

    @pytest.mark.asyncio
    async def test_chat_normalizes_anthropic_style_finish_reason(
        self, fake_litellm: Any
    ) -> None:
        """litellm 透传 anthropic 时 finish_reason 可能是 end_turn / max_tokens。"""
        from dscode.providers.litellm_adapter import LiteLLMProvider

        fake_litellm.acompletion.return_value = _mock_litellm_completion(
            content="ok",
            finish_reason="end_turn",
        )
        p = LiteLLMProvider(api_key="sk-test")
        resp = await p.chat(
            messages=[Message(role="user", content="q")],
            model="anthropic/claude-sonnet-4-5",
        )
        assert resp.finish_reason == "stop"

        fake_litellm.acompletion.return_value = _mock_litellm_completion(
            content="ok",
            finish_reason="max_tokens",
        )
        resp2 = await p.chat(
            messages=[Message(role="user", content="q")],
            model="anthropic/claude-sonnet-4-5",
        )
        assert resp2.finish_reason == "length"

    @pytest.mark.asyncio
    async def test_chat_stream_yields_deltas(self, fake_litellm: Any) -> None:
        from dscode.providers.litellm_adapter import LiteLLMProvider

        chunks = [
            {
                "model": "openai/gpt-4o",
                "choices": [
                    {
                        "index": 0,
                        "delta": {"content": "hel"},
                        "finish_reason": None,
                    }
                ],
            },
            {
                "model": "openai/gpt-4o",
                "choices": [
                    {
                        "index": 0,
                        "delta": {"content": "lo"},
                        "finish_reason": None,
                    }
                ],
            },
            {
                "model": "openai/gpt-4o",
                "choices": [
                    {
                        "index": 0,
                        "delta": {},
                        "finish_reason": "stop",
                    }
                ],
                "usage": {
                    "prompt_tokens": 5,
                    "completion_tokens": 2,
                    "total_tokens": 7,
                },
            },
        ]
        fake_litellm.acompletion.return_value = _async_gen(chunks)
        p = LiteLLMProvider(api_key="sk-test")
        deltas = []
        async for d in p.chat_stream(
            messages=[Message(role="user", content="hi")],
            model="openai/gpt-4o",
        ):
            deltas.append(d)
        assert len(deltas) == 3
        assert deltas[0].content == "hel"
        assert deltas[1].content == "lo"
        assert deltas[2].finish_reason == "stop"
        assert deltas[2].usage.total_tokens == 7


# ============================================================
# AnthropicCompatProvider
# ============================================================


class TestAnthropicCompatProvider:
    def test_messages_to_anthropic_extracts_system(self) -> None:
        from dscode.providers.anthropic_compat import AnthropicCompatProvider

        msgs = [
            Message(role="system", content="sys-1"),
            Message(role="system", content="sys-2"),
            Message(role="user", content="hi"),
        ]
        system, out = AnthropicCompatProvider.messages_to_anthropic(msgs)
        assert system == "sys-1\n\nsys-2"
        assert len(out) == 1
        assert out[0]["role"] == "user"
        assert out[0]["content"][0]["text"] == "hi"

    def test_messages_to_anthropic_converts_tool_calls_to_blocks(self) -> None:
        from dscode.providers.anthropic_compat import AnthropicCompatProvider

        msgs = [
            Message(role="user", content="please read"),
            Message(
                role="assistant",
                content="ok",
                tool_calls=[
                    ToolCallSpec(
                        id="tu_1",
                        function=ToolFunctionSpec(
                            name="do_read", arguments='{"path": "a.py"}'
                        ),
                    )
                ],
            ),
            Message(role="tool", content="file content", tool_call_id="tu_1"),
        ]
        _, out = AnthropicCompatProvider.messages_to_anthropic(msgs)
        # assistant 消息：content 含 text + tool_use 两个块
        assistant_msg = out[1]
        assert assistant_msg["role"] == "assistant"
        types_present = [b["type"] for b in assistant_msg["content"]]
        assert "text" in types_present
        assert "tool_use" in types_present
        tool_use_block = next(
            b for b in assistant_msg["content"] if b["type"] == "tool_use"
        )
        assert tool_use_block["id"] == "tu_1"
        assert tool_use_block["name"] == "do_read"
        assert tool_use_block["input"] == {"path": "a.py"}
        # tool 角色 → user / tool_result
        tool_msg = out[2]
        assert tool_msg["role"] == "user"
        assert tool_msg["content"][0]["type"] == "tool_result"
        assert tool_msg["content"][0]["tool_use_id"] == "tu_1"

    def test_tools_to_anthropic_conversion(self) -> None:
        from dscode.providers.anthropic_compat import AnthropicCompatProvider

        openai_tools = [
            {
                "type": "function",
                "function": {
                    "name": "do_x",
                    "description": "test",
                    "parameters": {"type": "object", "properties": {}},
                },
            }
        ]
        out = AnthropicCompatProvider.tools_to_anthropic(openai_tools)
        assert out is not None
        assert out[0]["name"] == "do_x"
        assert out[0]["description"] == "test"
        assert out[0]["input_schema"] == {"type": "object", "properties": {}}

    def test_anthropic_response_to_openai_flattens_content_blocks(self) -> None:
        from dscode.providers.anthropic_compat import AnthropicCompatProvider

        raw = {
            "id": "msg_1",
            "model": "claude-sonnet-4-5",
            "content": [
                {"type": "thinking", "thinking": "ponder..."},
                {"type": "text", "text": "the answer is "},
                {"type": "text", "text": "42"},
                {
                    "type": "tool_use",
                    "id": "tu_9",
                    "name": "do_calc",
                    "input": {"x": 1, "y": 2},
                },
            ],
            "stop_reason": "tool_use",
            "usage": {
                "input_tokens": 100,
                "output_tokens": 50,
                "cache_read_input_tokens": 80,
                "cache_creation_input_tokens": 20,
            },
        }
        resp = AnthropicCompatProvider.anthropic_response_to_openai(
            raw, model="anthropic/claude-sonnet-4-5"
        )
        assert resp.content == "the answer is 42"
        assert resp.reasoning_content == "ponder..."
        assert resp.tool_calls is not None
        assert resp.tool_calls[0].id == "tu_9"
        assert resp.tool_calls[0].function.name == "do_calc"
        assert json.loads(resp.tool_calls[0].function.arguments) == {
            "x": 1,
            "y": 2,
        }
        assert resp.finish_reason == "tool_calls"
        assert resp.usage.prompt_cache_hit_tokens == 80
        assert resp.usage.prompt_cache_miss_tokens == 20
        assert resp.cache_hit_rate == pytest.approx(0.8)

    @pytest.mark.asyncio
    async def test_chat_uses_sdk_when_available(self) -> None:
        """构造 provider 时若 anthropic SDK 已加载，应走 SDK 路径。"""
        from dscode.providers.anthropic_compat import AnthropicCompatProvider

        # 造一个伪 anthropic SDK 模块（如果用户系统真装了 anthropic，会被这个 patch 覆盖）
        fake_response = {
            "id": "msg_1",
            "model": "claude-sonnet-4-5",
            "content": [{"type": "text", "text": "from sdk"}],
            "stop_reason": "end_turn",
            "usage": {"input_tokens": 5, "output_tokens": 3},
        }
        fake_anthropic = MagicMock()
        fake_messages = MagicMock()
        fake_messages.create = AsyncMock(return_value=fake_response)
        fake_client = MagicMock()
        fake_client.messages = fake_messages
        fake_anthropic.AsyncAnthropic = MagicMock(return_value=fake_client)

        with patch(
            "dscode.providers.anthropic_compat._import_anthropic",
            return_value=fake_anthropic,
        ):
            p = AnthropicCompatProvider(api_key="sk-ant-test")
            resp = await p.chat(
                messages=[
                    Message(role="system", content="sys"),
                    Message(role="user", content="hi"),
                ],
                model="anthropic/claude-sonnet-4-5",
                thinking=True,
            )
        assert resp.content == "from sdk"
        assert resp.finish_reason == "stop"
        # 验证 model 前缀被剥离 + thinking 注入
        call_kwargs = fake_messages.create.call_args.kwargs
        assert call_kwargs["model"] == "claude-sonnet-4-5"
        assert call_kwargs["system"] == "sys"
        assert call_kwargs["thinking"]["type"] == "enabled"

    @pytest.mark.asyncio
    async def test_chat_falls_back_to_litellm_when_sdk_missing(
        self, fake_litellm: Any
    ) -> None:
        from dscode.providers.anthropic_compat import AnthropicCompatProvider

        fake_litellm.acompletion.return_value = _mock_litellm_completion(
            content="from litellm fallback",
            model="anthropic/claude-sonnet-4-5",
        )
        with patch(
            "dscode.providers.anthropic_compat._import_anthropic",
            return_value=None,
        ):
            p = AnthropicCompatProvider(api_key="sk-ant-test")
            assert p._client is None
            assert p._fallback is not None
            resp = await p.chat(
                messages=[Message(role="user", content="hi")],
                model="claude-sonnet-4-5",
            )
        assert resp.content == "from litellm fallback"
        # 验证 fallback 传给 litellm 的 model 加上了 anthropic/ 前缀
        call_kwargs = fake_litellm.acompletion.call_args.kwargs
        assert call_kwargs["model"] == "anthropic/claude-sonnet-4-5"

    @pytest.mark.asyncio
    async def test_chat_wraps_sdk_errors_as_runtime_error(self) -> None:
        from dscode.providers.anthropic_compat import AnthropicCompatProvider

        fake_anthropic = MagicMock()
        fake_messages = MagicMock()
        fake_messages.create = AsyncMock(side_effect=RuntimeError("boom"))
        fake_client = MagicMock()
        fake_client.messages = fake_messages
        fake_anthropic.AsyncAnthropic = MagicMock(return_value=fake_client)

        with patch(
            "dscode.providers.anthropic_compat._import_anthropic",
            return_value=fake_anthropic,
        ):
            p = AnthropicCompatProvider(api_key="sk-test")
            with pytest.raises(
                RuntimeError, match=r"anthropic\.messages\.create failed"
            ):
                await p.chat(
                    messages=[Message(role="user", content="hi")],
                    model="claude-sonnet-4-5",
                )

    @pytest.mark.asyncio
    async def test_chat_stream_parses_event_stream(self) -> None:
        from dscode.providers.anthropic_compat import AnthropicCompatProvider

        events = [
            {
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": "hel"},
            },
            {
                "type": "content_block_delta",
                "index": 0,
                "delta": {"type": "text_delta", "text": "lo"},
            },
            {
                "type": "message_delta",
                "delta": {"stop_reason": "end_turn"},
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 5,
                    "cache_read_input_tokens": 8,
                    "cache_creation_input_tokens": 2,
                },
            },
            {"type": "message_stop"},
        ]

        fake_anthropic = MagicMock()
        fake_messages = MagicMock()
        fake_messages.create = AsyncMock(return_value=_async_gen(events))
        fake_client = MagicMock()
        fake_client.messages = fake_messages
        fake_anthropic.AsyncAnthropic = MagicMock(return_value=fake_client)

        with patch(
            "dscode.providers.anthropic_compat._import_anthropic",
            return_value=fake_anthropic,
        ):
            p = AnthropicCompatProvider(api_key="sk-test")
            chunks = []
            async for d in p.chat_stream(
                messages=[Message(role="user", content="hi")],
                model="anthropic/claude-sonnet-4-5",
            ):
                chunks.append(d)
        # 两个 text delta + 一个 usage chunk
        text_chunks = [c for c in chunks if c.content]
        assert "".join(c.content for c in text_chunks) == "hello"
        usage_chunks = [c for c in chunks if c.usage.total_tokens > 0]
        assert usage_chunks
        assert usage_chunks[-1].usage.prompt_cache_hit_tokens == 8


# ============================================================
# make_provider 路由
# ============================================================


class TestMakeProvider:
    def test_routes_deepseek_to_native_client(self) -> None:
        from dscode.deepseek.client import DeepSeekClient
        from dscode.providers import make_provider

        p = make_provider("deepseek-v4-pro", api_key="sk-test")
        assert isinstance(p, DeepSeekClient)
        # 别名形式
        p2 = make_provider("deepseek/deepseek-chat", api_key="sk-test")
        assert isinstance(p2, DeepSeekClient)

    def test_routes_anthropic_prefix(self) -> None:
        from dscode.providers import AnthropicCompatProvider, make_provider

        # 不管 SDK 是否安装，都应返回 AnthropicCompatProvider 实例
        with patch(
            "dscode.providers.anthropic_compat._import_anthropic",
            return_value=None,
        ):
            # 同时也要让 litellm fallback 不爆炸：注入 fake
            fake = types.ModuleType("litellm")
            fake.acompletion = AsyncMock()  # type: ignore[attr-defined]
            with patch.dict(sys.modules, {"litellm": fake}):
                p = make_provider("anthropic/claude-sonnet-4-5", api_key="sk-test")
                assert isinstance(p, AnthropicCompatProvider)
                p2 = make_provider("claude-opus-4-5", api_key="sk-test")
                assert isinstance(p2, AnthropicCompatProvider)

    def test_routes_other_to_litellm(self, fake_litellm: Any) -> None:
        from dscode.providers import LiteLLMProvider, make_provider

        for model in (
            "openai/gpt-4o",
            "gemini/gemini-2.5-pro",
            "ollama/qwen3-32b",
            "groq/llama-3.3-70b-versatile",
        ):
            p = make_provider(model, api_key="sk-test")
            assert isinstance(p, LiteLLMProvider), f"expected LiteLLM for {model}"

    def test_provider_implements_protocol(self, fake_litellm: Any) -> None:
        """所有 provider 都应满足 runtime_checkable LLMProviderProtocol。"""
        from dscode.providers import make_provider

        # LiteLLM 路径
        p_litellm = make_provider("openai/gpt-4o", api_key="sk-test")
        assert isinstance(p_litellm, LLMProviderProtocol)

        # DeepSeek 路径
        p_ds = make_provider("deepseek-v4-flash", api_key="sk-test")
        assert isinstance(p_ds, LLMProviderProtocol)

        # Anthropic 路径（SDK 缺失也走 protocol）
        with patch(
            "dscode.providers.anthropic_compat._import_anthropic",
            return_value=None,
        ):
            p_an = make_provider("claude-sonnet-4-5", api_key="sk-test")
            assert isinstance(p_an, LLMProviderProtocol)
