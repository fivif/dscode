"""LiteLLM 适配器。

把 litellm 作为后端，支持任意 `provider/model` 模型字符串：
- `openai/gpt-4o`
- `anthropic/claude-sonnet-4-5`
- `gemini/gemini-2.5-pro`
- `ollama/qwen3-32b`
- `groq/llama-3.3-70b-versatile`
- ...

litellm 接受 OpenAI 兼容 dict，因此消息转换基本是直通；但响应需要从 litellm
的 ModelResponse 中提取 usage / tool_calls / reasoning_content（部分模型有）。

依赖 litellm 是可选项（在 pyproject.toml 的 `providers` extra 下）。
所以 import 是 **lazy** 的——只有真正构造或调用 LiteLLMProvider 才会触发。
"""
from __future__ import annotations

from collections.abc import AsyncGenerator
from typing import TYPE_CHECKING, Any, Literal

from dscode.core.types import (
    LLMResponse,
    Message,
    ToolCallSpec,
    ToolFunctionSpec,
    Usage,
)

if TYPE_CHECKING:  # pragma: no cover - 类型提示用
    import litellm as _litellm_types  # noqa: F401


def _import_litellm() -> Any:
    """惰性导入 litellm。失败时抛出明确的 ImportError 指引安装。"""
    try:
        import litellm  # type: ignore[import-untyped]
    except ImportError as e:  # pragma: no cover - 仅在缺失依赖时触发
        raise ImportError(
            "litellm 未安装。安装方式：pip install 'dscode[providers]' "
            "或 pip install 'litellm>=1.50'"
        ) from e
    return litellm


