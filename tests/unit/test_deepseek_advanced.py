"""DeepSeek 高级特性测试（A4 Phase 3）。

覆盖：
- strict_tools.validate_tools_schema / chat_with_strict_tools
- thinking.chat_with_thinking / append_with_reasoning（reasoning_content 不丢失）
- fim.fim_complete
- tools.fim_tool.make_fim_tool

全程 mock，不调真实 DeepSeek API。
"""
from __future__ import annotations

import json
from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from dscode.core.types import (
    LLMResponse,
    Message,
    ToolCallSpec,
    ToolFunctionSpec,
    ToolStatus,
    Usage,
)
from dscode.deepseek import (
    BETA_BASE_URL,
    DEFAULT_BASE_URL,
    DeepSeekClient,
    append_with_reasoning,
    chat_with_strict_tools,
    chat_with_thinking,
    fim_complete,
    validate_tools_schema,
)
from dscode.deepseek.strict_tools import (
    _strict_base_url,
    _validate_tool_call_args,
)
from dscode.tools.fim_tool import make_fim_tool

# ============================================================
# Helpers
# ============================================================


def _ok_tool_schema() -> list[dict[str, Any]]:
    return [
        {
            "type": "function",
            "function": {
                "name": "do_grep",
                "description": "search",
                "parameters": {
                    "type": "object",
                    "additionalProperties": False,
                    "properties": {
                        "pattern": {"type": "string"},
                        "path": {"type": "string"},
                    },
                    "required": ["pattern", "path"],
                },
            },
        }
    ]


def _mock_completion(
    content: str = "ok",
    reasoning_content: str | None = None,
    tool_calls: list[Any] | None = None,
    finish_reason: str = "stop",
) -> Any:
    msg = MagicMock()
    msg.content = content
    msg.tool_calls = tool_calls
    extras: dict[str, Any] = {}
    if reasoning_content is not None:
        extras["reasoning_content"] = reasoning_content
    msg.model_extra = extras
    msg.reasoning_content = reasoning_content

    choice = MagicMock()
    choice.message = msg
    choice.finish_reason = finish_reason

    completion = MagicMock()
    completion.choices = [choice]
    completion.model = "deepseek-v4-flash"

    usage_obj = MagicMock()
    usage_obj.model_dump = MagicMock(return_value={
        "prompt_tokens": 10,
        "completion_tokens": 5,
        "total_tokens": 15,
        "prompt_cache_hit_tokens": 8,
        "prompt_cache_miss_tokens": 2,
    })
    completion.usage = usage_obj
    return completion


def _mock_tool_call(name: str, arguments: str) -> Any:
    tc = MagicMock()
    tc.model_dump = MagicMock(return_value={
        "id": "call_test_1",
        "type": "function",
        "function": {"name": name, "arguments": arguments},
    })
    return tc


# ============================================================
# strict_tools.validate_tools_schema
# ============================================================


class TestValidateToolsSchema:
    def test_ok_schema_returns_empty(self) -> None:
        assert validate_tools_schema(_ok_tool_schema()) == []

    def test_missing_additional_properties_false(self) -> None:
        schema = _ok_tool_schema()
        del schema[0]["function"]["parameters"]["additionalProperties"]
        errs = validate_tools_schema(schema)
        assert any("additionalProperties" in e for e in errs)

    def test_property_not_in_required(self) -> None:
        schema = _ok_tool_schema()
        # 移除 required 中的 path，property 还在
        schema[0]["function"]["parameters"]["required"] = ["pattern"]
        errs = validate_tools_schema(schema)
        assert any("required" in e and "path" in e for e in errs)

    def test_oneof_is_rejected(self) -> None:
        schema = _ok_tool_schema()
        schema[0]["function"]["parameters"]["oneOf"] = [{"type": "object"}]
        errs = validate_tools_schema(schema)
        assert any("oneOf" in e for e in errs)

    def test_invalid_function_type(self) -> None:
        schema = [{"type": "not_function", "function": {"name": "x", "parameters": {}}}]
        errs = validate_tools_schema(schema)
        assert any("function" in e for e in errs)

    def test_missing_function_dict(self) -> None:
        schema = [{"type": "function", "function": None}]
        errs = validate_tools_schema(schema)
        assert len(errs) > 0

    def test_required_references_unknown_property(self) -> None:
        schema = _ok_tool_schema()
        schema[0]["function"]["parameters"]["required"] = ["pattern", "path", "ghost"]
        errs = validate_tools_schema(schema)
        assert any("ghost" in e for e in errs)

    def test_property_with_anyof(self) -> None:
        schema = _ok_tool_schema()
        schema[0]["function"]["parameters"]["properties"]["pattern"]["anyOf"] = [
            {"type": "string"}
        ]
        errs = validate_tools_schema(schema)
        assert any("anyOf" in e for e in errs)


