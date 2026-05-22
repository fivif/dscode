"""DeepSeek 原生客户端。

使用 OpenAI 兼容 SDK 调用 DeepSeek API。支持：
- thinking 模式（通过 extra_body 注入）
- reasoning_effort（low / medium / high / max）
- 缓存命中字段（prompt_cache_hit_tokens / prompt_cache_miss_tokens）
- reasoning_content 回传（thinking 模式必需）
- beta 端点（prefix completion 等）

支持模型：
- deepseek-v4-flash
- deepseek-v4-pro
- deepseek-chat
- deepseek-reasoner
"""
from __future__ import annotations

import os
from collections.abc import AsyncGenerator
from typing import Any, Literal

from openai import AsyncOpenAI

from dscode.core.types import (
    LLMResponse,
    Message,
    ToolCallSpec,
    ToolFunctionSpec,
    Usage,
)

DEFAULT_BASE_URL = "https://api.deepseek.com"
BETA_BASE_URL = "https://api.deepseek.com/beta"


class DeepSeekClient:
    """DeepSeek 原生客户端，实现 LLMProviderProtocol。"""

    def __init__(
        self,
        api_key: str | None = None,
        base_url: str = DEFAULT_BASE_URL,
        timeout: float = 120.0,
        max_retries: int = 2,
    ) -> None:
        """构造客户端。

        Args:
            api_key: DeepSeek API key。若 None，从环境变量 `DEEPSEEK_API_KEY` 读取。
            base_url: 端点 URL。默认 https://api.deepseek.com。
                      beta 功能（prefix completion 等）传 https://api.deepseek.com/beta。
            timeout: 单次请求超时（秒）。
            max_retries: 最大重试次数。
        """
        key = api_key or os.getenv("DEEPSEEK_API_KEY")
        if not key:
            # 允许延迟报错：实际调用时再抛
            key = "sk-placeholder"
        self.api_key = key
        self.base_url = base_url
        self._client = AsyncOpenAI(
            api_key=key,
            base_url=base_url,
            timeout=timeout,
            max_retries=max_retries,
        )

    # ------------------------------------------------------------
    # 内部工具
    # ------------------------------------------------------------

    @staticmethod
    def _messages_to_openai(messages: list[Message]) -> list[dict[str, Any]]:
        """将 Message 转为 OpenAI 兼容 dict。"""
        out: list[dict[str, Any]] = []
        for m in messages:
            d: dict[str, Any] = {"role": m.role}
            if m.content is not None:
                d["content"] = m.content
            if m.name is not None and m.role == "user":
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
            # thinking 模式：reasoning_content 必须完整回传，否则 400
            if m.reasoning_content is not None:
                d["reasoning_content"] = m.reasoning_content
            # 透传 extra 字段
            extra = m.model_dump(exclude_none=True, exclude={
                "role", "content", "name", "tool_call_id", "tool_calls", "reasoning_content",
            })
            for k, v in extra.items():
                if k not in d:
                    d[k] = v
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
        """组装 openai SDK 调用参数。"""
        req: dict[str, Any] = {
            "model": model,
            "messages": self._messages_to_openai(messages),
            "stream": stream,
        }
        if tools:
            req["tools"] = tools
        # extra_body：thinking + reasoning_effort 通过这里注入
        extra_body: dict[str, Any] = {}
        if thinking:
            extra_body["thinking"] = {"type": "enabled"}
        if reasoning_effort is not None:
            extra_body["reasoning_effort"] = reasoning_effort
        if extra_body:
            req["extra_body"] = extra_body

        # 透传其余 kwargs（temperature / max_tokens / stop / prefix 等）
        for k, v in kwargs.items():
            if k == "extra_body" and isinstance(v, dict):
                # 合并到 extra_body
                req.setdefault("extra_body", {}).update(v)
            else:
                req[k] = v
        return req

    @staticmethod
    def _parse_usage(raw: Any) -> Usage:
        """从 SDK 响应提取 Usage。"""
        if raw is None:
            return Usage()
        data = raw.model_dump() if hasattr(raw, "model_dump") else dict(raw)
        # DeepSeek 返回的字段名
        return Usage(
            prompt_tokens=data.get("prompt_tokens", 0) or 0,
            completion_tokens=data.get("completion_tokens", 0) or 0,
            total_tokens=data.get("total_tokens", 0) or 0,
            prompt_cache_hit_tokens=data.get("prompt_cache_hit_tokens", 0) or 0,
            prompt_cache_miss_tokens=data.get("prompt_cache_miss_tokens", 0) or 0,
            reasoning_tokens=(data.get("completion_tokens_details") or {}).get(
                "reasoning_tokens", 0
            ) or data.get("reasoning_tokens", 0) or 0,
        )

    @staticmethod
    def _parse_tool_calls(raw: Any) -> list[ToolCallSpec] | None:
        """解析 tool_calls。"""
        if not raw:
            return None
        out: list[ToolCallSpec] = []
        for tc in raw:
            tc_data = tc.model_dump() if hasattr(tc, "model_dump") else dict(tc)
            fn = tc_data.get("function") or {}
            out.append(
                ToolCallSpec(
                    id=tc_data.get("id") or "",
                    type=tc_data.get("type", "function"),
                    function=ToolFunctionSpec(
                        name=fn.get("name", ""),
                        arguments=fn.get("arguments", "") or "",
                    ),
                )
            )
        return out or None

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
        """非流式 chat 调用。

        Args:
            messages: 对话历史。
            model: 模型名（deepseek-v4-flash / deepseek-v4-pro / deepseek-chat / deepseek-reasoner）。
            tools: OpenAI 风格工具定义。
            stream: 兼容签名占位；本方法始终非流式（流式请用 chat_stream）。
            thinking: 是否开启思考模式（注入 extra_body.thinking）。
            reasoning_effort: 推理强度（low / medium / high / max）。
            **kwargs: 透传 temperature / max_tokens / stop / extra_body 等。

        Returns:
            LLMResponse，包含 content / reasoning_content / tool_calls / usage。
        """
        req = self._build_request(
            messages=messages,
            model=model,
            tools=tools,
            stream=False,  # chat 方法固定非流式
            thinking=thinking,
            reasoning_effort=reasoning_effort,
            **kwargs,
        )
        completion = await self._client.chat.completions.create(**req)
        choice = completion.choices[0] if completion.choices else None
        msg = choice.message if choice else None

        content = (msg.content if msg else "") or ""
        reasoning_content: str | None = None
        if msg is not None:
            # OpenAI SDK 没有 reasoning_content 字段，落在 model_extra
            extra = getattr(msg, "model_extra", None) or {}
            reasoning_content = extra.get("reasoning_content")
            if reasoning_content is None and hasattr(msg, "reasoning_content"):
                reasoning_content = getattr(msg, "reasoning_content", None)

        tool_calls = self._parse_tool_calls(getattr(msg, "tool_calls", None)) if msg else None
        usage = self._parse_usage(getattr(completion, "usage", None))
        finish_reason = getattr(choice, "finish_reason", None) if choice else None

        # 计算 cache hit rate
        total_prompt = usage.prompt_cache_hit_tokens + usage.prompt_cache_miss_tokens
        cache_rate = usage.prompt_cache_hit_tokens / total_prompt if total_prompt > 0 else 0.0

        return LLMResponse(
            content=content,
            reasoning_content=reasoning_content,
            tool_calls=tool_calls,
            finish_reason=finish_reason if finish_reason in {
                "stop", "length", "tool_calls", "content_filter",
            } else None,
            usage=usage,
            model=getattr(completion, "model", model) or model,
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
        """流式 chat 调用。

        每个 yield 出的 LLMResponse 表示一段增量内容（delta）。
        最后一个 chunk 包含完整 usage 信息。

        Args:
            messages: 对话历史。
            model: 模型名。
            tools: OpenAI 风格工具定义。
            thinking: 是否开启思考模式。
            reasoning_effort: 推理强度。
            **kwargs: 透传其他参数。

        Yields:
            LLMResponse 增量。
        """
        req = self._build_request(
            messages=messages,
            model=model,
            tools=tools,
            stream=True,
            thinking=thinking,
            reasoning_effort=reasoning_effort,
            **kwargs,
        )
        # OpenAI SDK 要求显式 stream_options 才回传 usage
        req.setdefault("stream_options", {"include_usage": True})

        stream = await self._client.chat.completions.create(**req)
        async for chunk in stream:
            choices = getattr(chunk, "choices", None) or []
            delta_content = ""
            delta_reasoning: str | None = None
            tool_calls_delta: list[ToolCallSpec] | None = None
            finish_reason = None
            if choices:
                ch = choices[0]
                delta = getattr(ch, "delta", None)
                if delta is not None:
                    delta_content = getattr(delta, "content", "") or ""
                    extra = getattr(delta, "model_extra", None) or {}
                    delta_reasoning = extra.get("reasoning_content")
                    if delta_reasoning is None and hasattr(delta, "reasoning_content"):
                        delta_reasoning = getattr(delta, "reasoning_content", None)
                    tool_calls_delta = self._parse_tool_calls(
                        getattr(delta, "tool_calls", None)
                    )
                finish_reason = getattr(ch, "finish_reason", None)

            usage = self._parse_usage(getattr(chunk, "usage", None))
            total_prompt = usage.prompt_cache_hit_tokens + usage.prompt_cache_miss_tokens
            cache_rate = (
                usage.prompt_cache_hit_tokens / total_prompt if total_prompt > 0 else 0.0
            )
            yield LLMResponse(
                content=delta_content,
                reasoning_content=delta_reasoning,
                tool_calls=tool_calls_delta,
                finish_reason=finish_reason if finish_reason in {
                    "stop", "length", "tool_calls", "content_filter",
                } else None,
                usage=usage,
                model=getattr(chunk, "model", model) or model,
                cache_hit_rate=cache_rate,
            )

    async def close(self) -> None:
        """关闭底层 HTTP client。"""
        await self._client.close()
