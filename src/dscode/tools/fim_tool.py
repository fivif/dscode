"""do_fim_complete —— FIM 代码补全工具。

把 DeepSeek beta FIM 端点暴露给 Agent 工具循环，专门用于"光标处自动补全 /
精确插入 / 自动重构"场景。与 do_edit / do_patch 是互补关系——FIM 给的是
模型生成的"中间段"，由上层决定如何与原文件组合。
"""
from __future__ import annotations

import time
from typing import Any

from dscode.core.types import ToolHandler, ToolResult, ToolSpec, ToolStatus
from dscode.deepseek.client import DeepSeekClient
from dscode.deepseek.fim import fim_complete


def make_fim_tool(client: DeepSeekClient) -> tuple[ToolSpec, ToolHandler]:
    """构造 do_fim_complete 工具的 (spec, handler) 对。

    工厂模式而非全局单例：每个 Agent 实例可注入自己的 DeepSeekClient
    （便于切端点 / 注入凭证 / 测试 mock）。

    Args:
        client: 已构造好的 DeepSeekClient。

    Returns:
        (ToolSpec, ToolHandler)——直接 `registry.register(spec, handler)`。
    """
    spec = ToolSpec(
        name="do_fim_complete",
        description=(
            "Fill-in-the-middle code completion via DeepSeek beta endpoint. "
            "Given a prefix (code BEFORE the cursor) and a suffix (code AFTER "
            "the cursor), returns the middle segment to be inserted. Ideal for "
            "automated refactoring, template filling, and cursor-position "
            "completion. Faster and more precise than chat-based continuation."
        ),
        parameters={
            "type": "object",
            "properties": {
                "prefix": {
                    "type": "string",
                    "description": "Code BEFORE the cursor / insertion point.",
                },
                "suffix": {
                    "type": "string",
                    "description": "Code AFTER the cursor / insertion point.",
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override; defaults to deepseek-v4-flash.",
                    "default": "deepseek-v4-flash",
                },
                "max_tokens": {
                    "type": "integer",
                    "description": "Maximum tokens for the middle segment.",
                    "default": 4000,
                },
            },
            "required": ["prefix", "suffix"],
        },
        capability="code_complete",
        timeout_s=60,
    )

    async def handler(args: dict[str, Any]) -> ToolResult:
        prefix = args.get("prefix")
        suffix = args.get("suffix")
        if not isinstance(prefix, str) or not isinstance(suffix, str):
            return ToolResult(
                status=ToolStatus.ERROR,
                content="",
                error="do_fim_complete: 'prefix' 和 'suffix' 必须为字符串。",
            )
        model = args.get("model") or "deepseek-v4-flash"
        max_tokens = int(args.get("max_tokens") or 4000)

        start = time.perf_counter()
        try:
            middle = await fim_complete(
                client=client,
                prefix=prefix,
                suffix=suffix,
                model=model,
                max_tokens=max_tokens,
            )
        except Exception as exc:
            # 工具层统一捕获，避免异常往主循环冒泡
            elapsed_ms = int((time.perf_counter() - start) * 1000)
            return ToolResult(
                status=ToolStatus.ERROR,
                content="",
                error=f"fim_complete 调用失败: {type(exc).__name__}: {exc}",
                elapsed_ms=elapsed_ms,
            )
        elapsed_ms = int((time.perf_counter() - start) * 1000)
        return ToolResult(
            status=ToolStatus.SUCCESS,
            content=middle,
            elapsed_ms=elapsed_ms,
            metadata={
                "model": model,
                "prefix_chars": len(prefix),
                "suffix_chars": len(suffix),
                "middle_chars": len(middle),
            },
        )

    return spec, handler


__all__ = ["make_fim_tool"]
