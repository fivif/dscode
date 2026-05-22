"""MCP（Model Context Protocol）一等公民适配器。

把外部 MCP server 暴露的 tools 转换成 dscode 原生的 ToolSpec/ToolHandler，
注册到 ToolRegistry 中——LLM 不感知调用的是原生工具还是 MCP 工具。

设计要点：

1. **Lazy import**：`mcp` SDK 在 `[mcp]` extra 中可选。若未安装，本模块仍可
   import，但 ``MCPRegistry.connect_all`` 会直接将所有 server 标记为不可用，
   主流程不受影响。
2. **Fail open**：单个 MCP server 启动失败、ping 失败、调用超时都不会抛进
   ToolRegistry——只会让对应工具不可用并记 warning。
3. **命名空间隔离**：MCP 工具注册名形如 ``mcp__<server>__<tool>``，避免和原
   生 ``do_*`` 撞名。
4. **JSON Schema 透传**：直接把 server 返回的 ``inputSchema`` 透传给
   ToolSpec.parameters，让 LLM 看到完整签名。

配置文件 ``~/.dscode/mcp_servers.json``::

    {
        "servers": [
            {
                "name": "filesystem",
                "command": "npx",
                "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
                "env": {"DEBUG": "0"}
            }
        ]
    }
"""
from __future__ import annotations

import asyncio
import json
import logging
from pathlib import Path
from typing import TYPE_CHECKING, Any

from dscode.core.types import (
    ToolHandler,
    ToolRegistryProtocol,
    ToolResult,
    ToolSpec,
    ToolStatus,
)

if TYPE_CHECKING:
    # 仅类型检查时引入，避免运行时强依赖 mcp SDK
    from mcp import ClientSession  # noqa: F401

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# 常量
# ---------------------------------------------------------------------------

DEFAULT_CONFIG_PATH = Path.home() / ".dscode" / "mcp_servers.json"
TOOL_NAME_PREFIX = "mcp__"
TOOL_NAME_SEP = "__"
DEFAULT_CONNECT_TIMEOUT_S = 5.0
DEFAULT_CALL_TIMEOUT_S = 60.0


def _qualified_tool_name(server_name: str, tool_name: str) -> str:
    """生成命名空间隔离的工具注册名。"""
    return f"{TOOL_NAME_PREFIX}{server_name}{TOOL_NAME_SEP}{tool_name}"


# ---------------------------------------------------------------------------
# 可选 SDK 加载
# ---------------------------------------------------------------------------


def _try_import_mcp() -> Any:
    """Lazy import mcp SDK。返回带有所需符号的 namespace 对象，失败时返回 None。"""
    try:
        import mcp as _mcp
        from mcp import ClientSession, StdioServerParameters
        from mcp.client.stdio import stdio_client
    except Exception as exc:  # pragma: no cover - 仅 SDK 缺失时触发
        logger.warning("mcp SDK 不可用 (%s)，所有 MCP server 将被禁用", exc)
        return None

    class _Bundle:
        pass

    bundle = _Bundle()
    bundle.mcp = _mcp
    bundle.ClientSession = ClientSession
    bundle.StdioServerParameters = StdioServerParameters
    bundle.stdio_client = stdio_client
    return bundle


# ---------------------------------------------------------------------------
# MCPClient
# ---------------------------------------------------------------------------


