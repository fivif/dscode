"""DeepSeek 思考模式 + tool calling 完整回传。

DeepSeek V3.2+ 在工具调用循环中开启 thinking 后，每一轮的 assistant 消息都
会带 `reasoning_content`。**下一轮请求必须把它完整回传，否则服务端 400**——
这是 DeepSeek 的硬约束。

本模块只做两件事：
1. `chat_with_thinking()`：薄封装，强制注入 `thinking: enabled`，并
   保证响应中的 `reasoning_content` 写入返回的 LLMResponse。
2. `append_with_reasoning()`：把 assistant 响应正确 append 到 messages
   列表（OpenAI 风格 dict）——不丢失 `reasoning_content`。

约定：
- 上层 messages 给 SDK 时是 dict，不是 pydantic Message——
  因为 `prefix=True`、`reasoning_content` 是 DeepSeek 扩展字段，
  pydantic 强校验会拒绝；直接走 dict 透传最稳。
- 不改 `core/types.py`：Message 已有 `reasoning_content` 字段，但为了
  与现实中"messages 是 dict 列表"的常规做法对齐，本工具同时支持
  Message 与 dict 两种输入。
"""
from __future__ import annotations

from typing import Any, Literal

from dscode.core.types import LLMResponse, Message
from dscode.deepseek.client import DeepSeekClient


async def chat_with_thinking(
    client: DeepSeekClient,
    messages: list[Message],
    model: str = "deepseek-v4-pro",
    tools: list[dict[str, Any]] | None = None,
    reasoning_effort: Literal["low", "medium", "high", "max"] = "high",
    **kwargs: Any,
) -> LLMResponse:
    """开启 thinking + reasoning_effort 的 chat。

    Args:
        client: 已有 DeepSeekClient。
        messages: 对话历史（pydantic Message）。
        model: 默认 v4-pro（thinking 在 pro 上效果最好）。
        tools: 可选工具定义；与 thinking 联用时必须遵守 reasoning_content 回传契约。
        reasoning_effort: 推理强度，默认 high。
        **kwargs: 透传 temperature / max_tokens 等。

    Returns:
        LLMResponse，`reasoning_content` 字段保证从响应中提取（若 SDK 提供）。
    """
    # 显式注入 extra_body，让 client._build_request 合并
    extra_body = dict(kwargs.pop("extra_body", {}) or {})
    extra_body.setdefault("thinking", {"type": "enabled"})

    return await client.chat(
        messages=messages,
        model=model,
        tools=tools,
        thinking=True,
        reasoning_effort=reasoning_effort,
        extra_body=extra_body,
        **kwargs,
    )


def _response_to_assistant_dict(response: LLMResponse) -> dict[str, Any]:
    """把 LLMResponse 转成 OpenAI 风格 assistant dict（保留 reasoning_content）。"""
    msg: dict[str, Any] = {
        "role": "assistant",
        # OpenAI 规范：assistant content 可以为 None
        "content": response.content if response.content else None,
    }
    if response.reasoning_content is not None:
        # 关键：DeepSeek 下一轮必须能看到完整的 reasoning_content
        msg["reasoning_content"] = response.reasoning_content
    if response.tool_calls:
        msg["tool_calls"] = [
            {
                "id": tc.id,
                "type": tc.type,
                "function": {
                    "name": tc.function.name,
                    "arguments": tc.function.arguments,
                },
            }
            for tc in response.tool_calls
        ]
    return msg


def _message_to_dict(m: Any) -> dict[str, Any]:
    """归一化 Message / dict → dict。"""
    if isinstance(m, Message):
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
        if m.reasoning_content is not None:
            d["reasoning_content"] = m.reasoning_content
        # 透传 model_extra 中的扩展字段（如 prefix=True）
        extra = m.model_dump(
            exclude_none=True,
            exclude={"role", "content", "name", "tool_call_id", "tool_calls", "reasoning_content"},
        )
        for k, v in extra.items():
            d.setdefault(k, v)
        return d
    if isinstance(m, dict):
        return dict(m)
    raise TypeError(f"无法把 {type(m).__name__} 转为 message dict")


def append_with_reasoning(
    messages: list[Any],
    response: LLMResponse,
) -> list[dict[str, Any]]:
    """把 assistant 响应追加到 messages，**保留 reasoning_content**。

    这是 DeepSeek thinking + tool 循环的硬性要求：下一轮请求若缺失上一轮
    assistant 的 reasoning_content，服务端会回 400。

    入参 messages 可混合 Message 与 dict——统一返回新的 dict 列表，
    可直接用于下一轮 `client.chat(messages=...)`（通过 pydantic Message
    包装时也会安全透传，因为 Message 设了 extra="allow"）。

    Args:
        messages: 现有对话列表（Message 或 dict 混合）。
        response: 来自 chat_with_thinking 的响应。

    Returns:
        新的 dict 列表（不修改入参），末尾追加了 assistant 消息。
    """
    out: list[dict[str, Any]] = [_message_to_dict(m) for m in messages]
    out.append(_response_to_assistant_dict(response))
    return out


__all__ = [
    "append_with_reasoning",
    "chat_with_thinking",
]
