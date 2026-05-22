"""DeepSeek 优化层单元测试。

不调用真实 API。client.py 用 mock 测请求构造。
"""
from __future__ import annotations

import json
from typing import Any
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

from dscode.core.types import Message, ToolCallSpec, ToolFunctionSpec, Usage
from dscode.deepseek import (
    AutoRouter,
    CacheStableAssembler,
    CacheTelemetry,
    DeepSeekClient,
    RouteDecision,
    force_json,
)
from dscode.deepseek.client import BETA_BASE_URL, DEFAULT_BASE_URL

# ============================================================
# CacheStableAssembler
# ============================================================


def _full_assembler() -> CacheStableAssembler:
    return CacheStableAssembler(
        system_prompt="SYS",
        spec_block="SPEC",
        tools_block="TOOLS",
        repo_summary="REPO",
        warm_memory="WARM",
        cold_memory="COLD",
        task_prd="PRD",
        round_history=[Message(role="user", content="round-1")],
        current_turn=[Message(role="user", content="now")],
    )


class TestCacheStableAssembler:
    def test_assemble_order_and_structure(self) -> None:
        asm = _full_assembler()
        msgs = asm.assemble()
        # 第一条 system 包含所有 7 块标记
        assert msgs[0].role == "system"
        sys_text = msgs[0].content or ""
        for marker in (
            "<|system_prompt|>",
            "<|spec_block|>",
            "<|tools_block|>",
            "<|repo_summary|>",
            "<|warm_memory|>",
            "<|cold_memory|>",
            "<|task_prd|>",
        ):
            assert marker in sys_text
        # 7 个块顺序正确
        idxs = [
            sys_text.index("<|system_prompt|>"),
            sys_text.index("<|spec_block|>"),
            sys_text.index("<|tools_block|>"),
            sys_text.index("<|repo_summary|>"),
            sys_text.index("<|warm_memory|>"),
            sys_text.index("<|cold_memory|>"),
            sys_text.index("<|task_prd|>"),
        ]
        assert idxs == sorted(idxs), "前 7 块顺序应当严格"

        # 之后是 round_history，再是 current_turn
        assert msgs[1].content == "round-1"
        assert msgs[2].content == "now"
        assert len(msgs) == 3

    def test_fingerprint_stable_when_prefix_unchanged(self) -> None:
        a = _full_assembler()
        b = _full_assembler()
        # 改变滚动尾部不应影响 fingerprint
        b.round_history.append(Message(role="assistant", content="r2"))
        b.current_turn.append(Message(role="user", content="another"))
        assert a.compute_prefix_fingerprint() == b.compute_prefix_fingerprint()

    def test_fingerprint_changes_when_any_prefix_block_changes(self) -> None:
        base = _full_assembler()
        baseline = base.compute_prefix_fingerprint()
        # 任意一个前缀块改变都应导致 fingerprint 变化
        for field in (
            "system_prompt",
            "spec_block",
            "tools_block",
            "repo_summary",
            "warm_memory",
            "cold_memory",
            "task_prd",
        ):
            mutant = _full_assembler()
            setattr(mutant, field, getattr(mutant, field) + "_mut")
            assert mutant.compute_prefix_fingerprint() != baseline, (
                f"changing {field} should alter fingerprint"
            )

    def test_fingerprint_is_sha256_hex(self) -> None:
        asm = _full_assembler()
        fp = asm.compute_prefix_fingerprint()
        assert len(fp) == 64
        int(fp, 16)  # 必须可解析为 hex

    def test_total_token_estimate_nonzero(self) -> None:
        asm = _full_assembler()
        # 字符总和 > 0 -> token > 0
        assert asm.total_token_estimate() > 0

    def test_empty_assembler_works(self) -> None:
        asm = CacheStableAssembler()
        msgs = asm.assemble()
        assert len(msgs) == 1
        assert msgs[0].role == "system"
        # 即使所有块为空，fingerprint 也是稳定可计算的
        assert len(asm.compute_prefix_fingerprint()) == 64


# ============================================================
# CacheTelemetry
# ============================================================