class MCPClient:
    """单个 MCP server 的客户端封装。

    生命周期：``connect()`` -> ``list_tools()`` / ``call_tool(...)`` -> ``close()``

    任何阶段失败都会把 ``available`` 翻成 False，但不会抛异常打断主流程。
    """

    def __init__(
        self,
        name: str,
        command: str,
        args: list[str] | None = None,
        env: dict[str, str] | None = None,
        connect_timeout_s: float = DEFAULT_CONNECT_TIMEOUT_S,
        call_timeout_s: float = DEFAULT_CALL_TIMEOUT_S,
        sdk: Any | None = None,
    ) -> None:
        self.name = name
        self.command = command
        self.args = list(args or [])
        self.env = dict(env or {})
        self.connect_timeout_s = connect_timeout_s
        self.call_timeout_s = call_timeout_s

        # SDK 注入：测试时可传入 mock；运行时为 None 表示走默认 lazy import
        self._sdk = sdk if sdk is not None else _try_import_mcp()

        self.available: bool = False
        self._session: Any | None = None
        self._tools_cache: list[ToolSpec] | None = None

        # 用于关闭传输 / 会话 contextmanager
        self._stdio_cm: Any | None = None
        self._session_cm: Any | None = None

    # ------------------------------------------------------------------
    # 连接
    # ------------------------------------------------------------------

    async def connect(self) -> bool:
        """连接 MCP server。返回是否成功（同时维护 ``available``）。"""
        if self._sdk is None:
            logger.warning("MCPClient(%s) 跳过连接：mcp SDK 不可用", self.name)
            self.available = False
            return False

        sdk = self._sdk
        try:
            params = sdk.StdioServerParameters(
                command=self.command,
                args=self.args,
                env=self.env or None,
            )
            self._stdio_cm = sdk.stdio_client(params)
            read, write = await asyncio.wait_for(
                self._stdio_cm.__aenter__(),
                timeout=self.connect_timeout_s,
            )
            self._session_cm = sdk.ClientSession(read, write)
            self._session = await self._session_cm.__aenter__()
            await asyncio.wait_for(
                self._session.initialize(),
                timeout=self.connect_timeout_s,
            )
        except Exception as exc:
            logger.warning("MCPClient(%s) 连接失败: %s", self.name, exc)
            await self._safe_close()
            self.available = False
            return False

        self.available = True
        return True

    # ------------------------------------------------------------------
    # tools
    # ------------------------------------------------------------------

    async def list_tools(self) -> list[ToolSpec]:
        """列出 server 暴露的工具，已转换为 ToolSpec。

        若未连接或调用失败，返回空列表并把 ``available`` 翻成 False。
        """
        if not self.available or self._session is None:
            return []
        if self._tools_cache is not None:
            return self._tools_cache
        try:
            result = await asyncio.wait_for(
                self._session.list_tools(),
                timeout=self.connect_timeout_s,
            )
        except Exception as exc:
            logger.warning("MCPClient(%s) list_tools 失败: %s", self.name, exc)
            self.available = False
            return []

        specs: list[ToolSpec] = []
        for tool in getattr(result, "tools", []) or []:
            try:
                specs.append(self._tool_to_spec(tool))
            except Exception as exc:  # 单个 tool 解析失败不阻塞其余
                logger.warning(
                    "MCPClient(%s) tool '%s' 解析失败: %s",
                    self.name,
                    getattr(tool, "name", "?"),
                    exc,
                )
        self._tools_cache = specs
        return specs

    def _tool_to_spec(self, tool: Any) -> ToolSpec:
        raw_schema = (
            getattr(tool, "inputSchema", None)
            or getattr(tool, "input_schema", None)
            or {"type": "object", "properties": {}}
        )
        if not isinstance(raw_schema, dict):
            # pydantic 模型场景
            try:
                raw_schema = raw_schema.model_dump()
            except Exception:
                raw_schema = {"type": "object", "properties": {}}
        return ToolSpec(
            name=_qualified_tool_name(self.name, tool.name),
            description=(getattr(tool, "description", "") or f"MCP tool from {self.name}"),
            parameters=raw_schema,
            capability=f"mcp:{self.name}",
            requires_confirmation=False,
            timeout_s=int(self.call_timeout_s),
        )

    # ------------------------------------------------------------------
    # call
    # ------------------------------------------------------------------

    async def call_tool(self, name: str, args: dict[str, Any]) -> ToolResult:
        """异步调用 server 上的一个工具，返回标准 ToolResult。"""
        if not self.available or self._session is None:
            return ToolResult(
                status=ToolStatus.ERROR,
                content="",
                error=f"MCP server '{self.name}' 不可用",
            )
        try:
            raw = await asyncio.wait_for(
                self._session.call_tool(name, args),
                timeout=self.call_timeout_s,
            )
        except TimeoutError:
            return ToolResult(
                status=ToolStatus.TIMEOUT,
                content="",
                error=f"MCP {self.name}.{name} 超时（>{self.call_timeout_s}s）",
            )
        except Exception as exc:
            return ToolResult(
                status=ToolStatus.ERROR,
                content="",
                error=f"MCP {self.name}.{name} 调用异常: {exc}",
            )

        return self._mcp_result_to_tool_result(raw)

    @staticmethod
    def _mcp_result_to_tool_result(raw: Any) -> ToolResult:
        """把 mcp.types.CallToolResult 折叠为 dscode ToolResult。"""
        is_error = bool(getattr(raw, "isError", False))
        contents = getattr(raw, "content", []) or []
        parts: list[str] = []
        for item in contents:
            text = getattr(item, "text", None)
            if text is not None:
                parts.append(str(text))
                continue
            # 其余类型（image / resource）退化为 JSON 描述
            try:
                parts.append(json.dumps(item.model_dump(), ensure_ascii=False))
            except Exception:
                parts.append(repr(item))
        content_text = "\n".join(parts)
        status = ToolStatus.ERROR if is_error else ToolStatus.SUCCESS
        return ToolResult(
            status=status,
            content=content_text,
            error=content_text if is_error else None,
            metadata={"mcp": True},
        )

    # ------------------------------------------------------------------
    # close
    # ------------------------------------------------------------------

    async def close(self) -> None:
        await self._safe_close()
        self.available = False

    async def _safe_close(self) -> None:
        if self._session_cm is not None:
            try:
                await self._session_cm.__aexit__(None, None, None)
            except Exception as exc:  # pragma: no cover
                logger.debug("MCPClient(%s) session close 警告: %s", self.name, exc)
            self._session_cm = None
            self._session = None
        if self._stdio_cm is not None:
            try:
                await self._stdio_cm.__aexit__(None, None, None)
            except Exception as exc:  # pragma: no cover
                logger.debug("MCPClient(%s) stdio close 警告: %s", self.name, exc)
            self._stdio_cm = None


