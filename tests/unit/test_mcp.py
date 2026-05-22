"""MCP adapter 单元测试。

不启动真实 MCP server——全部使用 mock。覆盖：
- MCPClient: connect 成功 / SDK 缺失 / list_tools / call_tool / 错误路径 / close
- MCPRegistry: 配置加载 / 配置缺失 / 工具汇总 / handler 路由
- attach_mcp_to_registry: 注册数量 + 命名空间隔离
"""
from __future__ import annotations

import json
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest

from dscode.core.types import ToolResult, ToolStatus
from dscode.tools.mcp_adapter import (
    MCPClient,
    MCPRegistry,
    _qualified_tool_name,
    attach_mcp_to_registry,
)
from dscode.tools.registry import ToolRegistry

# ---------------------------------------------------------------------------
# Mock SDK helpers
# ---------------------------------------------------------------------------


class _MockTool:
    def __init__(self, name: str, description: str = "", schema: dict | None = None):
        self.name = name
        self.description = description
        self.inputSchema = schema or {"type": "object", "properties": {}}


class _MockListResult:
    def __init__(self, tools: list[_MockTool]):
        self.tools = tools


class _MockTextContent:
    def __init__(self, text: str):
        self.type = "text"
        self.text = text

    def model_dump(self) -> dict[str, Any]:
        return {"type": "text", "text": self.text}


class _MockCallResult:
    def __init__(self, text: str, is_error: bool = False):
        self.isError = is_error
        self.content = [_MockTextContent(text)]


class _MockSession:
    """模拟 mcp.ClientSession。"""

    def __init__(
        self,
        tools: list[_MockTool] | None = None,
        call_results: dict[str, _MockCallResult] | None = None,
        raise_on_initialize: bool = False,
    ) -> None:
        self._tools = tools or []
        self._call_results = call_results or {}
        self._raise_on_initialize = raise_on_initialize
        self.initialized = False
        self.closed = False
        self.calls: list[tuple[str, dict[str, Any]]] = []

    async def __aenter__(self) -> _MockSession:
        return self

    async def __aexit__(self, *args: Any) -> None:
        self.closed = True

    async def initialize(self) -> SimpleNamespace:
        if self._raise_on_initialize:
            raise RuntimeError("init failed")
        self.initialized = True
        return SimpleNamespace(protocolVersion="2024-11-05")

    async def list_tools(self) -> _MockListResult:
        return _MockListResult(self._tools)

    async def call_tool(self, name: str, args: dict[str, Any]) -> _MockCallResult:
        self.calls.append((name, args))
        if name in self._call_results:
            return self._call_results[name]
        return _MockCallResult(f"called {name}")


class _MockStdioCm:
    """模拟 mcp.client.stdio.stdio_client()。"""

    def __init__(self, fail: bool = False):
        self._fail = fail
        self.entered = False
        self.exited = False

    async def __aenter__(self) -> tuple[str, str]:
        if self._fail:
            raise RuntimeError("stdio failed")
        self.entered = True
        return ("READ", "WRITE")

    async def __aexit__(self, *args: Any) -> None:
        self.exited = True


def _make_mock_sdk(
    tools: list[_MockTool] | None = None,
    call_results: dict[str, _MockCallResult] | None = None,
    fail_stdio: bool = False,
    fail_init: bool = False,
) -> SimpleNamespace:
    """组装一个 mock 版的 lazy-import bundle。"""
    session = _MockSession(
        tools=tools,
        call_results=call_results,
        raise_on_initialize=fail_init,
    )
    stdio_cm = _MockStdioCm(fail=fail_stdio)

    def _stdio_client(params: Any) -> _MockStdioCm:
        return stdio_cm

    def _client_session(read: Any, write: Any) -> _MockSession:
        return session

    class _StdioParams:
        def __init__(self, command: str, args: list[str] | None = None, env: dict | None = None):
            self.command = command
            self.args = args
            self.env = env

    bundle = SimpleNamespace(
        mcp=SimpleNamespace(),
        ClientSession=_client_session,
        StdioServerParameters=_StdioParams,
        stdio_client=_stdio_client,
    )
    # 暴露给测试断言
    bundle._session = session
    bundle._stdio_cm = stdio_cm
    return bundle


# ---------------------------------------------------------------------------
# MCPClient
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_client_without_sdk_marks_unavailable():
    """SDK 不可用时 connect 直接失败但不抛异常。"""
    client = MCPClient(name="fs", command="echo", sdk=None)
    # 显式触发 sdk=None 分支（lazy import 已被 sdk 注入跳过）
    client._sdk = None
    ok = await client.connect()
    assert ok is False
    assert client.available is False