class TestCacheTelemetry:
    def test_record_accumulates(self) -> None:
        tel = CacheTelemetry()
        tel.record(Usage(
            prompt_tokens=100,
            completion_tokens=50,
            total_tokens=150,
            prompt_cache_hit_tokens=80,
            prompt_cache_miss_tokens=20,
        ))
        tel.record(Usage(
            prompt_tokens=200,
            completion_tokens=100,
            total_tokens=300,
            prompt_cache_hit_tokens=180,
            prompt_cache_miss_tokens=20,
        ))
        assert tel.total_hit_tokens == 260
        assert tel.total_miss_tokens == 40
        assert tel.call_count == 2
        # hit_rate = 260 / 300
        assert tel.hit_rate == pytest.approx(260 / 300)

    def test_hit_rate_zero_when_no_data(self) -> None:
        tel = CacheTelemetry()
        assert tel.hit_rate == 0.0
        assert tel.total_saved_cny == 0.0

    def test_saved_cny_calculation(self) -> None:
        tel = CacheTelemetry()
        tel.record(Usage(
            prompt_cache_hit_tokens=1_000_000,
            prompt_cache_miss_tokens=0,
        ))
        # delta = 1.0 - 0.02 = 0.98 per M tokens
        assert tel.total_saved_cny == pytest.approx(0.98)

    def test_format_statusline_shape(self) -> None:
        tel = CacheTelemetry()
        tel.record(Usage(
            prompt_cache_hit_tokens=873_000,
            prompt_cache_miss_tokens=127_000,
        ))
        s = tel.format_statusline()
        assert s.startswith("[Cache: ")
        assert s.endswith("]")
        assert "Saved: ¥" in s
        assert "Tokens:" in s
        assert "87.3%" in s

    def test_format_tokens_units(self) -> None:
        assert CacheTelemetry._format_tokens(500) == "500"
        assert CacheTelemetry._format_tokens(1234) == "1.2K"
        assert CacheTelemetry._format_tokens(1_234_567) == "1.2M"

    def test_persistence_roundtrip(self, tmp_path: Any) -> None:
        path = tmp_path / "telemetry.json"
        tel = CacheTelemetry(persist_path=path)
        tel.record(Usage(
            prompt_cache_hit_tokens=500,
            prompt_cache_miss_tokens=100,
        ))
        assert path.exists()
        # 重新加载
        tel2 = CacheTelemetry(persist_path=path)
        assert tel2.total_hit_tokens == 500
        assert tel2.total_miss_tokens == 100
        assert tel2.call_count == 1

    def test_reset(self) -> None:
        tel = CacheTelemetry()
        tel.record(Usage(prompt_cache_hit_tokens=100, prompt_cache_miss_tokens=10))
        tel.reset()
        assert tel.total_hit_tokens == 0
        assert tel.call_count == 0
        assert tel.hit_rate == 0.0


# ============================================================
# DeepSeekClient (mocked)
# ============================================================


def _mock_completion(
    content: str = "ok",
    reasoning_content: str | None = None,
    tool_calls: list[Any] | None = None,
    usage: dict[str, Any] | None = None,
    model: str = "deepseek-v4-flash",
    finish_reason: str = "stop",
) -> Any:
    """造一个仿 OpenAI ChatCompletion 对象。"""
    msg = MagicMock()
    msg.content = content
    msg.tool_calls = tool_calls
    extras: dict[str, Any] = {}
    if reasoning_content is not None:
        extras["reasoning_content"] = reasoning_content
    msg.model_extra = extras
    # 同时设为属性，覆盖两种访问路径
    msg.reasoning_content = reasoning_content

    choice = MagicMock()
    choice.message = msg
    choice.finish_reason = finish_reason

    completion = MagicMock()
    completion.choices = [choice]
    completion.model = model

    usage_obj = MagicMock()
    usage_obj.model_dump = MagicMock(return_value=usage or {
        "prompt_tokens": 100,
        "completion_tokens": 50,
        "total_tokens": 150,
        "prompt_cache_hit_tokens": 80,
        "prompt_cache_miss_tokens": 20,
    })
    completion.usage = usage_obj
    return completion