# ---------------------------------------------------------------------------
# MCPRegistry
# ---------------------------------------------------------------------------


class MCPRegistry:
    """多 MCPClient 的协调器。

    职责：
    - 读 ``~/.dscode/mcp_servers.json`` 配置
    - 启动并 ping 所有 server，标记可用 / 不可用
    - 汇总所有可用工具的 ToolSpec
    - 生成符合 ToolHandler 协议的 handler，把调用路由回对应 client
    """

    def __init__(
        self,
        config_path: Path | None = None,
        clients: list[MCPClient] | None = None,
    ) -> None:
        self.config_path = config_path if config_path is not None else DEFAULT_CONFIG_PATH
        self._clients: dict[str, MCPClient] = {}
        if clients:
            for c in clients:
                self._clients[c.name] = c

    # ------------------------------------------------------------------
    # 配置
    # ------------------------------------------------------------------

    @classmethod
    def from_config(cls, config_path: Path | None = None) -> MCPRegistry:
        """从 JSON 配置创建 registry，不立即连接。"""
        reg = cls(config_path=config_path)
        reg.load_config()
        return reg

    def load_config(self) -> None:
        """从 ``self.config_path`` 读 server 列表。文件缺失则不抛异常。"""
        if not self.config_path.exists():
            logger.info("MCP 配置不存在 (%s)，跳过加载", self.config_path)
            return
        try:
            payload = json.loads(self.config_path.read_text(encoding="utf-8"))
        except Exception as exc:
            logger.warning("MCP 配置解析失败 (%s): %s", self.config_path, exc)
            return
        servers = payload.get("servers") if isinstance(payload, dict) else None
        if not isinstance(servers, list):
            logger.warning("MCP 配置 'servers' 字段缺失或非数组")
            return
        for entry in servers:
            if not isinstance(entry, dict):
                continue
            name = entry.get("name")
            command = entry.get("command")
            if not name or not command:
                logger.warning("MCP server 配置缺少 name/command: %s", entry)
                continue
            client = MCPClient(
                name=name,
                command=command,
                args=entry.get("args") or [],
                env=entry.get("env") or {},
            )
            self._clients[name] = client

    # ------------------------------------------------------------------
    # 连接 + 发现
    # ------------------------------------------------------------------

    def clients(self) -> list[MCPClient]:
        return list(self._clients.values())

    def get_client(self, name: str) -> MCPClient | None:
        return self._clients.get(name)

    async def connect_all(self) -> dict[str, bool]:
        """并发连接所有 client，返回每个 server 的连接结果。"""
        if not self._clients:
            return {}
        names = list(self._clients.keys())
        results = await asyncio.gather(
            *(self._clients[n].connect() for n in names),
            return_exceptions=True,
        )
        out: dict[str, bool] = {}
        for name, res in zip(names, results, strict=True):
            if isinstance(res, Exception):
                logger.warning("MCP server '%s' 连接异常: %s", name, res)
                self._clients[name].available = False
                out[name] = False
            else:
                out[name] = bool(res)
        return out

    async def discover_all(self) -> list[ToolSpec]:
        """收集所有可用 MCP 工具的 ToolSpec。"""
        specs: list[ToolSpec] = []
        for client in self._clients.values():
            if not client.available:
                continue
            specs.extend(await client.list_tools())
        return specs

    async def close_all(self) -> None:
        for client in self._clients.values():
            await client.close()

    # ------------------------------------------------------------------
    # handler 工厂
    # ------------------------------------------------------------------

    def make_handler(self, server_name: str, tool_name: str) -> ToolHandler:
        """生成符合 ``ToolHandler`` 协议的可调用对象。

        调用时若 server 已掉线，返回 ToolStatus.ERROR 而非抛异常。
        """

        async def _handler(args: dict[str, Any]) -> ToolResult:
            client = self._clients.get(server_name)
            if client is None:
                return ToolResult(
                    status=ToolStatus.ERROR,
                    content="",
                    error=f"MCP server '{server_name}' 未注册",
                )
            if not client.available:
                return ToolResult(
                    status=ToolStatus.ERROR,
                    content="",
                    error=f"MCP server '{server_name}' 不可用",
                )
            return await client.call_tool(tool_name, args or {})

        _handler.__name__ = _qualified_tool_name(server_name, tool_name)
        return _handler


# ---------------------------------------------------------------------------
# 注册到 ToolRegistry
# ---------------------------------------------------------------------------


async def attach_mcp_to_registry(
    registry: ToolRegistryProtocol,
    mcp_registry: MCPRegistry,
) -> int:
    """把所有可用 MCP 工具注册进 ToolRegistry，返回成功注册的数量。

    在主流程启动时调用一次即可。后续 LLM 看到的工具表就包含 MCP 工具，
    调用路径与原生工具完全一致。
    """
    count = 0
    for client in mcp_registry.clients():
        if not client.available:
            continue
        specs = await client.list_tools()
        for spec in specs:
            # spec.name 已是 mcp__<server>__<tool>，从中拆出 tool_name
            tool_name = spec.name[len(_qualified_tool_name(client.name, "")):]
            handler = mcp_registry.make_handler(client.name, tool_name)
            registry.register(spec, handler)
            count += 1
    return count


__all__ = [
    "DEFAULT_CONFIG_PATH",
    "TOOL_NAME_PREFIX",
    "MCPClient",
    "MCPRegistry",
    "attach_mcp_to_registry",
]