# ============================================================
# strict_tools._strict_base_url
# ============================================================


class TestStrictBaseUrl:
    def test_default_becomes_beta_with_strict(self) -> None:
        url = _strict_base_url(DEFAULT_BASE_URL)
        assert "/beta" in url
        assert "strict=true" in url

    def test_beta_already_appends_strict(self) -> None:
        url = _strict_base_url(BETA_BASE_URL)
        assert url.endswith("strict=true")

    def test_already_strict_returns_same(self) -> None:
        existing = f"{BETA_BASE_URL}?strict=true"
        assert _strict_base_url(existing) == existing


# ============================================================
# strict_tools.chat_with_strict_tools
# ============================================================


class TestChatWithStrictTools:
    @pytest.mark.asyncio
    async def test_schema_precheck_fails(self) -> None:
        client = DeepSeekClient(api_key="sk-test")
        bad_schema = [
            {
                "type": "function",
                "function": {
                    "name": "x",
                    "parameters": {
                        "type": "object",
                        # 缺 additionalProperties: false
                        "properties": {"a": {"type": "string"}},
                        "required": ["a"],
                    },
                },
            }
        ]
        with pytest.raises(ValueError, match="预检失败"):
            await chat_with_strict_tools(
                client=client,
                messages=[Message(role="user", content="hi")],
                tools=bad_schema,
                model="deepseek-v4-flash",
            )

    @pytest.mark.asyncio
    async def test_strict_call_passes_tools_and_uses_beta(self) -> None:
        client = DeepSeekClient(api_key="sk-test")
        tools = _ok_tool_schema()
        tc = _mock_tool_call("do_grep", json.dumps({"pattern": "foo", "path": "."}))
        fake = _mock_completion(content="", tool_calls=[tc], finish_reason="tool_calls")

        captured_urls: list[str] = []
        original_init = DeepSeekClient.__init__

        def spy_init(self: DeepSeekClient, *args: Any, **kw: Any) -> None:
            original_init(self, *args, **kw)
            captured_urls.append(self.base_url)
            # 给临时 client 替换底层 SDK 调用
            self._client.chat.completions.create = AsyncMock(return_value=fake)  # type: ignore[attr-defined]
            self._client.close = AsyncMock()  # type: ignore[attr-defined]

        with patch.object(DeepSeekClient, "__init__", spy_init):
            resp = await chat_with_strict_tools(
                client=client,
                messages=[Message(role="user", content="hi")],
                tools=tools,
                model="deepseek-v4-flash",
            )

        # 临时 client 必须用 strict 端点
        strict_urls = [u for u in captured_urls if "strict=true" in u]
        assert strict_urls, f"未观察到 strict URL，所有 URL: {captured_urls}"
        assert resp.tool_calls is not None and len(resp.tool_calls) == 1
        args = json.loads(resp.tool_calls[0].function.arguments)
        assert args == {"pattern": "foo", "path": "."}

    @pytest.mark.asyncio
    async def test_runtime_validation_catches_missing_required(self) -> None:
        client = DeepSeekClient(api_key="sk-test")
        tools = _ok_tool_schema()
        # 模型返回缺 'path' 的 args，应被本地 runtime 校验抓住
        tc = _mock_tool_call("do_grep", json.dumps({"pattern": "x"}))
        fake = _mock_completion(content="", tool_calls=[tc], finish_reason="tool_calls")

        original_init = DeepSeekClient.__init__

        def spy_init(self: DeepSeekClient, *args: Any, **kw: Any) -> None:
            original_init(self, *args, **kw)
            self._client.chat.completions.create = AsyncMock(return_value=fake)  # type: ignore[attr-defined]
            self._client.close = AsyncMock()  # type: ignore[attr-defined]

        with patch.object(DeepSeekClient, "__init__", spy_init):
            with pytest.raises(ValueError, match="path"):
                await chat_with_strict_tools(
                    client=client,
                    messages=[Message(role="user", content="hi")],
                    tools=tools,
                    model="deepseek-v4-flash",
                )

    def test_validate_tool_call_args_bad_json(self) -> None:
        tools = _ok_tool_schema()
        tc = ToolCallSpec(
            id="t1",
            function=ToolFunctionSpec(name="do_grep", arguments="{not json"),
        )
        errs = _validate_tool_call_args([tc], tools)
        assert any("非合法 JSON" in e for e in errs)

    def test_validate_tool_call_args_non_object(self) -> None:
        tools = _ok_tool_schema()
        tc = ToolCallSpec(
            id="t1",
            function=ToolFunctionSpec(name="do_grep", arguments="[]"),
        )
        errs = _validate_tool_call_args([tc], tools)
        assert any("object" in e for e in errs)