class LiteLLMProvider:
    """LiteLLM 跨模型适配器，实现 LLMProviderProtocol。

    用法::

        provider = LiteLLMProvider(api_key=os.getenv("OPENAI_API_KEY"))
        resp = await provider.chat(
            messages=[Message(role="user", content="hi")],
            model="openai/gpt-4o",
        )
    """

    def __init__(
        self,
        api_key: str | None = None,
        api_base: str | None = None,
        timeout: float = 120.0,
        max_retries: int = 2,
        **default_kwargs: Any,
    ) -> None:
        """构造适配器。

        Args:
            api_key: 默认 API key。多数 provider 也可以通过环境变量自动读取
                     （OPENAI_API_KEY / ANTHROPIC_API_KEY / GEMINI_API_KEY ...）。
            api_base: 自定义 endpoint（如本地 Ollama / vLLM）。
            timeout: 单次请求超时秒。
            max_retries: litellm 重试次数。
            **default_kwargs: 透传给 litellm.acompletion 的默认参数。
        """
        self.api_key = api_key
        self.api_base = api_base
        self.timeout = timeout
        self.max_retries = max_retries
        self.default_kwargs: dict[str, Any] = dict(default_kwargs)
        # 触发一次 lazy import，提前暴露 ImportError（构造时即报错，比首次调用清晰）
        self._litellm = _import_litellm()

    # ------------------------------------------------------------
    # 内部
    # ------------------------------------------------------------

    @staticmethod
    def _messages_to_litellm(messages: list[Message]) -> list[dict[str, Any]]:
        """把 Message 转为 litellm 接受的 OpenAI 兼容 dict。"""
        out: list[dict[str, Any]] = []
        for m in messages:
            d: dict[str, Any] = {"role": m.role}
            if m.content is not None:
                d["content"] = m.content
            if m.name is not None:
                d["name"] = m.name
            if m.tool_call_id is not None:
                d["tool_call_id"] = m.tool_call_id
            if m.tool_calls:
                d["tool_calls"] = [
                    {
                        "id": tc.id,
                        "type": tc.type,
                        "function": {
                            "name": tc.function.name,
                            "arguments": tc.function.arguments,
                        },
                    }
                    for tc in m.tool_calls
                ]
            # 部分模型支持 reasoning_content（如 DeepSeek via litellm）——尽可能透传
            if m.reasoning_content is not None:
                d["reasoning_content"] = m.reasoning_content
            out.append(d)
        return out

    def _build_request(
        self,
        messages: list[Message],
        model: str,
        tools: list[dict[str, Any]] | None,
        stream: bool,
        thinking: bool,
        reasoning_effort: Literal["low", "medium", "high", "max"] | None,
        **kwargs: Any,
    ) -> dict[str, Any]:
        """组装 litellm.acompletion 的参数。"""
        req: dict[str, Any] = {
            "model": model,
            "messages": self._messages_to_litellm(messages),
            "stream": stream,
            "timeout": self.timeout,
            "num_retries": self.max_retries,
        }
        if self.api_key:
            req["api_key"] = self.api_key
        if self.api_base:
            req["api_base"] = self.api_base
        if tools:
            req["tools"] = tools

        # thinking / reasoning_effort 通过 extra_body 注入，由 litellm 透传给上游
        extra_body: dict[str, Any] = {}
        if thinking:
            extra_body["thinking"] = {"type": "enabled"}
        if reasoning_effort is not None:
            extra_body["reasoning_effort"] = reasoning_effort
        if extra_body:
            req["extra_body"] = extra_body

        # 默认 kwargs 优先级低于显式 kwargs
        for k, v in self.default_kwargs.items():
            req.setdefault(k, v)
        for k, v in kwargs.items():
            if k == "extra_body" and isinstance(v, dict):
                req.setdefault("extra_body", {}).update(v)
            else:
                req[k] = v
        return req

    @staticmethod
    def _to_dict(obj: Any) -> dict[str, Any]:
        """统一把 SDK 对象转为 dict。"""
        if obj is None:
            return {}
        if isinstance(obj, dict):
            return obj
        if hasattr(obj, "model_dump"):
            try:
                return obj.model_dump()
            except Exception:  # pragma: no cover - 防御
                pass
        if hasattr(obj, "dict"):
            try:
                return obj.dict()  # type: ignore[no-any-return]
            except Exception:  # pragma: no cover
                pass
        if hasattr(obj, "__dict__"):
            return dict(obj.__dict__)
        return {}

    @classmethod
    def _parse_usage(cls, raw: Any) -> Usage:
        """从 litellm 响应里解析 Usage。"""
        if raw is None:
            return Usage()
        data = cls._to_dict(raw)
        details = data.get("completion_tokens_details") or {}
        details_dict = details if isinstance(details, dict) else cls._to_dict(details)
        # litellm 可能把 cache 字段命名为 cache_creation_input_tokens / cache_read_input_tokens
        # （Anthropic 风格）；同时支持 DeepSeek 风格 prompt_cache_hit_tokens
        hit = (
            data.get("prompt_cache_hit_tokens")
            or data.get("cache_read_input_tokens")
            or 0
        )
        miss = (
            data.get("prompt_cache_miss_tokens")
            or data.get("cache_creation_input_tokens")
            or 0
        )
        return Usage(
            prompt_tokens=data.get("prompt_tokens", 0) or 0,
            completion_tokens=data.get("completion_tokens", 0) or 0,
            total_tokens=data.get("total_tokens", 0) or 0,
            prompt_cache_hit_tokens=hit or 0,
            prompt_cache_miss_tokens=miss or 0,
            reasoning_tokens=(
                details_dict.get("reasoning_tokens", 0)
                or data.get("reasoning_tokens", 0)
                or 0
            ),
        )

    @classmethod
    def _parse_tool_calls(cls, raw: Any) -> list[ToolCallSpec] | None:
        """把 litellm 工具调用列表转回 ToolCallSpec。"""
        if not raw:
            return None
        out: list[ToolCallSpec] = []
        for tc in raw:
            tc_data = cls._to_dict(tc)
            fn_raw = tc_data.get("function") or {}
            fn = fn_raw if isinstance(fn_raw, dict) else cls._to_dict(fn_raw)
            args = fn.get("arguments", "")
            if not isinstance(args, str):
                import json as _json

                args = _json.dumps(args)
            tc_id = tc_data.get("id") or ""
            out.append(
                ToolCallSpec(
                    id=tc_id,
                    type=tc_data.get("type", "function") or "function",
                    function=ToolFunctionSpec(
                        name=fn.get("name", "") or "",
                        arguments=args or "",
                    ),
                )
            )
        return out or None

    @staticmethod
    def _normalize_finish_reason(
        raw: Any,
    ) -> Literal["stop", "length", "tool_calls", "content_filter"] | None:
        """litellm 可能返回 Anthropic 风格的 end_turn / max_tokens 等，统一映射。"""
        if raw is None:
            return None
        mapping = {
            "stop": "stop",
            "end_turn": "stop",
            "stop_sequence": "stop",
            "length": "length",
            "max_tokens": "length",
            "tool_calls": "tool_calls",
            "tool_use": "tool_calls",
            "function_call": "tool_calls",
            "content_filter": "content_filter",
        }
        return mapping.get(str(raw))  # type: ignore[return-value]

    # ------------------------------------------------------------
    # 公共 API
    # ------------------------------------------------------------

    async def chat(
        self,
        messages: list[Message],
        model: str,
        tools: list[dict[str, Any]] | None = None,
        stream: bool = False,
        thinking: bool = False,
        reasoning_effort: Literal["low", "medium", "high", "max"] | None = None,
        **kwargs: Any,
    ) -> LLMResponse:
        """非流式 chat。

        Raises:
            RuntimeError: 上游 API 出错时（包括 RateLimitError / APIError）。
        """
        req = self._build_request(
            messages=messages,
            model=model,
            tools=tools,
            stream=False,
            thinking=thinking,
            reasoning_effort=reasoning_effort,
            **kwargs,
        )
        try:
            completion = await self._litellm.acompletion(**req)
        except Exception as e:  # 捕 litellm.APIError / RateLimitError / 其余
            raise RuntimeError(f"litellm.acompletion failed: {e!s}") from e

        comp_dict = self._to_dict(completion)
        choices = comp_dict.get("choices") or []
        choice = choices[0] if choices else None
        msg_dict: dict[str, Any] = {}
        finish_reason_raw: Any = None
        if choice is not None:
            choice_dict = choice if isinstance(choice, dict) else self._to_dict(choice)
            msg_dict = choice_dict.get("message") or {}
            if not isinstance(msg_dict, dict):
                msg_dict = self._to_dict(msg_dict)
            finish_reason_raw = choice_dict.get("finish_reason")

        content = msg_dict.get("content") or ""
        if isinstance(content, list):
            # 极端情况：上游返回 content blocks（litellm 一般会拍平，但加保险）
            content = "".join(
                str(b.get("text", "")) if isinstance(b, dict) else str(b) for b in content
            )
        reasoning_content = (
            msg_dict.get("reasoning_content")
            or msg_dict.get("reasoning")
            or None
        )
        tool_calls = self._parse_tool_calls(msg_dict.get("tool_calls"))
        usage = self._parse_usage(comp_dict.get("usage"))

        total_prompt = usage.prompt_cache_hit_tokens + usage.prompt_cache_miss_tokens
        cache_rate = (
            usage.prompt_cache_hit_tokens / total_prompt if total_prompt > 0 else 0.0
        )

        return LLMResponse(
            content=content or "",
            reasoning_content=reasoning_content,
            tool_calls=tool_calls,
            finish_reason=self._normalize_finish_reason(finish_reason_raw),
            usage=usage,
            model=comp_dict.get("model") or model,
            cache_hit_rate=cache_rate,
        )

    async def chat_stream(
        self,
        messages: list[Message],
        model: str,
        tools: list[dict[str, Any]] | None = None,
        thinking: bool = False,
        reasoning_effort: Literal["low", "medium", "high", "max"] | None = None,
        **kwargs: Any,
    ) -> AsyncGenerator[LLMResponse, None]:
        """流式 chat。逐 chunk yield。"""
        req = self._build_request(
            messages=messages,
            model=model,
            tools=tools,
            stream=True,
            thinking=thinking,
            reasoning_effort=reasoning_effort,
            **kwargs,
        )
        req.setdefault("stream_options", {"include_usage": True})

        try:
            stream = await self._litellm.acompletion(**req)
        except Exception as e:
            raise RuntimeError(f"litellm.acompletion (stream) failed: {e!s}") from e

        async for chunk in stream:
            chunk_dict = self._to_dict(chunk)
            choices = chunk_dict.get("choices") or []
            delta_content = ""
            delta_reasoning: str | None = None
            tool_calls_delta: list[ToolCallSpec] | None = None
            finish_reason_raw: Any = None
            if choices:
                ch = choices[0]
                ch_dict = ch if isinstance(ch, dict) else self._to_dict(ch)
                delta = ch_dict.get("delta") or {}
                if not isinstance(delta, dict):
                    delta = self._to_dict(delta)
                delta_content = delta.get("content") or ""
                if isinstance(delta_content, list):
                    delta_content = "".join(
                        str(b.get("text", "")) if isinstance(b, dict) else str(b)
                        for b in delta_content
                    )
                delta_reasoning = (
                    delta.get("reasoning_content")
                    or delta.get("reasoning")
                    or None
                )
                tool_calls_delta = self._parse_tool_calls(delta.get("tool_calls"))
                finish_reason_raw = ch_dict.get("finish_reason")

            usage = self._parse_usage(chunk_dict.get("usage"))
            total_prompt = usage.prompt_cache_hit_tokens + usage.prompt_cache_miss_tokens
            cache_rate = (
                usage.prompt_cache_hit_tokens / total_prompt if total_prompt > 0 else 0.0
            )
            yield LLMResponse(
                content=delta_content or "",
                reasoning_content=delta_reasoning,
                tool_calls=tool_calls_delta,
                finish_reason=self._normalize_finish_reason(finish_reason_raw),
                usage=usage,
                model=chunk_dict.get("model") or model,
                cache_hit_rate=cache_rate,
            )