class TestDeepSeekClient:
    def test_default_base_url(self) -> None:
        c = DeepSeekClient(api_key="sk-test")
        assert c.base_url == DEFAULT_BASE_URL

    def test_env_var_pickup(self, monkeypatch: pytest.MonkeyPatch) -> None:
        monkeypatch.setenv("DEEPSEEK_API_KEY", "sk-env")
        c = DeepSeekClient()
        assert c.api_key == "sk-env"

    def test_messages_to_openai_basic(self) -> None:
        c = DeepSeekClient(api_key="sk-test")
        msgs = [
            Message(role="system", content="sys"),
            Message(role="user", content="hi"),
            Message(
                role="assistant",
                content="bye",
                tool_calls=[ToolCallSpec(
                    id="call_1",
                    function=ToolFunctionSpec(name="do_x", arguments='{"a":1}'),
                )],
            ),
            Message(role="tool", content="result", tool_call_id="call_1"),
        ]
        out = c._messages_to_openai(msgs)
        assert out[0] == {"role": "system", "content": "sys"}
        assert out[1] == {"role": "user", "content": "hi"}
        assert out[2]["role"] == "assistant"
        assert out[2]["tool_calls"][0]["function"]["name"] == "do_x"
        assert out[3]["tool_call_id"] == "call_1"

    def test_messages_to_openai_passes_reasoning_content(self) -> None:
        c = DeepSeekClient(api_key="sk-test")
        out = c._messages_to_openai([
            Message(role="assistant", content="x", reasoning_content="think..."),
        ])
        assert out[0]["reasoning_content"] == "think..."

    def test_build_request_injects_thinking_extra_body(self) -> None:
        c = DeepSeekClient(api_key="sk-test")
        req = c._build_request(
            messages=[Message(role="user", content="hi")],
            model="deepseek-v4-pro",
            tools=None,
            stream=False,
            thinking=True,
            reasoning_effort="high",
        )
        assert req["extra_body"]["thinking"] == {"type": "enabled"}
        assert req["extra_body"]["reasoning_effort"] == "high"
        assert req["model"] == "deepseek-v4-pro"
        assert req["stream"] is False

    def test_build_request_without_thinking(self) -> None:
        c = DeepSeekClient(api_key="sk-test")
        req = c._build_request(
            messages=[Message(role="user", content="hi")],
            model="deepseek-v4-flash",
            tools=None,
            stream=False,
            thinking=False,
            reasoning_effort=None,
        )
        # 无 thinking、无 reasoning_effort 时不应该带 extra_body
        assert "extra_body" not in req

    def test_build_request_passes_tools(self) -> None:
        c = DeepSeekClient(api_key="sk-test")
        tools = [{"type": "function", "function": {"name": "x", "parameters": {}}}]
        req = c._build_request(
            messages=[Message(role="user", content="hi")],
            model="deepseek-v4-flash",
            tools=tools,
            stream=False,
            thinking=False,
            reasoning_effort=None,
        )
        assert req["tools"] == tools

    @pytest.mark.asyncio
    async def test_chat_parses_response(self) -> None:
        c = DeepSeekClient(api_key="sk-test")
        fake = _mock_completion(
            content="answer",
            reasoning_content="thought trail",
            usage={
                "prompt_tokens": 1000,
                "completion_tokens": 200,
                "total_tokens": 1200,
                "prompt_cache_hit_tokens": 900,
                "prompt_cache_miss_tokens": 100,
            },
        )
        with patch.object(
            c._client.chat.completions, "create", new=AsyncMock(return_value=fake)
        ):
            resp = await c.chat(
                messages=[Message(role="user", content="q")],
                model="deepseek-v4-flash",
                thinking=True,
                reasoning_effort="medium",
            )
        assert resp.content == "answer"
        assert resp.reasoning_content == "thought trail"
        assert resp.usage.prompt_cache_hit_tokens == 900
        assert resp.usage.prompt_cache_miss_tokens == 100
        assert resp.cache_hit_rate == pytest.approx(0.9)
        assert resp.finish_reason == "stop"


# ============================================================
# AutoRouter
# ============================================================