# ============================================================
# thinking.chat_with_thinking
# ============================================================


class TestChatWithThinking:
    @pytest.mark.asyncio
    async def test_injects_thinking_extra_body(self) -> None:
        client = DeepSeekClient(api_key="sk-test")
        captured: dict[str, Any] = {}

        async def fake_chat(**kwargs: Any) -> LLMResponse:
            captured.update(kwargs)
            return LLMResponse(content="ok", reasoning_content="trail", usage=Usage())

        with patch.object(client, "chat", AsyncMock(side_effect=fake_chat)):
            resp = await chat_with_thinking(
                client=client,
                messages=[Message(role="user", content="hi")],
                model="deepseek-v4-pro",
                reasoning_effort="high",
            )
        assert captured["thinking"] is True
        assert captured["reasoning_effort"] == "high"
        assert captured["extra_body"]["thinking"] == {"type": "enabled"}
        assert resp.reasoning_content == "trail"

    @pytest.mark.asyncio
    async def test_thinking_passes_tools_through(self) -> None:
        client = DeepSeekClient(api_key="sk-test")
        tools = _ok_tool_schema()
        captured: dict[str, Any] = {}

        async def fake_chat(**kwargs: Any) -> LLMResponse:
            captured.update(kwargs)
            return LLMResponse(content="", reasoning_content="r", usage=Usage())

        with patch.object(client, "chat", AsyncMock(side_effect=fake_chat)):
            await chat_with_thinking(
                client=client,
                messages=[Message(role="user", content="hi")],
                tools=tools,
            )
        assert captured["tools"] == tools


# ============================================================
# thinking.append_with_reasoning
# ============================================================


class TestAppendWithReasoning:
    def test_append_preserves_reasoning_content(self) -> None:
        messages: list[Any] = [Message(role="user", content="q")]
        resp = LLMResponse(
            content="answer",
            reasoning_content="step-by-step thinking",
            usage=Usage(),
        )
        out = append_with_reasoning(messages, resp)
        assert len(out) == 2
        assert out[-1]["role"] == "assistant"
        assert out[-1]["content"] == "answer"
        # 关键：reasoning_content 必须存在，否则下一轮 DeepSeek 400
        assert out[-1]["reasoning_content"] == "step-by-step thinking"

    def test_append_preserves_tool_calls(self) -> None:
        resp = LLMResponse(
            content="",
            reasoning_content="r",
            tool_calls=[
                ToolCallSpec(
                    id="c1",
                    function=ToolFunctionSpec(
                        name="do_grep", arguments='{"pattern":"x"}'
                    ),
                )
            ],
            usage=Usage(),
        )
        out = append_with_reasoning([Message(role="user", content="hi")], resp)
        tc = out[-1]["tool_calls"][0]
        assert tc["function"]["name"] == "do_grep"
        assert tc["function"]["arguments"] == '{"pattern":"x"}'

    def test_append_does_not_mutate_input(self) -> None:
        original = [Message(role="user", content="q")]
        resp = LLMResponse(content="x", reasoning_content="r", usage=Usage())
        out = append_with_reasoning(original, resp)
        assert len(original) == 1
        assert len(out) == 2

    def test_append_accepts_dict_messages(self) -> None:
        messages: list[Any] = [{"role": "user", "content": "q"}]
        resp = LLMResponse(content="x", reasoning_content="trail", usage=Usage())
        out = append_with_reasoning(messages, resp)
        assert out[0] == {"role": "user", "content": "q"}
        assert out[1]["reasoning_content"] == "trail"

    def test_append_when_no_reasoning_content(self) -> None:
        # 非 thinking 模式下 reasoning_content 为 None，不应注入该字段
        resp = LLMResponse(content="x", reasoning_content=None, usage=Usage())
        out = append_with_reasoning([Message(role="user", content="q")], resp)
        assert "reasoning_content" not in out[-1]

    def test_full_round_trip_messages_can_serialize_to_dict(self) -> None:
        # 完整模拟一个 thinking + tool 循环的一轮：
        # user → assistant(reasoning + tool_calls) → tool → assistant 都不能丢 reasoning_content
        resp1 = LLMResponse(
            content="",
            reasoning_content="round-1 thought",
            tool_calls=[
                ToolCallSpec(
                    id="c1",
                    function=ToolFunctionSpec(name="do_grep", arguments="{}"),
                )
            ],
            usage=Usage(),
        )
        m1 = append_with_reasoning([Message(role="user", content="q")], resp1)
        # tool 回包后再来第二轮
        m2: list[Any] = [
            *m1,
            {"role": "tool", "tool_call_id": "c1", "content": "result"},
        ]
        resp2 = LLMResponse(
            content="done", reasoning_content="round-2 thought", usage=Usage()
        )
        m3 = append_with_reasoning(m2, resp2)
        # 第一轮 assistant 的 reasoning_content 全程保留
        assert m3[1]["reasoning_content"] == "round-1 thought"
        # 第二轮 assistant 的 reasoning_content 也存在
        assert m3[-1]["reasoning_content"] == "round-2 thought"


