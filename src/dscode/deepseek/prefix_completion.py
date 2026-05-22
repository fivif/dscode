"""Prefix Completion 结构化输出。

DeepSeek beta 端点支持 `{"role": "assistant", "content": "...", "prefix": True}`，
让模型从指定前缀续写。配合 stop tokens 可强制 JSON 输出。

要求：必须使用 base_url = https://api.deepseek.com/beta。
"""
from __future__ import annotations

import json
import re
from typing import Any

from dscode.core.types import Message
from dscode.deepseek.client import BETA_BASE_URL, DeepSeekClient

_DEFAULT_SYSTEM_PROMPT = (
    "You are a strict JSON producer. Always output a single valid JSON object that "
    "matches the requested schema. Do NOT output prose, markdown, or comments."
)


async def force_json(
    client: DeepSeekClient,
    schema_hint: str,
    model: str = "deepseek-v4-flash",
    *,
    user_prompt: str | None = None,
    system_prompt: str | None = None,
    temperature: float = 0.0,
    max_tokens: int = 1024,
) -> dict[str, Any]:
    """通过 prefix completion 强制 JSON 输出。

    使用 `{"role": "assistant", "content": '{"', "prefix": True}` 作为引导前缀，
    并以 `}` 作为 stop token，让模型只能产出闭合的 JSON。

    Args:
        client: DeepSeekClient 实例。必须使用 beta 端点（否则自动包一层 beta 客户端）。
        schema_hint: 给模型看的 schema 描述（文本，可以是 JSON Schema 或自然语言）。
        model: 模型名（默认 v4-flash）。
        user_prompt: 用户指令。默认根据 schema_hint 生成。
        system_prompt: 系统提示（默认严格 JSON 生产者）。
        temperature: 采样温度（默认 0）。
        max_tokens: 最大生成 token 数。

    Returns:
        解析后的 dict。

    Raises:
        ValueError: 模型输出无法解析为合法 JSON。
    """
    # beta 端点检查：如果客户端 base_url 非 beta，自动构造新客户端
    target_client = client
    if not client.base_url.rstrip("/").endswith("/beta"):
        target_client = DeepSeekClient(
            api_key=client.api_key,
            base_url=BETA_BASE_URL,
        )

    sys_prompt = system_prompt or _DEFAULT_SYSTEM_PROMPT
    usr_prompt = user_prompt or (
        f"Produce a JSON object matching this schema:\n\n{schema_hint}\n\n"
        "Output ONLY the JSON object."
    )

    # 前缀 `{"` 强制开局，stop 用 `}` 截断。
    # 注：assistant 消息的 prefix=True 是 DeepSeek 扩展字段，通过 extra="allow" 透传。
    messages: list[Message] = [
        Message(role="system", content=sys_prompt),
        Message(role="user", content=usr_prompt),
        Message(role="assistant", content='{"', prefix=True),  # type: ignore[call-arg]
    ]

    resp = await target_client.chat(
        messages=messages,
        model=model,
        thinking=False,
        reasoning_effort=None,
        temperature=temperature,
        max_tokens=max_tokens,
        stop=["```"],
    )

    # 重建完整 JSON 文本：模型续写的内容前面要加回开头的 `{"`
    raw = (resp.content or "").strip()
    if not raw.startswith("{"):
        raw = '{"' + raw
    # 若被 stop 截断，补全闭合花括号
    if not raw.rstrip().endswith("}"):
        raw = raw.rstrip().rstrip(",") + "}"

    try:
        return json.loads(raw)
    except json.JSONDecodeError:
        # 二次救援：抽取最外层 {...}
        match = re.search(r"\{.*\}", raw, re.DOTALL)
        if match:
            try:
                return json.loads(match.group(0))
            except json.JSONDecodeError as exc:
                raise ValueError(
                    f"force_json: model output not valid JSON: {raw!r}"
                ) from exc
        raise ValueError(f"force_json: model output not valid JSON: {raw!r}") from None