@pytest.mark.asyncio
async def test_client_connect_success_lists_tools():
    """连接成功后 list_tools 返回带命名空间的 ToolSpec。"""
    sdk = _make_mock_sdk(
        tools=[_MockTool("read_file", "读取文件", {"type": "object", "properties": {"path": {"type": "string"}}})],
    )
    client = MCPClient(name="fs", command="x", sdk=sdk)
    assert await client.connect() is True
    assert client.available is True

    specs = await client.list_tools()
    assert len(specs) == 1
    spec = specs[0]
    assert spec.name == "mcp__fs__read_file"
    assert spec.description == "读取文件"
    assert spec.parameters["properties"]["path"]["type"] == "string"
    assert spec.capability == "mcp:fs"


@pytest.mark.asyncio
async def test_client_connect_failure_returns_false():
    """stdio 启动失败时 connect 返回 False。"""
    sdk = _make_mock_sdk(fail_stdio=True)
    client = MCPClient(name="bad", command="x", sdk=sdk)
    ok = await client.connect()
    assert ok is False
    assert client.available is False


@pytest.mark.asyncio
async def test_client_initialize_failure_marks_unavailable():
    """initialize 抛错时 connect 返回 False。"""
    sdk = _make_mock_sdk(fail_init=True)
    client = MCPClient(name="bad", command="x", sdk=sdk)
    assert await client.connect() is False
    assert client.available is False


@pytest.mark.asyncio
async def test_client_call_tool_success_returns_text():
    """成功的 call_tool 把 TextContent 拼成 ToolResult.content。"""
    sdk = _make_mock_sdk(
        tools=[_MockTool("read_file")],
        call_results={"read_file": _MockCallResult("file contents here")},
    )
    client = MCPClient(name="fs", command="x", sdk=sdk)
    await client.connect()
    result = await client.call_tool("read_file", {"path": "/tmp/x"})
    assert isinstance(result, ToolResult)
    assert result.status == ToolStatus.SUCCESS
    assert "file contents here" in result.content
    assert result.metadata.get("mcp") is True
    # 透传到了 mock session
    assert sdk._session.calls == [("read_file", {"path": "/tmp/x"})]


@pytest.mark.asyncio
async def test_client_call_tool_error_flag_maps_to_error_status():
    """isError=True 应映射为 ToolStatus.ERROR。"""
    sdk = _make_mock_sdk(
        tools=[_MockTool("bad")],
        call_results={"bad": _MockCallResult("boom", is_error=True)},
    )
    client = MCPClient(name="fs", command="x", sdk=sdk)
    await client.connect()
    result = await client.call_tool("bad", {})
    assert result.status == ToolStatus.ERROR
    assert "boom" in (result.error or "")


@pytest.mark.asyncio
async def test_client_call_tool_when_unavailable_returns_error():
    """未连接时调用直接返回错误，不抛。"""
    sdk = _make_mock_sdk()
    client = MCPClient(name="fs", command="x", sdk=sdk)
    # 故意不 connect
    result = await client.call_tool("read_file", {})
    assert result.status == ToolStatus.ERROR
    assert "不可用" in (result.error or "")


@pytest.mark.asyncio
async def test_client_close_releases_resources():
    """close 后 stdio 与 session 都被 __aexit__。"""
    sdk = _make_mock_sdk()
    client = MCPClient(name="fs", command="x", sdk=sdk)
    await client.connect()
    await client.close()
    assert client.available is False
    assert sdk._stdio_cm.exited is True
    assert sdk._session.closed is True


# ---------------------------------------------------------------------------
# MCPRegistry
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_registry_load_config_from_file(tmp_path: Path):
    """配置文件存在时按 servers 数组生成 client。"""
    cfg = tmp_path / "mcp_servers.json"
    cfg.write_text(
        json.dumps(
            {
                "servers": [
                    {"name": "fs", "command": "npx", "args": ["-y", "x"]},
                    {"name": "git", "command": "uvx", "args": ["mcp-server-git"]},
                ]
            }
        ),
        encoding="utf-8",
    )
    reg = MCPRegistry.from_config(cfg)
    names = {c.name for c in reg.clients()}
    assert names == {"fs", "git"}


def test_registry_missing_config_silent(tmp_path: Path):
    """配置不存在时不抛异常，clients 为空。"""
    reg = MCPRegistry.from_config(tmp_path / "absent.json")
    assert reg.clients() == []


def test_registry_invalid_config_warns_only(tmp_path: Path):
    """非法 JSON 时 registry 仍然可用，没有 clients。"""
    cfg = tmp_path / "broken.json"
    cfg.write_text("{not valid json", encoding="utf-8")
    reg = MCPRegistry.from_config(cfg)
    assert reg.clients() == []


