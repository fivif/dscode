"""do_lsp_query —— LSP 查询占位实现。

v1: 用 Python 标准库 ast 解析，返回模块顶层符号。
v2 TODO: 接 pyright / pylsp，支持跨语言。
"""
from __future__ import annotations

import ast
import asyncio
import os
import time
from typing import Any

from dscode.core.types import ToolResult, ToolSpec, ToolStatus

SPEC = ToolSpec(
    name="do_lsp_query",
    description=(
        "Query top-level symbols of a Python module via AST. "
        "Returns functions / classes / module-level assignments. "
        "v2 will route to pyright / pylsp for cross-language queries."
    ),
    parameters={
        "type": "object",
        "properties": {
            "path": {"type": "string", "description": "Path to a .py file."},
            "kind": {
                "type": "string",
                "enum": ["all", "functions", "classes", "assignments"],
                "description": "Filter by symbol kind.",
                "default": "all",
            },
        },
        "required": ["path"],
    },
    capability="code_intel",
    timeout_s=15,
)


def _sync_parse(path: str) -> list[dict[str, Any]]:
    with open(path, encoding="utf-8") as f:
        source = f.read()
    tree = ast.parse(source, filename=path)
    symbols: list[dict[str, Any]] = []
    for node in tree.body:
        if isinstance(node, ast.FunctionDef | ast.AsyncFunctionDef):
            symbols.append(
                {
                    "kind": "function",
                    "name": node.name,
                    "line": node.lineno,
                    "is_async": isinstance(node, ast.AsyncFunctionDef),
                }
            )
        elif isinstance(node, ast.ClassDef):
            methods = [
                m.name
                for m in node.body
                if isinstance(m, ast.FunctionDef | ast.AsyncFunctionDef)
            ]
            symbols.append(
                {
                    "kind": "class",
                    "name": node.name,
                    "line": node.lineno,
                    "methods": methods,
                }
            )
        elif isinstance(node, ast.Assign):
            for target in node.targets:
                if isinstance(target, ast.Name):
                    symbols.append(
                        {
                            "kind": "assignment",
                            "name": target.id,
                            "line": node.lineno,
                        }
                    )
        elif isinstance(node, ast.AnnAssign) and isinstance(node.target, ast.Name):
            symbols.append(
                {
                    "kind": "assignment",
                    "name": node.target.id,
                    "line": node.lineno,
                    "annotated": True,
                }
            )
    return symbols


async def handler(args: dict[str, Any]) -> ToolResult:
    started = time.time()
    path = args.get("path")
    if not isinstance(path, str) or not path:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error="path is required",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    kind = args.get("kind", "all")
    if kind not in ("all", "functions", "classes", "assignments"):
        kind = "all"

    if not os.path.exists(path):
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"file not found: {path}",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    if not path.endswith(".py"):
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error="v1 supports .py files only (pyright/pylsp coming in v2)",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    try:
        symbols = await asyncio.to_thread(_sync_parse, path)
    except SyntaxError as e:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"syntax error in {path}: {e}",
            elapsed_ms=int((time.time() - started) * 1000),
        )
    except OSError as e:
        return ToolResult(
            status=ToolStatus.ERROR,
            content="",
            error=f"read failed: {e}",
            elapsed_ms=int((time.time() - started) * 1000),
        )

    if kind != "all":
        target_kind = {
            "functions": "function",
            "classes": "class",
            "assignments": "assignment",
        }[kind]
        symbols = [s for s in symbols if s["kind"] == target_kind]

    lines = [f"{s['kind']:>10}  line {s['line']:>4}  {s['name']}" for s in symbols]
    body = "\n".join(lines) if lines else "(no symbols)"

    return ToolResult(
        status=ToolStatus.SUCCESS,
        content=body,
        elapsed_ms=int((time.time() - started) * 1000),
        metadata={"path": path, "symbol_count": len(symbols), "kind_filter": kind},
    )
