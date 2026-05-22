"""Fail-closed 工具调用预检。"""
from __future__ import annotations

from typing import Any

from dscode.core.types import SafetyDecision, ToolRegistryProtocol


def fail_closed_check(
    tool_name: str,
    args: dict[str, Any],
    registry: ToolRegistryProtocol,
) -> SafetyDecision:
    """工具未注册或参数不完整 -> 拒绝。

    简单 required 字段检查；完整 JSON Schema 校验 TODO。
    """
    # 工具必须注册
    if registry.get_handler(tool_name) is None:
        return SafetyDecision(
            allowed=False,
            denied=True,
            reason=f"tool not registered: {tool_name}",
        )

    # 拿 spec 做 required 校验
    spec = None
    for s in registry.list_specs():
        if s.name == tool_name:
            spec = s
            break

    if spec is None:
        return SafetyDecision(
            allowed=False,
            denied=True,
            reason=f"spec not found: {tool_name}",
        )

    params_schema = spec.parameters or {}
    required = params_schema.get("required", [])
    if not isinstance(required, list):
        required = []

    missing = [r for r in required if r not in args]
    if missing:
        return SafetyDecision(
            allowed=False,
            denied=True,
            reason=f"missing required args for {tool_name}: {missing}",
        )

    return SafetyDecision(allowed=True)