@pytest.mark.asyncio
async def test_registry_discover_all_aggregates_from_clients():
    """所有可用 client 的工具都被聚合。"""
    sdk_a = _make_mock_sdk(tools=[_MockTool("read", "r"), _MockTool("write", "w")])
    sdk_b = _make_mock_sdk(tools=[_MockTool("status", "s")])
    client_a = MCPClient(name="fs", command="x", sdk=sdk_a)
    client_b = MCPClient(name="git", command="y", sdk=sdk_b)
    reg = MCPRegistry(clients=[client_a, client_b])
    await reg.connect_all()
    specs = await reg.discover_all()
    names = {s.name for s in specs}
    assert names == {"mcp__fs__read", "mcp__fs__write", "mcp__git__status"}


@pytest.mark.asyncio
async def test_registry_unavailable_clients_excluded_from_discover():
    """连接失败的 client 不会出现在 discover_all。"""
    sdk_ok = _make_mock_sdk(tools=[_MockTool("ok")])
    sdk_bad = _make_mock_sdk(fail_stdio=True)
    client_ok = MCPClient(name="ok", command="x", sdk=sdk_ok)
    client_bad = MCPClient(name="bad", command="y", sdk=sdk_bad)
    reg = MCPRegistry(clients=[client_ok, client_bad])
    results = await reg.connect_all()
    assert results == {"ok": True, "bad": False}
    specs = await reg.discover_all()
    assert {s.name for s in specs} == {"mcp__ok__ok"}


@pytest.mark.asyncio
async def test_registry_make_handler_routes_call_to_client():
    """make_handler 返回的 handler 调用应路由到对应 client.call_tool。"""
    sdk = _make_mock_sdk(
        tools=[_MockTool("read_file")],
        call_results={"read_file": _MockCallResult("payload")},
    )
    client = MCPClient(name="fs", command="x", sdk=sdk)
    reg = MCPRegistry(clients=[client])
    await reg.connect_all()
    handler = reg.make_handler("fs", "read_file")
    result = await handler({"path": "/x"})
    assert result.status == ToolStatus.SUCCESS
    assert "payload" in result.content
    assert sdk._session.calls == [("read_file", {"path": "/x"})]


@pytest.mark.asyncio
async def test_registry_make_handler_unknown_server_returns_error():
    """未注册的 server 名调用应返回 ERROR，而非抛异常。"""
    reg = MCPRegistry(clients=[])
    handler = reg.make_handler("ghost", "anything")
    result = await handler({})
    assert result.status == ToolStatus.ERROR
    assert "未注册" in (result.error or "")


# ---------------------------------------------------------------------------
# attach_mcp_to_registry
# ---------------------------------------------------------------------------


@pytest.mark.asyncio
async def test_attach_mcp_to_registry_registers_all_available_tools():
    """attach 后 ToolRegistry 应能找到所有 MCP 工具的 spec + handler。"""
    sdk = _make_mock_sdk(
        tools=[_MockTool("read", "r"), _MockTool("write", "w")],
        call_results={"read": _MockCallResult("R")},
    )
    client = MCPClient(name="fs", command="x", sdk=sdk)
    mcp_reg = MCPRegistry(clients=[client])
    await mcp_reg.connect_all()

    tool_reg = ToolRegistry()
    count = await attach_mcp_to_registry(tool_reg, mcp_reg)
    assert count == 2

    names = {s.name for s in tool_reg.list_specs()}
    assert names == {"mcp__fs__read", "mcp__fs__write"}

    # 通过注册中心拿到 handler 并实际调用，路径正确
    handler = tool_reg.get_handler("mcp__fs__read")
    assert handler is not None
    result = await handler({"path": "/x"})
    assert result.status == ToolStatus.SUCCESS
    assert "R" in result.content


@pytest.mark.asyncio
async def test_attach_skips_unavailable_clients():
    """不可用的 server 不应被注册到 ToolRegistry。"""
    sdk_ok = _make_mock_sdk(tools=[_MockTool("ping")])
    sdk_bad = _make_mock_sdk(fail_stdio=True)
    client_ok = MCPClient(name="ok", command="x", sdk=sdk_ok)
    client_bad = MCPClient(name="bad", command="y", sdk=sdk_bad)
    mcp_reg = MCPRegistry(clients=[client_ok, client_bad])
    await mcp_reg.connect_all()

    tool_reg = ToolRegistry()
    count = await attach_mcp_to_registry(tool_reg, mcp_reg)
    assert count == 1
    assert {s.name for s in tool_reg.list_specs()} == {"mcp__ok__ping"}


def test_qualified_name_avoids_native_collision():
    """命名空间前缀必须与原生 do_* 不冲突。"""
    name = _qualified_tool_name("fs", "read_file")
    assert name.startswith("mcp__")
    assert "fs" in name
    assert "read_file" in name
    assert not name.startswith("do_")
