"""FIM（Fill-In-Middle）补全。

DeepSeek beta 端点的 `/completions`（**不是** `/chat/completions`）支持
传统 `prompt + suffix` 两端约束的中间补全——专门用于"光标处补全"场景：
自动重构、模板填充、代码补全。

与 chat 续写的区别：FIM 同时看见 prefix 和 suffix，模型只产出中间段，
精度显著高于"读完上文猜下文"。

要求：必须用 beta base_url（`https://api.deepseek.com/beta`）。
"""
from __future__ import annotations

from typing import Any

from dscode.deepseek.client import BETA_BASE_URL, DeepSeekClient


async def fim_complete(
    client: DeepSeekClient,
    prefix: str,
    suffix: str,
    model: str = "deepseek-v4-flash",
    max_tokens: int = 4000,
    temperature: float = 0.0,
    stop: list[str] | None = None,
) -> str:
    """调用 DeepSeek beta `/completions` 端点做 FIM 补全。

    Args:
        client: 已有 DeepSeekClient。若 base_url 非 beta，自动包一层 beta client。
        prefix: 光标前的代码。
        suffix: 光标后的代码。
        model: 模型名，默认 v4-flash（FIM 推荐用 flash，便宜且足够）。
        max_tokens: 中间段最大 token。
        temperature: 采样温度，默认 0（确定性）。
        stop: 停止序列。

    Returns:
        模型补出来的中间文本（不含 prefix/suffix）。

    Raises:
        RuntimeError: SDK 调用失败或响应为空。
    """
    # 必须 beta 端点
    target = client
    owns_target = False
    if not client.base_url.rstrip("/").endswith("/beta"):
        target = DeepSeekClient(api_key=client.api_key, base_url=BETA_BASE_URL)
        owns_target = True

    req: dict[str, Any] = {
        "model": model,
        "prompt": prefix,
        "suffix": suffix,
        "max_tokens": max_tokens,
        "temperature": temperature,
    }
    if stop:
        req["stop"] = stop

    try:
        # openai SDK 的 .completions.create()——legacy completions 端点
        completion: Any = await target._client.completions.create(**req)  # type: ignore[attr-defined]
    finally:
        if owns_target:
            try:
                await target.close()
            except Exception:
                pass

    choices = getattr(completion, "choices", None) or []
    if not choices:
        return ""
    text = getattr(choices[0], "text", None)
    if text is None and hasattr(choices[0], "model_dump"):
        text = (choices[0].model_dump() or {}).get("text")
    return text or ""


__all__ = ["fim_complete"]