# ============================================================
# fim.fim_complete
# ============================================================


class TestFimComplete:
    @pytest.mark.asyncio
    async def test_fim_returns_middle_text(self) -> None:
        client = DeepSeekClient(api_key="sk-test", base_url=BETA_BASE_URL)
        fake_choice = MagicMock()
        fake_choice.text = "    return a + b\n"
        fake_resp = MagicMock()
        fake_resp.choices = [fake_choice]
        with patch.object(
            client._client.completions, "create", new=AsyncMock(return_value=fake_resp)
        ) as mock_create:
            out = await fim_complete(
                client=client,
                prefix="def add(a, b):\n",
                suffix="\n\nprint(add(1, 2))",
                model="deepseek-v4-flash",
                max_tokens=100,
            )
        assert out == "    return a + b\n"
        kwargs = mock_create.call_args.kwargs
        assert kwargs["prompt"] == "def add(a, b):\n"
        assert kwargs["suffix"] == "\n\nprint(add(1, 2))"
        assert kwargs["model"] == "deepseek-v4-flash"
        assert kwargs["max_tokens"] == 100

    @pytest.mark.asyncio
    async def test_fim_auto_wraps_beta_when_default_url(self) -> None:
        client = DeepSeekClient(api_key="sk-test", base_url=DEFAULT_BASE_URL)
        fake_choice = MagicMock()
        fake_choice.text = "x"
        fake_resp = MagicMock()
        fake_resp.choices = [fake_choice]

        captured_urls: list[str] = []
        original_init = DeepSeekClient.__init__

        def spy_init(self: DeepSeekClient, *args: Any, **kw: Any) -> None:
            original_init(self, *args, **kw)
            captured_urls.append(self.base_url)
            self._client.completions.create = AsyncMock(return_value=fake_resp)  # type: ignore[attr-defined]
            self._client.close = AsyncMock()  # type: ignore[attr-defined]

        with patch.object(DeepSeekClient, "__init__", spy_init):
            out = await fim_complete(
                client=client,
                prefix="a",
                suffix="b",
            )
        assert out == "x"
        # 应该构造过 beta client
        assert any(u.endswith("/beta") for u in captured_urls)

    @pytest.mark.asyncio
    async def test_fim_empty_choices_returns_empty_string(self) -> None:
        client = DeepSeekClient(api_key="sk-test", base_url=BETA_BASE_URL)
        fake_resp = MagicMock()
        fake_resp.choices = []
        with patch.object(
            client._client.completions, "create", new=AsyncMock(return_value=fake_resp)
        ):
            out = await fim_complete(client=client, prefix="a", suffix="b")
        assert out == ""


# ============================================================
# tools.fim_tool.make_fim_tool
# ============================================================


class TestMakeFimTool:
    def test_spec_name_and_required_args(self) -> None:
        client = DeepSeekClient(api_key="sk-test")
        spec, _handler = make_fim_tool(client)
        assert spec.name == "do_fim_complete"
        assert spec.capability == "code_complete"
        required = spec.parameters["required"]
        assert "prefix" in required and "suffix" in required

    @pytest.mark.asyncio
    async def test_handler_success_path(self) -> None:
        client = DeepSeekClient(api_key="sk-test", base_url=BETA_BASE_URL)
        _spec, handler = make_fim_tool(client)
        with patch(
            "dscode.tools.fim_tool.fim_complete",
            new=AsyncMock(return_value="middle"),
        ):
            result = await handler({"prefix": "p", "suffix": "s"})
        assert result.status == ToolStatus.SUCCESS
        assert result.content == "middle"
        assert result.metadata["middle_chars"] == len("middle")

    @pytest.mark.asyncio
    async def test_handler_invalid_args(self) -> None:
        client = DeepSeekClient(api_key="sk-test")
        _spec, handler = make_fim_tool(client)
        result = await handler({"prefix": "ok"})  # 缺 suffix
        assert result.status == ToolStatus.ERROR
        assert result.error and "suffix" in result.error

    @pytest.mark.asyncio
    async def test_handler_catches_exception(self) -> None:
        client = DeepSeekClient(api_key="sk-test", base_url=BETA_BASE_URL)
        _spec, handler = make_fim_tool(client)
        with patch(
            "dscode.tools.fim_tool.fim_complete",
            new=AsyncMock(side_effect=RuntimeError("boom")),
        ):
            result = await handler({"prefix": "p", "suffix": "s"})
        assert result.status == ToolStatus.ERROR
        assert result.error and "boom" in result.error
