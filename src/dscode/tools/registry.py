"""工具注册中心。

符合 ToolRegistryProtocol，支持注册 ToolSpec+ToolHandler，
导出 OpenAI tools 数组。
"""
from __future__ import annotations

from typing import Any

from dscode.core.types import ToolHandler, ToolSpec


class ToolRegistry:
    """工具注册中心。

    命名约定路由：tool spec.name 形如 'do_grep'，handler 自动绑定。
    """

    def __init__(self) -> None:
        self._specs: dict[str, ToolSpec] = {}
        self._handlers: dict[str, ToolHandler] = {}

    def register(self, spec: ToolSpec, handler: ToolHandler) -> None:
        """注册一个工具。同名重复注册会覆盖。"""
        self._specs[spec.name] = spec
        self._handlers[spec.name] = handler

    def list_specs(self) -> list[ToolSpec]:
        return list(self._specs.values())

    def get_handler(self, name: str) -> ToolHandler | None:
        return self._handlers.get(name)

    def get_spec(self, name: str) -> ToolSpec | None:
        return self._specs.get(name)

    def to_openai_tools(self) -> list[dict[str, Any]]:
        """转为 OpenAI tools 格式。"""
        return [
            {
                "type": "function",
                "function": {
                    "name": spec.name,
                    "description": spec.description,
                    "parameters": spec.parameters,
                },
            }
            for spec in self._specs.values()
        ]