class TestAutoRouter:
    @pytest.mark.asyncio
    async def test_route_simple(self) -> None:
        client = MagicMock(spec=DeepSeekClient)
        client.chat = AsyncMock(return_value=MagicMock(
            content=json.dumps({"complexity": "simple", "rationale": "tiny task"}),
        ))
        router = AutoRouter(client=client)
        decision = await router.route("列出 src 下的所有 python 文件")
        assert decision.recommended_model == "deepseek-v4-flash"
        assert decision.thinking is False
        assert decision.reasoning_effort is None
        assert "tiny task" in decision.rationale

    @pytest.mark.asyncio
    async def test_route_complex(self) -> None:
        client = MagicMock(spec=DeepSeekClient)
        client.chat = AsyncMock(return_value=MagicMock(
            content=json.dumps({"complexity": "complex", "rationale": "multi-file"}),
        ))
        router = AutoRouter(client=client)
        decision = await router.route("重构整个 Forge 模块")
        assert decision.recommended_model == "deepseek-v4-pro"
        assert decision.reasoning_effort == "high"
        assert decision.thinking is True

    @pytest.mark.asyncio
    async def test_route_deep(self) -> None:
        client = MagicMock(spec=DeepSeekClient)
        client.chat = AsyncMock(return_value=MagicMock(
            content=json.dumps({"complexity": "deep", "rationale": "algorithm"}),
        ))
        router = AutoRouter(client=client)
        decision = await router.route("证明这个数据结构的最坏复杂度")
        assert decision.recommended_model == "deepseek-v4-pro"
        assert decision.reasoning_effort == "max"

    @pytest.mark.asyncio
    async def test_route_falls_back_on_router_error(self) -> None:
        client = MagicMock(spec=DeepSeekClient)
        client.chat = AsyncMock(side_effect=RuntimeError("boom"))
        router = AutoRouter(client=client)
        decision = await router.route("修一个小 bug")
        # fallback 启发式：含 "修" -> medium
        assert isinstance(decision, RouteDecision)
        assert "router_error" in decision.rationale

    @pytest.mark.asyncio
    async def test_route_malformed_json_defaults_medium(self) -> None:
        client = MagicMock(spec=DeepSeekClient)
        client.chat = AsyncMock(return_value=MagicMock(content="not json at all"))
        router = AutoRouter(client=client)
        decision = await router.route("一些任务")
        # 无法解析 -> 默认 medium
        assert decision.recommended_model == "deepseek-v4-flash"
        assert decision.reasoning_effort == "medium"


# ============================================================
# force_json
# ============================================================


class TestForceJson:
    @pytest.mark.asyncio
    async def test_force_json_returns_dict(self) -> None:
        # 客户端在 beta 端点
        client = DeepSeekClient(api_key="sk-test", base_url=BETA_BASE_URL)
        # 让模型续写返回 `name": "alice", "age": 30}`，
        # force_json 会把前缀 `{"` 拼回去 -> 完整 `{"name": "alice", "age": 30}`
        fake = _mock_completion(content='name": "alice", "age": 30}')
        with patch.object(
            client._client.chat.completions, "create", new=AsyncMock(return_value=fake)
        ):
            result = await force_json(
                client=client,
                schema_hint='{"name": "string", "age": "int"}',
            )
        assert result == {"name": "alice", "age": 30}

    @pytest.mark.asyncio
    async def test_force_json_auto_wraps_beta_endpoint(self) -> None:
        # 客户端不是 beta 端点 -> force_json 应内部构造一个 beta 客户端
        client = DeepSeekClient(api_key="sk-test", base_url=DEFAULT_BASE_URL)
        fake = _mock_completion(content='ok": true}')
        with patch(
            "dscode.deepseek.prefix_completion.DeepSeekClient"
        ) as MockClient:
            instance = MagicMock()
            instance.base_url = BETA_BASE_URL
            instance._client = MagicMock()
            instance._client.chat = MagicMock()
            instance._client.chat.completions = MagicMock()
            instance._client.chat.completions.create = AsyncMock(return_value=fake)
            instance.chat = AsyncMock(return_value=MagicMock(content='ok": true}'))
            MockClient.return_value = instance

            result = await force_json(
                client=client,
                schema_hint='{"ok": "bool"}',
            )
            # 应当用 beta URL 构造了一个新 client
            MockClient.assert_called_once()
            kwargs = MockClient.call_args.kwargs
            assert kwargs["base_url"] == BETA_BASE_URL
            assert result == {"ok": True}

    @pytest.mark.asyncio
    async def test_force_json_raises_on_garbage(self) -> None:
        client = DeepSeekClient(api_key="sk-test", base_url=BETA_BASE_URL)
        # 模型返回完全无法解析的内容
        fake = _mock_completion(content="!!! no json here !!!")
        with patch.object(
            client._client.chat.completions, "create", new=AsyncMock(return_value=fake)
        ):
            with pytest.raises(ValueError):
                await force_json(client=client, schema_hint="anything")
